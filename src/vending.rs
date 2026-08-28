//! Selling shoe covers.
//!
//! One task owns the money, the counters and the hoppers, and everything else sends it events. That
//! is the whole design: there is exactly one place where the machine's idea of what it owes can
//! change, so there is no way for two tasks to disagree about whether a customer has been served.
//!
//! The state is written to flash at every point where losing it would cost somebody money — after
//! taking a coin, and after each unit dispensed — so that a machine which loses power mid-payout
//! comes back knowing what it still owes and finishes the job.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};

use crate::config::HOPPER_COUNT;
use crate::error::{self, Errors};
use crate::event::{Button, Event};
use crate::hopper::{Hopper, HopperError};
use crate::nvram::{self, Journal};
use crate::state::{AppState, Cash, Counters, Settings};
use crate::ui::{self, Screen};

/// Everything the sale needs, in one place so the state machine can be written as functions on it
/// rather than as a task with a dozen locals.
pub struct Machine {
    /// What an engineer configured.
    pub settings: Settings,
    /// What the machine has counted, and what it owes.
    pub counters: Counters,
    /// Where the counters are persisted.
    pub journal: Journal,
    /// Coin hoppers, in the order the settings list them.
    pub hoppers: [Hopper<'static>; HOPPER_COUNT],
    /// The shoe-cover dispenser.
    pub dispenser: Hopper<'static>,
}

impl Machine {
    /// What a pair of covers costs.
    fn price(&self) -> Cash {
        self.settings.item_dispenser.unit_value
    }

    /// Write the counters down.
    ///
    /// Called at every point where a power cut would otherwise lose money. Each call is a flash
    /// write of some milliseconds, which is why it is called at those points and not more often.
    fn persist(&mut self) {
        self.journal.save(&self.counters);
    }

    /// Show whatever the machine's situation currently is.
    fn refresh_display(&self) {
        let screen = if !error::errors().can_vend() {
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
        };
        crate::event::show(screen);
    }

    /// Take a coin.
    fn accept_coin(&mut self, value: Cash) {
        self.counters.cash = self.counters.cash.saturating_add(value);
        self.counters.overall.cash_in = self.counters.overall.cash_in.saturating_add(value);
        self.counters.period.cash_in = self.counters.period.cash_in.saturating_add(value);
        self.persist();
        self.refresh_display();
    }

    /// Work out how to pay `amount` out of the hoppers.
    ///
    /// Greedy, largest coin first, which for any real set of coin denominations gives the fewest
    /// coins. Returns what each hopper should dispense and what could not be paid at all.
    fn plan_change(&self, amount: Cash) -> ([u16; HOPPER_COUNT], Cash) {
        let mut plan = [0u16; HOPPER_COUNT];
        let mut left = amount;

        // Hopper indices, largest coin first. Sorting two or three elements by hand beats pulling
        // in a sort for it.
        let mut order = [0usize; HOPPER_COUNT];
        for (i, slot) in order.iter_mut().enumerate() {
            *slot = i;
        }
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

    /// Dispense the covers that have been paid for.
    ///
    /// Each unit is written down as it comes out, not at the end. A dispenser that jams after the
    /// second of three pairs must leave the machine owing one, and it can only know that if the two
    /// that came out were recorded while it was still running.
    async fn payout_items(&mut self) {
        while self.counters.items_pending > 0 || self.counters.free_items_pending > 0 {
            crate::event::show(Screen::Dispensing);

            let free = self.counters.items_pending == 0;
            let (dispensed, failure) = self.dispenser.dispense(1).await;

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
                self.counters.item_level = self.counters.item_level.saturating_sub(1);
                self.persist();
            }

            if let Some(e) = failure {
                // A dispenser that will not dispense stops the machine: there is nothing to sell.
                error::raise(match e {
                    HopperError::Jammed => Errors::ITEM_DISPENSER,
                    HopperError::Faulted => Errors::ITEM_DISPENSER,
                });
                self.persist();
                return;
            }

            // The dispenser has a level line too, but it is not wired to anything useful on these
            // boards — the original reads it as a constant zero — so the count is all there is.
            error::set(
                Errors::ITEM_DISPENSER_EMPTY,
                self.counters.item_level <= self.settings.item_dispenser.min_level,
            );
        }

        self.counters.app_state = AppState::ProcessResidual;
        self.persist();
    }

    /// Pay out what is owed in coins.
    async fn payout_change(&mut self) {
        // Anything planned earlier and not yet paid comes first: this is the path a machine takes
        // when it comes back from a power cut owing change.
        for i in 0..HOPPER_COUNT {
            while self.counters.coins_pending[i] > 0 {
                let (dispensed, failure) = self.hoppers[i].dispense(1).await;

                if dispensed > 0 {
                    self.counters.coins_pending[i] -= 1;
                    self.counters.coin_levels[i] = self.counters.coin_levels[i].saturating_sub(1);
                    let value = self.settings.coin_hoppers[i].unit_value;
                    self.counters.overall.cash_out =
                        self.counters.overall.cash_out.saturating_add(value);
                    self.counters.period.cash_out =
                        self.counters.period.cash_out.saturating_add(value);
                    self.persist();
                }

                if failure.is_some() {
                    // A hopper that cannot pay is a fault, but not one that stops the machine
                    // selling: it can still take exact money. What it must not do is quietly forget
                    // the coins it still owes, so they stay pending.
                    error::raise(Errors::COIN_HOPPER);
                    self.persist();
                    return;
                }
            }

            // Two ways to know a hopper is empty, and either is enough. The counter is what the
            // machine believes; the level line is what the hopper can see. They disagree whenever
            // somebody has refilled it without saying so, or coins have been taken out by hand.
            error::set(
                Errors::COIN_HOPPER_EMPTY,
                self.hoppers[i].is_low()
                    || self.counters.coin_levels[i] <= self.settings.coin_hoppers[i].min_level,
            );
        }
    }

    /// Turn the credit that is left into a change plan, and pay it.
    async fn process_residual(&mut self) {
        if self.counters.cash == 0 {
            self.counters.app_state = AppState::AcceptCash;
            self.persist();
            return;
        }

        let (plan, unpayable) = self.plan_change(self.counters.cash);

        // The plan is written down *before* a single coin moves. A machine that loses power between
        // planning and paying must come back owing coins, not owing credit it has already spent.
        for (i, count) in plan.iter().enumerate() {
            self.counters.coins_pending[i] += count;
        }
        self.counters.cash = unpayable;
        self.counters.app_state = AppState::PayoutReminder;
        self.persist();

        error::set(Errors::CANNOT_PAYOUT, unpayable > 0);

        let owed: Cash = plan
            .iter()
            .enumerate()
            .map(|(i, &n)| n as Cash * self.settings.coin_hoppers[i].unit_value)
            .sum();

        self.payout_change().await;

        if owed > 0 {
            ui::show_for(
                Screen::TakeChange {
                    amount: owed,
                    currency: self.settings.currency,
                },
                self.settings.payout_message_delay,
            )
            .await;
        }

        // Credit that could not be paid out stays as credit. The customer can put in more money and
        // buy another pair with it, which is better than the machine keeping it.
        if self.counters.cash == 0 {
            ui::show_for(Screen::Thanks, self.settings.thanks_message_delay).await;
        }

        self.counters.app_state = AppState::AcceptCash;
        self.persist();
    }

    /// Finish whatever the machine was doing when it last lost power.
    ///
    /// This runs before any event is handled, so a customer cannot start a new transaction on top
    /// of an unfinished one.
    pub async fn resume(&mut self) {
        if !self.counters.owes_anything() {
            self.counters.app_state = AppState::AcceptCash;
            return;
        }

        self.payout_items().await;
        self.payout_change().await;

        if self.counters.cash > 0 {
            self.process_residual().await;
        }
    }

    /// A customer has paid enough. Sell them a pair.
    async fn sell(&mut self) {
        let price = self.price();
        if self.counters.cash < price || !error::errors().can_vend() {
            return;
        }

        self.counters.cash -= price;
        self.counters.items_pending += 1;
        self.counters.app_state = AppState::PayoutItems;
        self.persist();

        self.payout_items().await;
        self.process_residual().await;
        self.refresh_display();
    }

    /// Give away a pair, from the service menu.
    pub async fn dispense_free(&mut self) {
        self.counters.free_items_pending += 1;
        self.counters.app_state = AppState::PayoutItems;
        self.persist();
        self.payout_items().await;
        self.refresh_display();
    }
}

/// Serve customers.
#[embassy_executor::task]
pub async fn vending_task(mut machine: Machine) {
    machine.resume().await;
    machine.refresh_display();

    loop {
        // Credit left sitting with nobody around is returned rather than kept. The timeout only
        // runs while there is credit: an idle machine should not be waking up to check a clock.
        let idle_limit = if machine.counters.cash > 0 {
            Duration::from_secs(machine.settings.cash_clear_timeout as u64)
        } else {
            Duration::from_secs(3600)
        };

        match select(crate::event::next(), Timer::after(idle_limit)).await {
            Either::First(event) => handle(&mut machine, event).await,
            Either::Second(()) => {
                if machine.counters.cash > 0 {
                    machine.process_residual().await;
                    machine.refresh_display();
                }
            }
        }
    }
}

/// Act on one event.
async fn handle(machine: &mut Machine, event: Event) {
    match event {
        Event::Coin { value, .. } => {
            machine.accept_coin(value);
            // The machine sells as soon as it can. Asking the customer to press a button after
            // paying is one more thing to explain on a sticker.
            if machine.counters.cash >= machine.price() {
                machine.sell().await;
            }
        }

        Event::Button(Button::Ok) => machine.sell().await,

        Event::Button(Button::Cancel) => {
            if machine.counters.cash > 0 {
                machine.process_residual().await;
            }
            machine.refresh_display();
        }

        Event::Button(_) => {}

        Event::Door { .. } => machine.refresh_display(),

        Event::Key(key) => {
            match crate::menu::Access::of(&key, &machine.settings) {
                Some(access) => {
                    // The acceptor is shut for the whole visit, so that a coin cannot arrive in the
                    // middle of a refill and be counted against a level the engineer is editing.
                    crate::coin_acceptor::set_inhibited(true);
                    crate::menu::run(machine, access).await;
                    crate::coin_acceptor::set_inhibited(false);
                }
                None => {
                    crate::event::show(crate::menu::unknown_key_screen());
                    Timer::after(Duration::from_secs(2)).await;
                }
            }
            machine.refresh_display();
        }
    }
}

/// Load what the machine knew before it was switched off.
pub fn restore() -> (Settings, Counters, Journal) {
    let settings = nvram::load_settings();
    let (journal, counters) = Journal::open();
    (settings, counters, journal)
}
