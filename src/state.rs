//! What the machine is configured to do, and what it has done so far.
//!
//! Two structures, kept apart because they have different lifetimes. [`Settings`] is what an
//! engineer decided and changes only from the service menu. [`Counters`] is what the machine has
//! since counted, and changes with every coin. Both are checksummed and both survive a power cut,
//! but conflating them would mean rewriting the settings every time somebody buys a pair of shoe
//! covers, and flash does not have the write cycles for that.
//!
//! The phone-number table the original keeps in the settings is absent: this build has no GSM.

use crate::config::{COIN_CHANNELS, HOPPER_COUNT};

/// Money, in the smallest unit of the currency — kopecks, cents, and so on.
///
/// Integer throughout. The machine deals in coins, so there is nothing below the smallest unit to
/// represent, and floating point on a CPU without an FPU would be both slower and wrong.
pub type Cash = u32;

/// A count of things: coins in a hopper, shoe covers left, a machine's number.
pub type Level = u16;

/// Version stamped into stored settings, so a newer firmware can tell it is reading an older
/// layout rather than silently misinterpreting the bytes.
pub const SETTINGS_VERSION: u8 = 1;

/// Version stamped into stored counters. See [`SETTINGS_VERSION`].
pub const COUNTERS_VERSION: u8 = 1;

/// How many access levels the key list has, from most privileged to least.
pub const KEY_ACCESS_LEVELS: usize = 3;
/// How many keys may be enrolled at each access level.
pub const KEYS_PER_LEVEL: usize = 2;

/// Interface language.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum Language {
    /// Russian.
    #[default]
    Russian,
    /// English.
    English,
}

/// Currency the prices are quoted in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum Currency {
    /// Rouble.
    #[default]
    Rub,
    /// Tenge.
    Kzt,
    /// Belarusian rouble.
    Byn,
}

impl Currency {
    /// The short name to put on the display.
    pub const fn symbol(self) -> &'static str {
        match self {
            Currency::Rub => "RUB",
            Currency::Kzt => "KZT",
            Currency::Byn => "BYN",
        }
    }
}

/// A DS1990 serial number, as the reader returns it: family byte, six of serial, checksum.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct IbuttonKey(pub [u8; 8]);

impl IbuttonKey {
    /// An empty slot. All-zero is not a serial number any real key can have, because the checksum
    /// of seven zero bytes is not zero.
    pub const EMPTY: IbuttonKey = IbuttonKey([0; 8]);

    /// Is this slot free?
    pub fn is_empty(&self) -> bool {
        self.0 == [0; 8]
    }
}

/// How the coin acceptor is set up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CoinAcceptorSettings {
    /// Whether the machine takes money through the acceptor at all.
    pub enabled: bool,
    /// Which channels are believed. A coin arriving on a masked-off channel is ignored.
    pub channel_mask: u8,
    /// What a coin on each channel is worth.
    pub channel_values: [Cash; COIN_CHANNELS],
    /// Whether the acceptor reports coins as a pulse count on one shared line rather than as one
    /// of six parallel channel lines.
    ///
    /// This mirrors the C original's `settings.coin_acceptor.pulse_mode`
    /// (`bah_settings.c`, `SetCoinAcceptorPulseMode`) — a per-machine, runtime-configurable choice
    /// of acceptor protocol, not a build-time flag. An acceptor wired for pulse signalling sends
    /// `N` pulses, all six lines OR-ed together, for its `N`th configured denomination; one wired
    /// for parallel signalling drives exactly one of the six lines per coin. Both protocols use the
    /// same six physical lines, so switching this setting is enough to support either without
    /// rewiring.
    pub pulse_mode: bool,
}

impl Default for CoinAcceptorSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            channel_mask: 0x3f,
            // Ten roubles down to fifty kopecks, which is the coin set the machines were sold with.
            channel_values: [1000, 500, 200, 100, 50, 0],
            pulse_mode: false,
        }
    }
}

/// How one hopper — of coins or of shoe covers — is set up.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct HopperSettings {
    /// Whether this hopper is fitted.
    pub enabled: bool,
    /// What one dispensed unit is worth. For the item dispenser this is the price of a pair.
    pub unit_value: Cash,
    /// Below this, a dispense is no longer guaranteed to succeed.
    pub min_level: Level,
    /// Below this, someone should be told to come and refill it.
    pub warn_level: Level,
    /// The most it holds, which is what a refill in the menu is clamped to.
    pub max_level: Level,
}

/// Everything an engineer can set.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    /// Layout version. See [`SETTINGS_VERSION`].
    pub version: u8,
    /// Language the customer sees.
    pub user_language: Language,
    /// Language the service menu is in, which need not be the customer's.
    pub service_language: Language,
    /// Currency prices are quoted in.
    pub currency: Currency,
    /// This machine's number, used to tell machines apart in reports.
    pub machine_id: Level,
    /// Coin acceptor configuration.
    pub coin_acceptor: CoinAcceptorSettings,
    /// Coin hoppers, in the order [`crate::config::HOPPERS`] lists them.
    pub coin_hoppers: [HopperSettings; HOPPER_COUNT],
    /// The shoe-cover dispenser.
    pub item_dispenser: HopperSettings,
    /// Enrolled keys, by access level, most privileged first.
    pub keys: [[IbuttonKey; KEYS_PER_LEVEL]; KEY_ACCESS_LEVELS],
    /// Hour of the day the machine starts trading.
    pub workday_start_hour: u8,
    /// Hour of the day it stops.
    pub workday_end_hour: u8,
    /// Seconds an unfinished transaction is held before its change is returned.
    pub residual_timeout: u8,
    /// Seconds of inactivity that drop the service menu back to trading.
    pub menu_exit_timeout: u8,
    /// Seconds credit is held with nothing happening before it is written off.
    pub cash_clear_timeout: u8,
    /// Seconds the thank-you message stays up.
    pub thanks_message_delay: u8,
    /// Seconds the take-your-change message stays up.
    pub payout_message_delay: u8,
}

impl Default for Settings {
    /// What a machine with no stored settings does.
    ///
    /// It trades: a machine that came up refusing to sell because nobody had configured it would be
    /// indistinguishable, to whoever found it, from a broken one.
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            user_language: Language::Russian,
            service_language: Language::Russian,
            currency: Currency::Rub,
            machine_id: 0,
            coin_acceptor: CoinAcceptorSettings::default(),
            coin_hoppers: [HopperSettings {
                enabled: true,
                unit_value: 100,
                min_level: 10,
                warn_level: 50,
                max_level: 1000,
            }; HOPPER_COUNT],
            item_dispenser: HopperSettings {
                enabled: true,
                unit_value: 500,
                min_level: 10,
                warn_level: 100,
                max_level: 10_000,
            },
            keys: [[IbuttonKey::EMPTY; KEYS_PER_LEVEL]; KEY_ACCESS_LEVELS],
            workday_start_hour: 0,
            workday_end_hour: 24,
            residual_timeout: 30,
            menu_exit_timeout: 60,
            cash_clear_timeout: 120,
            thanks_message_delay: 3,
            payout_message_delay: 5,
        }
    }
}

/// Money in and out over some period.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct Accounting {
    /// Taken through the coin acceptor.
    pub cash_in: Cash,
    /// Paid back out as change.
    pub cash_out: Cash,
    /// Pairs of shoe covers dispensed.
    pub items_dispensed: u32,
    /// Pairs dispensed without payment, from the service menu.
    pub items_free: u32,
}

/// Where the machine is in serving a customer.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum AppState {
    /// Waiting for money, or collecting more of it.
    #[default]
    AcceptCash,
    /// Paying out the shoe covers that have been bought.
    PayoutItems,
    /// Telling the customer to take their change.
    PayoutReminder,
    /// Returning what is left over after the covers have been dispensed.
    ProcessResidual,
}

/// What the machine has counted, and what it still owes.
///
/// The pending fields are what make a power cut survivable: a machine that died halfway through
/// paying out change comes back knowing it still owes it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct Counters {
    /// Layout version. See [`COUNTERS_VERSION`].
    pub version: u8,
    /// Pairs of shoe covers believed to be in the dispenser.
    pub item_level: Level,
    /// Coins believed to be in each hopper.
    pub coin_levels: [Level; HOPPER_COUNT],
    /// Credit the customer has in the machine right now.
    pub cash: Cash,
    /// Coins each hopper still owes.
    pub coins_pending: [Level; HOPPER_COUNT],
    /// Pairs still owed.
    pub items_pending: Level,
    /// Pairs still owed that were not paid for.
    pub free_items_pending: Level,
    /// Where serving the customer had got to.
    pub app_state: AppState,
    /// Everything since the machine was built.
    pub overall: Accounting,
    /// Everything since the last time an engineer cleared the period.
    pub period: Accounting,
}

impl Counters {
    /// A machine that has never done anything.
    pub fn new() -> Self {
        Self {
            version: COUNTERS_VERSION,
            ..Self::default()
        }
    }

    /// Does the machine still owe the customer something?
    pub fn owes_anything(&self) -> bool {
        self.items_pending > 0
            || self.free_items_pending > 0
            || self.coins_pending.iter().any(|&c| c > 0)
    }
}
