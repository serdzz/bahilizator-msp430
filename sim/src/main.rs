//! Interactive desktop simulator for the bahilizator firmware.
//!
//! This does **not** reuse the MSP430 firmware's async tasks (`buttons::input_task`,
//! `coin_acceptor::coin_task`, `vending::vending_task`, ...) verbatim: those are written against
//! `embassy-msp430`'s `Input`/`Output`/`Flex` GPIO types and `Hopper<'static>`, which read and
//! drive real pins and have no std equivalent worth faking convincingly. Instead this binary is a
//! synchronous reimplementation of the same state machine — coin acceptance, change planning,
//! payout bookkeeping, the service menu — built directly on the hardware-agnostic pieces that *do*
//! come from the `bahilizator` library unchanged: [`bahilizator::state`], [`bahilizator::error`],
//! [`bahilizator::ui`] (screen rendering into two 16-character rows), [`bahilizator::report`], and
//! [`bahilizator::menu::Access`] (key-to-privilege mapping). That keeps money-handling and rendering
//! logic byte-for-byte the same code the real firmware runs, while giving up reuse only of the
//! parts that are inherently about physical pins.
//!
//! See `SIM.md` for the keymap and known limitations.

use std::io::{Write, stdout};
use std::time::Duration as StdDuration;

use bahilizator::config;
use bahilizator::error::{self, Errors};
use bahilizator::event::Button;
use bahilizator::menu::Access;
use bahilizator::report;
use bahilizator::state::{AppState, Cash, Counters, IbuttonKey, Language, Level, Settings};
use bahilizator::ui::{self, Row, Screen};

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use crossterm::terminal;

/// A plausible DS1990 serial for each role, distinct from each other and from
/// [`bahilizator::state::IbuttonKey::EMPTY`]. Real checksums do not matter here: nothing in the
/// sim re-derives them from 1-Wire bits, [`Access::of`] just compares the bytes against what is
/// enrolled in `Settings::keys`, and the sim enrolls exactly these at startup so presenting 'o',
/// 's', or 'c' behaves like showing a real, already-enrolled key to the reader.
const OWNER_KEY: IbuttonKey = IbuttonKey([0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01, 0xAA]);
const SERVICE_KEY: IbuttonKey = IbuttonKey([0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x02, 0xBB]);
const COLLECTOR_KEY: IbuttonKey = IbuttonKey([0x01, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x03, 0xCC]);

/// One line of the service menu, mirroring `menu::Item` (private to that module, so reimplemented
/// here rather than exposed).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Item {
    Accounting,
    State,
    RefillItems,
    RefillHopper(usize),
    FreeItem,
    Price,
    ClearPeriod,
}

impl Item {
    fn label(self) -> String {
        match self {
            Item::Accounting => "Report: takings".into(),
            Item::State => "Report: stock".into(),
            Item::RefillItems => "Refill covers".into(),
            Item::RefillHopper(i) => format!("Refill coins {}", i + 1),
            Item::FreeItem => "Give away pair".into(),
            Item::Price => "Price".into(),
            Item::ClearPeriod => "Clear period".into(),
        }
    }

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

fn all_items() -> Vec<Item> {
    let mut v = vec![Item::Accounting, Item::State, Item::RefillItems];
    for i in 0..config::HOPPER_COUNT {
        v.push(Item::RefillHopper(i));
    }
    v.push(Item::FreeItem);
    v.push(Item::Price);
    v.push(Item::ClearPeriod);
    v
}

/// A coin hopper or the item dispenser, simulated: a stock count and nothing else. The real
/// [`bahilizator::hopper::Hopper`] runs a motor and counts sensor pulses; the sim has no motor, so
/// "dispensing" is just decrementing the count and reporting success unless it is already empty.
struct SimHopper {
    level: Level,
}

impl SimHopper {
    /// Dispense up to `count` units. Returns how many actually came out — always `count` unless
    /// the hopper ran dry partway through, exactly like the real one running out of coin.
    fn dispense(&mut self, count: Level) -> Level {
        let out = count.min(self.level);
        self.level -= out;
        out
    }
}

/// What the machine is doing right now, mirroring `vending::Machine` + the borrow the real service
/// menu takes on it — but synchronous, and with sim hoppers instead of real ones.
struct Machine {
    settings: Settings,
    counters: Counters,
    hoppers: Vec<SimHopper>,
    dispenser: SimHopper,
    log: Vec<String>,
}

fn push_log(log: &mut Vec<String>, line: String) {
    log.push(line);
    let len = log.len();
    if len > 200 {
        log.drain(0..len - 200);
    }
}

impl Machine {
    fn price(&self) -> Cash {
        self.settings.item_dispenser.unit_value
    }

    fn refresh_display(&self) -> Screen {
        if !error::errors().can_vend() {
            Screen::Fault(error::errors())
        } else if self.counters.cash > 0 {
            Screen::Credit {
                cash: self.counters.cash,
                price: self.price(),
                currency: self.settings.currency,
            }
        } else {
            Screen::Idle {
                price: self.price(),
                currency: self.settings.currency,
            }
        }
    }

    fn accept_coin(&mut self, value: Cash) {
        self.counters.cash = self.counters.cash.saturating_add(value);
        self.counters.overall.cash_in = self.counters.overall.cash_in.saturating_add(value);
        self.counters.period.cash_in = self.counters.period.cash_in.saturating_add(value);
    }

    fn plan_change(&self, amount: Cash) -> (Vec<Level>, Cash) {
        let n = config::HOPPER_COUNT;
        let mut plan = vec![0u16; n];
        let mut left = amount;

        let mut order: Vec<usize> = (0..n).collect();
        order.sort_unstable_by(|&a, &b| {
            self.settings.coin_hoppers[b]
                .unit_value
                .cmp(&self.settings.coin_hoppers[a].unit_value)
        });

        for &i in &order {
            let hopper = &self.settings.coin_hoppers[i];
            if !hopper.enabled || hopper.unit_value == 0 {
                continue;
            }
            let available = self.counters.coin_levels[i] as Cash;
            let wanted = left / hopper.unit_value;
            let take = wanted.min(available);
            plan[i] = take as u16;
            left -= take * hopper.unit_value;
        }

        (plan, left)
    }

    /// Pay out items already bought, all in one go (the sim has no motor to time).
    fn payout_items(&mut self) {
        while self.counters.items_pending > 0 || self.counters.free_items_pending > 0 {
            let free = self.counters.items_pending == 0;
            let dispensed = self.dispenser.dispense(1);

            if dispensed > 0 {
                if free {
                    self.counters.free_items_pending -= 1;
                    self.counters.overall.items_free += 1;
                    self.counters.period.items_free += 1;
                } else {
                    self.counters.items_pending -= 1;
                    self.counters.overall.items_dispensed += 1;
                    self.counters.period.items_dispensed += 1;
                }
                self.counters.item_level = self.dispenser.level;
            } else {
                error::raise(Errors::ITEM_DISPENSER);
                push_log(&mut self.log, "Dispenser: jammed/empty!".into());
                return;
            }

            error::set(
                Errors::ITEM_DISPENSER_EMPTY,
                self.counters.item_level <= self.settings.item_dispenser.min_level,
            );
        }
        self.counters.app_state = AppState::ProcessResidual;
    }

    fn payout_change(&mut self) {
        for i in 0..config::HOPPER_COUNT {
            while self.counters.coins_pending[i] > 0 {
                let dispensed = self.hoppers[i].dispense(1);
                if dispensed > 0 {
                    self.counters.coins_pending[i] -= 1;
                    self.counters.coin_levels[i] = self.hoppers[i].level;
                    let value = self.settings.coin_hoppers[i].unit_value;
                    self.counters.overall.cash_out =
                        self.counters.overall.cash_out.saturating_add(value);
                    self.counters.period.cash_out =
                        self.counters.period.cash_out.saturating_add(value);
                } else {
                    error::raise(Errors::COIN_HOPPER);
                    push_log(
                        &mut self.log,
                        format!("Hopper {}: empty, cannot finish payout.", i + 1),
                    );
                    return;
                }
            }
            error::set(
                Errors::COIN_HOPPER_EMPTY,
                self.counters.coin_levels[i] <= self.settings.coin_hoppers[i].min_level,
            );
        }
    }

    fn process_residual(&mut self, screen_out: &mut Option<Screen>) {
        if self.counters.cash == 0 {
            self.counters.app_state = AppState::AcceptCash;
            return;
        }
        let (plan, unpayable) = self.plan_change(self.counters.cash);
        for (i, count) in plan.iter().enumerate() {
            self.counters.coins_pending[i] += count;
        }
        self.counters.cash = unpayable;
        self.counters.app_state = AppState::PayoutReminder;
        error::set(Errors::CANNOT_PAYOUT, unpayable > 0);

        let owed: Cash = plan
            .iter()
            .enumerate()
            .map(|(i, &n)| n as Cash * self.settings.coin_hoppers[i].unit_value)
            .sum();

        self.payout_change();

        if owed > 0 {
            *screen_out = Some(Screen::TakeChange {
                amount: owed,
                currency: self.settings.currency,
            });
            push_log(&mut self.log, format!("Change dispensed: {} kop.", owed));
        }
        if self.counters.cash == 0 && owed == 0 {
            *screen_out = Some(Screen::Thanks);
        }
        self.counters.app_state = AppState::AcceptCash;
    }

    fn sell(&mut self, screen_out: &mut Option<Screen>) {
        let price = self.price();
        if self.counters.cash < price || !error::errors().can_vend() {
            return;
        }
        self.counters.cash -= price;
        self.counters.items_pending += 1;
        self.counters.app_state = AppState::PayoutItems;
        push_log(&mut self.log, "Sale: dispensing 1 pair.".into());
        self.payout_items();
        self.process_residual(screen_out);
    }

    fn dispense_free(&mut self) {
        self.counters.free_items_pending += 1;
        self.payout_items();
        push_log(&mut self.log, "Menu: gave away 1 pair free.".into());
    }
}

/// The two things the machine can be doing. The real firmware borrows the vending task while the
/// service menu is open; here that is a mode flag on the same loop.
enum Mode {
    Vending,
    Menu { cursor: usize, access: Access },
    Editing {
        item: Item,
        title: &'static str,
        value: u32,
        step: u32,
        max: u32,
        access: Access,
    },
    Report {
        lines: Vec<String>,
        at: usize,
        access: Access,
    },
}

fn main() -> std::io::Result<()> {
    let mut settings = Settings::default();
    settings.keys[0][0] = OWNER_KEY;
    settings.keys[1][0] = SERVICE_KEY;
    settings.keys[2][0] = COLLECTOR_KEY;

    let mut counters = Counters::new();
    counters.item_level = 50;
    counters.coin_levels = core::array::from_fn(|_| 30);

    let hoppers: Vec<SimHopper> = (0..config::HOPPER_COUNT)
        .map(|_| SimHopper { level: 30 })
        .collect();
    let dispenser = SimHopper { level: 50 };

    let mut machine = Machine {
        settings,
        counters,
        hoppers,
        dispenser,
        log: vec!["Simulator started. Press 'h' for help, 'q' to quit.".to_string()],
    };

    let mut mode = Mode::Vending;
    let mut screen = machine.refresh_display();
    let mut shown: Option<(Row, Row)> = None;

    terminal::enable_raw_mode()?;
    let mut out = stdout();
    // Fresh screen, cursor parked below where we draw.
    write!(out, "\x1b[2J")?;

    let result = run_loop(&mut machine, &mut mode, &mut screen, &mut shown, &mut out);

    terminal::disable_raw_mode()?;
    println!();
    result
}

fn run_loop(
    machine: &mut Machine,
    mode: &mut Mode,
    screen: &mut Screen,
    shown: &mut Option<(Row, Row)>,
    out: &mut std::io::Stdout,
) -> std::io::Result<()> {
    let display = mode_screen(machine, mode, screen);
    draw(machine, &display, shown, out)?;

    loop {
        if !event::poll(StdDuration::from_millis(80))? {
            continue;
        }
        let TermEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        let mut dirty = true;
        let mut popup: Option<Screen> = None;

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break,
            KeyCode::Char('h') => {
                push_log(
                    &mut machine.log,
                    "Keys: ←/→/Enter/\\=buttons  o/s/c=iButton  1-6=coins  A/S=hopper1/2  D=dispenser  q=quit"
                        .into(),
                );
            }

            // --- Buttons -------------------------------------------------------------------
            KeyCode::Left => handle_button(machine, mode, Button::Prev, &mut popup),
            KeyCode::Right => handle_button(machine, mode, Button::Next, &mut popup),
            KeyCode::Enter => handle_button(machine, mode, Button::Ok, &mut popup),
            KeyCode::Char('\\') | KeyCode::Delete | KeyCode::Backspace => {
                handle_button(machine, mode, Button::Cancel, &mut popup)
            }

            // --- Coin acceptor, channels 1-6 -------------------------------------------------
            KeyCode::Char(c @ '1'..='6') => {
                let channel = (c as u8 - b'1') as usize;
                if matches!(mode, Mode::Vending) {
                    let value = machine.settings.coin_acceptor.channel_values[channel];
                    push_log(
                        &mut machine.log,
                        format!("Coin: channel {} (+{} kop.)", channel + 1, value),
                    );
                    machine.accept_coin(value);
                    if machine.counters.cash >= machine.price() {
                        machine.sell(&mut popup);
                    }
                    *screen = machine.refresh_display();
                } else {
                    push_log(
                        &mut machine.log,
                        "Coin ignored: acceptor is inhibited while the menu is open.".into(),
                    );
                }
            }

            // --- iButton -----------------------------------------------------------------
            KeyCode::Char('o') => present_key(machine, mode, OWNER_KEY, "Owner"),
            KeyCode::Char('s') => present_key(machine, mode, SERVICE_KEY, "Service"),
            KeyCode::Char('c') => present_key(machine, mode, COLLECTOR_KEY, "Collector"),

            // --- Hoppers / dispenser, manual pulse -----------------------------------------
            KeyCode::Char('A') => manual_hopper(machine, 0),
            KeyCode::Char('S') => manual_hopper(machine, 1),
            KeyCode::Char('D') => manual_dispenser(machine),

            _ => dirty = false,
        }

        if let Some(p) = popup {
            *screen = p;
        } else if matches!(mode, Mode::Vending) {
            *screen = machine.refresh_display();
        }

        if dirty {
            let display = mode_screen(machine, mode, screen);
            draw(machine, &display, shown, out)?;
        }
    }

    Ok(())
}

fn handle_button(machine: &mut Machine, mode: &mut Mode, button: Button, popup: &mut Option<Screen>) {
    match mode {
        Mode::Vending => match button {
            Button::Ok => {
                machine.sell(popup);
            }
            Button::Cancel => {
                if machine.counters.cash > 0 {
                    machine.process_residual(popup);
                }
            }
            Button::Prev | Button::Next => {}
        },

        Mode::Menu { cursor, access } => {
            let items = all_items();
            let visible: Vec<Item> = items
                .into_iter()
                .filter(|i| i.needs() >= *access)
                .filter(|i| !matches!(i, Item::RefillHopper(n) if *n >= config::HOPPER_COUNT))
                .collect();
            if visible.is_empty() {
                *mode = Mode::Vending;
                return;
            }
            *cursor = (*cursor).min(visible.len() - 1);

            let access = *access;
            match button {
                Button::Cancel => *mode = Mode::Vending,
                Button::Next => *cursor = (*cursor + 1) % visible.len(),
                Button::Prev => {
                    *cursor = if *cursor == 0 { visible.len() - 1 } else { *cursor - 1 }
                }
                Button::Ok => {
                    let item = visible[*cursor];
                    run_menu_item(machine, mode, item, access)
                }
            }
        }

        Mode::Editing {
            item,
            value,
            step,
            max,
            access,
            ..
        } => match button {
            Button::Next => *value = (*value + *step).min(*max),
            Button::Prev => *value = value.saturating_sub(*step),
            Button::Cancel => *mode = Mode::Menu { cursor: 0, access: *access },
            Button::Ok => {
                apply_edit(machine, *item, *value);
                *mode = Mode::Menu { cursor: 0, access: *access };
            }
        },

        Mode::Report { lines, at, access } => match button {
            Button::Cancel => *mode = Mode::Vending,
            _ => {
                *at += 2;
                if *at >= lines.len() {
                    *mode = Mode::Menu { cursor: 0, access: *access };
                }
            }
        },
    }
}

fn apply_edit(machine: &mut Machine, item: Item, value: u32) {
    match item {
        Item::RefillItems => {
            machine.counters.item_level = value as Level;
            machine.dispenser.level = value as Level;
            error::clear(Errors::ITEM_DISPENSER_EMPTY);
            push_log(&mut machine.log, format!("Refilled covers to {}.", value));
        }
        Item::RefillHopper(i) => {
            machine.counters.coin_levels[i] = value as Level;
            machine.hoppers[i].level = value as Level;
            error::clear(Errors::COIN_HOPPER_EMPTY);
            push_log(&mut machine.log, format!("Refilled hopper {} to {}.", i + 1, value));
        }
        Item::Price => {
            machine.settings.item_dispenser.unit_value = value;
            push_log(&mut machine.log, format!("Price set to {} kop.", value));
        }
        _ => {}
    }
}

fn run_menu_item(machine: &mut Machine, mode: &mut Mode, item: Item, access: Access) {
    match item {
        Item::Accounting => {
            let text = report::accounting(&machine.counters, &machine.settings);
            *mode = Mode::Report {
                lines: text.lines().map(|s| s.to_string()).collect(),
                at: 0,
                access,
            };
        }
        Item::State => {
            let text = report::state(&machine.counters, &machine.settings);
            *mode = Mode::Report {
                lines: text.lines().map(|s| s.to_string()).collect(),
                at: 0,
                access,
            };
        }
        Item::RefillItems => {
            *mode = Mode::Editing {
                item,
                title: "Covers",
                value: machine.counters.item_level as u32,
                step: 5,
                max: machine.settings.item_dispenser.max_level as u32,
                access,
            };
        }
        Item::RefillHopper(i) => {
            *mode = Mode::Editing {
                item,
                title: "Coins",
                value: machine.counters.coin_levels[i] as u32,
                step: 5,
                max: machine.settings.coin_hoppers[i].max_level as u32,
                access,
            };
        }
        Item::FreeItem => {
            machine.dispense_free();
        }
        Item::Price => {
            *mode = Mode::Editing {
                item,
                title: "Price, kop.",
                value: machine.settings.item_dispenser.unit_value,
                step: 50,
                max: 100_000,
                access,
            };
        }
        Item::ClearPeriod => {
            machine.counters.period = Default::default();
            push_log(&mut machine.log, "Period counters cleared.".into());
        }
    }
}

fn present_key(machine: &mut Machine, mode: &mut Mode, key: IbuttonKey, name: &str) {
    if !matches!(mode, Mode::Vending) {
        push_log(&mut machine.log, format!("iButton {} ignored: menu already open.", name));
        return;
    }
    match Access::of(&key, &machine.settings) {
        Some(access) => {
            push_log(
                &mut machine.log,
                format!("iButton: {} presented -> service menu ({:?}).", name, access),
            );
            *mode = Mode::Menu { cursor: 0, access };
        }
        None => {
            push_log(&mut machine.log, format!("iButton: {} key not enrolled.", name));
        }
    }
}

fn manual_hopper(machine: &mut Machine, index: usize) {
    if index >= config::HOPPER_COUNT {
        push_log(
            &mut machine.log,
            format!(
                "Hopper {}: not configured (this build has {} coin hopper(s)).",
                index + 1,
                config::HOPPER_COUNT
            ),
        );
        return;
    }
    let dispensed = machine.hoppers[index].dispense(1);
    machine.counters.coin_levels[index] = machine.hoppers[index].level;
    if dispensed > 0 {
        push_log(
            &mut machine.log,
            format!(
                "Hopper {}: manual payout pulse, 1 coin (level now {}).",
                index + 1,
                machine.hoppers[index].level
            ),
        );
    } else {
        push_log(&mut machine.log, format!("Hopper {}: empty, no coin to pay out.", index + 1));
    }
}

fn manual_dispenser(machine: &mut Machine) {
    let dispensed = machine.dispenser.dispense(1);
    machine.counters.item_level = machine.dispenser.level;
    if dispensed > 0 {
        push_log(
            &mut machine.log,
            format!("Dispenser: manual pulse, 1 pair (level now {}).", machine.dispenser.level),
        );
    } else {
        push_log(&mut machine.log, "Dispenser: empty, no pair to dispense.".into());
    }
    error::set(
        Errors::ITEM_DISPENSER_EMPTY,
        machine.counters.item_level <= machine.settings.item_dispenser.min_level,
    );
}

// -----------------------------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------------------------

fn mode_screen(_machine: &Machine, mode: &Mode, fallback: &Screen) -> Screen {
    match mode {
        Mode::Vending => fallback.clone(),
        Mode::Menu { cursor, access } => {
            let items = all_items();
            let visible: Vec<Item> = items
                .into_iter()
                .filter(|i| i.needs() >= *access)
                .filter(|i| !matches!(i, Item::RefillHopper(n) if *n >= config::HOPPER_COUNT))
                .collect();
            if visible.is_empty() {
                return Screen::Text(Row::new(), Row::new());
            }
            let idx = (*cursor).min(visible.len() - 1);
            let mut top = Row::new();
            let _ = top.push_str(&items_truncate(&visible[idx].label()));
            let mut bottom = Row::new();
            let _ = std::fmt::write(&mut bottom, format_args!("{}/{}", idx + 1, visible.len()));
            Screen::Text(top, bottom)
        }
        Mode::Editing { title, value, .. } => {
            let mut top = Row::new();
            let _ = top.push_str(title);
            let mut bottom = Row::new();
            let _ = std::fmt::write(&mut bottom, format_args!("{}", value));
            Screen::Text(top, bottom)
        }
        Mode::Report { lines, at, .. } => {
            let a = lines.get(*at).map(String::as_str).unwrap_or("");
            let b = lines.get(*at + 1).map(String::as_str).unwrap_or("");
            let mut top = Row::new();
            let _ = top.push_str(&items_truncate(a));
            let mut bottom = Row::new();
            let _ = bottom.push_str(&items_truncate(b));
            Screen::Text(top, bottom)
        }
    }
}

fn items_truncate(s: &str) -> String {
    s.chars().take(config::LCD_COLUMNS).collect()
}

fn draw(
    machine: &Machine,
    fallback: &Screen,
    shown: &mut Option<(Row, Row)>,
    out: &mut std::io::Stdout,
) -> std::io::Result<()> {
    // Recompute mode-derived screen each redraw so menu/report/editing text stays fresh even
    // though `fallback` is only updated by vending events.
    let (top, bottom) = ui::render(fallback, Language::Russian);
    *shown = Some((top.clone(), bottom.clone()));

    // In raw mode the terminal does NOT translate '\n' into a carriage return, so a bare
    // '\n' just moves the cursor down one row without returning to column 0 — every
    // subsequent line drifts further right ("staircasing"). Every line end must be an
    // explicit "\r\n", and writeln!/println! must not be used here.
    write!(out, "\x1b[H")?; // cursor home, keep scrollback intact
    write!(out, "  ,{}.\r\n", "-".repeat(config::LCD_COLUMNS + 2))?;
    write!(out, "  |{:col$}|\r\n", pad(&top), col = config::LCD_COLUMNS + 2)?;
    write!(out, "  |{:col$}|\r\n", pad(&bottom), col = config::LCD_COLUMNS + 2)?;
    write!(out, "  `{}'\r\n", "-".repeat(config::LCD_COLUMNS + 2))?;
    write!(out, "\r\n")?;
    write!(
        out,
        "  Cash: {:>6} kop.   Price: {:>6} kop.   Items left: {:>4}   Errors: {:?}\x1b[K\r\n",
        machine.counters.cash,
        machine.settings.item_dispenser.unit_value,
        machine.counters.item_level,
        error::errors()
    )?;
    let coin_levels: Vec<String> = machine
        .counters
        .coin_levels
        .iter()
        .enumerate()
        .map(|(i, l)| format!("hopper{}={}", i + 1, l))
        .collect();
    write!(out, "  Coin hoppers: {}\x1b[K\r\n", coin_levels.join(", "))?;
    write!(out, "\r\n")?;
    write!(out, "  \u{2190}/\u{2192}/Enter/\\ или Delete/Backspace=buttons  o/s/c=iButton  1-6=coins  A/S=hopper  D=dispenser  h=help  q=quit\x1b[K\r\n")?;
    write!(out, "  ---- log ----\x1b[K\r\n")?;
    let tail: Vec<&String> = machine.log.iter().rev().take(8).collect();
    for line in tail.iter().rev() {
        write!(out, "  {}\x1b[K\r\n", line)?;
    }
    for _ in tail.len()..8 {
        write!(out, "\x1b[K\r\n")?;
    }
    write!(out, "\x1b[J")?; // clear anything left over from a longer previous frame
    out.flush()?;
    Ok(())
}

fn pad(row: &Row) -> String {
    let mut s = row.as_str().to_string();
    while s.chars().count() < config::LCD_COLUMNS {
        s.push(' ');
    }
    format!(" {} ", s)
}
