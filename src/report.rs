//! Turning the counters into something a person can read.
//!
//! In the original machine these lines were the body of an SMS. This build has no GSM, so they go
//! to the display instead — but they are built here, apart from both, because the numbers a report
//! contains have nothing to do with how it is delivered. Adding the modem back is then a matter of
//! handing these lines to it.

use core::fmt::Write;

use heapless::String;

use crate::config::HOPPER_COUNT;
use crate::state::{Accounting, Counters, Settings};
use crate::ui::{format_cash, Row};

/// The longest a report gets. Sized to hold the accounting block with room to spare, and small
/// enough that a copy on the stack is not a problem on a machine with eight kilobytes of RAM.
pub const REPORT_LEN: usize = 256;

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
