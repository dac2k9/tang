//! A window of the program's own, for a plugin's editor to live in (step 112).
//!
//! A plugin that has a face of its own (the sound browser of a ROLI player, Pianoteq's keyboard and knobs) draws it into a native view that the host gives it, so the host needs a window to put that view in
//! and an event loop that gives the window its clicks and keys. Tang has neither: it is a terminal program and a server. This module is the little of both that it needs, and all that the plugin formats know of
//! the platform: the format's own code (`plugin::vst3::editor`) asks for a [`Window`], hands the plugin its [`parent`](Window::parent) and does what the plugin says about the size.
//!
//! All of it is for the MAIN thread, which is where the controller runs and where plugins are loaded (AppKit insists on it, and so do the plugins). No thread of ours turns the window's events: whoever has a
//! window open calls [`pump`] now and then, and nobody may while the scan of the installed plugins is running (`midi.rs` says why: a plugin of the JUCE kind keeps its message queue on the main run loop, and a run
//! loop that turned while the scan let go of one crashed the program, f179).

use std::ffi::c_void;
use std::rc::Rc;
use std::time::Duration;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{available, open, pump};

/// How a window is to be opened.
// (The macOS window reads every field; a platform that has no window does not, and says so in its answer to `open`.)
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct Spec<'a> {
    pub title: &'a str,
    /// The size of what is inside the window, in points (see [`Window::size`]).
    pub width: i32,
    pub height: i32,
    /// Whether the person may drag the window's edge: a plugin says it can be, and is told the new size.
    pub resizable: bool,
    /// Whether the window takes the keyboard from the program that has it (the terminal): what a person who asked for the window wants, and what a check that runs while somebody types does not.
    pub focus: bool,
}

/// A window with a plugin's view in it, as a plugin format sees it.
pub trait Window {
    /// What the plugin attaches its view to: the window's content view (an `NSView *` on macOS). It is good for as long as the window is.
    fn parent(&self) -> *mut c_void;

    /// The size of what is inside the window, in points, the unit that a plugin's view is measured in on macOS (a platform that counts pixels will say so here when it is built).
    fn size(&self) -> (i32, i32);

    /// Make what is inside the window this size.
    fn set_size(&self, width: i32, height: i32);

    /// Whether the window is on the screen: false once the person has closed it, and after [`Window::close`].
    fn is_open(&self) -> bool;

    /// Bring the window to the front again, as it was when it was opened: `focus` is whether it takes the keyboard from the program that has it.
    fn raise(&self, focus: bool);

    /// Take the window away. A window that is already gone is left as it is.
    fn close(&self);

    /// What is in the window, as a PNG, for a person who cannot see the screen (`tang window --snapshot`) and for a test; `None` where it cannot be read. A view that is drawn by the graphics card (a plugin that draws
    /// with Metal) may give a blank picture, and says nothing.
    fn snapshot_png(&self) -> Option<Vec<u8>>;

    /// The views that are in the window, one to a line and each indented by how deep it is (its class, its size), and then the program's other windows: what a person reads when a plugin's window is empty (a plugin that has
    /// made nothing in it shows as a window of one view, and one that makes a window of its own shows as another).
    fn describe(&self) -> String;
}

/// What a platform that has no windows says to every question: Tang opens a plugin's window on macOS only so far.
#[cfg(not(target_os = "macos"))]
mod unsupported {
    use std::rc::Rc;
    use std::time::Duration;

    use super::{Spec, Window};

    const WHY: &str = "Tang shows a plugin's own window on macOS only so far";

    pub fn available() -> Result<(), String> {
        Err(WHY.to_string())
    }

    pub fn open(_spec: &Spec) -> Result<Rc<dyn Window>, String> {
        Err(WHY.to_string())
    }

    pub fn pump(limit: Duration) {
        std::thread::sleep(limit);
    }
}
#[cfg(not(target_os = "macos"))]
pub use unsupported::{available, open, pump};

/// The type of [`open`], which the editors take as an argument so that a test can give them a window that is not on any screen.
pub type Opener<'a> = &'a dyn Fn(&Spec) -> Result<Rc<dyn Window>, String>;

/// How long a person who waits on the windows (the probe, the loops of the program) lets [`pump`] wait for an event: a turn is this long at most when nothing happens, and nothing waits on it longer than this to
/// notice that a window was closed.
pub const PUMP_TURN: Duration = Duration::from_millis(2);
