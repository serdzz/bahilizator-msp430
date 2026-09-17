//! How the tasks talk to each other.
//!
//! Everything that happens to the machine — a coin, a press, a door, a key — arrives at the vending
//! state machine as an [`Event`] on one channel. The drivers do not know what a coin means and the
//! state machine does not know which pin it came in on, which is what lets either be changed
//! without touching the other.
//!
//! The channel is deliberately small. If the state machine has fallen far enough behind that eight
//! events are queued, the machine has a worse problem than a lost event, and a driver blocked on a
//! full queue is a driver that stops watching its hardware.

#[cfg(feature = "hw")]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
#[cfg(feature = "hw")]
use embassy_sync::channel::{Channel, Sender};
#[cfg(feature = "hw")]
use embassy_sync::signal::Signal;

use crate::state::{Cash, IbuttonKey};

/// The buttons on the front panel, by what they do rather than where they are.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Button {
    /// Move up, or back.
    Prev,
    /// Move down, or forward.
    Next,
    /// Accept.
    Ok,
    /// Go back, or abandon.
    Cancel,
}

impl Button {
    /// The buttons in the order [`crate::config::BUTTON_PINS`] lists their pins.
    pub const ALL: [Button; 4] = [Button::Prev, Button::Next, Button::Ok, Button::Cancel];
}

/// Something that happened.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// A coin went into the acceptor.
    Coin {
        /// Which channel reported it, counted from zero.
        channel: u8,
        /// What that channel says the coin is worth.
        value: Cash,
    },
    /// A button was pressed. Repeats are not sent; a hold is one event.
    Button(Button),
    /// A door was opened or closed.
    Door {
        /// Which door, indexing [`crate::config::DOOR_PINS`].
        index: usize,
        /// True if it is now open.
        open: bool,
    },
    /// A key was held against the reader.
    ///
    /// Hopper sensor pulses are deliberately not events. The hopper driver reads its own sensor
    /// while it is running the motor, because the only thing that cares how many coins came out is
    /// the code that asked for them.
    Key(IbuttonKey),
}

/// How many events may be queued before a driver has to wait.
#[cfg(feature = "hw")]
const EVENT_QUEUE: usize = 8;

/// The one queue everything arrives on.
#[cfg(feature = "hw")]
static EVENTS: Channel<CriticalSectionRawMutex, Event, EVENT_QUEUE> = Channel::new();

/// A handle the drivers use to report what they saw.
#[cfg(feature = "hw")]
pub type EventSender = Sender<'static, CriticalSectionRawMutex, Event, EVENT_QUEUE>;

/// The sending end of the event queue.
#[cfg(feature = "hw")]
pub fn sender() -> EventSender {
    EVENTS.sender()
}

/// Wait for the next event.
///
/// Only the vending task should call this: events are consumed, so a second reader would take
/// events the state machine never sees.
#[cfg(feature = "hw")]
pub async fn next() -> Event {
    EVENTS.receive().await
}

/// What the display should be showing.
///
/// A signal rather than a queue, because the display only ever needs to show the newest thing. If
/// two screens are produced before either is drawn, drawing the older one first would be a flicker
/// nobody asked for.
#[cfg(feature = "hw")]
pub static SCREEN: Signal<CriticalSectionRawMutex, crate::ui::Screen> = Signal::new();

/// Ask the display to show `screen`.
#[cfg(feature = "hw")]
pub fn show(screen: crate::ui::Screen) {
    SCREEN.signal(screen);
}
