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

    let mut pulses = [Pulse::Idle; COIN_CHANNELS];
    let mut ticker = Ticker::every(Duration::from_millis(POLL_MS));

    let mut enabled = settings.enabled;

    loop {
        ticker.next().await;

        let wanted = settings.enabled
            && !INHIBITED.load(core::sync::atomic::Ordering::Relaxed)
            && error::errors().can_vend();
        if wanted != enabled {
            acceptor.set_enabled(wanted);
            enabled = wanted;
        }

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
