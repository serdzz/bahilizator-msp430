# SIM.md — the desktop simulator

`sim/` is a std, terminal-based simulator of the bahilizator vending machine. It lets you exercise
the vending logic, the service menu, coin acceptance, hopper payouts and iButton access levels on a
desktop or laptop, with no MSP430 board, programmer, or `embassy-msp430` HAL involved.

## Running it

```sh
cd sim
cargo run --release --bin sim
```

(`sim/.cargo/config.toml` pins the build to the host target, `x86_64-unknown-linux-gnu` here, and
turns off the `build-std` the MSP430 build needs — see "Why a separate crate" below.)

The terminal is put into raw mode for the duration of the program; it is restored on exit (`q` /
`Esc`, or Ctrl-C).

## What it looks like

```
  ,------------------.
  | Бахилы           |
  | Цена 5.00 RUB    |
  `------------------'

  Cash:      0 kop.   Price:    500 kop.   Items left:   50   Errors: Errors(0x0)
  Coin hoppers: hopper1=30

  ←/→/Enter/\=buttons  o/s/c=iButton  1-6=coins  A/S=hopper  D=dispenser  h=help  q=quit
  ---- log ----
  Simulator started. Press 'h' for help, 'q' to quit.
```

The bordered box is the 16×2 LCD, redrawn only when its content changes — exactly like the real
`ui::run` loop, whose `render()` function this reuses verbatim. Below it: live counters, and a
scrolling log of the last few things that happened (coins, sales, menu actions, hopper pulses),
which the real machine has no equivalent of — added because a terminal has room for it and it makes
the simulator much easier to follow than a bare 16×2 display would be on its own.

## Keymap

| Key | Effect |
| --- | --- |
| `←` | `Button::Prev` — the "up / back" button |
| `→` | `Button::Next` — the "down / forward" button |
| `Enter` | `Button::Ok` — accept / dispense |
| `\` | `Button::Cancel` — the fourth physical button (see `event::Button::ALL`, position 4; wired to P2.3) |
| `o` | Present an iButton with **Owner** access (all menu items, including price) |
| `s` | Present an iButton with **Service** access (refills, free items, clear period) |
| `c` | Present an iButton with **Collector** access (reports only) |
| `1`–`6` | Simulate a coin pulse on coin-acceptor channel 1–6 (`config::COIN_CHANNELS = 6`) |
| `A` | Manual payout pulse from coin hopper **1** — `config::HOPPERS[0]` |
| `S` | Manual payout pulse from coin hopper **2** — `config::HOPPERS[1]`, if the build has one (this build's `config::HOPPER_COUNT = 1`, so `S` reports "not configured"; see below) |
| `D` | Manual dispense pulse from the item dispenser — `config::ITEM_DISPENSER`, the shoe-cover hopper, separate from the coin hoppers |
| `h` | Print a one-line keymap reminder into the log |
| `q` / `Esc` | Quit |

Uppercase `A`/`S` are deliberately distinct from lowercase `s` (iButton Service role): `s` opens the
service menu, `S` pulses the second coin hopper. If a future build configures more than two coin
hoppers, extend this by binding the next unused uppercase letters (e.g. `F`, `G`, ...) to
`hoppers[2]`, `hoppers[3]`, ... in `sim/src/main.rs`'s key-match arm, and update this table —
`config::HOPPER_COUNT` and `config::HOPPERS` are the single source of truth for how many hoppers
exist in a given build.

Coins and buttons are ignored while the service menu, an edit screen, or a report is open — same as
the real firmware, which inhibits the coin acceptor and drops non-button events for the duration
(`vending::handle`, `menu::button`).

## What is reused from the real firmware, and what is not

The workspace was split into a library crate (`bahilizator`, `src/lib.rs`) and two binaries:

* `bahilizator` (`src/main.rs`, `--features hw`, `msp430-none-elf` target only) — the real firmware,
  unchanged in behaviour. It still uses `embassy-msp430` GPIO, `embassy-executor`'s MSP430 platform,
  and on-chip flash NVRAM.
* `sim` (`sim/src/main.rs`, plain `cargo build`, host target) — this simulator.

Reused **unchanged**, byte-for-byte the same code the real firmware runs:

* `state` — `Settings`, `Counters`, `Cash`, `Currency`, `IbuttonKey`, ... (hardware-agnostic from
  the start)
* `error` — the `Errors` bitflags and the shared atomic fault set
* `config` — pin tables are irrelevant on a desktop, but the *counts* (`HOPPER_COUNT`,
  `COIN_CHANNELS`, `LCD_COLUMNS`, `LCD_ROWS`) and the hopper/dispenser tables are used to size the
  simulator the same way the real build is sized
* `ui::render` and `ui::format_cash` — turns a `Screen` into the two 16-character rows shown in the
  bordered box; this is the *exact* function the real LCD driver calls
* `report::accounting` / `report::state` — the same report text a real engineer would page through
* `menu::Access` — `Access::of()`, the iButton-key-to-privilege lookup, unchanged

Reimplemented, not reused, in `sim/src/main.rs`:

* The event loop and money state machine (`vending::Machine`'s methods — `accept_coin`,
  `plan_change`, `payout_items`, `payout_change`, `process_residual`, `sell`) are re-derived as a
  synchronous `Machine` struct with the same field names and the same logic, one function call at a
  time rather than `async`/`.await`.
* The service menu (`menu::run`, `menu::Item`, `menu::edit`, `menu::show_report`) is likewise
  reimplemented as a synchronous `Mode` state machine (`Mode::Menu` / `Mode::Editing` /
  `Mode::Report`).
* Hoppers: the real `hopper::Hopper` drives a motor and times sensor pulses against a live coin
  sensor; there is no such hardware to simulate meaningfully, so `SimHopper` is just a stock count
  that decrements by one per dispense call and reports failure once empty — the same *contract*
  (returns how many actually came out) without the timing.

**Why not reuse the async task functions verbatim?** `buttons::input_task`, `coin_acceptor::coin_task`,
`ibutton::ibutton_task` and `vending::vending_task` are written directly against
`embassy_msp430::gpio::{Input, Output, Flex}` and `hopper::Hopper<'static>` in their signatures —
types that read and drive real MSP430 pins. Faking those types well enough to satisfy the type
checker would mean writing an entire parallel embassy-msp430-shaped HAL, which is far more code than
reimplementing the (comparatively small) state machine those tasks drive. The event/channel
plumbing (`event::Event`, `event::Button`, `embassy_sync::Channel`) is not MSP430-specific at all —
`embassy-sync` and `embassy-executor` both support a `std` platform — but wiring the sim through a
real async executor and channel would only reproduce, at more complexity, the same ordering the
plain synchronous version already gets for free from being single-threaded and driven by one
`match` per keystroke.

## Known limitations

* **No real NVRAM persistence.** The simulator starts fresh every run — `Counters::new()`, default
  `Settings`, 50 covers, 30 coins per hopper. `nvram.rs` (flash journal read/write) is `hw`-only and
  is not linked into `sim` at all. Nothing here reads or writes a file across runs; state does not
  survive quitting.
* **No GSM/report delivery.** Same as the real firmware build — reports are viewable in the service
  menu, not sent anywhere.
* **Hoppers have no timing model.** A manual pulse (`A`/`S`) or a sale/payout always succeeds
  instantly unless the hopper is empty; there is no jam simulation, no `HopperError::Faulted`, and
  the real firmware's stuck-sensor and timeout logic (`hopper::Hopper::run`) has no counterpart
  here.
* **1-Wire bit timing is not simulated.** Presenting an iButton is a single keypress that either is
  or is not one of the three enrolled roles; there is no bus, no CRC, no "key half-removed" state.
* **Doors are not modelled.** `Errors::DOOR_OPENED` never triggers in the simulator; there is no key
  bound to a door switch.
* **One coin hopper in this build** (`config::HOPPER_COUNT = 1`), so `S` reports "not configured" —
  this matches the real firmware's `config.rs`, which explains in its own doc comment why the
  second coin hopper (`HOPPER_C`) is not wired into this build (its pins clash with the coin
  acceptor's).

## Building the real firmware after this change

Nothing about the MSP430 build changed except that it now needs `--features hw` and the target/
build-std flags are passed on the command line instead of living in `.cargo/config.toml` (that file
used to set them globally, which broke the simulator's std build — see the comment at the top of
`.cargo/config.toml`):

```sh
cargo +nightly build --release --features hw --target msp430-none-elf -Z build-std=core
```

This was verified in the environment this port was built in: it still produces a normal MSP430 ELF
(`target/msp430-none-elf/release/bahilizator`), unchanged from before this branch except for the
`--features hw` requirement.
