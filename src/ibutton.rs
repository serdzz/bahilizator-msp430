//! Reading a DS1990 key off the 1-Wire bus.
//!
//! # Why this does not use `embassy-time`
//!
//! 1-Wire is timed in microseconds: a write-one is six microseconds low, a read samples nine
//! microseconds after the edge, and being ten microseconds late turns a one into a zero. The
//! machine's time driver ticks at 32768 Hz, so its finest interval is about thirty microseconds —
//! five times the shortest thing that has to be measured here.
//!
//! So the bit slots are busy-waited from the CPU clock and run with interrupts off. That blocks the
//! executor for about a millisecond per byte, which is why nothing else does this: it is acceptable
//! only because a key is read when somebody holds one against the reader, and a millisecond of
//! deafness is invisible against the second they hold it there.
//!
//! The delays are calculated from [`MCLK_HZ`], and the loop below is *assumed* to be three cycles.
//! That assumption is worth a scope on the first board: the standard tolerates a good deal of slop,
//! but not a factor of two.

use embassy_msp430::gpio::{Flex, Pull};
use embassy_time::{Duration, Timer};

use crate::config::MCLK_HZ;
use crate::event::{Event, EventSender};
use crate::state::IbuttonKey;

/// Cycles the delay loop below takes per iteration: decrement, compare, branch.
const CYCLES_PER_LOOP: u32 = 3;

/// Loop iterations that take about one microsecond.
const LOOPS_PER_US: u32 = MCLK_HZ / (1_000_000 * CYCLES_PER_LOOP);

/// Busy-wait for roughly `us` microseconds.
///
/// `volatile` keeps the loop from being optimised away, which it otherwise would be: it has no
/// effect other than taking time, and that is not something the compiler models.
#[inline(always)]
fn delay_us(us: u32) {
    let mut n = us * LOOPS_PER_US;
    while n > 0 {
        // SAFETY: reading a local through a volatile pointer, purely to defeat the optimiser.
        n = unsafe { core::ptr::read_volatile(&n) } - 1;
    }
}

/// The reader.
pub struct OneWire<'d> {
    /// The bus. Driven low, or released to the pull-up — never driven high, because a key holding
    /// the line down against a driven output would be a short.
    pin: Flex<'d>,
}

impl<'d> OneWire<'d> {
    /// Take the pin and release the bus.
    pub fn new(mut pin: Flex<'d>) -> Self {
        pin.set_low();
        pin.set_as_input(Pull::Up);
        Self { pin }
    }

    /// Pull the bus down.
    #[inline(always)]
    fn drive_low(&mut self) {
        // The output register is already low from `new`, so making the pin an output is the whole
        // of driving it low, and it happens in one instruction.
        self.pin.set_as_output();
    }

    /// Let the bus go back up.
    #[inline(always)]
    fn release(&mut self) {
        self.pin.set_as_input(Pull::Up);
    }

    /// Reset the bus and see whether anything answers.
    ///
    /// Returns true if a device pulled the line down in the presence window.
    fn reset(&mut self) -> bool {
        critical_section::with(|_| {
            self.drive_low();
            delay_us(480);
            self.release();
            delay_us(70);
            let present = self.pin.is_low();
            delay_us(410);
            present
        })
    }

    /// Send one bit.
    ///
    /// Both slots are the same length; what distinguishes them is how much of it the master spends
    /// holding the line down.
    #[inline(always)]
    fn write_bit(&mut self, bit: bool) {
        self.drive_low();
        if bit {
            delay_us(6);
            self.release();
            delay_us(64);
        } else {
            delay_us(60);
            self.release();
            delay_us(10);
        }
    }

    /// Read one bit.
    ///
    /// The master starts the slot; the device either holds the line down for the rest of it, which
    /// is a zero, or leaves it to the pull-up, which is a one.
    #[inline(always)]
    fn read_bit(&mut self) -> bool {
        self.drive_low();
        delay_us(6);
        self.release();
        delay_us(9);
        let bit = self.pin.is_high();
        delay_us(55);
        bit
    }

    /// Send a byte, least significant bit first.
    fn write_byte(&mut self, byte: u8) {
        critical_section::with(|_| {
            for i in 0..8 {
                self.write_bit(byte & (1 << i) != 0);
            }
        });
    }

    /// Read a byte, least significant bit first.
    fn read_byte(&mut self) -> u8 {
        critical_section::with(|_| {
            let mut byte = 0u8;
            for i in 0..8 {
                if self.read_bit() {
                    byte |= 1 << i;
                }
            }
            byte
        })
    }

    /// Read the serial number of the one key on the bus.
    ///
    /// Returns `None` if nothing answered or the checksum did not hold. There is no search here:
    /// the reader takes one key at a time, and two keys on the bus at once would collide into a
    /// serial number that fails its own checksum, which is exactly the answer wanted.
    pub fn read_key(&mut self) -> Option<IbuttonKey> {
        if !self.reset() {
            return None;
        }

        /// Read ROM. The only command a DS1990 understands.
        const READ_SERIAL: u8 = 0x33;
        self.write_byte(READ_SERIAL);

        let mut key = [0u8; 8];
        for byte in key.iter_mut() {
            *byte = self.read_byte();
        }

        // A key that has just been taken off the reader mid-read gives all ones; one that was never
        // there gives all zeros. Neither is a serial number, and both pass no checksum.
        if crc8(&key) != 0 {
            return None;
        }

        Some(IbuttonKey(key))
    }
}

/// The Maxim 1-Wire CRC-8, over a whole serial number including its own checksum byte.
///
/// Running it over all eight bytes gives zero for a good key, which is one comparison rather than
/// seven bytes hashed and then compared against the eighth.
fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        let mut b = byte;
        for _ in 0..8 {
            let mix = (crc ^ b) & 1;
            crc >>= 1;
            if mix != 0 {
                // x^8 + x^5 + x^4 + 1, reflected.
                crc ^= 0x8c;
            }
            b >>= 1;
        }
    }
    crc
}

/// How often the reader is checked for a key.
const POLL_MS: u64 = 250;

/// How long the same key must be away before presenting it again counts as a new event.
///
/// Without this, holding a key against the reader would open the service menu, and then keep
/// re-opening it four times a second for as long as it was held.
const REPEAT_LOCKOUT_POLLS: u8 = 4;

/// Watch the reader and report keys.
#[embassy_executor::task]
pub async fn ibutton_task(mut bus: OneWire<'static>, events: EventSender) {
    let mut last: Option<IbuttonKey> = None;
    let mut absent_polls = 0u8;

    loop {
        Timer::after(Duration::from_millis(POLL_MS)).await;

        match bus.read_key() {
            Some(key) => {
                absent_polls = 0;
                if last != Some(key) {
                    last = Some(key);
                    let _ = events.try_send(Event::Key(key));
                }
            }
            None => {
                absent_polls = absent_polls.saturating_add(1);
                if absent_polls >= REPEAT_LOCKOUT_POLLS {
                    last = None;
                }
            }
        }
    }
}
