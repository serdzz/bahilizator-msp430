//! HD44780 character display, driven four bits at a time on port 4.
//!
//! The read/write line is held low and never lifted, so the busy flag cannot be polled and every
//! command is followed by a delay long enough for the slowest part the boards were built with. That
//! costs a few hundred microseconds per screen, which the machine has: the display changes when a
//! customer does something, not continuously.
//!
//! The data lines are the top nibble of the port and the control lines are three of the bottom
//! four, so each pin is driven on its own rather than the port being written as a byte. That leaves
//! P4.0 — the modem's power supply on this board — untouched, which writing the whole port would
//! not.

use embassy_msp430::gpio::{Level, Output};
use embassy_time::{Duration, Timer};

/// A parked instruction: what to send, and how long it needs afterwards.
struct Command {
    byte: u8,
    delay: Duration,
}

/// The two delays the controller needs. Clear and home rewrite the whole of display RAM and take
/// over a millisecond; everything else settles in 40 microseconds.
const SHORT: Duration = Duration::from_micros(50);
const LONG: Duration = Duration::from_micros(2000);

/// The display.
pub struct Lcd<'d> {
    rs: Output<'d>,
    /// Held low for the life of the display. Kept only so it cannot be reused as something else.
    _rw: Output<'d>,
    e: Output<'d>,
    data: [Output<'d>; 4],
    /// Columns per row, so [`Lcd::write_str`] can stop at the edge instead of wrapping into the
    /// other row's RAM.
    columns: usize,
}

impl<'d> Lcd<'d> {
    /// Take the pins and bring the controller up.
    ///
    /// `rw` is taken and pinned low rather than left to the caller, because a display whose
    /// read/write line floats will occasionally drive the data bus against this one.
    pub async fn new(
        rs: Output<'d>,
        mut rw: Output<'d>,
        e: Output<'d>,
        data: [Output<'d>; 4],
        columns: usize,
    ) -> Self {
        rw.set_low();

        let mut lcd = Self {
            rs,
            _rw: rw,
            e,
            data,
            columns,
        };
        lcd.init().await;
        lcd
    }

    /// The power-on sequence from the datasheet.
    ///
    /// The three 0x3 nibbles are not redundant. The controller may come out of reset in either
    /// four-bit or eight-bit mode — after a warm restart it is whatever it was — and this sequence
    /// is the one thing that means "eight-bit mode" in both. Only then is it safe to ask for four.
    async fn init(&mut self) {
        Timer::after(Duration::from_millis(50)).await;

        self.rs.set_low();
        for delay in [
            Duration::from_millis(5),
            Duration::from_micros(150),
            Duration::from_micros(150),
        ] {
            self.write_nibble(0x3);
            Timer::after(delay).await;
        }
        self.write_nibble(0x2);
        Timer::after(SHORT).await;

        for cmd in [
            Command { byte: 0x28, delay: SHORT }, // four bits, two lines, 5x8 glyphs
            Command { byte: 0x08, delay: SHORT }, // display off while it is set up
            Command { byte: 0x01, delay: LONG },  // clear
            Command { byte: 0x06, delay: SHORT }, // advance the cursor, do not scroll
            Command { byte: 0x0c, delay: SHORT }, // display on, no cursor, no blink
        ] {
            self.command(cmd).await;
        }
    }

    /// Put one nibble on the data lines and strobe it in.
    ///
    /// The enable pulse has no delay around it on purpose: the controller needs 450 ns of setup and
    /// 230 ns of hold, and at 8 MHz a single instruction is already 125 ns, so the surrounding pin
    /// writes are longer than the requirement.
    fn write_nibble(&mut self, nibble: u8) {
        for (i, pin) in self.data.iter_mut().enumerate() {
            pin.set_level(Level::from(nibble & (1 << i) != 0));
        }
        self.e.set_high();
        self.e.set_low();
    }

    /// Send a whole byte, high nibble first, and wait for it to be digested.
    async fn write_byte(&mut self, byte: u8, delay: Duration) {
        self.write_nibble(byte >> 4);
        self.write_nibble(byte & 0x0f);
        Timer::after(delay).await;
    }

    async fn command(&mut self, cmd: Command) {
        self.rs.set_low();
        self.write_byte(cmd.byte, cmd.delay).await;
    }

    /// Move the cursor.
    ///
    /// The two rows are not contiguous in display RAM — row 1 starts at 0x40 — which is why this
    /// exists rather than callers counting characters.
    pub async fn set_cursor(&mut self, row: usize, column: usize) {
        let base = if row == 0 { 0x00 } else { 0x40 };
        self.command(Command {
            byte: 0x80 | (base + column as u8),
            delay: SHORT,
        })
        .await;
    }

    /// Write one character, as the display's own character set encodes it.
    pub async fn write_raw(&mut self, byte: u8) {
        self.rs.set_high();
        self.write_byte(byte, SHORT).await;
    }

    /// Write text, translated into the display's character set, stopping at the right-hand edge.
    ///
    /// Returns how many cells were used, which is not `text.len()` — a Cyrillic letter is two bytes
    /// of UTF-8 and one cell.
    pub async fn write_str(&mut self, text: &str, column: usize) -> usize {
        let room = self.columns.saturating_sub(column);
        let mut written = 0;
        for c in text.chars().take(room) {
            self.write_raw(crate::cyrillic::encode_or_replacement(c)).await;
            written += 1;
        }
        written
    }

    /// Draw one row: `text`, left-aligned, with the rest of the row blanked.
    ///
    /// Blanking is part of drawing rather than a separate clear, because clearing the whole display
    /// between screens is what makes it flicker.
    pub async fn write_row(&mut self, row: usize, text: &str) {
        self.set_cursor(row, 0).await;
        let written = self.write_str(text, 0).await;
        for _ in written..self.columns {
            self.write_raw(b' ').await;
        }
    }
}
