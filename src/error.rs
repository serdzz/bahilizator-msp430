//! Everything that can be wrong with the machine at once.
//!
//! Faults here are conditions, not events: a jammed hopper stays a fault until someone clears the
//! jam, and the vending logic asks "can I pay out?" by looking at the set rather than by
//! remembering what it was last told. One shared set means one answer, and the display, the
//! reporting and the state machine cannot disagree about whether the machine is well.
//!
//! The width is a deviation from the C original and from the port design, both of which used 64
//! bits. Nineteen flags fit in 32, and on a 16-bit CPU with no barrel shifter every bit above that
//! costs real instructions in code that runs on every state transition.

use core::sync::atomic::Ordering;

use bitflags::bitflags;
use portable_atomic::AtomicU32;

bitflags! {
    /// The faults the machine can be in.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Errors: u32 {
        /// The firmware image does not match its checksum.
        const FIRMWARE = 1 << 0;
        /// Stored settings were written by a firmware version this one cannot read.
        const SETTINGS_VERSION = 1 << 1;
        /// Stored settings failed their checksum.
        const SETTINGS_CRC = 1 << 2;
        /// Stored counters were written by a firmware version this one cannot read.
        const STATE_VERSION = 1 << 3;
        /// Stored counters failed their checksum.
        const STATE_CRC = 1 << 4;
        /// The backup battery is flat, so the clock is not to be trusted.
        const BATTERY_LOW = 1 << 5;
        /// The external memory is unreadable.
        const FLASH_CORRUPTED = 1 << 6;
        /// Non-volatile memory is unreadable.
        const NVRAM_CORRUPTED = 1 << 7;
        /// The coin acceptor is not answering.
        const COIN_ACCEPTOR = 1 << 8;
        /// The item dispenser is jammed.
        const ITEM_DISPENSER = 1 << 9;
        /// A coin hopper is jammed.
        const COIN_HOPPER = 1 << 10;
        /// The coin acceptor has been inhibited, so no money can come in.
        const COIN_ACCEPTOR_OFF = 1 << 11;
        /// The item dispenser is out of shoe covers.
        const ITEM_DISPENSER_EMPTY = 1 << 12;
        /// A coin hopper is out of coins.
        const COIN_HOPPER_EMPTY = 1 << 13;
        /// Change is owed that the machine has no way to pay.
        const CANNOT_PAYOUT = 1 << 14;
        /// A door is open.
        const DOOR_OPENED = 1 << 15;
    }
}

impl Errors {
    /// Faults that stop the machine selling anything.
    ///
    /// An empty hopper is deliberately not among them: the machine can still sell for exact money,
    /// and refusing to trade because it cannot give change would turn a small fault into a dead
    /// machine.
    pub const FATAL: Errors = Errors::FIRMWARE
        .union(Errors::ITEM_DISPENSER)
        .union(Errors::ITEM_DISPENSER_EMPTY)
        .union(Errors::DOOR_OPENED);

    /// Is the machine well enough to sell?
    pub fn can_vend(self) -> bool {
        !self.intersects(Self::FATAL)
    }
}

/// The machine's current faults, shared by every task.
///
/// This is an atomic rather than a mutex because the interesting operations are set-a-bit and
/// clear-a-bit from tasks that must not block, and because the display wants to read it without
/// waiting for whoever is writing.
static ERRORS: AtomicU32 = AtomicU32::new(0);

/// The faults the machine currently has.
pub fn errors() -> Errors {
    Errors::from_bits_truncate(ERRORS.load(Ordering::Relaxed))
}

/// Record `e` as present. Faults already set stay set.
pub fn raise(e: Errors) {
    ERRORS.fetch_or(e.bits(), Ordering::Relaxed);
}

/// Record `e` as gone.
pub fn clear(e: Errors) {
    ERRORS.fetch_and(!e.bits(), Ordering::Relaxed);
}

/// Set `e` to `present`, whichever way it was before.
pub fn set(e: Errors, present: bool) {
    if present {
        raise(e);
    } else {
        clear(e);
    }
}
