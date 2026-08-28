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

use crate::config::{HOPPER_COIN_TIMEOUT_MS, HOPPER_PULSE_MAX_MS, HOPPER_PULSE_MIN_MS};

/// How often the coin sensor is read while the motor runs.
///
/// The shortest pulse worth believing is [`HOPPER_PULSE_MIN_MS`], so sampling must be comfortably
/// faster than that or a coin will pass unseen.
const POLL_MS: u64 = 1;

/// Why a dispense stopped early.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HopperError {
    /// The motor ran and nothing came out. Jammed, empty, or unplugged.
    Jammed,
    /// The hopper's own error line says it is unhappy.
    Faulted,
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
}

impl<'d> Hopper<'d> {
    /// Take the lines, with the motor stopped.
    pub fn new(
        mut control: Output<'d>,
        coin: Input<'d>,
        error: Input<'d>,
        level: Input<'d>,
    ) -> Self {
        control.set_low();
        Self {
            control,
            coin,
            error,
            level,
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
            return (0, Some(HopperError::Faulted));
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
                return (dispensed, Some(HopperError::Faulted));
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
        let limit = Duration::from_millis(HOPPER_PULSE_MAX_MS);

        let ended = with_timeout(limit, async {
            while self.coin.is_low() {
                Timer::after(Duration::from_millis(POLL_MS)).await;
            }
        })
        .await;

        if ended.is_err() {
            return PulseResult::Stuck;
        }

        if (Instant::now() - started).as_millis() >= HOPPER_PULSE_MIN_MS {
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
