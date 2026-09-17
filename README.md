# bahilizator

Firmware for the shoe-cover vending machine, rewritten on [Embassy] for the MSP430F2618.

It replaces about 8500 lines of C. The behaviour is the same machine: take coins, dispense a pair of
shoe covers, give change, keep counters across a power cut, and let an engineer with a DS1990 key
into a service menu.

[Embassy]: https://embassy.dev

## What is here

| Module | What it does |
| --- | --- |
| `config` | Where everything is wired, taken from the C original's headers |
| `state` | Settings and counters |
| `error` | The faults the machine can be in, as one shared set |
| `event` | The single queue everything reports to |
| `lcd`, `cyrillic`, `ui` | The display, its Russian character set, and what to say |
| `buttons` | Front panel and door switches, debounced |
| `coin_acceptor` | Six parallel channels and the inhibit line |
| `hopper` | The dispenser and the coin hopper |
| `ibutton` | DS1990 keys on 1-Wire |
| `nvram` | Settings and a wear-levelled counter journal, in on-chip flash |
| `menu` | The service menu |
| `report` | The numbers an engineer, or a modem, would want |
| `vending` | The sale itself |

Six tasks. One of them — `vending` — owns the money, the counters and the hoppers, and everything
else sends it events, which is why there is no mutex in this firmware. The service menu is not a
task: it borrows the vending task while an engineer is using it, so a refill cannot interleave with
a customer's transaction.

## What is deliberately not here

**GSM.** The original reported takings and faults by SMS over a SIM340 on port 3. That is out of
scope for this build. `report` still builds the lines it would have sent, and shows them on the
display instead, so wiring a modem back in means handing those lines to it rather than writing them
again.

**The second coin hopper.** The original counts two, but the second is hopper C, whose pins on port
7 are the coin acceptor's. A machine has one or the other. See the note at the top of `config.rs`.

**The external SPI memory** on port 5, which the original used for its transaction and event logs.
The counters that matter are in on-chip flash; the logs are not.

## Building

The HAL is `embassy-msp430`, taken by path from a checkout of
[serdzz/embassy](https://github.com/serdzz/embassy) on the `dev/msp430` branch, expected as a
sibling of this directory:

```
.../embassy          <- the fork, on dev/msp430
.../bahilizator      <- this
```

Needs a nightly toolchain (`msp430-none-elf` is tier 3, so `core` is built from source) and TI's
`msp430-elf-gcc`, which is the linker. `rust-lld` cannot be used: it does not implement the
`R_MSP430_SYM_DIFF` relocation that `msp430-rt`'s startup code contains.

```sh
cargo build --release --features hw --target msp430-none-elf -Z build-std=core
```

The `--features hw` pulls in `embassy-msp430`, `embassy-executor`, `msp430-rt`, `panic-msp430` and
the other MSP430-only dependencies (see "Two ways to build this crate" below); the target and
build-std flags used to live in `.cargo/config.toml`, but were moved onto the command line because
they cannot be scoped to one target the way linker flags can, and having them apply unconditionally
broke `cargo build` for the desktop simulator (see `SIM.md`).

The toolchain is TI's [MSP430-GCC], not a Homebrew package. Put its `bin` on `PATH`. The macOS
build is x86_64 only, so on Apple silicon it runs under Rosetta.

Two linker arguments in `.cargo/config.toml` are worth knowing about, because without them the build
gets all the way to the final link and then fails on undefined `__mspabi_*` symbols. The CPU has no
multiplier and no divider, so those operations become calls into libgcc, and rustc links a bare
metal target with `-nodefaultlibs`:

* `-lgcc` for the division helpers.
* `-mhwmult=none` and `-lmul_none` for the multiplication ones. This part *does* have a 16×16
  hardware multiplier and would be faster using it, but it is a peripheral with shared registers: a
  multiply inside an interrupt handler would corrupt one it interrupted, and the HAL multiplies
  inside handlers. The software routines have no such problem.

[MSP430-GCC]: https://www.ti.com/tool/MSP430-GCC-OPENSOURCE

## Flashing

With an MSP-FET or an MSP-FET430UIF and `mspdebug`:

```sh
cargo run --release --features hw --target msp430-none-elf -Z build-std=core
```

which runs `mspdebug tilib "prog <elf>" reset`.

## Playing with it on a desktop, no board required

`sim/` is a separate std binary that reimplements the same vending logic, service menu and iButton
access levels as an interactive terminal program — a 16×2 LCD rendered as text, arrow keys and
Enter/`\` for the four buttons, `o`/`s`/`c` for iButton roles, `1`–`6` for coins, `A`/`S`/`D` for
hopper and dispenser payouts. See **[SIM.md](SIM.md)** for the full keymap, what it reuses from
this crate unchanged versus reimplements, and known limitations.

```sh
cd sim
cargo run --release --bin sim
```

## Two ways to build this crate

This repository now produces two binaries from one `Cargo.toml` (`bahilizator`) plus a workspace
member (`sim`):

* `cargo build --release --features hw --target msp430-none-elf -Z build-std=core` — the real
  firmware, unchanged in behaviour from before this split, for the MSP430F2618.
* `cd sim && cargo run --release --bin sim` — the desktop simulator above, on the host.

The split lives in `src/lib.rs`: modules that touch real hardware (`buttons`, `coin_acceptor`,
`hopper`, `ibutton`, `nvram`, `vending`, `lcd`) are behind `#[cfg(feature = "hw")]` and only compile
for the MSP430 build; `config`, `state`, `error`, `event`, `ui`, `report` and the pure parts of
`menu` (`Access`) are hardware-agnostic and compiled into both.

## Room

The F2618 has 116 kB of flash. This firmware can use about 50 kB of it: the rest lives above 0xFFFF
and needs the 20-bit addressing that Rust's `msp430-none-elf` target does not have. The original C
put constants up there; that is not available here. The linked firmware is 34054 bytes of flash — two
thirds of the 50880 the `ROM` region gives — and 1694 bytes of RAM out of 8192.

`memory.x` also carves four 512-byte flash segments out below the code for `nvram`. They are their
own region so that the linker cannot place code in them — erasing a segment holds the CPU, and
erasing the code that is running would be the last thing the machine did.

## Running it in a simulator

`mspdebug`'s simulator gets far enough to be useful. `sim/board.mspdebug` describes as much of the
board as its device models can express — Timer_B7 for the time driver, port 1 for the indicators and
door switches, port 2 for the buttons, port 7 for the coin channels, and the DCO calibration
constants a real chip has in information memory.

```sh
mspdebug sim "prog target/msp430-none-elf/release/bahilizator" \
             "read sim/board.mspdebug" "reset" "step 12000000" \
             "simio config p1 set 3 0" "step 6000000" \
             "simio config p1 set 3 1" "step 6000000"
```

Port 1 is set `verbose`, so every change to an indicator is printed. That run opens a door and
closes it again:

```
gpio: state change on p1: H--- ----   green on, machine fit to trade
gpio: state change on p1: l--- ----   door opened: green off
gpio: state change on p1: -H-- ----             and red on
gpio: state change on p1: H--- ----   door closed: green back on
gpio: state change on p1: -l-- ----              and red off
```

Dropping a coin in — `simio config p7 set 0 0`, a few hundred thousand steps, then back to 1 —
takes the sale all the way to the dispenser, which the simulator does not have, so the machine
correctly declares it jammed after the two-second timeout and lights the red indicator.

What this covers: the clock, the time driver on Timer_B, the executor and its low-power wake path,
GPIO in both directions, the debouncer, the event queue, the fault set, the vending state machine
and the flash journal.

What it does not: ports 4, 5 and 8, because the F2618 interleaves and scatters those registers in a
way the simulator's eight-consecutive-registers GPIO model cannot express. So the display, the
hoppers and the external memory are not exercised at all.

## Two things to check on the first board

Neither can be settled without hardware, and both are cheap to check:

1. **The display's character set.** `cyrillic.rs` targets the table used by МЭЛТ MT-16S2H and the
   Cyrillic Winstar parts. A display with a different ROM will draw correct ASCII and wrong
   Cyrillic.
2. **The 1-Wire bit timing.** `ibutton.rs` busy-waits from `MCLK_HZ` on the assumption that its
   delay loop is three cycles. The standard tolerates a good deal of slop, but that assumption is
   worth a scope.
