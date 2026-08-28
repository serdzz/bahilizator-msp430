//! Where everything is wired, and the numbers that describe the machine.
//!
//! The pin assignments are taken from the C firmware's headers, not invented: the board exists and
//! these are the connections it has. Names below are the ones the schematic uses.
//!
//! # A conflict worth knowing about
//!
//! The coin acceptor and the third coin hopper share port 7 — channel 2 and the hopper's control
//! line are both P7.7, channel 3 and its error line are both P7.6, channel 4 and its level line are
//! both P7.5. A machine can have one or the other, never both, and the original firmware made the
//! same choice at build time.

/// Ports, as the HAL numbers them: P1 is 0.
///
/// Only the two the hoppers are on are here. Everywhere else a pin is named by its singleton — the
/// display's, the buttons' — and a port number would be a second copy of the same fact, free to
/// drift away from the first.
pub mod port {
    /// P7, where the coin acceptor and the third hopper live.
    pub const P7: u8 = 6;
    /// P8, where the dispenser and the coin hopper live.
    pub const P8: u8 = 7;
}

// ---------------------------------------------------------------------------------------------
// Indicators and switches
// ---------------------------------------------------------------------------------------------

/// How many doors the cabinet has.
///
/// The C original masks three contacts on port 1, P1.3 through P1.5, but counts two doors. The
/// third is wired and readable; nothing in the firmware uses it.
pub const DOOR_COUNT: usize = 2;

/// How many buttons the front panel has.
pub const BUTTON_COUNT: usize = 4;

// ---------------------------------------------------------------------------------------------
// Coin acceptor — port 7
// ---------------------------------------------------------------------------------------------

/// How many coin denominations the acceptor can report.
///
/// Which pin each channel is on is in `main`, where the pins are claimed: the six are scattered
/// across port 7 rather than consecutive, so there is nothing to compute and nothing to gain from
/// writing the list down twice.
pub const COIN_CHANNELS: usize = 6;

// ---------------------------------------------------------------------------------------------
// Hoppers — ports 7 and 8
// ---------------------------------------------------------------------------------------------

/// One hopper's four lines.
///
/// The control line is inverted in hardware: the port bit *set* runs the motor. The original's
/// macros are named for the logical drive level after the inverter, which reads backwards; the
/// names here describe the port.
#[derive(Copy, Clone, Debug)]
pub struct HopperPins {
    /// Port the hopper is wired to.
    pub port: u8,
    /// Motor control, an output. Setting it runs the hopper.
    pub control: u8,
    /// Unit sensor, an input pulled low while a unit is in front of it.
    pub coin: u8,
    /// Error line, an input. Low means the hopper is unhappy.
    pub error: u8,
    /// Level line, an input. Low means the hopper is down to its last units.
    pub level: u8,
}

/// Hopper A, on port 8 — this is the shoe-cover dispenser, not a coin hopper.
///
/// Its level line is wired but the original never reads it, and neither does this: the count of
/// covers left comes from the counters, which are decremented as they are dispensed.
pub const ITEM_DISPENSER: HopperPins = HopperPins {
    port: port::P8,
    control: 7,
    coin: 4,
    error: 6,
    level: 5,
};

/// Hopper B, on port 8. The coin hopper.
pub const HOPPER_B: HopperPins = HopperPins {
    port: port::P8,
    control: 3,
    coin: 0,
    error: 2,
    level: 1,
};

/// Hopper C, on port 7 — the second coin hopper, and the one that clashes with the coin acceptor.
///
/// Kept here because the wiring exists and a machine built without an acceptor can use it, but it
/// is not in [`HOPPERS`]: this build has an acceptor.
#[allow(dead_code)]
pub const HOPPER_C: HopperPins = HopperPins {
    port: port::P7,
    control: 7,
    coin: 4,
    error: 6,
    level: 5,
};

/// Coin hoppers this build drives.
///
/// One, not two. The original counts two, but the second is hopper C, and on a machine with a coin
/// acceptor its pins are the acceptor's. See the note at the top of this module.
pub const HOPPER_COUNT: usize = 1;

/// The coin hoppers, in order.
pub const HOPPERS: [HopperPins; HOPPER_COUNT] = [HOPPER_B];

// ---------------------------------------------------------------------------------------------
// Display — port 4, HD44780 in 4-bit mode: RS on P4.1, R/W on P4.2, E on P4.3, data on P4.4-P4.7
// ---------------------------------------------------------------------------------------------

/// Display geometry.
pub const LCD_COLUMNS: usize = 16;
/// Display geometry.
pub const LCD_ROWS: usize = 2;

// ---------------------------------------------------------------------------------------------
// Timings
// ---------------------------------------------------------------------------------------------

/// How often the buttons and door switches are sampled.
///
/// The contacts are read rather than interrupted on, so this also sets how quickly a press is
/// noticed. 25 ms was the original's figure and is comfortably under the shortest deliberate press.
pub const INPUT_POLL_MS: u64 = 25;

/// How long a contact has to hold its new state before it counts as changed.
pub const DEBOUNCE_MS: u64 = 50;

/// How long a hopper may run without its coin sensor pulsing before it is called jammed.
pub const HOPPER_COIN_TIMEOUT_MS: u64 = 2_000;

/// How long the coin sensor pulse is expected to last, at the extremes.
///
/// Anything shorter is noise; anything longer is a coin that has stuck in front of the sensor.
pub const HOPPER_PULSE_MIN_MS: u64 = 5;
/// See [`HOPPER_PULSE_MIN_MS`].
pub const HOPPER_PULSE_MAX_MS: u64 = 500;

// ---------------------------------------------------------------------------------------------
// 1-Wire — port 2
//
// The DS1990 key reader is P2.4, open drain with a pull-up. It shares a port with the buttons,
// which is only worth noting because port 2 is one of the two that can interrupt, and neither of
// these uses that.
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------------------------

/// What MCLK is set to, and what the busy-wait delays are calibrated against.
///
/// This has to agree with the clock configuration [`crate::main`] applies. It is a constant rather
/// than a runtime read because the 1-Wire bit timing needs it at compile time: working it out
/// inside a timed region would itself take longer than the bit slot being timed.
pub const MCLK_HZ: u32 = 8_000_000;
