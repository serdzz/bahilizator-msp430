//! What the customer sees.
//!
//! The rest of the firmware describes the situation — [`Screen`] — and this module decides what
//! that looks like in two rows of sixteen characters. Keeping the wording here rather than at the
//! point each message is raised means the vending logic reads as vending logic, and that adding a
//! language is one match arm rather than a search through every task.

use core::fmt::Write;

use embassy_time::{Duration, Timer};
use heapless::String;

use crate::config::{LCD_COLUMNS, LCD_ROWS};
use crate::error::Errors;
use crate::event::SCREEN;
use crate::lcd::Lcd;
use crate::state::{Cash, Currency, Language};

/// One row of text, sized to the display.
pub type Row = String<LCD_COLUMNS>;

/// The situation the display is meant to convey.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Screen {
    /// Nothing is happening; invite a customer.
    Idle {
        /// What a pair of covers costs.
        price: Cash,
        /// Currency to quote it in.
        currency: Currency,
    },
    /// Money is in the machine but not enough, or not yet spent.
    Credit {
        /// What the customer has put in.
        cash: Cash,
        /// What a pair costs.
        price: Cash,
        /// Currency to quote both in.
        currency: Currency,
    },
    /// Covers are being dispensed.
    Dispensing,
    /// Change is waiting in the tray.
    TakeChange {
        /// How much.
        amount: Cash,
        /// Currency to quote it in.
        currency: Currency,
    },
    /// The transaction finished.
    Thanks,
    /// The machine cannot trade.
    Fault(Errors),
    /// Two rows chosen by whoever raised them — the service menu's screens.
    Text(Row, Row),
}

/// Money as a person reads it: `12.50 RUB`.
///
/// The division is by a constant the compiler can turn into shifts and adds, which matters because
/// this CPU has no divider and every screen calls it.
pub fn format_cash(cash: Cash, currency: Currency, out: &mut Row) {
    let _ = write!(out, "{}.{:02} {}", cash / 100, cash % 100, currency.symbol());
}

/// What to say about the worst thing currently wrong.
///
/// One fault at a time, most serious first. A display that cycled through every fault would be
/// unreadable, and the one that stops the machine trading is the one worth acting on.
fn fault_text(errors: Errors, language: Language) -> (&'static str, &'static str) {
    let russian = matches!(language, Language::Russian);
    if errors.contains(Errors::DOOR_OPENED) {
        if russian { ("Открыта дверь", "") } else { ("Door open", "") }
    } else if errors.contains(Errors::ITEM_DISPENSER_EMPTY) {
        if russian { ("Бахилы", "закончились") } else { ("Out of", "shoe covers") }
    } else if errors.contains(Errors::ITEM_DISPENSER) {
        if russian { ("Автомат", "неисправен") } else { ("Machine", "out of order") }
    } else if errors.contains(Errors::FIRMWARE) {
        if russian { ("Ошибка", "прошивки") } else { ("Firmware", "error") }
    } else if errors.contains(Errors::COIN_ACCEPTOR) {
        if russian { ("Монетоприёмник", "неисправен") } else { ("Coin acceptor", "fault") }
    } else if errors.contains(Errors::CANNOT_PAYOUT) {
        if russian { ("Нет сдачи", "Без сдачи") } else { ("No change", "Exact money only") }
    } else if russian {
        ("Автомат", "неисправен")
    } else {
        ("Machine", "out of order")
    }
}

/// Turn a screen into the two rows to draw.
pub fn render(screen: &Screen, language: Language) -> (Row, Row) {
    let russian = matches!(language, Language::Russian);
    let mut top = Row::new();
    let mut bottom = Row::new();

    match screen {
        Screen::Idle { price, currency } => {
            let _ = top.push_str(if russian { "Бахилы" } else { "Shoe covers" });
            let _ = bottom.push_str(if russian { "Цена " } else { "Price " });
            format_cash(*price, *currency, &mut bottom);
        }
        Screen::Credit { cash, price, currency } => {
            let _ = top.push_str(if russian { "Внесено " } else { "Credit " });
            format_cash(*cash, *currency, &mut top);
            if cash < price {
                let _ = bottom.push_str(if russian { "Нужно ещё " } else { "Need " });
                format_cash(price - cash, *currency, &mut bottom);
            } else {
                let _ = bottom.push_str(if russian { "Нажмите ВЫДАЧА" } else { "Press DISPENSE" });
            }
        }
        Screen::Dispensing => {
            let _ = top.push_str(if russian { "Выдача" } else { "Dispensing" });
            let _ = bottom.push_str(if russian { "Подождите..." } else { "Please wait..." });
        }
        Screen::TakeChange { amount, currency } => {
            let _ = top.push_str(if russian { "Возьмите сдачу" } else { "Take your change" });
            format_cash(*amount, *currency, &mut bottom);
        }
        Screen::Thanks => {
            let _ = top.push_str(if russian { "Спасибо!" } else { "Thank you!" });
        }
        Screen::Fault(errors) => {
            let (a, b) = fault_text(*errors, language);
            let _ = top.push_str(a);
            let _ = bottom.push_str(b);
        }
        Screen::Text(a, b) => {
            top = a.clone();
            bottom = b.clone();
        }
    }

    (top, bottom)
}

/// Draw whatever the machine most recently asked for.
///
/// Only the newest screen is drawn. If two arrive between redraws the older one is dropped rather
/// than queued, because showing it first would be a flicker that told the customer nothing.
pub async fn run(mut lcd: Lcd<'_>, language: Language) {
    let mut shown: Option<(Row, Row)> = None;

    loop {
        let screen = SCREEN.wait().await;
        let rows = render(&screen, language);

        // Redrawing what is already up would blink the display for no reason: the enable strobe
        // walks the cursor across every cell whether the byte changed or not.
        if shown.as_ref() == Some(&rows) {
            continue;
        }

        for row in 0..LCD_ROWS {
            let text = if row == 0 { &rows.0 } else { &rows.1 };
            lcd.write_row(row, text).await;
        }
        shown = Some(rows);
    }
}

/// Show `screen` for `seconds`, then let whatever comes next take over.
///
/// The wait is here rather than in the caller so that a message with a dwell time cannot be
/// silently replaced a millisecond after it appears.
pub async fn show_for(screen: Screen, seconds: u8) {
    crate::event::show(screen);
    Timer::after(Duration::from_secs(seconds as u64)).await;
}
