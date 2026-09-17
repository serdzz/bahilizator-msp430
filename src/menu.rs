//! The service menu.
//!
//! It runs *inside* the vending task rather than beside it. That is deliberate: the menu refills
//! hoppers, gives covers away and changes prices, all of which are writes to the same counters the
//! sale logic owns. Running it as its own task would mean sharing that state behind a lock, and a
//! lock is a way for a customer's transaction and an engineer's refill to interleave. Borrowing the
//! task instead makes the interleaving impossible rather than merely unlikely.
//!
//! While the menu is open the machine is not selling. It stops taking coins on the way in and takes
//! them again on the way out.

#[cfg(feature = "hw")]
use core::fmt::Write;

#[cfg(feature = "hw")]
use embassy_futures::select::{Either, select};
#[cfg(feature = "hw")]
use embassy_time::{Duration, Timer};
use heapless::String;

#[cfg(feature = "hw")]
use crate::config::HOPPER_COUNT;
#[cfg(feature = "hw")]
use crate::event::{Button, Event};
#[cfg(feature = "hw")]
use crate::nvram;
#[cfg(feature = "hw")]
use crate::report;
#[cfg(feature = "hw")]
use crate::state::Level;
use crate::state::{IbuttonKey, Settings};
use crate::ui::{Row, Screen};
#[cfg(feature = "hw")]
use crate::vending::Machine;

/// How far into the machine a key lets somebody.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Access {
    /// Everything, including prices and enrolled keys.
    Owner,
    /// Refilling, counters, and clearing the period.
    Service,
    /// Counters only.
    Collector,
}

impl Access {
    /// Which access level `key` has, if any.
    pub fn of(key: &IbuttonKey, settings: &Settings) -> Option<Access> {
        for (level, keys) in settings.keys.iter().enumerate() {
            if keys.iter().any(|k| !k.is_empty() && k == key) {
                return Some(match level {
                    0 => Access::Owner,
                    1 => Access::Service,
                    _ => Access::Collector,
                });
            }
        }
        None
    }
}

/// One line of the menu.
#[cfg(feature = "hw")]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Item {
    /// Show what has been taken and sold.
    Accounting,
    /// Show what is left in the machine.
    State,
    /// Add covers to the dispenser's count.
    RefillItems,
    /// Add coins to a hopper's count.
    RefillHopper(usize),
    /// Give away one pair.
    FreeItem,
    /// Change what a pair costs.
    Price,
    /// Zero the period counters.
    ClearPeriod,
}

#[cfg(feature = "hw")]
impl Item {
    /// The label to show.
    fn label(self) -> &'static str {
        match self {
            Item::Accounting => "Отчёт: касса",
            Item::State => "Отчёт: остаток",
            Item::RefillItems => "Загрузка бахил",
            Item::RefillHopper(0) => "Загрузка монет 1",
            Item::RefillHopper(_) => "Загрузка монет 2",
            Item::FreeItem => "Выдать бахилы",
            Item::Price => "Цена",
            Item::ClearPeriod => "Сброс периода",
        }
    }

    /// The lowest access level that may use this item.
    ///
    /// Ordered so that a collector, who only empties the cashbox, cannot change a price.
    fn needs(self) -> Access {
        match self {
            Item::Accounting | Item::State => Access::Collector,
            Item::RefillItems | Item::RefillHopper(_) | Item::FreeItem | Item::ClearPeriod => {
                Access::Service
            }
            Item::Price => Access::Owner,
        }
    }
}

/// Every item, in the order they appear.
#[cfg(feature = "hw")]
const ITEMS: [Item; 8] = [
    Item::Accounting,
    Item::State,
    Item::RefillItems,
    Item::RefillHopper(0),
    Item::RefillHopper(1),
    Item::FreeItem,
    Item::Price,
    Item::ClearPeriod,
];

/// Wait for a button, or give up after the configured idle time.
///
/// Returns `None` on the timeout, which every screen below treats as "leave things as they were".
/// An engineer who walks away from an open menu must not leave the machine out of service.
#[cfg(feature = "hw")]
async fn button(settings: &Settings) -> Option<Button> {
    let limit = Duration::from_secs(settings.menu_exit_timeout as u64);
    loop {
        match select(crate::event::next(), Timer::after(limit)).await {
            Either::Second(()) => return None,
            Either::First(Event::Button(b)) => return Some(b),
            // Coins and keys arriving while the menu is open are ignored rather than queued: acting
            // on them later, after the engineer has changed the price, would be worse than losing
            // them, and the acceptor is inhibited so there should not be any.
            Either::First(_) => {}
        }
    }
}

/// Show a report one screen at a time.
///
/// Reports are longer than the display, so they are paged rather than scrolled: two rows at a time,
/// advanced by any button. Scrolling would need a timer and would move under the reader's eye.
#[cfg(feature = "hw")]
async fn show_report(text: &report::Report, settings: &Settings) {
    let mut lines = text.lines();
    loop {
        let Some(top) = lines.next() else { return };
        let bottom = lines.next().unwrap_or("");

        let mut a = Row::new();
        let _ = a.push_str(top);
        let mut b = Row::new();
        let _ = b.push_str(bottom);
        crate::event::show(Screen::Text(a, b));

        if button(settings).await.is_none() {
            return;
        }
    }
}

/// Edit a number with the up and down buttons.
///
/// Returns the new value, or `None` if the engineer cancelled or walked away. The step is a
/// parameter because refilling a hopper in ones would take all afternoon.
#[cfg(feature = "hw")]
async fn edit(
    title: &str,
    mut value: u32,
    step: u32,
    max: u32,
    settings: &Settings,
) -> Option<u32> {
    loop {
        let mut top = Row::new();
        let _ = top.push_str(title);
        let mut bottom = Row::new();
        let _ = write!(bottom, "{}", value);
        crate::event::show(Screen::Text(top, bottom));

        match button(settings).await? {
            Button::Next => value = (value + step).min(max),
            Button::Prev => value = value.saturating_sub(step),
            Button::Ok => return Some(value),
            Button::Cancel => return None,
        }
    }
}

/// Run one menu item.
#[cfg(feature = "hw")]
async fn run_item(machine: &mut Machine, item: Item) {
    match item {
        Item::Accounting => {
            let text = report::accounting(&machine.counters, &machine.settings);
            show_report(&text, &machine.settings).await;
        }

        Item::State => {
            let text = report::state(&machine.counters, &machine.settings);
            show_report(&text, &machine.settings).await;
        }

        Item::RefillItems => {
            let max = machine.settings.item_dispenser.max_level as u32;
            if let Some(v) = edit(
                "Бахилы",
                machine.counters.item_level as u32,
                100,
                max,
                &machine.settings,
            )
            .await
            {
                machine.counters.item_level = v as Level;
                machine.journal.save(&machine.counters);
                crate::error::clear(crate::error::Errors::ITEM_DISPENSER_EMPTY);
            }
        }

        Item::RefillHopper(i) if i < HOPPER_COUNT => {
            let max = machine.settings.coin_hoppers[i].max_level as u32;
            if let Some(v) = edit(
                "Монеты",
                machine.counters.coin_levels[i] as u32,
                10,
                max,
                &machine.settings,
            )
            .await
            {
                machine.counters.coin_levels[i] = v as Level;
                machine.journal.save(&machine.counters);
                crate::error::clear(crate::error::Errors::COIN_HOPPER_EMPTY);
            }
        }

        Item::RefillHopper(_) => {}

        Item::FreeItem => machine.dispense_free().await,

        Item::Price => {
            let current = machine.settings.item_dispenser.unit_value;
            if let Some(v) = edit("Цена, коп.", current, 50, 100_000, &machine.settings).await {
                machine.settings.item_dispenser.unit_value = v;
                nvram::save_settings(&machine.settings);
            }
        }

        Item::ClearPeriod => {
            machine.counters.period = Default::default();
            machine.journal.save(&machine.counters);
        }

    }
}

/// Open the menu at `access` and stay in it until the engineer leaves or stops pressing things.
#[cfg(feature = "hw")]
pub async fn run(machine: &mut Machine, access: Access) {
    let mut cursor = 0usize;

    loop {
        // Items above the holder's access level are hidden rather than shown and refused. There is
        // nothing to be gained by telling a collector what they are not allowed to do.
        let visible: heapless::Vec<Item, 8> = ITEMS
            .iter()
            .copied()
            .filter(|i| i.needs() >= access)
            // A machine built with one coin hopper must not offer to refill a second.
            .filter(|i| !matches!(i, Item::RefillHopper(n) if *n >= HOPPER_COUNT))
            .collect();

        if visible.is_empty() {
            return;
        }
        cursor = cursor.min(visible.len() - 1);

        let mut top = Row::new();
        let _ = top.push_str(visible[cursor].label());
        let mut bottom = Row::new();
        let _ = write!(bottom, "{}/{}", cursor + 1, visible.len());
        crate::event::show(Screen::Text(top, bottom));

        match button(&machine.settings).await {
            None | Some(Button::Cancel) => return,
            Some(Button::Next) => cursor = (cursor + 1) % visible.len(),
            Some(Button::Prev) => {
                cursor = if cursor == 0 { visible.len() - 1 } else { cursor - 1 }
            }
            Some(Button::Ok) => run_item(machine, visible[cursor]).await,
        }
    }
}

/// The greeting shown when an unknown key is presented.
///
/// Silence would be indistinguishable from a broken reader, and somebody holding a key that used to
/// work deserves to know which of the two it is.
pub fn unknown_key_screen() -> Screen {
    let mut top = Row::new();
    let _ = top.push_str("Ключ неизвестен");
    Screen::Text(top, String::new())
}
