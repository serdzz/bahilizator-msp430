//! Turning the counters into something a person can read.
//!
//! In the original machine these lines were the body of an SMS. This build has no GSM, so they go
//! to the display instead — but they are built here, apart from both, because the numbers a report
//! contains have nothing to do with how it is delivered. Adding the modem back is then a matter of
//! handing these lines to it.

use core::fmt::Write;

use heapless::String;

use crate::config::HOPPER_COUNT;
#[cfg(feature = "hw")]
use crate::nvram::{EventLog, EventLogEntry, EVENT_LOG_ENTRIES};
#[cfg(feature = "hw")]
use crate::state::EventKind;
use crate::state::{Accounting, Counters, Settings};
use crate::ui::{format_cash, Row};

/// The longest a report gets. Sized to hold the event log (up to `EVENT_LOG_ENTRIES` lines, the
/// largest report this module produces) with room to spare, and small enough that a copy on the
/// stack is not a problem on a machine with eight kilobytes of RAM.
pub const REPORT_LEN: usize = 768;

/// A whole report.
pub type Report = String<REPORT_LEN>;

/// Money in, money out, and what was sold, over one period.
fn write_accounting(out: &mut Report, label: &str, a: &Accounting, settings: &Settings) {
    let _ = writeln!(out, "{}", label);

    let mut cash = Row::new();
    format_cash(a.cash_in, settings.currency, &mut cash);
    let _ = writeln!(out, " in  {}", cash);

    cash.clear();
    format_cash(a.cash_out, settings.currency, &mut cash);
    let _ = writeln!(out, " out {}", cash);

    let _ = writeln!(out, " sold {}", a.items_dispensed);
    let _ = writeln!(out, " free {}", a.items_free);
}

/// What the machine has taken and sold.
pub fn accounting(counters: &Counters, settings: &Settings) -> Report {
    let mut out = Report::new();
    let _ = writeln!(out, "ID {}", settings.machine_id);
    write_accounting(&mut out, "TOTAL", &counters.overall, settings);
    write_accounting(&mut out, "PERIOD", &counters.period, settings);
    out
}

/// What is left in the machine, and what is wrong with it.
pub fn state(counters: &Counters, settings: &Settings) -> Report {
    let mut out = Report::new();
    let _ = writeln!(out, "ID {}", settings.machine_id);
    let _ = writeln!(out, "items {}", counters.item_level);

    for i in 0..HOPPER_COUNT {
        let _ = writeln!(out, "hopper{} {}", i + 1, counters.coin_levels[i]);
    }

    let errors = crate::error::errors();
    if errors.is_empty() {
        let _ = writeln!(out, "ok");
    } else {
        let _ = writeln!(out, "err {:04x}", errors.bits());
    }

    out
}

/// The event log, newest entry last, for the "Логи" menu item.
///
/// This is the technician-facing view of [`EventLog`] — the equivalent of the C original's "View
/// Events" (`Bahilizator.c`'s menu table, §6 of PORT_AUDIT.md), read through the same paged
/// `show_report` mechanism every other report already uses (`menu.rs::show_report`). One line per
/// entry, oldest first so a technician reading top-to-bottom sees the order things happened in;
/// the newest entries are at the *end*, which on a paged display means paging forward reaches the
/// most recent event last — the direction a story would be told in, not the direction most
/// interesting to a hurried technician, but consistent with how the entries are actually stored
/// and simplest to get right.
#[cfg(feature = "hw")]
pub fn events(log: &EventLog) -> Report {
    let mut entries = [EventLogEntry {
        seq: 0,
        kind: EventKind::Coin,
        value: 0,
    }; EVENT_LOG_ENTRIES as usize];
    let n = log.read_all(&mut entries);

    let mut out = Report::new();
    if n == 0 {
        let _ = writeln!(out, "(пусто)");
        return out;
    }

    for entry in &entries[..n] {
        let line = match entry.kind {
            EventKind::Coin => {
                let mut cash = Row::new();
                // No currency symbol here: the report has no `Settings` to hand, and the raw
                // smallest-unit value is unambiguous enough for a technician cross-checking against
                // a specific coin's denomination.
                let _ = write!(cash, "{}", entry.value);
                cash
            }
            EventKind::ItemDispensed => {
                let mut r = Row::new();
                let _ = r.push_str(if entry.value != 0 { "paid" } else { "free" });
                r
            }
            EventKind::HopperPayout => {
                let mut r = Row::new();
                let _ = write!(r, "#{}", entry.value + 1);
                r
            }
            EventKind::ErrorRaised | EventKind::ErrorCleared => {
                let mut r = Row::new();
                let _ = write!(r, "{:08x}", entry.value);
                r
            }
            EventKind::ServiceEntered => {
                let mut r = Row::new();
                let _ = write!(
                    r,
                    "{}",
                    match entry.value {
                        0 => "owner",
                        1 => "service",
                        _ => "collector",
                    }
                );
                r
            }
            EventKind::ServiceExited => Row::new(),
        };

        let kind = match entry.kind {
            EventKind::Coin => "coin",
            EventKind::ItemDispensed => "item",
            EventKind::HopperPayout => "payout",
            EventKind::ErrorRaised => "err+",
            EventKind::ErrorCleared => "err-",
            EventKind::ServiceEntered => "menu>",
            EventKind::ServiceExited => "menu<",
        };
        let _ = writeln!(out, "{} {} {}", entry.seq, kind, line);
    }

    out
}
