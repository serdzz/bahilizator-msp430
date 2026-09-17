//! Firmware for the shoe-cover vending machine, on an MSP430F2618.
//!
//! # How it is put together
//!
//! Six tasks, and one of them owns everything that matters:
//!
//! * [`vending`] holds the money, the counters and the hoppers. Every change to what the machine
//!   owes happens inside it, which is why there is no mutex anywhere in this firmware.
//! * [`buttons`], [`coin_acceptor`] and [`ibutton`] watch hardware and send events to it.
//! * [`ui`] draws whatever it is asked to.
//! * The service menu is not a task: it borrows the vending task while an engineer is using it.
//!
//! # What is not here
//!
//! The GSM modem. The original reported takings and faults by SMS over a SIM340 on port 3; that is
//! out of scope for this build. Everything it would have sent is built by [`report`] and shown on
//! the display instead, so wiring a modem back in is a matter of handing those lines to it.
//!
//! # Room
//!
//! The F2618 has 116 kB of flash, and this firmware can use about fifty of it: the rest is above
//! 0xFFFF and needs the 20-bit addressing that Rust's `msp430-none-elf` target does not have. See
//! `memory.x`.

#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]
#![warn(missing_docs)]

use embassy_executor::Executor;
use embassy_msp430::clock::{AclkSource, DcoFreq, Div};
use embassy_msp430::gpio::{Flex, Input, Level, Output, Pull};
use embassy_msp430::gpio;
use static_cell::StaticCell;

use crate::config::HopperPins;
use crate::hopper::{Hopper, HopperKind};

mod buttons;
mod coin_acceptor;
mod config;
mod cyrillic;
mod error;
mod event;
mod hopper;
mod ibutton;
mod lcd;
mod menu;
mod nvram;
mod report;
mod state;
mod ui;
mod vending;

use panic_msp430 as _;

/// Build the four lines of one hopper.
///
/// The pins are stolen rather than taken from the peripheral singletons, because a hopper is
/// described by [`HopperPins`] — a table read at runtime — and the singletons are distinct types
/// known at compile time. The safety argument is the table itself: every pin in `config` appears
/// in exactly one place, and nothing else in this firmware claims one.
fn hopper_from(pins: HopperPins, kind: HopperKind) -> Hopper<'static> {
    // SAFETY: `config` assigns each of these pins to this hopper and to nothing else, and this
    // function is called once per hopper from `main` before any task starts.
    unsafe {
        Hopper::new(
            Output::new(gpio::AnyPin::steal(pins.port, pins.control), Level::Low),
            Input::new(gpio::AnyPin::steal(pins.port, pins.coin), Pull::Up),
            Input::new(gpio::AnyPin::steal(pins.port, pins.error), Pull::Up),
            Input::new(gpio::AnyPin::steal(pins.port, pins.level), Pull::Up),
            kind,
        )
    }
}

/// The executor, which has to outlive `main`.
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Bring the hardware up, claim every pin, and hand over to the executor.
#[msp430_rt::entry]
fn main() -> ! {
    let mut hal = embassy_msp430::Config::default();
    // 8 MHz from the factory-calibrated DCO. Fast enough for the 1-Wire bit timing to be
    // comfortable, and slow enough not to need the 3.3 V that 16 MHz would.
    hal.clock.dco = DcoFreq::_8MHz;
    hal.clock.mclk_div = Div::_1;
    hal.clock.smclk_div = Div::_1;
    // The board has a watch crystal; the time driver runs from it, so the machine keeps time in low
    // power mode between customers.
    hal.clock.aclk = AclkSource::Xt1;
    let p = embassy_msp430::init(hal);

    // The display, four bits wide on the top of port 4. P4.0 is the modem's supply and is left
    // alone.
    let lcd_pins = (
        Output::new(p.P4_1, Level::Low), // register select
        Output::new(p.P4_2, Level::Low), // read/write, held low
        Output::new(p.P4_3, Level::Low), // enable
        [
            Output::new(p.P4_4, Level::Low),
            Output::new(p.P4_5, Level::Low),
            Output::new(p.P4_6, Level::Low),
            Output::new(p.P4_7, Level::Low),
        ],
    );

    let buttons = [
        Input::new(p.P2_0, Pull::Up),
        Input::new(p.P2_1, Pull::Up),
        Input::new(p.P2_2, Pull::Up),
        Input::new(p.P2_3, Pull::Up),
    ];
    let doors = [
        Input::new(p.P1_3, Pull::Up),
        Input::new(p.P1_4, Pull::Up),
    ];

    let one_wire = ibutton::OneWire::new(Flex::new(p.P2_4));

    // The coin acceptor's six channels are scattered across port 7, so they are listed rather than
    // computed. `config::COIN_CHANNEL_BITS` is the same list in mask form, for the harness.
    let coin_channels = [
        Input::new(p.P7_0, Pull::Up),
        Input::new(p.P7_7, Pull::Up),
        Input::new(p.P7_6, Pull::Up),
        Input::new(p.P7_5, Pull::Up),
        Input::new(p.P7_3, Pull::Up),
        Input::new(p.P7_2, Pull::Up),
    ];
    let acceptor = coin_acceptor::CoinAcceptor::new(
        coin_channels,
        Output::new(p.P7_1, Level::High),
    );

    let dispenser = hopper_from(config::ITEM_DISPENSER, HopperKind::ItemDispenser);
    let hoppers: [Hopper<'static>; config::HOPPER_COUNT] =
        core::array::from_fn(|i| hopper_from(config::HOPPERS[i], HopperKind::CoinHopper));

    // The indicators. Green means the machine will sell; red means it will not.
    let leds = (
        Output::new(p.P1_6, Level::Low),
        Output::new(p.P1_7, Level::Low),
    );

    let (settings, counters, journal) = vending::restore();
    let machine = vending::Machine {
        settings,
        counters,
        journal,
        hoppers,
        dispenser,
    };

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        // Each of these has a pool of one and is spawned once, so the token can only be an error if
        // this code were changed to spawn one twice. Unwrapping is the check that it has not been.
        spawner.spawn(display_task(lcd_pins, settings.user_language).unwrap());
        spawner.spawn(buttons::input_task(buttons, doors, event::sender()).unwrap());
        spawner.spawn(
            coin_acceptor::coin_task(acceptor, settings.coin_acceptor, event::sender()).unwrap(),
        );
        spawner.spawn(ibutton::ibutton_task(one_wire, event::sender()).unwrap());
        spawner.spawn(led_task(leds).unwrap());
        spawner.spawn(vending::vending_task(machine).unwrap());
    })
}

/// Bring the display up and then draw for the rest of time.
///
/// Initialising the controller takes about sixty milliseconds of waiting, which is why it happens
/// in a task rather than in `main`: there is no executor to wait on until `main` has finished.
#[embassy_executor::task]
async fn display_task(
    pins: (
        Output<'static>,
        Output<'static>,
        Output<'static>,
        [Output<'static>; 4],
    ),
    language: state::Language,
) {
    let (rs, rw, e, data) = pins;
    let lcd = lcd::Lcd::new(rs, rw, e, data, config::LCD_COLUMNS).await;
    ui::run(lcd, language).await
}

/// Show, on the outside of the machine, whether it is working.
///
/// Green steady means it will sell. Red steady means it will not. Nothing blinks: a blinking light
/// on a machine in a hospital corridor is noise, and the display already says what is wrong.
#[embassy_executor::task]
async fn led_task(leds: (Output<'static>, Output<'static>)) {
    let (mut red, mut green) = leds;
    let mut ticker = embassy_time::Ticker::every(embassy_time::Duration::from_millis(500));
    loop {
        ticker.next().await;
        let ok = error::errors().can_vend();
        green.set_level(Level::from(ok));
        red.set_level(Level::from(!ok));
    }
}
