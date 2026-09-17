//! The coin acceptor: six parallel channels and an inhibit line.
//!
//! The acceptor decides for itself what a coin is; all it says is which of its six channels the
//! coin belonged to, by pulsing that channel's line. What the channel is *worth* is a setting, so
//! the same machine takes different money by being reconfigured rather than reflashed.
//!
//! Port 7 cannot interrupt on this device — only P1 and P2 can — so the lines are polled. That is
//! not a compromise: a coin pulse is tens of milliseconds and the poll is five, so no coin can slip
//! between samples, and a channel that has stuck active is noticed rather than counted repeatedly.

use embassy_msp430::gpio::{Input, Output};
use embassy_time::{Duration, Instant, Ticker};

use crate::config::COIN_CHANNELS;
use crate::error::{self, Errors};
use crate::event::{Event, EventSender};
use crate::state::CoinAcceptorSettings;

/// Whether something other than the settings currently wants the acceptor shut.
///
/// The service menu raises this so that a coin cannot land in the middle of a refill. It is an
/// atomic rather than a message because the acceptor task must see it at its next poll, not at its
/// next chance to receive.
static INHIBITED: portable_atomic::AtomicBool = portable_atomic::AtomicBool::new(false);

/// Ask the acceptor to stop, or let it start again.
pub fn set_inhibited(on: bool) {
    INHIBITED.store(on, core::sync::atomic::Ordering::Relaxed);
}

/// How often the channel lines are read.
const POLL_MS: u64 = 5;

/// How long a pulse must be to be a coin rather than a glitch on the harness.
const PULSE_MIN_MS: u64 = 10;

/// How long a channel may stay active before the acceptor is called faulty.
///
/// A real coin pulse is well under this. A line held down for a second means a shorted harness or a
/// dead acceptor, and counting one coin per poll for as long as it lasted would be a gift.
const PULSE_MAX_MS: u64 = 1_000;

// ---------------------------------------------------------------------------------------------
// Pulse mode
//
// A second, mutually exclusive acceptor protocol, selected at runtime by
// `CoinAcceptorSettings::pulse_mode` (see `state.rs`) — the same field the C original toggles at
// `bah_settings.c`/`SetCoinAcceptorPulseMode`. Instead of one of six parallel lines carrying a coin,
// all six lines are OR-ed together and the coin's *value* is the number of pulses in one burst: the
// Nth configured denomination is signalled by N pulses, separated by less than
// `PULSE_INTER_MAX_MS` from one another, so that a run of pulses can be told apart from two separate
// coins arriving close together.
//
// The four timing constants below are taken directly from the C original's
// `coin_acceptor.h` (`COIN_ACCEPTOR_PULSE_MIN/MAX`, `COIN_ACCEPTOR_INTER_PULSE_MAX`), which are 1 ms
// system ticks there (`timer.c`) just as they are milliseconds here — no unit conversion needed.
// ---------------------------------------------------------------------------------------------

/// Shortest a single pulse may be to count as a pulse rather than noise. `COIN_ACCEPTOR_PULSE_MIN`.
const PULSE_MODE_PULSE_MIN_MS: u64 = 20;
/// Longest a single pulse may be before the burst is abandoned as a malfunction.
/// `COIN_ACCEPTOR_PULSE_MAX`.
const PULSE_MODE_PULSE_MAX_MS: u64 = 250;
/// How long the line may sit released between pulses of the same burst before the burst is taken
/// to have ended and the accumulated pulse count reported as a coin. `COIN_ACCEPTOR_INTER_PULSE_MAX`.
const PULSE_MODE_INTER_PULSE_MAX_MS: u64 = 200;

/// The lines the acceptor is wired to.
pub struct CoinAcceptor<'d> {
    /// One per channel, in channel order. Active low.
    channels: [Input<'d>; COIN_CHANNELS],
    /// Driven high to stop the acceptor taking money.
    inhibit: Output<'d>,
}

impl<'d> CoinAcceptor<'d> {
    /// Take the lines. The acceptor starts inhibited, so no coin can be taken before the machine
    /// knows whether it is fit to trade.
    pub fn new(channels: [Input<'d>; COIN_CHANNELS], mut inhibit: Output<'d>) -> Self {
        inhibit.set_high();
        error::raise(Errors::COIN_ACCEPTOR_OFF);
        Self { channels, inhibit }
    }

    /// Let the acceptor take money, or stop it.
    pub fn set_enabled(&mut self, on: bool) {
        if on {
            self.inhibit.set_low();
        } else {
            self.inhibit.set_high();
        }
        error::set(Errors::COIN_ACCEPTOR_OFF, !on);
    }
}

/// What one channel is in the middle of.
#[derive(Copy, Clone)]
enum Pulse {
    /// Line released.
    Idle,
    /// Line active since `since`. `counted` records whether the coin has already been reported,
    /// which is what stops one long pulse being counted at every poll.
    Active {
        /// When the line went active.
        since: Instant,
        /// Whether this pulse has already produced a coin.
        counted: bool,
    },
    /// Line active far longer than any coin takes, and already reported as a fault.
    Stuck,
}

/// Watch the channels and report coins.
///
/// A coin is counted on the way *in*, once the pulse has lasted long enough to be real, rather than
/// when the line is released. A customer who puts in the last coin and immediately reaches for the
/// button should not have to wait for the acceptor to finish its pulse.
#[embassy_executor::task]
pub async fn coin_task(
    mut acceptor: CoinAcceptor<'static>,
    settings: CoinAcceptorSettings,
    events: EventSender,
) {
    acceptor.set_enabled(settings.enabled);
    let mut enabled = settings.enabled;

    if settings.pulse_mode {
        run_pulse_mode(&mut acceptor, &settings, &events, &mut enabled).await;
    } else {
        run_parallel_mode(&mut acceptor, &settings, &events, &mut enabled).await;
    }
}

/// Update whether the acceptor is allowed to take money, from the same conditions both modes use.
fn update_enabled(acceptor: &mut CoinAcceptor<'static>, settings: &CoinAcceptorSettings, enabled: &mut bool) {
    let wanted = settings.enabled
        && !INHIBITED.load(core::sync::atomic::Ordering::Relaxed)
        && error::errors().can_vend();
    if wanted != *enabled {
        acceptor.set_enabled(wanted);
        *enabled = wanted;
    }
}

/// One line per channel, matching the C original's `processNormalMode` (`coin_acceptor.c:37-82`).
async fn run_parallel_mode(
    acceptor: &mut CoinAcceptor<'static>,
    settings: &CoinAcceptorSettings,
    events: &EventSender,
    enabled: &mut bool,
) -> ! {
    let mut pulses = [Pulse::Idle; COIN_CHANNELS];
    let mut ticker = Ticker::every(Duration::from_millis(POLL_MS));

    loop {
        ticker.next().await;
        update_enabled(acceptor, settings, enabled);

        let now = Instant::now();
        let mut any_stuck = false;

        for (i, line) in acceptor.channels.iter().enumerate() {
            if !line.is_low() {
                pulses[i] = Pulse::Idle;
                continue;
            }

            pulses[i] = match pulses[i] {
                Pulse::Idle => Pulse::Active {
                    since: now,
                    counted: false,
                },
                Pulse::Active { since, counted } => {
                    let held = (now - since).as_millis();
                    if held >= PULSE_MAX_MS {
                        Pulse::Stuck
                    } else if !counted && held >= PULSE_MIN_MS {
                        // Channels the settings do not believe are still watched, so a
                        // misconfigured machine reports a stuck line rather than ignoring it, but
                        // no money is credited for them.
                        if settings.channel_mask & (1 << i) != 0 {
                            let _ = events.try_send(Event::Coin {
                                channel: i as u8,
                                value: settings.channel_values[i],
                            });
                        }
                        Pulse::Active {
                            since,
                            counted: true,
                        }
                    } else {
                        Pulse::Active { since, counted }
                    }
                }
                Pulse::Stuck => Pulse::Stuck,
            };

            any_stuck |= matches!(pulses[i], Pulse::Stuck);
        }

        error::set(Errors::COIN_ACCEPTOR, any_stuck);
    }
}

/// Where a pulse-mode burst currently is.
///
/// Mirrors the C original's `coin_process_stage` (`ACCEPTOR_IDLE`/`ACCEPTOR_ACCEPT`/
/// `ACCEPTOR_POST_ACCEPT`, `coin_acceptor.c:84-132`) — there is no `ACCEPTOR_PRE_ACCEPT` here because
/// that stage belongs only to the parallel-channel state machine.
enum PulseModeStage {
    /// Waiting for the burst to start.
    Idle,
    /// A pulse is in progress; waiting to see whether it is real or noise.
    Pulsing {
        /// When the line went active for this pulse.
        since: Instant,
    },
    /// Between pulses of the same burst, waiting to see whether another pulse follows or the burst
    /// has ended.
    Gap {
        /// When the line released.
        since: Instant,
        /// Pulses counted in this burst so far.
        count: u8,
    },
}

/// All six lines OR-ed together, matching the C original's `COIN_ACCEPTOR_ANY_CH_LO/HI_STATE`.
fn any_channel_low(acceptor: &CoinAcceptor<'static>) -> bool {
    acceptor.channels.iter().any(|c| c.is_low())
}

/// Pulse counting on one shared line, matching the C original's `processPulseMode`
/// (`coin_acceptor.c:84-132`). `N` pulses in one burst is the `N`th configured denomination —
/// `channel_values[pulse_count - 1]` here, where the C original returns the bitmask `1<<(N-1)` for
/// `ProcessCoinAcceptor` (`bah_io.c:1173-1196`) to turn back into a channel index; doing the
/// subtraction once here is equivalent and avoids reintroducing that indirection.
async fn run_pulse_mode(
    acceptor: &mut CoinAcceptor<'static>,
    settings: &CoinAcceptorSettings,
    events: &EventSender,
    enabled: &mut bool,
) -> ! {
    let mut stage = PulseModeStage::Idle;
    let mut ticker = Ticker::every(Duration::from_millis(POLL_MS));

    loop {
        ticker.next().await;
        update_enabled(acceptor, settings, enabled);

        let now = Instant::now();
        let active = any_channel_low(acceptor);
        let mut malfunction = false;

        stage = match stage {
            PulseModeStage::Idle => {
                if active {
                    PulseModeStage::Pulsing { since: now }
                } else {
                    PulseModeStage::Idle
                }
            }
            PulseModeStage::Pulsing { since } => {
                if active {
                    // Still pulsing. A pulse held longer than the maximum is a malfunction, exactly
                    // as the C original treats it (`coin_acceptor.c:101-107`).
                    if (now - since).as_millis() > PULSE_MODE_PULSE_MAX_MS {
                        malfunction = true;
                        PulseModeStage::Idle
                    } else {
                        PulseModeStage::Pulsing { since }
                    }
                } else {
                    // The line released. A pulse shorter than the minimum is also a malfunction —
                    // the C original checks both bounds before accepting a pulse
                    // (`coin_acceptor.c:95-96`).
                    let held = (now - since).as_millis();
                    if held < PULSE_MODE_PULSE_MIN_MS {
                        malfunction = true;
                        PulseModeStage::Idle
                    } else {
                        PulseModeStage::Gap { since: now, count: 1 }
                    }
                }
            }
            PulseModeStage::Gap { since, count } => {
                if active {
                    // Another pulse in the same burst has started.
                    PulseModeStage::Pulsing { since: now }
                } else if (now - since).as_millis() >= PULSE_MODE_INTER_PULSE_MAX_MS {
                    // The gap has run out: the burst is over. Report the coin the pulse count names,
                    // if the settings believe in that many channels and the channel is not masked
                    // off — the same channel-mask check the parallel path applies.
                    let index = (count - 1) as usize;
                    if index < COIN_CHANNELS && settings.channel_mask & (1 << index) != 0 {
                        let _ = events.try_send(Event::Coin {
                            channel: index as u8,
                            value: settings.channel_values[index],
                        });
                    }
                    PulseModeStage::Idle
                } else {
                    PulseModeStage::Gap { since, count }
                }
            }
        };

        error::set(Errors::COIN_ACCEPTOR, malfunction);
    }
}
