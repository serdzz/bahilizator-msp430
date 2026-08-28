//! The front panel buttons and the door switches.
//!
//! Both are mechanical contacts on pins that could interrupt, and both are polled instead. An
//! interrupt per edge would fire dozens of times per press while the contact bounces, and the work
//! to suppress that is the same debouncing done here — with the difference that polling cannot be
//! swamped by a contact that has started to chatter with age.
//!
//! A contact counts as changed once it has held its new state for [`DEBOUNCE_MS`]. Presses are
//! reported once, on the way down; a hold is not a repeat.

use embassy_msp430::gpio::Input;
use embassy_time::{Duration, Ticker};

use crate::config::{BUTTON_COUNT, DEBOUNCE_MS, DOOR_COUNT, INPUT_POLL_MS};
use crate::error::{self, Errors};
use crate::event::{Button, Event, EventSender};

/// Consecutive agreeing samples a contact needs before it is believed.
///
/// At least one, however the timings are set: zero would mean believing every sample, which is what
/// debouncing exists to avoid.
const STABLE_SAMPLES: u8 = {
    let n = (DEBOUNCE_MS / INPUT_POLL_MS) as u8;
    if n == 0 { 1 } else { n }
};

/// One contact's debounce state.
struct Debounced {
    /// The level currently believed.
    stable: bool,
    /// The level the last sample saw.
    last: bool,
    /// How many samples in a row have agreed with `last`.
    agreed: u8,
}

impl Debounced {
    /// Start out believing whatever the pin says, so that a door already open at power-on is known
    /// to be open rather than reported as opening later.
    fn new(level: bool) -> Self {
        Self {
            stable: level,
            last: level,
            agreed: STABLE_SAMPLES,
        }
    }

    /// Feed in a sample. Returns the new level if this sample settled a change.
    fn update(&mut self, level: bool) -> Option<bool> {
        if level != self.last {
            self.last = level;
            self.agreed = 1;
            return None;
        }

        if self.agreed < STABLE_SAMPLES {
            self.agreed += 1;
        }

        if self.agreed >= STABLE_SAMPLES && self.stable != level {
            self.stable = level;
            return Some(level);
        }
        None
    }
}

/// Watch the buttons and the doors, and report what changes.
///
/// Both sets of contacts are pulled up and close to ground, so a pressed button and an open door
/// are both a low pin — hence the inversions below rather than at the call sites.
#[embassy_executor::task]
pub async fn input_task(
    buttons: [Input<'static>; BUTTON_COUNT],
    doors: [Input<'static>; DOOR_COUNT],
    events: EventSender,
) {
    let mut button_state: [Debounced; BUTTON_COUNT] =
        core::array::from_fn(|i| Debounced::new(buttons[i].is_low()));
    let mut door_state: [Debounced; DOOR_COUNT] =
        core::array::from_fn(|i| Debounced::new(doors[i].is_low()));

    // A door found open at power-on is a fault from the first moment, not from the first change.
    error::set(
        Errors::DOOR_OPENED,
        door_state.iter().any(|d| d.stable),
    );

    let mut ticker = Ticker::every(Duration::from_millis(INPUT_POLL_MS));
    loop {
        ticker.next().await;

        for (i, pin) in buttons.iter().enumerate() {
            // Only the press is an event. Reporting the release too would double every menu
            // keystroke, and nothing in the machine acts on a button coming back up.
            if button_state[i].update(pin.is_low()) == Some(true) {
                // A full queue means the state machine is not keeping up; the press is dropped
                // rather than stalling this task, which also watches the doors.
                let _ = events.try_send(Event::Button(Button::ALL[i]));
            }
        }

        for (i, pin) in doors.iter().enumerate() {
            if let Some(open) = door_state[i].update(pin.is_low()) {
                let _ = events.try_send(Event::Door { index: i, open });
                error::set(
                    Errors::DOOR_OPENED,
                    door_state.iter().any(|d| d.stable),
                );
            }
        }
    }
}
