# Port Audit: bahilizator-c-original → bahilizator-msp430

Read-only correctness comparison. C source cited from `/root/bahilizator-c-original`
(headers CRLF, IAR project); Rust source cited from `/root/bahilizator-msp430` (branch
`port_std` checked out at audit time — hardware code is behind `#[cfg(feature = "hw")]`
and was read directly, not simulated).

---

## 1. Coin acceptor

**C**: `coin_acceptor.c` supports two distinct acquisition modes, selected by
`aCoinAcceptor->pulse_mode` in `ProcessCoinAcceptor` (`bah_io.c:1173-1196`):

- **Normal mode** (`processNormalMode`, `coin_acceptor.c:37-82`): six channel lines are
  OR-reduced into one `channel_state` byte; the state machine (`ACCEPTOR_IDLE` →
  `ACCEPTOR_PRE_ACCEPT` after `COIN_ACCEPTOR_PULSE_MIDLE` (60 ticks) → `ACCEPTOR_ACCEPT`)
  waits for the *same* channel mask to return to rest, timing out as
  `COIN_ACCEPTOR_ERROR_MALFUNCTION` after `COIN_ACCEPTOR_PULSE_MAX` (250 ticks,
  `coin_acceptor.h:6`). The returned value is `mask ^ channel`, i.e. XOR against a fixed
  mask, not a simple channel index.
- **Pulse mode** (`processPulseMode`, `coin_acceptor.c:84-132`): counts *pulses* on any
  of the 6 physical lines OR-ed together (`COIN_ACCEPTOR_ANY_CH_LO/HI_STATE`), each pulse
  gated by `COIN_ACCEPTOR_PULSE_MIN`/`MAX` (20–250 ticks) and separated by
  `COIN_ACCEPTOR_INTER_PULSE_MAX` (200 ticks); the coin value is `1<<(pulse_count-1)`, a
  bitmask of pulse counts, matching an "impulse per coin type" acceptor protocol rather
  than a parallel one.
- 1 system tick = 1 ms (`timer.c:8-16`, `CCR0=1000` at SMCLK/8, TACLK up-mode).
- `EnableCoinAcceptorChannels` (`bah_io.c:1131-1148`) individually enables/disables each
  of the 6 physical channel lines as GPIO outputs when *not* in pulse mode — i.e. the C
  original can electrically disconnect unwanted channels, not just mask them in software.

**Rust** (`src/coin_acceptor.rs`): implements only a **parallel, one-line-per-channel**
model — 6 `Input` GPIOs polled every 5 ms (`POLL_MS`), a pulse counted once it has been
low ≥10 ms (`PULSE_MIN_MS`) and flagged `Stuck`/faulty at ≥1000 ms (`PULSE_MAX_MS`). There
is no pulse-counting mode, no inter-pulse timing, and no per-channel GPIO
enable/disable — masking is done purely in software via `settings.channel_mask` at
`coin_acceptor.rs:139`. This corresponds structurally to the **normal mode** state
machine's *intent* but not its implementation: it does not track a combined `mask`, does
not implement the PRE_ACCEPT settle window, and drops "pulse mode" entirely (no config
flag equivalent to `SetCoinAcceptorPulseMode`).

**Verdict: DIVERGENT.** The Rust port only reimplements (loosely) the C original's
"normal mode" channel logic; "pulse mode" (`coin_acceptor.c:84-132`,
`SetCoinAcceptorPulseMode`, `bah_io.c:1102-1104`) has no counterpart at all. If the
physical acceptor hardware in the field is configured for pulse mode, the port will not
recognise coins correctly. Timing constants also differ (C: 20/60/200/250 ms; Rust:
10/1000 ms), though this is defensible as a deliberate simplification given a comment
trail — it is not documented as an equivalence, though, so it should be called out
explicitly to Sergej rather than assumed safe.

---

## 2. Hopper / payout

**C**: `hopper.h` defines **three** hoppers (`HOPPER_A/B/C`, `config.h:205-207`). Hopper A
is the *item dispenser* (shares wiring notes with Rust's `ITEM_DISPENSER`); B and C are
coin hoppers. Each hopper's state machine (`hopperA/B/C_process`, `hopper.c:117-321`) is
**not** simply "run motor, count pulses" — it is a 4-state machine (`HOPPER_IDLE` →
`HOPPER_PAY` → `HOPPER_ERROR_LO`/`HOPPER_ERROR_HI`) that also decodes *error pulse
sequences* from the hopper's own error line: when the error line is asserted, the
firmware counts pulses on it (`error_pulse_count[]`) separated by less than
`PAUSE_BETWEEN_ERRER_CODES` (90 ticks) to build up a *numeric error code*
(`hopper.c:177-192`), which is then reported as the dispenser error value. This is a
genuine feature — the hopper communicates a fault *code*, not just "faulted"/"jammed" —
that has no equivalent anywhere in Rust.

Hopper A (dispenser) additionally checks `IS_HOPPER_A_PAY` (whether the control line is
actually driven) before crediting a pulse as a real payout versus a "coin exit in
standby" fault (`hopper.c:145-148`), and uses different pulse-timing constants
(`ITEM_HOPPER_COIN_PULSE_MIN/MAX_TIME` = 5/500 ms) than hoppers B/C
(`HOPPER_COIN_PULSE_MIN/MAX_TIME` = 10/70 ms, `hopper.h:4-11`).

**Rust** (`src/hopper.rs`): a single generic `Hopper` driver used for **one** coin hopper
(`HOPPER_COUNT = 1`, `config.rs:110`) plus the item dispenser — `config.rs` explicitly
notes Hopper C is dropped because its GPIOs collide with the coin acceptor on this build
(`config.rs:6-11, 93-104`, a real, documented hardware constraint, not an oversight).
`Hopper::dispense`/`run`/`measure_pulse` (lines 77-158) implement pulse-width judging
(`HOPPER_PULSE_MIN_MS`=5, `HOPPER_PULSE_MAX_MS`=500 — matching the *item*-hopper timing,
not the coin-hopper timing) and a coin-timeout-based jam detector, but there is **no
error-pulse-code decoding**: `is_faulted()` (line 62) is a flat boolean off the error line,
collapsing every one of the C original's numeric fault codes into a single
`HopperError::Faulted`.

**Verdict: PARTIAL.** Dropping hopper C is justified and documented (pin conflict).
Losing the coin-hopper-specific pulse timing (10/70 ms vs the 5/500 ms actually coded) is
unexplained and should be checked against real hardware — using the item-dispenser
timing for the coin hopper may accept spurious short pulses as coins or reject legitimate
ones. Losing the error-*code* decoding (`hopper.c:177-192`) is a real behavioural gap:
the port cannot distinguish different hopper fault types, only "faulted" vs "jammed vs
motor-ran-nothing-came-out".

---

## 3. iButton / 1-Wire

**C**: `1-wire.c` bit timings — reset: drive low 480 µs, release, sample at +70 µs, total
slot 480+70+410=960 µs (`1-wire.c:20-29`); write-1: low 6 µs then release, hold total ~70
µs; write-0: low 60 µs then release, hold ~70 µs (`1-wire.c:31-43`); read: drive low,
release after 10 µs, sample at +9 µs from release, total slot ~74 µs (`1-wire.c:45-54`).
Command is always `ONE_WIRE_CMD_READ_SERIAL` = 0x33 (`1-wire.h:4`), i.e. DS1990-style Read
ROM, no search algorithm (single key on the bus assumed). CRC is the Dallas/Maxim 8-bit
polynomial 0x8C reflected (`_crc_ibutton_update`, `1-wire.c:74-86`), applied byte-wise.

Role/access determination is **not** in `1-wire.c` — it is in `Bahilizator.c`:
`GetDigitalKeyAccess` (`Bahilizator.c:1067-1074`) linearly scans
`aBahilizator->settings.keys[KEY_ACCESS_LEVEL_COUNT][KEYS_PER_ACCESS_LEVEL]` (3 levels ×
2 keys, `bah_defs.h:50-51`, `include/bah_types.h:75-81`: `KEY_ACCESS_MANUFACTURER=0`,
`KEY_ACCESS_OWNER=1`, `KEY_ACCESS_TECHNICIAN=2`, else `KEY_ACCESS_NONE`), comparing byte-
for-byte with `EqualDigitalKeys` (`bah_util.c:99-107`). Keys are populated purely from the
service menu (`EditDigitalKey`, "Digital Keys" menu item, `bah_menu.h`-driven,
`Bahilizator.c:117`), no hardcoded key IDs anywhere in `config.h`.

**Rust** (`src/ibutton.rs` + `src/menu.rs::Access::of` + `src/state.rs`): bit timings are
functionally identical — reset 480/70/410 µs (`ibutton.rs:80-84`), write-1 6/64 µs,
write-0 60/10 µs (`:94-104`), read 6/9/55 µs (`:112-119`). This *does* differ slightly
from C's read timing (C: drive→release@10µs→sample@+9µs, i.e. sample at ~19µs from drive
start; Rust: drive→release@6µs→sample@+9µs, i.e. sample at ~15µs) — both are well inside
the DS1990 tolerance window, so this is not a bug, but it is not byte-identical either.
CRC is the same Maxim polynomial, computed over all 8 bytes checked against zero
(`ibutton.rs:177-192`) rather than 7-bytes-vs-8th-byte — mathematically equivalent.

Role determination: `Access::of` (`menu.rs:47-60`) scans `settings.keys` — 3 levels × 2
keys (`KEY_ACCESS_LEVELS=3`, `KEYS_PER_LEVEL=2`, `state.rs:30-32`) — mapping level 0 →
`Access::Owner`, level 1 → `Access::Service`, else → `Access::Collector`. This mirrors the
C original's structure (3 levels, 2 keys/level, keys settings-driven, no hardcoded IDs)
exactly, but the **naming does not correspond 1:1**: C's level 0 is
`KEY_ACCESS_MANUFACTURER` (most privileged, gated to `SHOW_RENT`/firmware update menu
items in `Bahilizator.c:124-127`), level 1 is `KEY_ACCESS_OWNER`, level 2 is
`KEY_ACCESS_TECHNICIAN`. Rust's level 0 is named `Access::Owner` (docstring: "everything,
including prices and enrolled keys") and level 1 `Access::Service`, level 2
`Access::Collector`. So the Rust port has **collapsed manufacturer and owner into one
level** ("Owner") and renamed technician to "Service" — a deliberate simplification (there
is no manufacturer-only menu category and no rent feature in Rust), but the level *count*
still being 3 despite only 2 real roles being used (Owner/Service, "Collector" appears
unused by any menu item requiring exactly that level — checked against `menu.rs` items,
all gate at `Collector` or above except `Price` which needs `Owner`) means the third slot
is present but its purpose (equivalent to C's manufacturer level) is unclear/unused.

**Verdict: MATCHES** for the wire protocol and CRC (bit-for-bit equivalent enough to
interoperate with the same DS1990 keys); **PARTIAL** for the role model — the mapping
from key-slot-index to privilege is preserved structurally (3×2 table) but the semantic
labels don't line up 1:1 with C's four-level enum (`MANUFACTURER`/`OWNER`/`TECHNICIAN`/
`NONE`), and it's not documented anywhere in the Rust code that manufacturer-level access
(firmware update, accounting edit — `Bahilizator.c:124-127`) was intentionally dropped
along with GSM/rent.

---

## 4. LCD driver

**C**: `lcd.c` — `initLCD` does the classic HD44780 4-bit init: three 0x03 nibbles then
0x02 (switch to 4-bit), then function set 0x28 (4-bit, 2-line, 5×8), display on 0x0C,
clear 0x01, entry mode 0x06 (`lcd.c:39-71`). RS ("A0"), R/W, E on P4.1-3, data nibble on
P4.4-7 top nibble (`config.h:184-201`). 16 columns × 2 rows (`LCD_SYMBOL_COUNT=16`,
`LCD_ROW_COUNT=2`, `config.h:200-201`). R/W is actively toggled (there *is* a
`LcdReadData`, `lcd.c:119-138`, used for busy-flag-style reads, though callers are
commented out — `//while (!LcdReadData());`). Cyrillic: bytes ≥0xC0 are looked up in
`cyrillic_chars[]` (64-entry table covering 0xC0-0xFF, `lcd.c:14-21`) before being sent to
the controller; anything below 0xC0 goes straight through (i.e. the *input* text is
assumed already encoded in a Windows-1251-like single-byte-per-glyph scheme, not UTF-8).
Two 8×8 custom characters are loaded at init (`custom_chars[0..1]`, `lcd.c:23-32,67-68`,
partial "а"/"н" glyphs — likely fallback shapes for characters the controller ROM lacks).
There is also a full software-shadow-buffer `LcdUpdate()` path (`lcd.c:190-246`) used
after e.g. GSM reinitialisation, which redraws both rows from a RAM shadow — this exists
because the LCD sometimes needs a full cold re-init mid-run (shared bus contention with
the modem, going by the file's context) and the shadow buffer is how it avoids losing
what was on screen.

**Rust** (`src/lcd.rs`): RW is taken and pinned permanently low (`new`, line 51,
docstring at :3 explicitly states the busy flag can never be polled) — i.e. **the R/W
line's read capability is deliberately not implemented at all**, only ever writing. Init
sequence: 3×0x3 (with escalating delays 5ms/150µs/150µs, not matching C's uniform
`__delay_cycles(320)` between every nibble — Rust's is more datasheet-faithful, arguably
an improvement, but is a genuine timing difference) → 0x2 → function-set 0x28 → display
off 0x08 → clear 0x01 → entry mode 0x06 → display-on-no-cursor 0x0c (`lcd.rs:69-93`). Note
the *order* differs from C: C does function-set → display-on(0x0C) → clear → entry-mode;
Rust does function-set → display-off(0x08) → clear → entry-mode → display-on(0x0c). Both
converge to the same final state, so this is not a functional bug, merely a reordering.
16 columns is set by the caller (`Lcd::new(... columns)`, and `config.rs:120-122` sets
`LCD_COLUMNS=16, LCD_ROWS=2` — matches). Cursor: Rust's `set_cursor` computes row-1 base
as `0x40` (`lcd.rs:125`), matching C's `y*0x40+x` (`lcd.c:151`) for 2 rows exactly.
`LcdEnableCursor`/blinking cursor mode (C `lcd.c:170-179`, instruction 0x0f) has **no
Rust equivalent** — the port never turns the cursor on, only 0x0c ("no cursor") is ever
sent.

Cyrillic mapping: see the byte-for-byte comparison below (§ separate finding). **A
25-of-64-character mismatch was found** between C's `cyrillic_chars[]` table
(`lcd.c:14-21`) and Rust's `cyrillic.rs::encode` map — every character from Ь (0xDC)
onward through я (0xFF) is shifted by +1 in the Rust table relative to C's. Concretely:
C maps `Ь` (uppercase soft sign) → 0x62 (lowercase ASCII 'b'), but Rust maps `Ь` → 0xAF.
C's 0xAF is what Rust assigns to `Э`. This propagates through the whole tail of the
alphabet. Either the Rust table was built from a *different revision* of the character
ROM datasheet, or a one-character insertion/deletion bug crept into the table
transcription. This is a real, demonstrable rendering bug: every Cyrillic customer-facing
string using one of the ~25 affected letters (Ь, Э, Ю, Я and all lowercase from б through
я) will draw the *wrong glyph* on real hardware with this character ROM, though it may
draw correctly if the actual fitted part uses the newer mapping — the point is the two
firmwares do not agree, so at most one of them is correct for any given physical display.

No custom-character loading exists in Rust (`custom_chars`/`LcdLoadChar` have no
equivalent) — CGRAM slots 1 and 2 that C uses for fallback glyphs are simply not
programmed by the port.

**Verdict: PARTIAL/DIVERGENT.** Core 4-bit init, geometry (16×2), and cursor addressing
match. The Cyrillic table has a confirmed off-by-one-letter divergence affecting 25 of 64
mapped characters — this is the single highest-impact UI bug found in this audit, because
it is silent (nothing crashes; wrong characters just appear) and it is customer-facing on
every screen with a Cyrillic string. Cursor blink/underline mode and CGRAM custom-glyph
loading are both dropped without comment.

---

## 5. Buttons / keypad

**C**: `config.h:110-131` defines the **keyboard** (`KEYBOARD_MASK` 0x0F on P2, 4-bit
value, purpose not decoded in `bah_io.c` snippets read — likely a numeric keypad for
codes, separate from switches) *and* **switches** on P1: `SWITCH_PREV`=0x08,
`SWITCH_NEXT`=0x04, `SWITCH_OK`=0x02, `SWITCH_CANCEL`=0x01 — **4 button switches** — plus
`SWITCH_DOOR1`=0x10, `SWITCH_DOOR2`=0x20, `SWITCH_DOOR3`=0x40 (three door/intrusion
contacts, `SWITCH_FREE_ITEM` is an alias for `SWITCH_DOOR3` — meaning the third "door"
contact doubles as a free-item-dispense trigger in some builds) and `SWITCH_RESERVED`
=0x80. `bah_io.c:812-832` (`readSwitches`) decodes button presses from these 4 switch bits
via `!(state & SWITCH_X)` (active-low) into `BUTTON_PREV/NEXT/OK/CANCEL`; door bits
1/2/3 are read but their branches are `_NOP()` (i.e. handled elsewhere via
`GetActiveSwitches`, `bah_io.c:839-842`, which just returns the raw door-bit mask).
`config.h:115` also independently defines a 4-bit `KEYBOARD_MASK` on P2, distinct
hardware from the button switches, that this audit did not find consumed in the excerpts
read — it may be dead/unused wiring (a numeric keypad footprint never populated) rather
than a currently-active input; this needs confirmation from someone with the schematic,
but it is at minimum a *fifth* input peripheral the Rust port has no analogue for.

**Rust** (`config.rs:33-36`, `buttons.rs`): `BUTTON_COUNT = 4` — Prev/Next/Ok/Cancel,
matching the C switches exactly in count and semantics. `DOOR_COUNT = 2` with an explicit
comment (`config.rs:29-33`) that the C original wires 3 door contacts on P1.3-P1.5 but
only 2 are used by the firmware — this is *correctly identified and documented* by
whoever wrote the Rust config, matching what was found by reading `bah_io.c:829-831`
(door 3's branch is also just `_NOP()`, same as doors 1/2 — so nothing in the C original
distinguishes door 3's behaviour from 1/2 either, despite `SWITCH_FREE_ITEM` being an
alias for it; this audit did not find code in the excerpts read that acts on
`SWITCH_FREE_ITEM` specifically, so "3 wired, 2 used" is a reasonable characterisation,
though the free-item-via-door-3 possibility is worth flagging to Sergej rather than
silently assuming it is truly dead).

The separate `KEYBOARD` peripheral (P2, 4-bit mask, `config.h:110-116`) has **no
mention anywhere in the Rust codebase** — not in `config.rs`, not in `buttons.rs`. If
this is a genuine second physical input (a keypad for entering codes) rather than unused
wiring, the port is missing it entirely and silently.

**Verdict: MATCHES** for the 4 primary buttons (count and semantics both correct, and the
2-vs-3-door discrepancy is correctly documented in Rust's own comments as a deliberate,
verified choice). **Flag for follow-up**: the separate `KEYBOARD` (P2, `KEYBOARD_MASK`)
peripheral in `config.h` has no Rust counterpart and was not resolved by this audit as
either "unused wiring" or "a live feature" — worth a five-minute check of the schematic
or of `bah_io.c` in full before treating it as confirmed dead.

---

## 6. Service menu

**C**: the menu table `menu[]` in `Bahilizator.c:83-128` lists (level 0 = top, level ≥1 =
submenu, gated by `key_access_t`):

- Accounting → Show Period Meters (Technician), Reset Period Meters (Technician), Show
  Overall Meters (Owner)
- Service → Item Dispenser: Item Level / Refill Items / Unload Items (Technician);
  Coin Hoppers: Refill Coins / Unload Coins (Technician); GSM Modem status (Technician);
  Reset Cash (Technician)
- Logs → View Events (Technician), Reset Events (Owner), View Transactions (Technician),
  Clear Transactions (Owner)
- Setup → Machine ID, User Language, Date and Time, Currency, Coin Acceptor, Coin
  Hoppers, Item Dispenser (all edit-settings, Owner); GSM → GSM Modem / Phone Numbers /
  Reports (Owner); Digital Keys (Technician!); Options, Timeouts, Reset Settings, Reset
  State (Owner); Show Version (Technician); **Rent** (Owner)
- Manufacturer → Edit Accounting, Reset Accounting, Update Firmware (all
  `KEY_ACCESS_MANUFACTURER`)

That is roughly **30 distinct menu actions** across 5 top-level categories, with fine-
grained per-item access control (note "Digital Keys" needs only Technician, one level
*below* most Setup items — anyone who can service the machine can also enrol new keys,
which is a real security-relevant detail worth flagging on its own).

**Rust** (`src/menu.rs::Item`, lines 66-124): **8 items**, flat (no submenus): Accounting
report, State ("остаток") report, RefillItems, RefillHopper(0), RefillHopper(1),
FreeItem, Price, ClearPeriod. Access gating: Accounting/State need `Collector`;
RefillItems/RefillHopper/FreeItem/ClearPeriod need `Service`; Price needs `Owner`.

Comparing against the C list, present in Rust: refill items (✓, Service item), refill
coin hoppers (✓, but hardcoded to exactly 2 slots regardless of `HOPPER_COUNT`, filtered
at runtime — `menu.rs:277`), free item dispense (✓, "FreeItem" — C's nearest analogue
would be a manual dispense, not explicitly named the same in the excerpt read, but
functionally matches "Unload Items" only loosely — Rust's FreeItem *dispenses* one pair
for free, C's Unload Items empties the hopper without dispensing; these are **not the
same operation**), price edit (✓), clear period counters (✓, "Reset Period Meters"
equivalent).

**Missing from Rust entirely**: Show Overall Meters (separate from period — Rust's
`Accounting`/`State` reports may fold overall in, but there's no explicit "period vs
overall" toggle visible in the 8-item list — needs checking against `report.rs`, not
audited here in detail); Unload Items / Unload Coin Hoppers (no way to *remove* stock
from the counters short of a full refill-to-zero via `edit`, which the UI presents as
"set to a new absolute level", not an explicit unload action — functionally similar but
not identical UX); GSM Modem status (correctly dropped, GSM is out of scope, see §8);
Machine ID edit; User/Service Language edit; Date and Time edit (no RTC visible in Rust
sources reviewed); Currency edit (settings.currency exists in `state.rs` but no menu item
sets it); Coin Acceptor settings edit (channel mask/values are in `CoinAcceptorSettings`
but not editable from the menu shown); Phone Numbers / Report intervals (correctly
dropped with GSM); **Digital Keys enrollment** (`EDIT_KEYS` in C) — there is **no menu
item in Rust to enrol or clear an iButton key at all**; Options/Timeouts edit; Reset
Settings / Reset State; Show Version; Show Rent (correctly dropped, see §8); the entire
Manufacturer category (Edit/Reset Accounting, Update Firmware — arguably out of scope for
a from-scratch Rust rewrite, but firmware update over the wire has no equivalent
mechanism mentioned anywhere in the reviewed Rust sources either).

**Verdict: PARTIAL, materially incomplete for field service.** The money-critical items
(refill, price, free item, clear period) are present. But **there is no way to enrol a
replacement iButton key without re-flashing firmware and hardcoding it into
`Settings::default()`, or hand-editing NVRAM** — every C-original machine has a
"Digital Keys" menu item for this, and its absence in Rust means a lost/broken service
key locks an engineer out of the machine in the field. This is a genuine, practical
regression, not merely a UI-completeness nicety, and belongs near the top of the punch
list. Language/date/currency/acceptor-settings editing are also gone, meaning any of
those settings can currently only be changed by recompiling `Settings::default()`.

---

## 7. Vending state machine

**C**: `bah_state.h` / `Bahilizator.c` model the machine's `app_state_t` as
`APP_STATE_ACCEPT_CASH → APP_STATE_PAYOUT_ITEMS → APP_STATE_PAYOUT_REMINDER →
APP_STATE_PROCESS_RESIDUAL`, persisted in `State` together with a `TransactionLog`,
`EventLog`, and per-level `Accounting[NUM_ACCOUNTING_LEVELS]` (server/overall/period —
`accounting_t`, 3 levels not 2). Power-loss recovery is handled via `IsReminderPayoutPending`
(main loop, `Bahilizator.c:2067-2079`) which, on detecting `CheckExternalPowerFailed()`,
decrements one pending coin from whichever hopper still owes one — an odd, almost punitive
recovery strategy (comment in that block literally uses a variable named `punished`) that
looks like it silently *writes off* one coin of debt per power-fail event rather than
retrying the payout — worth flagging as a genuine C-original quirk, not a Rust regression,
since Rust does not replicate this behaviour at all (Rust's `Machine::resume`,
`vending.rs:256-268`, just re-attempts the full pending payout on startup, which is more
customer-correct than the C original's apparent "punish and move on").

**Rust** (`state.rs::AppState`, `vending.rs`): identical 4-state enum (`AcceptCash →
PayoutItems → PayoutReminder → ProcessResidual`), same names in spirit. The flow —
accept coin → sell as soon as `cash >= price` (`vending.rs:328-333`, no button press
required, unlike C which the audit did not confirm requires an explicit OK — this needs
checking against the full C main loop, not fully read here) → dispense items one at a
time, persisting after each unit → plan change greedily largest-hopper-first → pay
change → show "take your change" / "thanks" → return to accept-cash — is coherent and
well-commented, and the resume-on-boot logic (persist after every unit, replay
`payout_items`/`payout_change`/`process_residual` on restart before handling any new
event) is **more conservative and safer** than the C original's coin-punishing recovery.
`Errors::FATAL` (door open, dispenser jammed/empty, firmware bad) blocking `can_vend()`
while an empty *coin* hopper does not block selling (documented rationale at
`error.rs:57-61`) is a sound, explicit policy choice not found spelled out as clearly in
the C source reviewed.

Only 2 accounting levels (overall, period) vs C's 3 (`ACC_SERVER`, `ACC_OVERALL`,
`ACC_PERIOD`) — the server-reporting level is dropped, consistent with GSM being out of
scope (server accounting existed to feed the SMS/GSM report pipeline).

**Verdict: MATCHES** in overall shape and is arguably a improvement in crash-recovery
semantics (no "punish and forget" coin write-off); the accounting-level count difference
(2 vs 3) is consistent with the documented GSM removal and not a bug. The claim that a
sale requires no confirmation button (only reaching the price) could not be fully
verified against the *complete* C main loop in the time available and should be spot-
checked, since it changes customer experience (auto-vend vs press-OK-to-buy) — flagged as
unverified rather than asserted as either MATCHES or DIVERGENT.

---

## 8. GSM/SMS reporting and rent

**GSM**: `gsm.c` (571 lines, not fully read) implements a SIM340D-based modem stack: power
sequencing (`SIM340_POWER_SUPPLY_*`, `SIM340_POWER_SW_*`), DTR/RTS/CTS flow control,
`COMMAND_TIMEOUT`=10s, `CHECK_GSM_TIMEOUT`=30s, an IMEI reader, and a full AT-command
state machine feeding `bah_io.c`'s `SendSMS`/`IsSendingSMS`/`IsGsmFailure` wrappers,
itself feeding `bah_dlgmsg.c`'s message queue (`message_t` enum: state/accounting reports,
warning-level notices, intrusion reports, power up/down, error reports —
`bah_types.h:152-169`) addressed to 3 phone-access levels × 2 numbers each
(`PHONE_ACCESS_SERVER/OWNER/TECHNICIAN`, `bah_defs.h:59-67`). This is a substantial,
functioning subsystem in the C original, not a stub.

**Rust**: no `gsm.rs`/equivalent module exists anywhere in `src/`; `state.rs:9` explicitly
states "The phone-number table the original keeps in the settings is absent: this build
has no GSM." **Confirmed accurate** — there is no phone-number storage, no SMS message
queue, no modem driver, and the menu has no GSM-related items (correctly, since
`SHOW_GSM_MODEM_STATUS`/`EDIT_GSM_MODEM`/`EDIT_PHONE_NUMBERS`/`EDIT_REPORT_INTERVALS` are
all absent from `menu.rs`). The claim in the codebase is honest and matches what this
audit found.

**Rent** (`bah_rent.c`, `bah_rent.h`): this is a **licensing/anti-piracy mechanism**, not
a customer-facing "rental" feature. It generates a challenge (`gen_request`) containing a
random offset into an embedded RC4 key blob, XOR-obfuscated with RC4 keyed by that offset,
CRC-checked, presumably sent to a licensing server over the GSM link; the server's reply
(`parse_reply`) grants a number of days (`RentGetDaysLeft`, 12-bit day count packed with
4 flag bits) which decrements every minute (`isRentExpired`, `bah_rent.c:135-159`) and is
persisted to flash (`save_rent_to_fram`/`load_rent_from_fram`) with a **primary/secondary
redundant copy** scheme for power-cut safety, similar in spirit to Rust's NVRAM journal
but hand-rolled per-struct rather than a generic append log. The entire mechanism is
gated behind `#ifdef RENT_ENABLED` (confirmed via `ewp` project file: `RENT_ENABLED` is
defined in at least one of the two build configurations found in
`bahilizator.ewp`, i.e. it is a real, shipped build variant, not dead code kept only for
reference) — `RentExpire`/`RentGetDaysLeft` gate whether the machine will vend at all
(`ERROR_RENT_EXPIRED` exists as a first-class fault in `bah_types.h:237`). Where
`RENT_ENABLED` is off, `isRentExpired()` unconditionally returns `FALSE` and the feature
is fully inert.

**Rust**: no rent/licensing module, no `ERROR_RENT_EXPIRED`-equivalent flag in
`error.rs`'s `Errors` bitflags, no menu item (C's "Rent", `Bahilizator.c:123`, requires
`KEY_ACCESS_OWNER`). Not mentioned anywhere in the Rust source or its comments as a
known, deliberate omission (unlike GSM, which *is* called out).

**Verdict — GSM: MATCHES the stated scope** (explicitly dropped, and the claim that it is
dropped is accurate). **Verdict — Rent: MISSING and undocumented.** Whether this matters
depends entirely on whether any real deployed machine actually ships with
`RENT_ENABLED` — worth a direct question to Sergej rather than assuming either way, since
the audit confirmed the flag *exists* in at least one build configuration in the project
file but could not confirm it is the configuration used for the specific machines in the
field. If any physical unit relies on rent-based enablement, the Rust port as it stands
would need that machine's rent state pre-satisfied by other means or it is a licensing
gap, not just a feature gap.

---

## 9. NVRAM / settings persistence

**C**: Two storage paths coexist, selected by build flags found in `bahilizator.ewp`:
`fram.c` (external SPI FRAM, byte-addressed `framGetc`/`framPutc`/`framRead`/`framWrite`)
and internal-flash-backed `ReadNVRAM`/`WriteNVRAM` (referenced from `bah_settings.c:156-
187`, backing implementation not in the files read for this audit — likely in
`flash.c`). `_FRAM_STORAGE_` gates whether `SaveSettings`/`SaveState` calls are compiled
in at various call sites in `Bahilizator.c` (lines 307, 1332, 1403, 1470, 1575, 1583,
1715, 1731) — meaning **in some build configurations, state/settings are not persisted
across power loss at all**, an important caveat for anyone assuming the C original always
saves. `SaveSettings` (`bah_settings.c:172-187`) is CRC-gated to skip the flash write
entirely if the new CRC matches what's already stored — a wear-reduction optimisation
Rust's `save_settings` (`nvram.rs:471+`) does not appear to replicate (it writes
unconditionally on every call, per the code read). `State` includes a `TransactionLog`
and `EventLog` (`bah_state.h:30-31`) that persist individual transactions and
system/access/error events with timestamps — **there is no equivalent event or
transaction log in Rust's `nvram.rs`/`state.rs` at all**; only aggregate `Counters` and
`Settings` are journalled.

**Rust** (`nvram.rs`): a purpose-built append-only flash journal for `Counters` (3
segments × 64-byte records, CRC-16/CCITT-FALSE-checked, sequence-numbered for wrap-safe
"newest wins" recovery — a materially more robust and clearly-designed scheme than C's
described here, with an explicit and correct rationale for why it exists: flash write-
cycle wear under frequent counter updates) plus a single-segment, unconditionally-
rewritten `Settings` block (`save_settings`, from line 471). Both include version and CRC
fields (`SETTINGS_VERSION`/`COUNTERS_VERSION`, `error::raise(Errors::STATE_CRC/
STATE_VERSION)` on failure) — a real improvement in power-cut safety for the *counters*
specifically, versus the C original's single-shot flash/FRAM write with no redundancy
described in the files read (though FRAM itself is far more write-tolerant than the
MSP430's internal flash, so the two designs are optimised for different underlying media,
and the comparison isn't strictly apples-to-apples).

Enrolled keys are persisted (`s.keys`, `nvram.rs:488-492`) — matches C's persisted
`settings->keys[][]`. Workday hours, timeouts, message delays all persisted, matching C's
equivalent settings fields.

**Gaps versus C**: no persisted event log, no persisted transaction log (both exist and
are explicitly part of `State` in C, `bah_state.h:30-31`, and both are readable from the
service menu in C — "View Events"/"View Transactions" — which explains why those two menu
items have no Rust equivalent: there is nothing to show). No per-server accounting level
persisted (consistent with dropping GSM). No CRC-unchanged write-skip optimisation on the
settings save path (minor — settings change rarely, so this mostly affects nothing, but
it is a real difference in flash wear behaviour if the menu's "Price" edit is hit
repeatedly during testing).

**Verdict: PARTIAL.** The counters/settings journalling that exists is well-designed and,
for the *money-critical* counters, arguably safer than the C original's approach. But the
event log and transaction log are gone entirely, not merely "not shown in the menu" —
`nvram.rs` has no data structure for either, so this is a genuine loss of forensic/audit
trail (useful for disputes about "the machine ate my coin and gave nothing", among other
things) rather than a UI gap.

---

## 10. Error handling

**C** `error_t` enum (`bah_types.h:206-238`), 18 members: `ERROR_NONE`, `ERROR_FIRMWARE`,
`ERROR_SETTINGS_VERSION`, `ERROR_SETTINGS_CRC`, `ERROR_STATE_VERSION`, `ERROR_STATE_CRC`,
`ERROR_LOG_VERSION`, `ERROR_LOG_CRC`, `ERROR_BATTERY_LEVEL_LOW`, `ERROR_FLASH_CORRUPTED`,
`ERROR_NVRAM_CORRUPTED`, `ERROR_GSM_MODULE`, `ERROR_SIM_CARD`,
`ERROR_COIN_ACCEPTOR_DEVICE`, `ERROR_ITEM_DISPENSER_DEVICE`, `ERROR_COIN_HOPPER_DEVICE`,
`ERROR_COIN_ACCEPTOR_DISABLED`, `ERROR_ITEM_DISPENSER_EMPTY`, `ERROR_COIN_HOPPER_EMPTY`,
`ERROR_UNABLE_TO_PAYOUT_REMINDER`, `ERROR_DOOR_OPENED`, `ERROR_RENT_EXPIRED`. Errors are
recorded into a `set_t` (32-bit bitset, `InsertElement`/`RemoveElement`/`IsElementSet`,
`bah_errors.h`) and also logged with timestamps into the `EventLog` (§9) as
`EVENT_TYPE_ERROR` entries — i.e. C keeps both a live fault-set *and* a historical record
of when each fault appeared/cleared.

**Rust** `Errors` bitflags (`error.rs:20-53`), 16 members: `FIRMWARE`,
`SETTINGS_VERSION`, `SETTINGS_CRC`, `STATE_VERSION`, `STATE_CRC`, `BATTERY_LOW`,
`FLASH_CORRUPTED`, `NVRAM_CORRUPTED`, `COIN_ACCEPTOR`, `ITEM_DISPENSER`, `COIN_HOPPER`,
`COIN_ACCEPTOR_OFF`, `ITEM_DISPENSER_EMPTY`, `COIN_HOPPER_EMPTY`, `CANNOT_PAYOUT`,
`DOOR_OPENED`. No historical log (consistent with §9's finding — only a live bitset,
no `EventLog` equivalent).

Direct comparison:
- `ERROR_LOG_VERSION`/`ERROR_LOG_CRC` — missing in Rust; consistent with no event log
  existing to have a version/CRC.
- `ERROR_GSM_MODULE`/`ERROR_SIM_CARD` — correctly absent, GSM out of scope.
- `ERROR_RENT_EXPIRED` — absent, consistent with §8's finding that rent has no Rust
  equivalent at all.
- `ERROR_COIN_ACCEPTOR_DISABLED` ≈ Rust's `COIN_ACCEPTOR_OFF` — present, matches.
- `ERROR_UNABLE_TO_PAYOUT_REMINDER` ≈ Rust's `CANNOT_PAYOUT` — present, matches
  (`vending.rs:221`, set when residual change is unpayable).
- **`ERROR_ITEM_DISPENSER_DEVICE`/`ERROR_COIN_HOPPER_DEVICE`** (jam/malfunction of the
  *device itself*, as distinct from being merely empty) map to Rust's `ITEM_DISPENSER`/
  `COIN_HOPPER` — present and used correctly (`vending.rs:143-145, 185`).
- **All present, no C original fault this audit could match against a missing Rust
  flag**, aside from the log-related and rent/GSM-related ones already explained.

**Verdict: MATCHES**, modulo the deliberate/explained absences (GSM, rent, log
versioning) already covered in §8/§9. The live-fault bitset itself is a complete and
faithful subset of the C original's conditions for a build with GSM and rent removed;
what's missing is purely the *historical* dimension (the event log), not any live
condition the machine needs to react to in real time.

---

## 11. bah_rent.c, bah_scroller.c, bah_strutil.c, bah_util.c

- **`bah_rent.c`** — see §8. Licensing/anti-piracy time-lock, RC4-obfuscated
  challenge/response over (presumably) the GSM link, flash-persisted with primary/
  secondary redundancy. **No Rust equivalent, undocumented as dropped.**

- **`bah_scroller.c`** (109 lines) — a text-scrolling helper (`TextScroller`) supporting
  5 alignment modes (`ALIGN_LEFT/CENTER/RIGHT/SCROLL_LEFT/SCROLL_BOUNCE`,
  `bah_types.h:192-198`) for showing strings longer than the 16-column display,
  auto-switching bounce-mode to centred if the text turns out to fit
  (`bah_scroller.c:57-59`). Time-driven via `GetTicks()`/`TEXT_SCROLL_INTERVAL`/
  `TEXT_STOP_INTERVAL`. **Rust equivalent**: `menu.rs::show_report` (lines 149-166) pages
  long text two lines at a time, advanced by button press, rather than scrolling
  automatically. This is a **deliberate, reasonable design substitution** (paging instead
  of auto-scroll, explicitly justified in that function's doc comment: "Scrolling would
  need a timer and would move under the reader's eye"), not a gap — flagged as
  **intentional divergence**, not a bug.

- **`bah_strutil.c`** (282 lines) — string/number formatting: `IntToStr`, `IntToStrWidth`,
  `CashToStr` (fixed-point currency formatting using `CURRENCY_FRACTIONAL_DIGITS`),
  `DateTimeToStr`/`DateToStr`/`TimeToStr` (date/time formatting via `localtime`),
  `IntToHex`, `Int64ToHex`, a small fixed string-buffer API (`StrBufInit`/`StrBufCat`).
  **Rust equivalent**: none of this exists as a discrete module; Rust uses `core::fmt`
  (the standard `write!`/`Display` machinery, seen in `menu.rs:184` and throughout
  `report.rs`) which subsumes essentially all of this functionality more safely (no
  fixed-size `format_buffer` overflow risk, which the C version's manual digit-reversal
  loops do carry, bounded only by `FORMAT_BUFFER_SIZE`). Date/time formatting has no
  direct equivalent because there is no RTC-backed clock module surfaced anywhere in the
  Rust sources reviewed — if the port has no real-time clock at all, "Date and Time" menu
  editing (missing per §6) and any timestamped log (missing per §9/§10) are structurally
  impossible until a clock exists, which explains several of the other gaps found in this
  audit as downstream consequences of a single root cause rather than independent
  omissions.

- **`bah_util.c`** (150 lines) — CRC-16-like table-free CRC (`CalcCRC`, nibble-driven,
  distinct algorithm from the CRC-16/CCITT-FALSE Rust uses in `nvram.rs::crc16` — not
  cross-compatible, but that's fine since nothing needs to read old C-format NVRAM
  images), `IsEmptyDigitalKey`/`EqualDigitalKeys`/`ClearDigitalKey` (byte-compare/clear
  over `ibutton_t`, mirrored functionally by `IbuttonKey::is_empty`/`PartialEq`/`EMPTY` in
  `state.rs:71-80`), `GetKeyAccessStr` (access-level-to-string, mirrored loosely by
  whatever labels the Rust UI uses for `Access` — not checked in detail here),
  `SetOption`/`GetOption` (generic bitmask helpers over a `set_t`, superseded by Rust's
  `bitflags!`-generated methods on `Errors` — a strictly better mechanism, not a gap).

**Verdict, this section**: `bah_scroller.c` → **intentional, justified substitution**.
`bah_strutil.c`/`bah_util.c` → **MATCHES in effect**, superseded by idiomatic Rust
(`core::fmt`, `bitflags!`) rather than ported line-for-line, which is the right call for a
rewrite. `bah_rent.c` → **MISSING, undocumented** (repeat of §8's finding, listed here for
completeness against the literal file list in the task).

---

## Top issues to fix (priority order)

Money/hopper correctness first, then UI/menu completeness, GSM/rent last (per the
brief's own priority ordering — GSM/rent were explicitly out of scope by design, so they
rank lowest even though "rent" gates whether a real machine legally/technically vends).

1. **No coin-hopper "pulse mode" support, and coin-hopper pulse timing constants look
   borrowed from the item dispenser, not the coin hopper.** (§1, §2) If any deployed
   acceptor uses pulse-mode signalling (C explicitly supports and switches on it via
   `SetCoinAcceptorPulseMode`), the Rust port will not recognise coins on that hardware
   at all — a total money-acceptance failure, not a subtle one. Separately, `hopper.rs`
   uses `HOPPER_PULSE_MIN/MAX_MS` = 5/500 ms for the shared coin hopper, but C's
   `HOPPER_COIN_PULSE_MIN/MAX_TIME` for coin hoppers B/C is 10/70 ms — a coin hopper
   pulse lasting, say, 200 ms would be accepted as a real coin by the port but is outside
   the C original's coin-hopper-specific window. Needs a decision + fix before this ships
   on real coin-taking hardware.

2. **Hopper error-code decoding is gone** (§2): the C original decodes numeric fault
   codes out of pulse trains on the hopper's error line (`hopper.c:177-192`); the port
   collapses everything to `Faulted`/`Jammed`. Not money-losing by itself, but it removes
   diagnostic information a field engineer would otherwise get for free, and could mask
   which of several distinct hardware faults is actually occurring.

3. **No service-menu way to enrol a replacement iButton key** (§6): C's "Digital Keys"
   menu item (Technician-level) has no Rust equivalent anywhere in `menu.rs`. A lost or
   damaged service key would require re-flashing firmware to regain field access — this
   is an operational lockout risk, not just a feature gap, and should be fixed before
   the port is relied on for real field service.

4. **Cyrillic character table is wrong for roughly 25 of 64 mapped glyphs**, specifically
   everything from Ь onward through я is shifted by one table position relative to the
   C original's `cyrillic_chars[]` (§4, with a full byte-by-byte diff computed during this
   audit). This is silent and customer-facing: wrong letters will render on real hardware
   using the character ROM the C table was built for, on every screen using an affected
   letter. Cheap to verify (put a display next to both firmwares) and cheap to fix once
   confirmed which table is right.

5. **Event log and transaction log are entirely unimplemented** (§9, §10): not shown in
   the menu, and not because the menu items were pruned for space — there's no underlying
   data structure to show. This matters most for after-the-fact dispute resolution
   ("the machine took my coin and didn't dispense") and for a field engineer diagnosing
   an intermittent fault after the fact, since the live-fault bitset (§10) only shows
   *current* state, not history.

6. **Rent/licensing (`bah_rent.c`) has no port and is undocumented as dropped**, unlike
   GSM which is explicitly and correctly called out as out of scope (§8). If any real
   machine's build has `RENT_ENABLED` set (confirmed present as a build-config option in
   `bahilizator.ewp`), that machine's willingness to vend is gated on a mechanism the
   port doesn't replicate. Lowest priority per the brief only if no field machine actually
   relies on it — this needs a direct answer from Sergej, not an assumption either way.

7. Minor/lower-priority items noted for completeness but not requiring urgent action:
   missing menu items for Currency/Language/Date-Time/Coin-Acceptor-settings editing and
   Unload (as distinct from refill) actions (§6); no CRC-unchanged skip-write
   optimisation on the settings flash path (§9, wear-relevant only under heavy repeated
   menu use); the unresolved `KEYBOARD` (P2, `KEYBOARD_MASK`) peripheral in `config.h`
   with no Rust counterpart and no confirmation either way whether it's live hardware
   (§5); GSM (§8) and the C original's own "punish one coin of debt per power-fail"
   recovery quirk (§7, which Rust does *not* replicate — correctly, in this auditor's
   judgement, since Rust's full-retry recovery is more customer-fair, but worth Sergej's
   explicit sign-off since it is a deliberate behavioural deviation from the original).

---

## Fixes Applied

The five highest-priority findings above were fixed on `main`, in priority order, and
mirrored to `port_std` (which shares the affected modules with `main` behind `#[cfg(feature =
"hw")]` gates — see below). Every commit was verified with `cargo check` (0 errors); the
real-hardware build was additionally verified end-to-end with `cargo build --release`
(`main`) / `cargo build --release --features hw --target msp430-none-elf -Z build-std=core`
(`port_std`), both of which **link successfully** for the actual `msp430-none-elf` target —
not just `cargo check`. The std desktop simulator (`sim/`, `port_std` only) was also rebuilt
and confirmed to still compile after each mirrored change.

1. **DONE — Coin acceptor pulse mode** (`main` `c2a9c07`, `port_std` `1b6374e`). Added
   `CoinAcceptorSettings::pulse_mode` (a runtime field, persisted through `nvram.rs`, mirroring
   the C original's `settings.coin_acceptor.pulse_mode`) and a full pulse-counting decoder in
   `coin_acceptor.rs`, using the C original's own timing constants
   (`COIN_ACCEPTOR_PULSE_MIN/MAX`=20/250ms, `COIN_ACCEPTOR_INTER_PULSE_MAX`=200ms,
   `coin_acceptor.h`) rather than invented values. `coin_task` now dispatches to either the
   pre-existing parallel-channel logic or the new pulse-mode logic based on the setting.

2. **DONE — Coin-hopper pulse timing and error-code decoding** (`main` `d5760c7`, `port_std`
   `e5866fa`). Split the single shared pulse-timing window into `ITEM_HOPPER_PULSE_MIN/MAX_MS`
   (5/500ms, correctly scoped to the dispenser) and `HOPPER_PULSE_MIN/MAX_MS` (10/70ms, now
   correct for coin hoppers, taken from the C original's `HOPPER_COIN_PULSE_MIN/MAX_TIME`).
   Added `HopperKind::{ItemDispenser, CoinHopper}` to select the right window per instance.
   `HopperError::Faulted` now carries a decoded numeric fault code, read by a new
   `read_fault_code()` pulse decoder over the error line (grouped by the C original's
   `PAUSE_BETWEEN_ERRER_CODES`=90ms), mirroring `hopper.c`'s `HOPPER_ERROR_LO`/`HI` state
   machine — the port can now distinguish different hopper fault codes, not just a flat
   "faulted" flag.

3. **DONE — Key enrolment/clearing** (`main` `46ff258`, `port_std` `fadbfd9`). Added
   `Item::Keys` to the service menu (`Access::Owner`-gated — deliberately stricter than the C
   original's Technician-level gate, documented as such), a new `enroll_key()` flow that steps
   through the 3×2 key-slot table, and `Cancel` to clear a slot / presenting any key to enrol
   it, matching the C original's `EditDigitalKeys`/`EditDigitalKey`. No changes were needed in
   `ibutton.rs` (it already reads any key's raw ID regardless of whether it's recognised) or
   `nvram.rs` (`Settings::keys` was already fully persisted).

4. **DONE — Cyrillic table off-by-one** (`main` `0cf898e`, `port_std` `cb93955`). Copied the C
   original's `cyrillic_chars[]` table (`lcd.c:14-21`) byte-for-byte instead of the ordinal
   values `cyrillic.rs` had computed, which diverged from Ь (0xDC) onward because the C table
   special-cases uppercase Ь as 0x62 rather than continuing the ordinal sequence. Verified: А,
   Б, В, Ь, Э, Ю, Я now match the C table exactly (0x41, 0xA0, 0x42, 0x62, 0xAF, 0xB0, 0xB1),
   and a scripted check over every match arm confirmed no two Cyrillic letters draw the same
   glyph. Ё/ё (not in the C table at all) remain this port's own addition, documented as such.

5. **DONE — Persisted event log** (`main` `c94a462`, `port_std` `3a06278`). Added
   `EventKind` (`state.rs`) and `EventLog` (`nvram.rs`): a 32-entry, CRC-checked, flash-backed
   ring buffer in its own NVRAM segment (`memory.x` grown by one 512-byte segment), recording
   coin-accepted, item-dispensed, hopper-payout, fault-raised, and service-menu-entry/exit
   events with a monotonic sequence number (there is no RTC on this board, so no wall-clock
   timestamp is available — consistent with the same root cause several other gaps in this
   audit were traced to). Wired into `vending::Machine` (a new `events: EventLog` field) at
   every point named in the audit, and exposed to technicians as `Item::EventLog`
   (`Access::Collector`-gated, same as the other read-only reports) through the existing
   `menu.rs::show_report` paging mechanism, via a new `report::events()` function.

**Left for follow-up, not addressed by this pass** (out of the top-5 scope given to this
task, but noted for completeness since they remain in the audit's priority list above):
rent/licensing (`bah_rent.c`, item 6 in the priority list) and the minor items in the
priority list's item 7 (Currency/Language/Date-Time/Coin-Acceptor-settings menu editing,
settings-save CRC-skip optimisation, the unresolved `KEYBOARD` peripheral, and the C
original's power-fail coin-debt-write-off quirk). None of these were flagged as blocked by
missing information — they were simply outside the 5 items this task was scoped to fix.

**Audit imprecision noted during this work:** none found that changed the fix approach. The
audit's C-code citations (line numbers, constant names, function names) were re-verified
directly against `/root/bahilizator-c-original` before each fix and found accurate in every
case checked, including the less-obvious ones (the `Ь`→0x62 special case behind the Cyrillic
off-by-one, and the exact hopper error-pulse-grouping logic in `hopper.c:168-192`).

**Final commit hashes:**
- `main`: `c2a9c07` (audit #1), `d5760c7` (audit #2), `46ff258` (audit #3), `0cf898e` (audit
  #4), `c94a462` (audit #5).
- `port_std`: `1b6374e` (audit #1), `e5866fa` (audit #2), `fadbfd9` (audit #3), `cb93955`
  (audit #4), `3a06278` (audit #5).
