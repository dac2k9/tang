//! A CLAP plugin's own window, for a session (step 114, part C): the `UI` button on its node, `u` in the terminal, and `tang window`.
//!
//! The plugin's `gui` extension is asked for a window that goes in a window of Tang's own (`window::open`: the content view is what `set_parent` gives it), the way a host does it: `create` for the system's own API, `set_scale`,
//! `get_size`, `can_resize`, `set_parent`, `show`; and at the end `hide` and `destroy`. The window's events are turned by whoever has a window open (`window::pump`), and `poll` tells the plugin the size that a person drags the
//! window's edge to and answers what the plugin asks of the host (`request_resize`, `closed`: `Asked`, which the host's shared handler writes into, from whichever thread the plugin asks on).
//!
//! **The plugin is on the audio thread and its window is the main thread's.** A session moves a `ClapPlugin` to the audio thread, and the instance that clack gives a host for the `gui` calls (`PluginMainThreadHandle`) is
//! borrowed from it. The CLAP rules allow what is wanted (the functions of the `gui` extension are the main thread's and run while the audio thread processes), so the editor keeps the handle that was made when the plugin was
//! loaded, with its lifetime made `'static`, in the registry of the windows (`plugin::editors`) that VST3 plugins have: a table of the main thread, from which the plugin takes its editor **first** when it is let go of
//! (`ClapPlugin::drop`), which closes the window before the plugin is destroyed. A plugin that was let go of on another thread is found by its token (`alive`), and the open editor then calls nothing in it.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window as ClapWindow};
use clack_host::prelude::{PluginBundle, PluginMainThreadHandle};

use super::{Editor, OpenEditor};
use crate::window::{self, Window};

/// What a plugin asks of the host about its window, written down by whichever thread asks (the host's shared handler: `HostGuiImpl`) and answered by the loop that turns the window's events (`OpenClapEditor::poll`).
#[derive(Default)]
pub struct Asked {
    /// The size that the plugin would like its window to be.
    resize: Mutex<Option<(u32, u32)>>,
    /// The plugin has closed its window, or lost its connection to it.
    closed: AtomicBool,
    /// The plugin has asked the host to call its `on_main_thread` (`request_callback`, step 115: `clap_callbacks`).
    callback: AtomicBool,
}

impl Asked {
    pub fn request_resize(&self, width: u32, height: u32) {
        *self.resize.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((width, height));
    }

    pub fn closed(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// The plugin asks for a call of its `on_main_thread`: asks that come together are one call (what the CLAP rules say a host may do).
    pub fn request_callback(&self) {
        self.callback.store(true, Ordering::Release);
    }

    /// Whether the plugin has asked for a call since the last time this was asked, which it is by whoever makes the call.
    pub(super) fn take_callback(&self) -> bool {
        self.callback.swap(false, Ordering::AcqRel)
    }

    pub(super) fn take_resize(&self) -> Option<(u32, u32)> {
        self.resize.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
    }

    pub(super) fn take_closed(&self) -> bool {
        self.closed.swap(false, Ordering::AcqRel)
    }
}

/// The calls into a plugin's `gui` extension that the editor makes. The plugin's own are `RealGui`; a test's write them down.
pub trait GuiCalls {
    /// Whether the plugin can put its window in a window of the host's, for this system's own API.
    fn embedded_supported(&self) -> bool;
    fn create(&self) -> Result<(), String>;
    fn set_scale(&self, scale: f64);
    /// The size that the plugin's window is, in points, if it says.
    fn size(&self) -> Option<(u32, u32)>;
    fn can_resize(&self) -> bool;
    /// The closest size that the plugin's window can be to this one (the same, if the plugin does not say).
    fn adjust_size(&self, size: (u32, u32)) -> (u32, u32);
    fn set_size(&self, size: (u32, u32)) -> Result<(), String>;
    /// Put the plugin's window in this one: the content view of a window of ours.
    fn set_parent(&self, parent: *mut c_void) -> Result<(), String>;
    fn show(&self) -> Result<(), String>;
    fn hide(&self);
    fn destroy(&self);
}

/// The window of this system's own kind that the plugin is given.
fn parent_window(parent: *mut c_void) -> ClapWindow<'static> {
    #[cfg(windows)]
    return ClapWindow::from_win32_hwnd(parent);
    #[cfg(not(windows))]
    return ClapWindow::from_cocoa_nsview(parent);
}

/// What the plugin is asked for: a window that goes in the host's, for the API that this system's windows are.
fn configuration() -> Option<GuiConfiguration<'static>> {
    GuiApiType::default_for_current_platform().map(|api_type| GuiConfiguration { api_type, is_floating: false })
}

/// A plugin's `gui` extension and the handle that it is called through.
pub struct RealGui {
    /// The plugin's main-thread handle, made `'static` when the plugin was loaded: good for as long as the plugin is, which the registry holds the editor to (see the module's own words).
    handle: RefCell<PluginMainThreadHandle<'static>>,
    gui: PluginGui,
}

impl RealGui {
    /// # Safety
    ///
    /// `handle` must be the handle of an instance that is not destroyed while this is used: the editor is let go of, with its window closed, before the plugin is (`ClapPlugin::drop`), and calls nothing in a plugin whose
    /// token is gone. Every call is the main thread's.
    pub unsafe fn new(handle: PluginMainThreadHandle<'_>, gui: PluginGui) -> RealGui {
        // SAFETY: only the lifetime is made another; the caller keeps the instance for as long as this is used.
        let handle = unsafe { std::mem::transmute::<PluginMainThreadHandle<'_>, PluginMainThreadHandle<'static>>(handle) };
        RealGui { handle: RefCell::new(handle), gui }
    }
}

impl GuiCalls for RealGui {
    fn embedded_supported(&self) -> bool {
        configuration().is_some_and(|configuration| self.gui.is_api_supported(&mut self.handle.borrow_mut(), configuration))
    }

    fn create(&self) -> Result<(), String> {
        let configuration = configuration().ok_or("this system has no window API that CLAP knows")?;
        self.gui.create(&mut self.handle.borrow_mut(), configuration).map_err(|why| why.to_string())
    }

    fn set_scale(&self, scale: f64) {
        // (A plugin that works out its own scale says no, and that is as good.)
        let _ = self.gui.set_scale(&mut self.handle.borrow_mut(), scale);
    }

    fn size(&self) -> Option<(u32, u32)> {
        self.gui.get_size(&mut self.handle.borrow_mut()).map(|size| (size.width, size.height))
    }

    fn can_resize(&self) -> bool {
        self.gui.can_resize(&mut self.handle.borrow_mut())
    }

    fn adjust_size(&self, (width, height): (u32, u32)) -> (u32, u32) {
        self.gui.adjust_size(&mut self.handle.borrow_mut(), GuiSize { width, height }).map_or((width, height), |size| (size.width, size.height))
    }

    fn set_size(&self, (width, height): (u32, u32)) -> Result<(), String> {
        self.gui.set_size(&mut self.handle.borrow_mut(), GuiSize { width, height }).map_err(|why| why.to_string())
    }

    fn set_parent(&self, parent: *mut c_void) -> Result<(), String> {
        // SAFETY: the parent is the content view of a window that `OpenClapEditor` keeps until after `destroy`, on the main thread.
        unsafe { self.gui.set_parent(&mut self.handle.borrow_mut(), parent_window(parent)) }.map_err(|why| why.to_string())
    }

    fn show(&self) -> Result<(), String> {
        self.gui.show(&mut self.handle.borrow_mut()).map_err(|why| why.to_string())
    }

    fn hide(&self) {
        let _ = self.gui.hide(&mut self.handle.borrow_mut());
    }

    fn destroy(&self) {
        self.gui.destroy(&mut self.handle.borrow_mut());
    }
}

/// A CLAP plugin's window, as the registry of the windows keeps it for as long as the plugin is.
pub struct ClapEditor {
    gui: Rc<dyn GuiCalls>,
    name: String,
    asked: Arc<Asked>,
    /// The plugin's token: gone when the plugin is.
    alive: Weak<()>,
    /// Keeps the library loaded for as long as a window of the plugin may be (the registry forgets the editor when the plugin goes, or, if it went on another thread, when it finds out that it did).
    _bundle: Option<PluginBundle>,
}

impl ClapEditor {
    pub fn new(gui: Rc<dyn GuiCalls>, name: &str, asked: Arc<Asked>, alive: &Arc<()>, bundle: Option<PluginBundle>) -> ClapEditor {
        ClapEditor { gui, name: name.to_string(), asked, alive: Arc::downgrade(alive), _bundle: bundle }
    }

    /// Open the window with `opener` for the window that the plugin is put in (a test gives one that is on no screen).
    pub fn open_in(&self, title: &str, focus: bool, opener: window::Opener) -> Result<Box<dyn OpenEditor>, String> {
        let name = &self.name;
        if !self.gui.embedded_supported() {
            return Err(format!("{name} has no window that Tang can put in one of its own (it has none, or makes a floating one itself, which Tang does not show yet)"));
        }
        self.gui.create().map_err(|why| format!("{name}: the window could not be made: {why}"))?;
        // (macOS counts points and has scaled them already, as a host there says: the scale is 1.)
        self.gui.set_scale(1.0);
        let (width, height) = self.gui.size().unwrap_or((640, 480));
        let resizable = self.gui.can_resize();
        let side = |length: u32, fallback: i32| i32::try_from(length).ok().filter(|&length| length > 0).unwrap_or(fallback);
        let (width, height) = (side(width, 640), side(height, 480));
        let host_window = match opener(&window::Spec { title, width, height, resizable, focus }) {
            Ok(window) => window,
            Err(why) => {
                self.gui.destroy();
                return Err(why);
            }
        };
        // Whatever the plugin asked or said before it had a window is not for this one.
        self.asked.take_resize();
        self.asked.take_closed();
        if let Err(why) = self.gui.set_parent(host_window.parent()) {
            self.gui.destroy();
            host_window.close();
            return Err(format!("{name}: the window could not be put in Tang's: {why}"));
        }
        if let Err(why) = self.gui.show() {
            self.gui.hide();
            self.gui.destroy();
            host_window.close();
            return Err(format!("{name}: the window could not be shown: {why}"));
        }
        let told = host_window.size();
        Ok(Box::new(OpenClapEditor { gui: self.gui.clone(), window: host_window, asked: self.asked.clone(), alive: self.alive.clone(), told, open: true }))
    }
}

impl Editor for ClapEditor {
    fn open(&self, title: &str, focus: bool) -> Result<Box<dyn OpenEditor>, String> {
        self.open_in(title, focus, &window::open)
    }
}

/// A plugin's window while it is open: dropping it closes it, the plugin's window first.
struct OpenClapEditor {
    gui: Rc<dyn GuiCalls>,
    window: Rc<dyn Window>,
    asked: Arc<Asked>,
    alive: Weak<()>,
    /// The size that the plugin has been told its window is, in points, so that it is told again only of a change.
    told: (i32, i32),
    open: bool,
}

impl OpenClapEditor {
    /// Make the window this size and tell the plugin: the closest that it can be to the size that is asked for.
    fn make(&mut self, width: i32, height: i32) -> Result<(), String> {
        let wanted = (u32::try_from(width).unwrap_or(0).max(1), u32::try_from(height).unwrap_or(0).max(1));
        let size = self.gui.adjust_size(wanted);
        let (made_width, made_height) = (i32::try_from(size.0).unwrap_or(width), i32::try_from(size.1).unwrap_or(height));
        if self.window.size() != (made_width, made_height) {
            self.window.set_size(made_width, made_height);
        }
        self.told = self.window.size();
        self.gui.set_size(size)
    }
}

impl OpenEditor for OpenClapEditor {
    fn poll(&mut self) -> bool {
        if !self.open {
            return false;
        }
        if !self.window.is_open() || self.asked.take_closed() {
            self.close();
            return false;
        }
        // What the plugin asked of the window's size.
        if let Some((width, height)) = self.asked.take_resize() {
            self.window.set_size(i32::try_from(width).unwrap_or(self.told.0), i32::try_from(height).unwrap_or(self.told.1));
        }
        // What a person made of it by dragging its edge: the plugin is told once for each size, the closest that it can be.
        let now = self.window.size();
        if now != self.told
            && let Err(why) = self.make(now.0, now.1)
        {
            log::warn!("The plugin did not take a window of {} x {}: {why}", now.0, now.1);
        }
        true
    }

    fn raise(&mut self, focus: bool) {
        self.window.raise(focus);
    }

    fn close(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        // The plugin's window first, while the plugin is there: a plugin that has been let go of, on another thread, is not called.
        if self.alive.upgrade().is_some() {
            self.gui.hide();
            self.gui.destroy();
        } else {
            log::error!("A CLAP plugin's window was open when the plugin was let go of: it is closed without a word to the plugin");
        }
        self.window.close();
    }

    fn size(&self) -> (i32, i32) {
        self.window.size()
    }

    fn snapshot_png(&self) -> Option<Vec<u8>> {
        self.window.snapshot_png()
    }

    fn describe(&self) -> String {
        self.window.describe()
    }

    fn resize(&mut self, width: i32, height: i32) -> Result<(), String> {
        if !self.open {
            return Err("the window is closed".to_string());
        }
        self.make(width, height)
    }
}

impl Drop for OpenClapEditor {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    type Log = Rc<RefCell<Vec<String>>>;

    /// A plugin's `gui` extension that writes down what it is asked, and answers as a test says.
    struct FakeGui {
        log: Log,
        embedded: Cell<bool>,
        create_says: RefCell<Result<(), String>>,
        size: Cell<Option<(u32, u32)>>,
        resizable: Cell<bool>,
        /// `adjust_size` makes a size a multiple of this (1: as it is).
        step: Cell<u32>,
        /// What `adjust_size` says whatever it is asked, if a test says so.
        adjusted: Cell<Option<(u32, u32)>>,
        set_parent_says: RefCell<Result<(), String>>,
        show_says: RefCell<Result<(), String>>,
        set_size_says: RefCell<Result<(), String>>,
    }

    impl FakeGui {
        fn new(log: &Log) -> Rc<FakeGui> {
            Rc::new(FakeGui {
                log: log.clone(),
                embedded: Cell::new(true),
                create_says: RefCell::new(Ok(())),
                size: Cell::new(Some((1100, 720))),
                resizable: Cell::new(true),
                step: Cell::new(1),
                adjusted: Cell::new(None),
                set_parent_says: RefCell::new(Ok(())),
                show_says: RefCell::new(Ok(())),
                set_size_says: RefCell::new(Ok(())),
            })
        }

        fn say(&self, line: String) {
            self.log.borrow_mut().push(line);
        }
    }

    impl GuiCalls for FakeGui {
        fn embedded_supported(&self) -> bool {
            self.say("gui.supported".to_string());
            self.embedded.get()
        }

        fn create(&self) -> Result<(), String> {
            self.say("gui.create".to_string());
            self.create_says.borrow().clone()
        }

        fn set_scale(&self, scale: f64) {
            self.say(format!("gui.set_scale({scale})"));
        }

        fn size(&self) -> Option<(u32, u32)> {
            self.say("gui.size".to_string());
            self.size.get()
        }

        fn can_resize(&self) -> bool {
            self.say("gui.can_resize".to_string());
            self.resizable.get()
        }

        fn adjust_size(&self, (width, height): (u32, u32)) -> (u32, u32) {
            self.say(format!("gui.adjust_size({width}, {height})"));
            if let Some(size) = self.adjusted.get() {
                return size;
            }
            let step = self.step.get();
            (width / step * step, height / step * step)
        }

        fn set_size(&self, (width, height): (u32, u32)) -> Result<(), String> {
            self.say(format!("gui.set_size({width}, {height})"));
            self.set_size_says.borrow().clone()
        }

        fn set_parent(&self, parent: *mut c_void) -> Result<(), String> {
            self.say(format!("gui.set_parent({})", if parent.is_null() { "null" } else { "the window's view" }));
            self.set_parent_says.borrow().clone()
        }

        fn show(&self) -> Result<(), String> {
            self.say("gui.show".to_string());
            self.show_says.borrow().clone()
        }

        fn hide(&self) {
            self.say("gui.hide".to_string());
        }

        fn destroy(&self) {
            self.say("gui.destroy".to_string());
        }
    }

    /// A window that is on no screen.
    struct FakeWindow {
        log: Log,
        size: Cell<(i32, i32)>,
        open: Cell<bool>,
        content: u8,
    }

    impl Window for FakeWindow {
        fn parent(&self) -> *mut c_void {
            std::ptr::from_ref(&self.content).cast_mut().cast()
        }

        fn size(&self) -> (i32, i32) {
            self.size.get()
        }

        fn set_size(&self, width: i32, height: i32) {
            self.log.borrow_mut().push(format!("window.set_size({width}, {height})"));
            self.size.set((width, height));
        }

        fn is_open(&self) -> bool {
            self.open.get()
        }

        fn raise(&self, focus: bool) {
            self.log.borrow_mut().push(format!("window.raise({focus})"));
        }

        fn close(&self) {
            self.log.borrow_mut().push("window.close".to_string());
            self.open.set(false);
        }

        fn snapshot_png(&self) -> Option<Vec<u8>> {
            Some(vec![1, 2, 3])
        }

        fn describe(&self) -> String {
            "the views".to_string()
        }
    }

    /// An editor, a log that its plugin and its window write in, the plugin's token and what the plugin asks.
    struct Rig {
        editor: ClapEditor,
        gui: Rc<FakeGui>,
        log: Log,
        asked: Arc<Asked>,
        /// The plugin's token, which the plugin takes with it when it is let go of.
        alive: RefCell<Option<Arc<()>>>,
        window: RefCell<Option<Rc<FakeWindow>>>,
    }

    impl Rig {
        fn new() -> Rig {
            let log = Log::default();
            let gui = FakeGui::new(&log);
            let asked = Arc::new(Asked::default());
            let alive = Arc::new(());
            let editor = ClapEditor::new(gui.clone(), "Tang", asked.clone(), &alive, None);
            Rig { editor, gui, log, asked, alive: RefCell::new(Some(alive)), window: RefCell::new(None) }
        }

        /// The plugin is let go of (on another thread, with its window open: the editor has the token's weak half only).
        fn plugin_goes(&self) {
            self.alive.borrow_mut().take();
        }

        /// Open the window that the plugin is put in, which is on no screen, and say what it was opened as.
        fn open(&self) -> Result<Box<dyn OpenEditor>, String> {
            self.editor.open_in("Tang", true, &|spec| {
                self.log.borrow_mut().push(format!("window.open({} {}x{}{}{})", spec.title, spec.width, spec.height, if spec.resizable { " resizable" } else { "" }, if spec.focus { " focus" } else { "" }));
                let window = Rc::new(FakeWindow { log: self.log.clone(), size: Cell::new((spec.width, spec.height)), open: Cell::new(true), content: 0 });
                *self.window.borrow_mut() = Some(window.clone());
                Ok(window)
            })
        }

        fn said(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.borrow_mut())
        }

        fn window(&self) -> Rc<FakeWindow> {
            self.window.borrow().clone().expect("a window was opened")
        }
    }

    #[test]
    fn the_plugin_is_asked_in_the_order_that_a_host_asks_it_and_the_window_is_the_size_that_it_says() {
        let rig = Rig::new();
        let _open = rig.open().unwrap();
        assert_eq!(
            rig.said(),
            ["gui.supported", "gui.create", "gui.set_scale(1)", "gui.size", "gui.can_resize", "window.open(Tang 1100x720 resizable focus)", "gui.set_parent(the window's view)", "gui.show"],
            "the window comes after the plugin has said its size, and the plugin is put in it, and shown, after that"
        );
    }

    #[test]
    fn a_plugin_that_cannot_be_resized_has_a_window_that_cannot_be_and_one_that_says_no_size_has_a_default() {
        let rig = Rig::new();
        rig.gui.resizable.set(false);
        rig.gui.size.set(None);
        let _open = rig.open().unwrap();
        assert!(rig.said().contains(&"window.open(Tang 640x480 focus)".to_string()), "the default size, and not resizable");
        let rig = Rig::new();
        rig.gui.size.set(Some((0, u32::MAX)));
        let _open = rig.open().unwrap();
        assert!(rig.said().contains(&"window.open(Tang 640x480 resizable focus)".to_string()), "a side that is none, and one that is more than a window can be, are the default sides");
    }

    #[test]
    fn a_plugin_that_has_no_window_for_the_host_is_not_asked_to_make_one() {
        let rig = Rig::new();
        rig.gui.embedded.set(false);
        let why = rig.open().err().expect("refused");
        assert!(why.contains("Tang has no window that Tang can put in one of its own"), "{why}");
        assert_eq!(rig.said(), ["gui.supported"], "nothing was made, and no window was opened");
    }

    #[test]
    fn a_window_that_the_plugin_cannot_make_says_why_and_opens_none() {
        let rig = Rig::new();
        *rig.gui.create_says.borrow_mut() = Err("no way".to_string());
        let why = rig.open().err().expect("refused");
        assert!(why.contains("the window could not be made") && why.contains("no way"), "{why}");
        assert!(!rig.said().iter().any(|line| line.starts_with("window.")), "no window was opened");
    }

    #[test]
    fn a_window_of_ours_that_cannot_be_opened_lets_the_plugins_window_go_and_says_why() {
        let rig = Rig::new();
        let why = rig.editor.open_in("Tang", true, &|_| Err("this session has no screen".to_string())).err().expect("refused");
        assert_eq!(why, "this session has no screen");
        assert_eq!(rig.said().last().map(String::as_str), Some("gui.destroy"), "the plugin made a window and is told that it is not wanted");
    }

    #[test]
    fn a_plugin_that_will_not_be_put_in_the_window_is_let_go_of_and_the_window_is_taken_away() {
        let rig = Rig::new();
        *rig.gui.set_parent_says.borrow_mut() = Err("not a window of mine".to_string());
        let why = rig.open().err().expect("refused");
        assert!(why.contains("could not be put in Tang's") && why.contains("not a window of mine"), "{why}");
        let said = rig.said();
        assert_eq!(&said[said.len() - 3..], ["gui.set_parent(the window's view)", "gui.destroy", "window.close"], "the plugin first, then the window");
        assert!(!said.contains(&"gui.show".to_string()), "and it was not shown");
    }

    #[test]
    fn a_plugin_that_will_not_be_shown_is_hidden_and_let_go_of_and_the_window_is_taken_away() {
        let rig = Rig::new();
        *rig.gui.show_says.borrow_mut() = Err("not now".to_string());
        let why = rig.open().err().expect("refused");
        assert!(why.contains("could not be shown") && why.contains("not now"), "{why}");
        let said = rig.said();
        assert_eq!(&said[said.len() - 4..], ["gui.show", "gui.hide", "gui.destroy", "window.close"]);
    }

    #[test]
    fn the_size_that_a_person_drags_the_edge_to_is_told_to_the_plugin_once_the_closest_that_it_can_be() {
        let rig = Rig::new();
        rig.gui.step.set(10);
        let mut open = rig.open().unwrap();
        rig.said();
        assert!(open.poll(), "nothing changed: the window is open and nobody is told");
        assert!(rig.said().is_empty());
        // A person drags the edge: the window is 905 x 603 now, and the plugin says that it can be 900 x 600.
        rig.window().size.set((905, 603));
        assert!(open.poll());
        assert_eq!(rig.said(), ["gui.adjust_size(905, 603)", "window.set_size(900, 600)", "gui.set_size(900, 600)"], "the closest size, which the window is made, and the plugin told it");
        assert!(open.poll());
        assert!(rig.said().is_empty(), "told once for each size");
        // A size that the plugin takes as it is leaves the window alone.
        rig.window().size.set((1000, 700));
        assert!(open.poll());
        assert_eq!(rig.said(), ["gui.adjust_size(1000, 700)", "gui.set_size(1000, 700)"]);
    }

    #[test]
    fn a_plugin_that_does_not_take_the_size_is_a_warning_and_the_window_goes_on() {
        let rig = Rig::new();
        *rig.gui.set_size_says.borrow_mut() = Err("no".to_string());
        let mut open = rig.open().unwrap();
        rig.said();
        rig.window().size.set((905, 603));
        assert!(open.poll(), "the window is still open");
        assert!(open.poll());
        assert_eq!(rig.said(), ["gui.adjust_size(905, 603)", "gui.set_size(905, 603)"], "and the plugin is not asked again for the same size");
    }

    #[test]
    fn what_the_plugin_asks_of_the_size_is_made_of_the_window_and_then_told_back() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        rig.asked.request_resize(800, 500);
        assert!(open.poll());
        assert_eq!(rig.said(), ["window.set_size(800, 500)", "gui.adjust_size(800, 500)", "gui.set_size(800, 500)"], "the window is made what it asks for, and it is told the size that the window is");
        assert!(open.poll());
        assert!(rig.said().is_empty(), "the request was taken");
        assert_eq!(open.size(), (800, 500));
    }

    #[test]
    fn a_size_that_no_window_can_be_is_the_size_that_it_was() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        rig.asked.request_resize(u32::MAX, u32::MAX);
        assert!(open.poll());
        assert_eq!(rig.said(), ["window.set_size(1100, 720)"], "the window stays as it was, and the plugin is not told of a change that there was none");
    }

    #[test]
    fn a_window_of_no_size_is_asked_of_the_plugin_as_the_least_that_there_is() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        open.resize(-5, 0).unwrap();
        assert_eq!(rig.said(), ["gui.adjust_size(1, 1)", "window.set_size(1, 1)", "gui.set_size(1, 1)"]);
    }

    #[test]
    fn a_plugin_that_would_have_a_window_bigger_than_any_is_given_the_size_that_was_asked() {
        let rig = Rig::new();
        rig.gui.adjusted.set(Some((u32::MAX, u32::MAX)));
        let mut open = rig.open().unwrap();
        rig.said();
        open.resize(905, 603).unwrap();
        assert_eq!(rig.said(), ["gui.adjust_size(905, 603)", "window.set_size(905, 603)", "gui.set_size(4294967295, 4294967295)"], "the window is the size that was asked for, and the plugin is told what it said");
    }

    #[test]
    fn what_the_plugin_asked_or_said_before_it_had_a_window_is_not_for_this_one() {
        let rig = Rig::new();
        rig.asked.request_resize(10, 10);
        rig.asked.closed();
        let mut open = rig.open().unwrap();
        rig.said();
        assert!(open.poll(), "a plugin that closed a window of the one before has not closed this");
        assert!(rig.said().is_empty(), "and what it asked for the one before is not asked of this");
    }

    #[test]
    fn a_plugin_that_closes_its_window_ends_the_editor_and_is_let_go_of_before_the_window_is() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        rig.asked.closed();
        assert!(!open.poll());
        assert_eq!(rig.said(), ["gui.hide", "gui.destroy", "window.close"], "the plugin's window first");
        assert!(!open.poll());
        assert!(rig.said().is_empty(), "and nothing is done twice");
    }

    #[test]
    fn a_person_who_closes_the_window_ends_the_editor_and_the_plugin_is_let_go_of_first_all_the_same() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        rig.window().open.set(false);
        assert!(!open.poll());
        assert_eq!(rig.said(), ["gui.hide", "gui.destroy", "window.close"]);
    }

    #[test]
    fn a_window_is_closed_once_and_a_dropped_one_is_closed() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        open.close();
        open.close();
        assert_eq!(rig.said(), ["gui.hide", "gui.destroy", "window.close"], "once");
        let open = rig.open().unwrap();
        rig.said();
        drop(open);
        assert_eq!(rig.said(), ["gui.hide", "gui.destroy", "window.close"], "a window that is dropped is closed, the plugin's first");
    }

    #[test]
    fn a_plugin_that_has_been_let_go_of_is_not_called_when_its_window_is_closed() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        rig.plugin_goes();
        assert!(open.poll(), "a window that is open stays so, and the plugin is not asked about it");
        assert!(rig.said().is_empty());
        open.close();
        assert_eq!(rig.said(), ["window.close"], "no word to a plugin that is gone: only the window is taken away");
        let rig = Rig::new();
        let open = rig.open().unwrap();
        rig.said();
        rig.plugin_goes();
        drop(open);
        assert_eq!(rig.said(), ["window.close"], "and the same for a window that is dropped");
    }

    #[test]
    fn the_window_is_made_the_size_that_is_asked_for_from_outside_and_the_plugin_is_told() {
        let rig = Rig::new();
        rig.gui.step.set(10);
        let mut open = rig.open().unwrap();
        rig.said();
        open.resize(905, 603).unwrap();
        assert_eq!(rig.said(), ["gui.adjust_size(905, 603)", "window.set_size(900, 600)", "gui.set_size(900, 600)"]);
        assert_eq!(open.size(), (900, 600));
        assert!(open.poll());
        assert!(rig.said().is_empty(), "and the next turn does not tell it again");
        *rig.gui.set_size_says.borrow_mut() = Err("not that".to_string());
        assert_eq!(open.resize(800, 500), Err("not that".to_string()), "the plugin's own words");
        open.close();
        assert_eq!(open.resize(800, 500), Err("the window is closed".to_string()));
    }

    #[test]
    fn a_window_is_raised_with_the_keyboard_or_without_and_shows_what_is_in_it() {
        let rig = Rig::new();
        let mut open = rig.open().unwrap();
        rig.said();
        open.raise(true);
        open.raise(false);
        assert_eq!(rig.said(), ["window.raise(true)", "window.raise(false)"]);
        assert_eq!(open.snapshot_png(), Some(vec![1, 2, 3]));
        assert_eq!(open.describe(), "the views");
    }

    #[test]
    fn what_a_plugin_asks_is_kept_until_it_is_taken_and_the_last_request_of_a_size_is_the_one() {
        let asked = Asked::default();
        assert_eq!((asked.take_resize(), asked.take_closed()), (None, false));
        asked.request_resize(1, 2);
        asked.request_resize(3, 4);
        asked.closed();
        assert_eq!(asked.take_resize(), Some((3, 4)), "the last request");
        assert_eq!(asked.take_resize(), None, "taken");
        assert!(asked.take_closed());
        assert!(!asked.take_closed(), "taken");
    }
}
