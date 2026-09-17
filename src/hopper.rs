//! Hoppers: the things that pay out.
//!
//! A coin hopper and the shoe-cover dispenser are the same device as far as the firmware is
//! concerned — a motor, a sensor that pulses once per unit dispensed, and two status lines — so
//! they share this driver and differ only in what a unit is worth.
//!
//! The rule that shapes everything here is that a dispense must be **countable across a power
//! cut**. The caller is told how many units actually came out, never how many were asked for, and
//! it is the caller's job to write that number down before asking for more. A hopper that stops
//! halfway through paying out change must leave the machine knowing exactly what it still owes.

use embassy_msp430::gpio::{Input, Output};
use embassy_time::{Duration, Instant, Timer, with_timeout};

use crate::config::{
    HOPPER_COIN_TIMEOUT_MS, HOPPER_ERROR_PULSE_GAP_MS, HOPPER_PULSE_MAX_MS, HOPPER_PULSE_MIN_MS,
    ITEM_HOPPER_PULSE_MAX_MS, ITEM_HOPPER_PULSE_MIN_MS,
};

/// How often the coin sensor is read while the motor runs.
///
/// The shortest pulse worth believing is [`HOPPER_PULSE_MIN_MS`], so sampling must be comfortably
/// faster than that or a coin will pass unseen.
const POLL_MS: u64 = 1;

/// The longest this driver will listen to a fault burst on the error line before giving up on
/// decoding a numeric code and reporting the fault as code 0 ("unknown/ungraded fault").
///
/// The C original has no such bound (`hooperProcess`'s `HOPPER_ERROR_HI`/`_LO` states run for as
/// long as the caller keeps calling it), because it is driven from a fixed-rate main loop that has
/// nothing better to do. This driver is a single `async fn` a caller `.await`s, so an upper bound is
/// needed to guarantee it returns even if the hopper's error line never settles.
const ERROR_CODE_TIMEOUT_MS: u64 = 3_000;

/// Which kind of hopper this is, which decides which pulse-timing constants apply.
///
/// The C original hard-codes hopper A's timing (`ITEM_HOPPER_COIN_PULSE_MIN/MAX_TIME`) into
/// `hopperA_process` and hoppers B/C's timing (`HOPPER_COIN_PULSE_MIN/MAX_TIME`) into
/// `hopperB_process`/`hopperC_process` (`hopper.c:117-321`) — two different functions rather than
/// one parameterised driver. This port shares one driver for both roles, so the distinction has to
/// be carried as data instead; see PORT_AUDIT.md §2, which found the port previously used the item
/// dispenser's timing for the coin hopper unconditionally.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HopperKind {
    /// Hopper A: the shoe-cover dispenser. `ITEM_HOPPER_COIN_PULSE_MIN/MAX_TIME` = 5/500 ms.
    ItemDispenser,
    /// Hoppers B/C: coin hoppers. `HOPPER_COIN_PULSE_MIN/MAX_TIME` = 10/70 ms.
    CoinHopper,
}

impl HopperKind {
    /// The pulse-width window that counts as a real unit for this kind of hopper.
    fn pulse_window_ms(self) -> (u64, u64) {
        match self {
            HopperKind::ItemDispenser => (ITEM_HOPPER_PULSE_MIN_MS, ITEM_HOPPER_PULSE_MAX_MS),
            HopperKind::CoinHopper => (HOPPER_PULSE_MIN_MS, HOPPER_PULSE_MAX_MS),
        }
    }
}

/// Why a dispense stopped early.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HopperError {
    /// The motor ran and nothing came out. Jammed, empty, or unplugged.
    Jammed,
    /// The hopper's own error line says it is unhappy, with the numeric fault code decoded from the
    /// pulse train on that line — matching the C original's `error_pulse_count[]`-derived code
    /// (`hopper.c:177-192`), which distinguishes different hardware faults (jam, empty, sensor
    /// fault, and whatever else the specific hopper model signals) rather than collapsing them all
    /// into one flag. `0` means the line was asserted but no pulse burst could be decoded from it
    /// within [`ERROR_CODE_TIMEOUT_MS`] — an "unhappy but ungraded" fault, which is the best this
    /// driver can report rather than blocking forever.
    Faulted {
        /// The decoded fault code, or 0 if none could be read.
        code: u8,
    },
}

/// One hopper.
pub struct Hopper<'d> {
    /// Motor control. High runs it.
    control: Output<'d>,
    /// Pulses once per unit dispensed. Active low.
    coin: Input<'d>,
    /// Low when the hopper reports a fault of its own.
    error: Input<'d>,
    /// Low when the hopper is down to its last few units.
    level: Input<'d>,
    /// Whether this is the item dispenser or a coin hopper — decides pulse timing.
    kind: HopperKind,
}

impl<'d> Hopper<'d> {
    /// Take the lines, with the motor stopped.
    pub fn new(
        mut control: Output<'d>,
        coin: Input<'d>,
        error: Input<'d>,
        level: Input<'d>,
        kind: HopperKind,
    ) -> Self {
        control.set_low();
        Self {
            control,
            coin,
            error,
            level,
            kind,
        }
    }

    /// Is the hopper reporting a fault of its own?
    pub fn is_faulted(&self) -> bool {
        self.error.is_low()
    }

    /// Is the hopper running low?
    pub fn is_low(&self) -> bool {
        self.level.is_low()
    }

    /// Read a numeric fault code off the error line.
    ///
    /// Called once a fault has already been noticed (the error line is low). Counts pulses on the
    /// line — rising edges, i.e. the end of each low period — grouping them into one burst as long
    /// as consecutive pulses are separated by less than [`HOPPER_ERROR_PULSE_GAP_MS`], and returns
    /// the pulse count once a gap that long is seen with at least one pulse counted. This is the
    /// same grouping the C original's `HOPPER_ERROR_LO`/`HOPPER_ERROR_HI` states and
    /// `PAUSE_BETWEEN_ERRER_CODES` implement (`hopper.c:168-192`), read as: the hopper signals its
    /// fault code as a short burst of pulses on its error line, and a gap longer than the threshold
    /// marks the burst as finished.
    async fn read_fault_code(&mut self) -> u8 {
        let deadline = Instant::now() + Duration::from_millis(ERROR_CODE_TIMEOUT_MS);
        let mut count = 0u8;
        let mut was_low = self.error.is_low();
        let mut last_edge = Instant::now();

        loop {
            if Instant::now() >= deadline {
                return count;
            }
            Timer::after(Duration::from_millis(POLL_MS)).await;

            let low = self.error.is_low();
            if low && !was_low {
                // Falling edge: a new pulse has started.
                last_edge = Instant::now();
            } else if !low && was_low {
                // Rising edge: one pulse has finished.
                count = count.saturating_add(1);
                last_edge = Instant::now();
            } else if !low
                && count > 0
                && (Instant::now() - last_edge).as_millis() >= HOPPER_ERROR_PULSE_GAP_MS
            {
                return count;
            }
            was_low = low;
        }
    }

    /// Dispense up to `count` units.
    ///
    /// Returns how many actually came out, and why it stopped if that was fewer than asked. The
    /// motor is stopped before returning on every path, including the error ones — this is the
    /// function that decides whether a hopper keeps running after something has gone wrong, and it
    /// must not.
    pub async fn dispense(&mut self, count: u16) -> (u16, Option<HopperError>) {
        if count == 0 {
            return (0, None);
        }
        if self.is_faulted() {
            let code = self.read_fault_code().await;
            return (0, Some(HopperError::Faulted { code }));
        }

        self.control.set_high();
        let result = self.run(count).await;
        self.control.set_low();
        result
    }

    /// The dispensing loop, with the motor already running.
    ///
    /// Split out so that [`Hopper::dispense`] can stop the motor on every exit path — including a
    /// panic unwinding, if this were a target that unwound — rather than repeating the stop at each
    /// `return`.
    async fn run(&mut self, count: u16) -> (u16, Option<HopperError>) {
        let mut dispensed = 0u16;
        let mut last_unit = Instant::now();
        // The sensor may already be blocked by the coin that is sitting in front of it, so a
        // dispense starts by waiting for the line to clear rather than counting it as a unit.
        let mut was_active = self.coin.is_low();

        loop {
            Timer::after(Duration::from_millis(POLL_MS)).await;

            if self.is_faulted() {
                let code = self.read_fault_code().await;
                return (dispensed, Some(HopperError::Faulted { code }));
            }

            let active = self.coin.is_low();
            if active && !was_active {
                // A pulse has started. Time it: too short is electrical noise, and too long is a
                // coin that has stopped in front of the sensor, which is a jam whatever the
                // hopper's own error line says.
                match self.measure_pulse().await {
                    PulseResult::Unit => {
                        dispensed += 1;
                        last_unit = Instant::now();
                        if dispensed >= count {
                            return (dispensed, None);
                        }
                    }
                    PulseResult::Noise => {}
                    PulseResult::Stuck => return (dispensed, Some(HopperError::Jammed)),
                }
                was_active = false;
                continue;
            }
            was_active = active;

            if (Instant::now() - last_unit).as_millis() >= HOPPER_COIN_TIMEOUT_MS {
                return (dispensed, Some(HopperError::Jammed));
            }
        }
    }

    /// Wait for the pulse that has just begun to end, and judge it.
    async fn measure_pulse(&mut self) -> PulseResult {
        let started = Instant::now();
        let (pulse_min_ms, pulse_max_ms) = self.kind.pulse_window_ms();
        let limit = Duration::from_millis(pulse_max_ms);

        let ended = with_timeout(limit, async {
            while self.coin.is_low() {
                Timer::after(Duration::from_millis(POLL_MS)).await;
            }
        })
        .await;

        if ended.is_err() {
            return PulseResult::Stuck;
        }

        if (Instant::now() - started).as_millis() >= pulse_min_ms {
            PulseResult::Unit
        } else {
            PulseResult::Noise
        }
    }
}

/// What one sensor pulse turned out to be.
enum PulseResult {
    /// A unit came out.
    Unit,
    /// Too short to be real.
    Noise,
    /// The sensor never cleared.
    Stuck,
}
