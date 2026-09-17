//! Shared library crate for the bahilizator firmware.
//!
//! Split out of `main.rs` so that the hardware-agnostic modules — state, error handling, display
//! rendering, config constants, reports, and the pure parts of the service menu — can be reused by
//! both the real MSP430 firmware binary (`main.rs`, built with `--features hw` for the
//! `msp430-none-elf` target) and the std desktop simulator (`sim/`, built for the host).
//!
//! Modules that touch real hardware (GPIO, flash, 1-Wire bit-banging, the embassy executor/time
//! drivers) are gated behind the `hw` feature, which is only enabled for the MSP430 build. The std
//! simulator never enables `hw` and gets none of those dependencies compiled in.
#![cfg_attr(feature = "hw", no_std)]
#![cfg_attr(feature = "hw", feature(impl_trait_in_assoc_type))]

pub mod config;
pub mod cyrillic;
pub mod error;
pub mod event;
pub mod menu;
pub mod report;
pub mod state;
pub mod ui;

#[cfg(feature = "hw")]
pub mod buttons;
#[cfg(feature = "hw")]
pub mod lcd;
#[cfg(feature = "hw")]
pub mod coin_acceptor;
#[cfg(feature = "hw")]
pub mod hopper;
#[cfg(feature = "hw")]
pub mod ibutton;
#[cfg(feature = "hw")]
pub mod nvram;
#[cfg(feature = "hw")]
pub mod vending;
