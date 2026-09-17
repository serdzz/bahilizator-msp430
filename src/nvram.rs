//! Keeping settings and counters across a power cut.
//!
//! Both live in on-chip flash, in the four segments `memory.x` carves out below the code. They are
//! kept differently, because they are written at wildly different rates.
//!
//! [`Settings`] changes when an engineer changes it — a few dozen times in a machine's life — so it
//! is simply rewritten into its own segment.
//!
//! [`Counters`] changes on every coin. Flash endures about ten thousand erases, and a busy machine
//! would burn through that in a season if each save erased a segment. So counters are *journalled*:
//! each save appends a fresh record after the last one, and a segment is erased only when it fills.
//! At sixty-four bytes a record that is seven saves per erase per segment, across three segments —
//! roughly two hundred thousand saves before the flash is worn, which outlives the machine.
//!
//! Every record carries a checksum, and the reader takes the newest one that passes. A save
//! interrupted by the power going off leaves a half-written record that fails its checksum, and the
//! previous record — still intact, because nothing overwrote it — is what comes back.

use crate::config::{COIN_CHANNELS, HOPPER_COUNT};
use crate::error::{self, Errors};
use crate::state::{
    Accounting, AppState, Cash, CoinAcceptorSettings, Counters, Currency, EventKind,
    HopperSettings, IbuttonKey, Language, Level, Settings, COUNTERS_VERSION, KEYS_PER_LEVEL,
    KEY_ACCESS_LEVELS, SETTINGS_VERSION,
};

/// Flash segment size on this device.
const SEGMENT: u16 = 512;

/// Where the NVRAM region starts. Must match `memory.x`.
const NVRAM_BASE: u16 = 0x3100;

/// The segment settings live in.
const SETTINGS_SEGMENT: u16 = NVRAM_BASE;

/// Where the counter journal starts, and how many segments it spans.
const JOURNAL_BASE: u16 = NVRAM_BASE + SEGMENT;
const JOURNAL_SEGMENTS: u16 = 3;

/// The segment the event log lives in — one segment, after the counter journal.
const EVENT_LOG_SEGMENT: u16 = JOURNAL_BASE + JOURNAL_SEGMENTS * SEGMENT;

/// One journal record, padded so that records never straddle a segment boundary.
const RECORD: u16 = 64;

/// How many records fit in the journal.
const RECORDS: u16 = JOURNAL_SEGMENTS * (SEGMENT / RECORD);

// Flash controller registers.
const FCTL1: u16 = 0x0128;
const FCTL2: u16 = 0x012a;
const FCTL3: u16 = 0x012c;

/// The write key. Any access without it resets the chip, which is the point of it.
const FWKEY: u16 = 0xa500;
const ERASE: u16 = 0x0002;
const WRT: u16 = 0x0040;
const LOCK: u16 = 0x0010;
const BUSY: u16 = 0x0001;

#[inline]
fn read16(addr: u16) -> u16 {
    // SAFETY: a volatile read of a flash controller register or of flash itself.
    unsafe { (addr as *mut u16).read_volatile() }
}

#[inline]
fn write16(addr: u16, value: u16) {
    // SAFETY: a volatile write to a flash controller register or, with the controller unlocked, to
    // flash. Callers below are responsible for having unlocked it.
    unsafe { (addr as *mut u16).write_volatile(value) }
}

/// How many times to read `BUSY` before giving up on the flash controller.
///
/// An erase is the slowest thing the controller does and takes about 30 ms on this part; the loop
/// below is a handful of cycles, so this is roughly a hundredfold margin at 8 MHz.
///
/// The bound is the point. Interrupts are off for the whole of an erase, and the watchdog is
/// stopped, so a controller that never cleared `BUSY` would leave the machine dead with nothing
/// able to notice. Giving up and recording a fault is worse than succeeding and far better than
/// hanging.
const BUSY_LIMIT: u32 = 1_000_000;

/// Wait for the controller to finish. Returns false if it never did.
fn wait_done() -> bool {
    for _ in 0..BUSY_LIMIT {
        if read16(FCTL3) & BUSY == 0 {
            return true;
        }
    }
    error::raise(Errors::NVRAM_CORRUPTED);
    false
}

/// Point the flash timing generator at a clock in the range the controller needs.
///
/// The controller wants between 257 and 476 kHz whatever the CPU is doing, so the divider is worked
/// out from the clock the HAL actually configured rather than assumed.
fn set_timing() {
    let smclk = embassy_msp430::clocks().map(|c| c.smclk).unwrap_or(1_000_000);
    // Round up, so the result is never above the top of the range: too slow only makes the write
    // take longer, while too fast can leave a bit half-programmed.
    let divider = ((smclk + 399_999) / 400_000).clamp(1, 64) as u16;
    write16(FCTL2, FWKEY | 0x0040 | (divider - 1)); // FSSEL = SMCLK
}

/// Erase the 512-byte segment containing `addr`.
///
/// The CPU is held for the whole erase — some milliseconds — and interrupts are off for it. That is
/// not politeness: an interrupt taken here would try to fetch from flash that is mid-erase.
fn erase_segment(addr: u16) {
    critical_section::with(|_| {
        set_timing();
        write16(FCTL3, FWKEY); // clear LOCK
        write16(FCTL1, FWKEY | ERASE);
        write16(addr, 0); // the dummy write that starts it
        wait_done();
        write16(FCTL1, FWKEY);
        write16(FCTL3, FWKEY | LOCK);
    });
}

/// Write `data` to `addr`, which must already be erased.
///
/// Flash can only clear bits, so writing over a record that is not erased gives the bitwise AND of
/// the two — which is why the journal never rewrites a record in place.
fn write_bytes(addr: u16, data: &[u8]) {
    critical_section::with(|_| {
        set_timing();
        write16(FCTL3, FWKEY);
        write16(FCTL1, FWKEY | WRT);

        for (i, chunk) in data.chunks(2).enumerate() {
            let word = chunk[0] as u16 | ((*chunk.get(1).unwrap_or(&0xff) as u16) << 8);
            write16(addr + (i as u16) * 2, word);
            // Stop at the first word that does not take. Carrying on would write the rest of the
            // record after a hole in it, and a record with a hole passes no checksum but takes the
            // slot a good one could have had.
            if !wait_done() {
                break;
            }
        }

        write16(FCTL1, FWKEY);
        write16(FCTL3, FWKEY | LOCK);
    });
}

/// Read `len` bytes from flash at `addr`.
fn read_bytes(addr: u16, out: &mut [u8]) {
    for (i, byte) in out.iter_mut().enumerate() {
        // SAFETY: a volatile read of flash, which is always readable.
        *byte = unsafe { ((addr + i as u16) as *mut u8).read_volatile() };
    }
}

/// CRC-16/CCITT-FALSE.
///
/// Bytewise and table-free: a 256-entry table would cost half a kilobyte of the flash this is
/// protecting, to save microseconds on an operation that already takes milliseconds.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

// ---------------------------------------------------------------------------------------------
// Serialisation
// ---------------------------------------------------------------------------------------------

/// A little cursor over a byte buffer, so that reading and writing a record are visibly the same
/// sequence of fields in the same order.
struct Cursor<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn u8(&mut self, v: u8) {
        self.buf[self.at] = v;
        self.at += 1;
    }

    fn u16(&mut self, v: u16) {
        self.buf[self.at..self.at + 2].copy_from_slice(&v.to_le_bytes());
        self.at += 2;
    }

    fn u32(&mut self, v: u32) {
        self.buf[self.at..self.at + 4].copy_from_slice(&v.to_le_bytes());
        self.at += 4;
    }

    fn bytes(&mut self, v: &[u8]) {
        self.buf[self.at..self.at + v.len()].copy_from_slice(v);
        self.at += v.len();
    }
}

/// The reading counterpart of [`Cursor`].
struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn u8(&mut self) -> u8 {
        let v = self.buf[self.at];
        self.at += 1;
        v
    }

    fn u16(&mut self) -> u16 {
        let v = u16::from_le_bytes([self.buf[self.at], self.buf[self.at + 1]]);
        self.at += 2;
        v
    }

    fn u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.buf[self.at..self.at + 4]);
        self.at += 4;
        u32::from_le_bytes(b)
    }

    fn bytes(&mut self, out: &mut [u8]) {
        out.copy_from_slice(&self.buf[self.at..self.at + out.len()]);
        self.at += out.len();
    }
}

fn put_accounting(c: &mut Cursor, a: &Accounting) {
    c.u32(a.cash_in);
    c.u32(a.cash_out);
    c.u32(a.items_dispensed);
    c.u32(a.items_free);
}

fn get_accounting(r: &mut Reader) -> Accounting {
    Accounting {
        cash_in: r.u32(),
        cash_out: r.u32(),
        items_dispensed: r.u32(),
        items_free: r.u32(),
    }
}

/// Serialise counters into a record, with its sequence number and checksum.
fn encode_counters(c: &Counters, seq: u16, out: &mut [u8; RECORD as usize]) {
    out.fill(0);
    let mut cur = Cursor::new(out);
    cur.u16(seq);
    cur.u8(c.version);
    cur.u16(c.item_level);
    for level in &c.coin_levels {
        cur.u16(*level);
    }
    cur.u32(c.cash);
    for pending in &c.coins_pending {
        cur.u16(*pending);
    }
    cur.u16(c.items_pending);
    cur.u16(c.free_items_pending);
    cur.u8(c.app_state as u8);
    put_accounting(&mut cur, &c.overall);
    put_accounting(&mut cur, &c.period);

    let end = cur.at;
    let crc = crc16(&out[..end]);
    out[end..end + 2].copy_from_slice(&crc.to_le_bytes());
}

/// Read a record back, or `None` if it is blank, corrupt, or from a layout this build cannot read.
fn decode_counters(bytes: &[u8; RECORD as usize]) -> Option<(u16, Counters)> {
    // An erased record is all ones. Checking for that first keeps a blank journal from being
    // reported as a corrupt one, which would be a fault on a machine that is merely new.
    if bytes.iter().all(|&b| b == 0xff) {
        return None;
    }

    let mut r = Reader::new(bytes);
    let seq = r.u16();
    let version = r.u8();
    let item_level = r.u16();
    let mut coin_levels = [0u16; HOPPER_COUNT];
    for level in coin_levels.iter_mut() {
        *level = r.u16();
    }
    let cash = r.u32();
    let mut coins_pending = [0u16; HOPPER_COUNT];
    for pending in coins_pending.iter_mut() {
        *pending = r.u16();
    }
    let items_pending = r.u16();
    let free_items_pending = r.u16();
    let app_state = match r.u8() {
        0 => AppState::AcceptCash,
        1 => AppState::PayoutItems,
        2 => AppState::PayoutReminder,
        3 => AppState::ProcessResidual,
        _ => return None,
    };
    let overall = get_accounting(&mut r);
    let period = get_accounting(&mut r);

    let end = r.at;
    let stored = u16::from_le_bytes([bytes[end], bytes[end + 1]]);
    if crc16(&bytes[..end]) != stored {
        error::raise(Errors::STATE_CRC);
        return None;
    }
    if version != COUNTERS_VERSION {
        error::raise(Errors::STATE_VERSION);
        return None;
    }

    Some((
        seq,
        Counters {
            version,
            item_level,
            coin_levels,
            cash,
            coins_pending,
            items_pending,
            free_items_pending,
            app_state,
            overall,
            period,
        },
    ))
}

// ---------------------------------------------------------------------------------------------
// The journal
// ---------------------------------------------------------------------------------------------

/// The counter journal: where the newest record is, and where the next one goes.
pub struct Journal {
    /// Slot the last good record was found in.
    slot: u16,
    /// Sequence number of that record. Wraps, and the reader compares by wrapped difference so that
    /// the wrap is not mistaken for the journal starting over.
    seq: u16,
}

impl Journal {
    /// Address of a slot.
    fn slot_addr(slot: u16) -> u16 {
        JOURNAL_BASE + slot * RECORD
    }

    /// Find the newest good record, and the counters it holds.
    ///
    /// Scans every slot rather than stopping at the first blank one. A journal that wrapped has good
    /// records on both sides of the blank, and stopping early would find the older half.
    pub fn open() -> (Self, Counters) {
        let mut best: Option<(u16, u16, Counters)> = None;

        for slot in 0..RECORDS {
            let mut bytes = [0u8; RECORD as usize];
            read_bytes(Self::slot_addr(slot), &mut bytes);
            let Some((seq, counters)) = decode_counters(&bytes) else {
                continue;
            };

            // Newer means "ahead by less than half the sequence space", which is the comparison
            // that survives the counter wrapping round.
            let newer = match &best {
                None => true,
                Some((best_seq, _, _)) => seq.wrapping_sub(*best_seq) < 0x8000,
            };
            if newer {
                best = Some((seq, slot, counters));
            }
        }

        match best {
            Some((seq, slot, counters)) => (Self { slot, seq }, counters),
            // Nothing readable. A machine that has never been switched on looks exactly like one
            // whose journal has been wiped, and both want the same thing: start counting.
            None => (
                Self {
                    slot: RECORDS - 1,
                    seq: 0,
                },
                Counters::new(),
            ),
        }
    }

    /// Append `counters` to the journal.
    ///
    /// The record is written to the *next* slot, leaving the previous one intact until it is
    /// overwritten a full lap later. That is what makes a save interrupted by a power cut safe.
    pub fn save(&mut self, counters: &Counters) {
        let next = (self.slot + 1) % RECORDS;

        // Crossing into a new segment means erasing it first — and the erase is what destroys old
        // records, so it happens as late as possible.
        if next % (SEGMENT / RECORD) == 0 {
            erase_segment(Self::slot_addr(next));
        }

        let seq = self.seq.wrapping_add(1);
        let mut bytes = [0u8; RECORD as usize];
        encode_counters(counters, seq, &mut bytes);
        write_bytes(Self::slot_addr(next), &bytes);

        self.slot = next;
        self.seq = seq;
    }
}

// ---------------------------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------------------------

/// How many bytes a serialised [`Settings`] takes. Comfortably inside one segment.
///
/// Bumped from 160 to make room for `coin_acceptor.pulse_mode` (§1 of `PORT_AUDIT.md`), one extra
/// byte over the 158 actually used by the format above — headroom against the next field added.
const SETTINGS_LEN: usize = 168;

fn put_hopper(c: &mut Cursor, h: &HopperSettings) {
    c.u8(h.enabled as u8);
    c.u32(h.unit_value);
    c.u16(h.min_level);
    c.u16(h.warn_level);
    c.u16(h.max_level);
}

fn get_hopper(r: &mut Reader) -> HopperSettings {
    HopperSettings {
        enabled: r.u8() != 0,
        unit_value: r.u32(),
        min_level: r.u16(),
        warn_level: r.u16(),
        max_level: r.u16(),
    }
}

fn language(v: u8) -> Language {
    if v == 0 { Language::Russian } else { Language::English }
}

fn currency(v: u8) -> Currency {
    match v {
        1 => Currency::Kzt,
        2 => Currency::Byn,
        _ => Currency::Rub,
    }
}

/// Write settings to their segment.
///
/// One erase and one write, both of which hold the CPU. This is called from the service menu, where
/// a few milliseconds of stopped machine is not noticeable.
pub fn save_settings(s: &Settings) {
    let mut buf = [0u8; SETTINGS_LEN];
    let mut c = Cursor::new(&mut buf);
    c.u8(s.version);
    c.u8(s.user_language as u8);
    c.u8(s.service_language as u8);
    c.u8(s.currency as u8);
    c.u16(s.machine_id);
    c.u8(s.coin_acceptor.enabled as u8);
    c.u8(s.coin_acceptor.channel_mask);
    for value in &s.coin_acceptor.channel_values {
        c.u32(*value);
    }
    c.u8(s.coin_acceptor.pulse_mode as u8);
    for hopper in &s.coin_hoppers {
        put_hopper(&mut c, hopper);
    }
    put_hopper(&mut c, &s.item_dispenser);
    for level in &s.keys {
        for key in level {
            c.bytes(&key.0);
        }
    }
    c.u8(s.workday_start_hour);
    c.u8(s.workday_end_hour);
    c.u8(s.residual_timeout);
    c.u8(s.menu_exit_timeout);
    c.u8(s.cash_clear_timeout);
    c.u8(s.thanks_message_delay);
    c.u8(s.payout_message_delay);

    let end = c.at;
    let crc = crc16(&buf[..end]);
    buf[end..end + 2].copy_from_slice(&crc.to_le_bytes());

    erase_segment(SETTINGS_SEGMENT);
    write_bytes(SETTINGS_SEGMENT, &buf[..end + 2]);
}

/// Read the stored settings, or the defaults if there are none that can be trusted.
///
/// Never fails. A machine with unreadable settings still has to stand there and sell shoe covers;
/// what it does instead is raise the fault, so that the display and any engineer can see that what
/// it is running is not what was configured.
pub fn load_settings() -> Settings {
    let mut buf = [0u8; SETTINGS_LEN];
    read_bytes(SETTINGS_SEGMENT, &mut buf);

    if buf.iter().all(|&b| b == 0xff) {
        return Settings::default();
    }

    let mut r = Reader::new(&buf);
    let version = r.u8();
    let user_language = language(r.u8());
    let service_language = language(r.u8());
    let currency = currency(r.u8());
    let machine_id = r.u16();
    let coin_acceptor = {
        let enabled = r.u8() != 0;
        let channel_mask = r.u8();
        let mut channel_values = [0 as Cash; COIN_CHANNELS];
        for value in channel_values.iter_mut() {
            *value = r.u32();
        }
        let pulse_mode = r.u8() != 0;
        CoinAcceptorSettings {
            enabled,
            channel_mask,
            channel_values,
            pulse_mode,
        }
    };
    let mut coin_hoppers = [HopperSettings::default(); HOPPER_COUNT];
    for hopper in coin_hoppers.iter_mut() {
        *hopper = get_hopper(&mut r);
    }
    let item_dispenser = get_hopper(&mut r);
    let mut keys = [[IbuttonKey::EMPTY; KEYS_PER_LEVEL]; KEY_ACCESS_LEVELS];
    for level in keys.iter_mut() {
        for key in level.iter_mut() {
            r.bytes(&mut key.0);
        }
    }
    let workday_start_hour = r.u8();
    let workday_end_hour = r.u8();
    let residual_timeout = r.u8();
    let menu_exit_timeout = r.u8();
    let cash_clear_timeout = r.u8();
    let thanks_message_delay = r.u8();
    let payout_message_delay = r.u8();

    let end = r.at;
    let stored = u16::from_le_bytes([buf[end], buf[end + 1]]);
    if crc16(&buf[..end]) != stored {
        error::raise(Errors::SETTINGS_CRC);
        return Settings::default();
    }
    if version != SETTINGS_VERSION {
        error::raise(Errors::SETTINGS_VERSION);
        return Settings::default();
    }

    Settings {
        version,
        user_language,
        service_language,
        currency,
        machine_id,
        coin_acceptor,
        coin_hoppers,
        item_dispenser,
        keys,
        workday_start_hour,
        workday_end_hour,
        residual_timeout,
        menu_exit_timeout,
        cash_clear_timeout,
        thanks_message_delay,
        payout_message_delay,
    }
}

/// A compile-time check that the level in `Level` is what the record layout assumed.
const _: () = assert!(core::mem::size_of::<Level>() == 2);

// ---------------------------------------------------------------------------------------------
// Event log
//
// A small ring buffer of the last few things that happened, persisted in its own flash segment so
// a technician can reconstruct a dispute ("the machine took my coin and gave nothing") after the
// fact, or spot an intermittent fault the live `Errors` bitset (error.rs) has since cleared. This
// is the port's equivalent of the C original's `EventLog`/`TransactionLog` (`bah_events.c`,
// PORT_AUDIT.md §9/§10), which the port previously had no data structure for at all — only the
// live fault bitset and the aggregate accounting counters were kept.
//
// Read through the service menu the same way the accounting/state reports are: `report::events()`
// builds a paged `Report`, shown by `menu.rs::show_report` exactly like `Item::Accounting`. See
// `Item::EventLog` in `menu.rs`.
// ---------------------------------------------------------------------------------------------

/// How many entries the log holds. One segment of 512 bytes at 16 bytes/entry fits 32; that is a
/// few days of a busy machine's traffic, which is enough to catch a dispute raised soon after it
/// happened without needing a second segment.
pub const EVENT_LOG_ENTRIES: u16 = SEGMENT / EVENT_RECORD;

/// One event-log entry's size on flash: 2 bytes sequence, 1 byte kind, 4 bytes value, 1 byte
/// padding, 2 bytes CRC = 10, rounded up to 16 so entries land on a convenient boundary and there
/// is headroom to widen `value` later without a layout version bump.
const EVENT_RECORD: u16 = 16;

/// One thing the event log remembers.
///
/// There is no RTC on this board (see `ibutton.rs`'s and `cyrillic.rs`'s module docs on why several
/// other features are absent for the same reason), so entries are ordered by a monotonic sequence
/// number rather than a wall-clock timestamp — "the 401st thing that happened" rather than "at
/// 14:32" — which is enough to reconstruct the *order* of events around a dispute even though it
/// cannot give a technician a clock time without cross-referencing something else (a receipt, a
/// phone).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EventLogEntry {
    /// Monotonically increasing across the whole log's life. Wraps like the counter journal's
    /// sequence number does, and is compared the same wrapped way.
    pub seq: u16,
    /// What happened.
    pub kind: EventKind,
    /// The detail that goes with `kind` — see [`EventKind`]'s variants for what this means for
    /// each one.
    pub value: u32,
}

fn encode_event(entry: &EventLogEntry, out: &mut [u8; EVENT_RECORD as usize]) {
    out.fill(0xff);
    let mut cur = Cursor::new(out);
    cur.u16(entry.seq);
    let kind = match entry.kind {
        EventKind::Coin => 0u8,
        EventKind::ItemDispensed => 1,
        EventKind::HopperPayout => 2,
        EventKind::ErrorRaised => 3,
        EventKind::ErrorCleared => 4,
        EventKind::ServiceEntered => 5,
        EventKind::ServiceExited => 6,
    };
    cur.u8(kind);
    cur.u32(entry.value);

    let end = cur.at;
    let crc = crc16(&out[..end]);
    out[end..end + 2].copy_from_slice(&crc.to_le_bytes());
}

fn decode_event(bytes: &[u8; EVENT_RECORD as usize]) -> Option<EventLogEntry> {
    if bytes.iter().all(|&b| b == 0xff) {
        return None;
    }

    let mut r = Reader::new(bytes);
    let seq = r.u16();
    let kind = match r.u8() {
        0 => EventKind::Coin,
        1 => EventKind::ItemDispensed,
        2 => EventKind::HopperPayout,
        3 => EventKind::ErrorRaised,
        4 => EventKind::ErrorCleared,
        5 => EventKind::ServiceEntered,
        6 => EventKind::ServiceExited,
        _ => return None,
    };
    let value = r.u32();

    let end = r.at;
    let stored = u16::from_le_bytes([bytes[end], bytes[end + 1]]);
    if crc16(&bytes[..end]) != stored {
        return None;
    }

    Some(EventLogEntry { seq, kind, value })
}

/// The event log: where the newest entry is, and where the next one goes.
///
/// Modelled on [`Journal`] — append-only within one segment, oldest entries silently lost once the
/// segment fills and wraps — but simpler, because there is only one segment: losing the oldest
/// event once 32 have happened since is an acceptable trade for not needing a second segment on a
/// device this flash-constrained, and the log is diagnostic aid, not the money-critical counters
/// [`Journal`] exists to protect.
pub struct EventLog {
    /// Slot the last entry was written to, or `EVENT_LOG_ENTRIES` if the log has never been
    /// written and the segment needs erasing before the first write.
    slot: u16,
    /// Sequence number of the last entry written.
    seq: u16,
}

impl EventLog {
    fn slot_addr(slot: u16) -> u16 {
        EVENT_LOG_SEGMENT + slot * EVENT_RECORD
    }

    /// Find the newest entry, if any, so appends continue the sequence rather than restart it.
    pub fn open() -> Self {
        let mut best: Option<(u16, u16)> = None;

        for slot in 0..EVENT_LOG_ENTRIES {
            let mut bytes = [0u8; EVENT_RECORD as usize];
            read_bytes(Self::slot_addr(slot), &mut bytes);
            let Some(entry) = decode_event(&bytes) else {
                continue;
            };
            let newer = match &best {
                None => true,
                Some((best_seq, _)) => entry.seq.wrapping_sub(*best_seq) < 0x8000,
            };
            if newer {
                best = Some((entry.seq, slot));
            }
        }

        match best {
            Some((seq, slot)) => Self { slot, seq },
            None => Self {
                slot: EVENT_LOG_ENTRIES - 1,
                seq: 0,
            },
        }
    }

    /// Append one entry.
    ///
    /// Erases the segment before the very first write a machine ever makes to it (an all-`0xff`
    /// segment cannot be written into without erasing first), and otherwise never erases — flash
    /// can only clear bits, so each new entry lands on a slot that is either genuinely blank
    /// (never written since the last erase) or about to be overwritten as part of a full-segment
    /// erase when the write wraps back to slot 0.
    pub fn record(&mut self, kind: EventKind, value: u32) {
        let next = if self.slot + 1 >= EVENT_LOG_ENTRIES {
            0
        } else {
            self.slot + 1
        };

        // Wrapping back to the start of the segment is when the old entries in the rest of the
        // segment would otherwise linger unreadable-but-not-blank; erase the whole segment once,
        // here, rather than track which slots are stale. This also covers the very first write a
        // machine ever makes: `open()` leaves `slot` at `EVENT_LOG_ENTRIES - 1` when nothing
        // readable was found, so `next` is 0 on that first call too.
        if next == 0 {
            erase_segment(EVENT_LOG_SEGMENT);
        }

        let seq = self.seq.wrapping_add(1);
        let entry = EventLogEntry { seq, kind, value };
        let mut bytes = [0u8; EVENT_RECORD as usize];
        encode_event(&entry, &mut bytes);
        write_bytes(Self::slot_addr(next), &bytes);

        self.slot = next;
        self.seq = seq;
    }

    /// Read every live entry back, oldest first.
    ///
    /// `out` is filled left-to-right; entries beyond its length are silently dropped, which only
    /// happens if a caller asks for fewer than [`EVENT_LOG_ENTRIES`] — `report::events` does not.
    pub fn read_all(&self, out: &mut [EventLogEntry]) -> usize {
        let mut entries: heapless::Vec<EventLogEntry, 32> = heapless::Vec::new();

        for slot in 0..EVENT_LOG_ENTRIES {
            let mut bytes = [0u8; EVENT_RECORD as usize];
            read_bytes(Self::slot_addr(slot), &mut bytes);
            if let Some(entry) = decode_event(&bytes) {
                let _ = entries.push(entry);
            }
        }

        // Oldest first: sort by sequence number, wrap-aware in the same way `Journal::open` picks
        // the newest record, but here every live entry matters, not just the latest.
        entries.sort_unstable_by(|a, b| {
            let relative_to_a = b.seq.wrapping_sub(a.seq);
            if relative_to_a == 0 {
                core::cmp::Ordering::Equal
            } else if relative_to_a < 0x8000 {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Greater
            }
        });

        let n = entries.len().min(out.len());
        out[..n].copy_from_slice(&entries[..n]);
        n
    }
}

/// A compile-time check that one event-log segment holds a whole number of records with room to
/// spare, so [`EventLog::record`]'s wrap-at-zero logic never has to straddle a segment boundary.
const _: () = assert!(SEGMENT % EVENT_RECORD == 0);
const _: () = assert!(EVENT_LOG_ENTRIES as usize <= 32);
