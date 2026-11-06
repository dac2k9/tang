//! The plugin's window (step 114): the page of the instance, in a web view that a window of the host's holds.
//!
//! A host that can put a plugin's face in a window of its own (Bitwig does) asks for an embedded window: `create`, `set_scale`, `get_size`, then `set_parent` with the window that it made (an `HWND` on Windows, an `NSView *` on
//! macOS), and `show`; at the end `hide` and `destroy`. All of it is the host's main thread. Tang's face is its page, which the controller of the instance serves on a port of its own, so the window is a web view that shows that
//! address: WebView2 on Windows and WKWebView on macOS, through `wry`. The other systems have no such view here and keep what the plugin had before it had this: the floating window that the plugin shows itself, which is the page
//! in the browser.
//!
//! What is in this file is the order of the calls, the sizes and what the view shows, written against two small traits (`Maker` and `View`) so that all of it is held by tests that have no window. `native` is the one part that is
//! not: the system's web view, which is tried with a real window (`tang window Tang.clap`, on macOS).

use std::ffi::c_void;

use clack_extensions::gui::{AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize};

/// The size that the window is made at the first time (the page is a canvas and wants room), before it is scaled: in points on macOS, where the system has scaled them already, and in what Windows calls 96 dpi pixels, which
/// `Gui::size` makes the pixels of the screen (CLAP counts those there, and tells the plugin how many there are to a pixel of 96 dpi: the scale).
pub const DEFAULT_SIZE: GuiSize = GuiSize { width: 1100, height: 720 };
/// The least that a window is made (scaled in the same way), and the most.
pub const LEAST: GuiSize = GuiSize { width: 480, height: 320 };
pub const MOST: GuiSize = GuiSize { width: 16_384, height: 16_384 };

/// A size with each side `unit` times as long: the pixels that a size of 96 dpi pixels is on a screen that has `unit` of them to one.
pub fn scaled(size: GuiSize, unit: f64) -> GuiSize {
    // (A side is a whole number of pixels, and not more than the most that a window is.)
    let side = |length: u32| ((f64::from(length) * unit).round() as u32).min(MOST.width);
    GuiSize { width: side(size.width), height: side(size.height) }
}

/// A size that a host asks for, as a size that the window can be, at this `unit` (see `scaled`).
pub fn clamp(size: GuiSize, unit: f64) -> GuiSize {
    let least = scaled(LEAST, unit);
    GuiSize { width: size.width.clamp(least.width, MOST.width), height: size.height.clamp(least.height, MOST.height) }
}

/// What the web view shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// Tang's page, at this address.
    Url(String),
    /// Tang has not started yet (a host starts a plugin when it switches it on, which may come after the person asks for the window): a few words say so, and the page comes when it does.
    Waiting,
}

impl Page {
    /// The page that an instance has: its address, if it is serving one.
    pub fn of(url: Option<String>) -> Page {
        url.map_or(Page::Waiting, Page::Url)
    }
}

/// What the view shows while Tang has not started.
pub const WAITING: &str = "<!doctype html><meta charset=utf-8><title>Tang</title><body style=\"margin:0;height:100vh;display:grid;place-items:center;background:#17171c;color:#c9c9d3;font:15px system-ui,sans-serif\"><p>Tang has not started yet: the host starts it when it switches the plugin on.</p></body>";

/// The host's window that the web view goes in: the `HWND` or the `NSView *` that `set_parent` was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parent(pub *mut c_void);

/// The web view that a window of the host's holds. Every call is the main thread's.
pub trait View {
    /// Make the view this size (in the host's units) and put it at the top left of its parent. `scale` is what the host said at `set_scale`.
    fn set_bounds(&self, size: GuiSize, scale: f64) -> Result<(), String>;
    fn set_visible(&self, visible: bool) -> Result<(), String>;
    /// Show another page.
    fn load(&self, page: &Page) -> Result<(), String>;
}

/// What makes the view, and knows what the machine can do.
pub trait Maker {
    /// Whether this machine can show a web view in a window of the host's: no, with the reason, where the system has none (a Windows that has no WebView2) or Tang has none for it (Linux).
    fn available(&self) -> Result<(), String>;
    /// Whether `parent` can hold a view now: a view that the host has not put in a window yet cannot, on macOS (it is tried again at `show`).
    fn ready(&self, parent: Parent) -> bool;
    /// Make the view in `parent`, not shown yet, at this size, with this page.
    fn make(&self, parent: Parent, size: GuiSize, scale: f64, page: &Page) -> Result<Box<dyn View>, String>;
}

/// What a test gives to make a plugin's views with.
pub type MakeMaker = fn() -> Box<dyn Maker>;

thread_local! {
    /// What makes the views of the plugins that are made on this thread, if a test has said so.
    static MAKER: std::cell::Cell<Option<MakeMaker>> = const { std::cell::Cell::new(None) };
}

/// **For tests**: the plugins that are made on this thread from now on make their views with `make` and not with the system's web view, so that a test that runs the plugin through a host in the same process (`tests/window.rs`)
/// can give it a window that is not on any screen, and see what the plugin does with it. It is per thread, and a test has a thread of its own.
pub fn use_maker_on_this_thread(make: MakeMaker) {
    MAKER.with(|maker| maker.set(Some(make)));
}

/// What a window that the host has made is: put in a window of the host's, or the plugin's own (the page in the browser).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Embedded,
    Floating,
}

/// What `show` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// The view is shown in the host's window.
    Embedded,
    /// The window is the plugin's own: the caller opens the page in the browser.
    Floating,
}

/// The state of the plugin's window.
pub struct Gui {
    maker: Box<dyn Maker>,
    /// Whether the host counts pixels of the screen (Windows), and tells the plugin the scale to scale its size by, or points (macOS), which are scaled already.
    physical: bool,
    /// What the host made (`create`), if it has and has not destroyed it.
    kind: Option<Kind>,
    /// The size that the host has made the window, if it has: until it does, the window is the default size, scaled.
    size: Option<GuiSize>,
    scale: f64,
    /// The host's window, kept so that a view that could not be made at `set_parent` is made at `show`.
    parent: Option<Parent>,
    view: Option<Box<dyn View>>,
    /// What the view was last told to show.
    page: Option<Page>,
}

impl Gui {
    pub fn new(maker: Box<dyn Maker>, physical: bool) -> Gui {
        Gui { maker, physical, kind: None, size: None, scale: 1.0, parent: None, view: None, page: None }
    }

    /// The window of this system's web view (`native`), where there is one: or the maker that a test of this thread has given (`use_maker_on_this_thread`).
    pub fn native() -> Gui {
        match MAKER.with(std::cell::Cell::get) {
            Some(make) => Gui::new(make(), cfg!(windows)),
            None => Gui::new(Box::new(native::Native), cfg!(windows)),
        }
    }

    /// A floating window is the page in the browser, which every system has; an embedded one is for the API that the system's windows are, and a machine that can show a web view.
    pub fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
        configuration.is_floating || (GuiApiType::default_for_current_platform() == Some(configuration.api_type) && self.maker.available().is_ok())
    }

    /// What Tang would rather be asked for: a window of the host's where the machine can show one, and the browser where it cannot.
    pub fn preferred(&self) -> Option<GuiConfiguration<'static>> {
        let api_type = GuiApiType::default_for_current_platform()?;
        Some(GuiConfiguration { api_type, is_floating: self.maker.available().is_err() })
    }

    /// The host makes the window. One that was made before is let go of first.
    pub fn create(&mut self, configuration: GuiConfiguration) -> Result<(), String> {
        if !self.is_api_supported(configuration) {
            return Err(match self.maker.available() {
                Err(why) if !configuration.is_floating => format!("an embedded window is not possible here: {why}"),
                _ => "that is not a window API that Tang has".to_string(),
            });
        }
        self.destroy();
        self.kind = Some(if configuration.is_floating { Kind::Floating } else { Kind::Embedded });
        Ok(())
    }

    /// The host lets go of the window: the view goes, and everything that was said about the window with it (the size is what it was made at the next time, and `scale` is the host's to say again).
    pub fn destroy(&mut self) {
        // (The view first: it is in the host's window, which the host takes away when this returns.)
        self.view = None;
        self.parent = None;
        self.page = None;
        self.kind = None;
        self.size = None;
    }

    /// The host's scale (1 is 96 dpi on Windows; on macOS the points are already scaled, the host says 1 or nothing, and it does not change a size).
    pub fn set_scale(&mut self, scale: f64) -> Result<(), String> {
        if !(scale.is_finite() && scale > 0.0) {
            return Err(format!("{scale} is not a scale"));
        }
        self.scale = scale;
        self.fit()
    }

    /// How many pixels of the host's there are to a unit of the sizes that Tang has in mind: the scale where the host counts pixels, and 1 where it counts points.
    fn unit(&self) -> f64 {
        if self.physical { self.scale } else { 1.0 }
    }

    /// The size of the window now: what the host made it, or, before it has, the default size at the host's scale.
    pub fn size(&self) -> GuiSize {
        self.size.unwrap_or_else(|| scaled(DEFAULT_SIZE, self.unit()))
    }

    /// The closest size that the window can be to the one that the host would like.
    pub fn adjust_size(&self, size: GuiSize) -> GuiSize {
        clamp(size, self.unit())
    }

    /// The host has made the window this size (or is about to): the view is made the same.
    pub fn set_size(&mut self, size: GuiSize) -> Result<(), String> {
        self.size = Some(self.adjust_size(size));
        self.fit()
    }

    /// What the host is told of how the window is resized: both ways, whatever the shape.
    pub fn hints() -> GuiResizeHints {
        GuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, strategy: AspectRatioStrategy::Disregard }
    }

    fn fit(&self) -> Result<(), String> {
        match &self.view {
            Some(view) => view.set_bounds(self.size(), self.scale),
            None => Ok(()),
        }
    }

    /// The host gives the window that the view goes in. A view that the machine cannot make says why; one that the parent cannot hold yet (it is not in a window) is made at `show`.
    pub fn set_parent(&mut self, parent: Parent, page: Page) -> Result<(), String> {
        if self.kind != Some(Kind::Embedded) {
            return Err("a window of the host's is for an embedded window, and none was made".to_string());
        }
        // (A parent that is given again: the first view goes with it.)
        self.view = None;
        self.page = None;
        self.parent = Some(parent);
        self.make(page)
    }

    fn make(&mut self, page: Page) -> Result<(), String> {
        let Some(parent) = self.parent else {
            return Err("there is no window of the host's to put the view in".to_string());
        };
        if !self.maker.ready(parent) {
            log::debug!("The host's window is not in a window yet: the view is made when it is shown");
            return Ok(());
        }
        let view = self.maker.make(parent, self.size(), self.scale, &page)?;
        self.view = Some(view);
        self.page = Some(page);
        Ok(())
    }

    /// The host shows the window. `page` is the page that the instance has now.
    pub fn show(&mut self, page: Page) -> Result<Shown, String> {
        match self.kind {
            None => Err("no window was made: `create` comes before `show`".to_string()),
            Some(Kind::Floating) => Ok(Shown::Floating),
            Some(Kind::Embedded) => {
                if self.view.is_none() {
                    self.make(page.clone())?;
                }
                if self.view.is_none() {
                    return Err("the host's window is not in a window, so there is nowhere to show the view".to_string());
                }
                self.refresh(page)?;
                if let Some(view) = &self.view {
                    view.set_visible(true)?;
                }
                Ok(Shown::Embedded)
            }
        }
    }

    /// The host hides the window (it may show it again).
    pub fn hide(&mut self) -> Result<(), String> {
        match &self.view {
            Some(view) => view.set_visible(false),
            None => Ok(()),
        }
    }

    /// The instance has another page than the view shows (it started, or was made again at another rate and has another address): the view shows it. Nothing is done for the page that it shows already.
    pub fn refresh(&mut self, page: Page) -> Result<(), String> {
        let Some(view) = &self.view else { return Ok(()) };
        if self.page.as_ref() == Some(&page) {
            return Ok(());
        }
        view.load(&page)?;
        self.page = Some(page);
        Ok(())
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod native {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
    use wry::{PageLoadEvent, Rect, WebViewBuilder, dpi};

    use super::{GuiSize, Maker, Page, Parent, View, WAITING};

    /// The system's web view, through wry.
    pub struct Native;

    /// The host's window as wry asks for one.
    struct Holder(RawWindowHandle);

    impl HasWindowHandle for Holder {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            // SAFETY: the handle is the host's window, which the host keeps until it has called `destroy`; the view is let go of there, before anything of the window is.
            Ok(unsafe { WindowHandle::borrow_raw(self.0) })
        }
    }

    #[cfg(windows)]
    fn raw_handle(parent: Parent) -> Result<RawWindowHandle, String> {
        let hwnd = std::num::NonZeroIsize::new(parent.0 as isize).ok_or("the host's window is null")?;
        Ok(RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(hwnd)))
    }

    #[cfg(target_os = "macos")]
    fn raw_handle(parent: Parent) -> Result<RawWindowHandle, String> {
        let view = std::ptr::NonNull::new(parent.0).ok_or("the host's view is null")?;
        Ok(RawWindowHandle::AppKit(raw_window_handle::AppKitWindowHandle::new(view)))
    }

    /// Whether the host's view is in a window already: wry asks the window of the view that it is put in, and a view that has none is a program that ends.
    #[cfg(target_os = "macos")]
    fn in_window(parent: Parent) -> bool {
        use objc2_app_kit::NSView;
        // SAFETY: the host gave this as an `NSView` of its own, which it keeps until `destroy`, on the main thread, where it is only asked a question.
        let view = unsafe { (parent.0 as *const NSView).as_ref() };
        view.is_some_and(|view| view.window().is_some())
    }

    #[cfg(windows)]
    fn in_window(parent: Parent) -> bool {
        !parent.0.is_null()
    }

    /// The place of the view in its parent: all of it, from the top left. CLAP counts points on macOS and pixels on Windows.
    fn rect(size: GuiSize) -> Rect {
        #[cfg(target_os = "macos")]
        return Rect { position: dpi::LogicalPosition::new(0, 0).into(), size: dpi::LogicalSize::new(size.width, size.height).into() };
        #[cfg(windows)]
        return Rect { position: dpi::PhysicalPosition::new(0, 0).into(), size: dpi::PhysicalSize::new(size.width, size.height).into() };
    }

    /// A call into wry that may panic (it asks a view for its window and takes the answer as there being one): the host's process is not the place for the panic to end.
    fn guarded<T>(what: &str, call: impl FnOnce() -> Result<T, wry::Error>) -> Result<T, String> {
        match catch_unwind(AssertUnwindSafe(call)) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(why)) => Err(format!("{what}: {why}")),
            Err(_) => Err(format!("{what}: the web view panicked")),
        }
    }

    struct WryView(wry::WebView);

    impl View for WryView {
        fn set_bounds(&self, size: GuiSize, _scale: f64) -> Result<(), String> {
            guarded("the web view could not be resized", || self.0.set_bounds(rect(size)))
        }

        fn set_visible(&self, visible: bool) -> Result<(), String> {
            guarded("the web view could not be shown or hidden", || self.0.set_visible(visible))
        }

        fn load(&self, page: &Page) -> Result<(), String> {
            guarded("the web view could not load the page", || match page {
                Page::Url(url) => self.0.load_url(url),
                Page::Waiting => self.0.load_html(WAITING),
            })
        }
    }

    /// WebView2 keeps its profile in a folder, which is by default beside the program that uses it: a host's own directory, which a person cannot write to. Tang's is in the person's own.
    #[cfg(windows)]
    fn context() -> wry::WebContext {
        let folder = std::env::var_os("LOCALAPPDATA").map(|base| std::path::PathBuf::from(base).join("Tang").join("WebView2"));
        if let Some(folder) = &folder
            && let Err(why) = std::fs::create_dir_all(folder)
        {
            log::warn!("The folder for the web view ({}) could not be made: {why}", folder.display());
        }
        wry::WebContext::new(folder)
    }

    impl Maker for Native {
        fn available(&self) -> Result<(), String> {
            match wry::webview_version() {
                Ok(version) => {
                    log::debug!("The system's web view is version {version}");
                    Ok(())
                }
                Err(why) => Err(format!("the system has no web view for Tang's window ({why})")),
            }
        }

        fn ready(&self, parent: Parent) -> bool {
            in_window(parent)
        }

        fn make(&self, parent: Parent, size: GuiSize, _scale: f64, page: &Page) -> Result<Box<dyn View>, String> {
            let holder = Holder(raw_handle(parent)?);
            #[cfg(windows)]
            let mut context = context();
            #[cfg(windows)]
            let builder = WebViewBuilder::new_with_web_context(&mut context);
            #[cfg(target_os = "macos")]
            let builder = WebViewBuilder::new();
            let builder = builder
                .with_bounds(rect(size))
                // (Not shown until the host shows the window, and not focused: a view that takes the focus makes the program that has it the active one, which is the host and not ours to say.)
                .with_visible(false)
                .with_focused(false)
                .with_accept_first_mouse(true)
                .with_on_page_load_handler(|event, url| {
                    if matches!(event, PageLoadEvent::Finished) {
                        log::info!("The window's page has loaded: {url}");
                    }
                });
            let builder = match page {
                Page::Url(url) => builder.with_url(url.clone()),
                Page::Waiting => builder.with_html(WAITING),
            };
            let view = guarded("the web view could not be made", || builder.build_as_child(&holder))?;
            log::info!("The window's web view is made, {} x {}", size.width, size.height);
            Ok(Box::new(WryView(view)))
        }
    }
}

/// What the systems that have no web view for Tang answer: none, and the browser.
#[cfg(not(any(windows, target_os = "macos")))]
mod native {
    use super::{GuiSize, Maker, Page, Parent, View};

    pub struct Native;

    impl Maker for Native {
        fn available(&self) -> Result<(), String> {
            Err("Tang has no web view for this system".to_string())
        }

        fn ready(&self, _parent: Parent) -> bool {
            false
        }

        fn make(&self, _parent: Parent, _size: GuiSize, _scale: f64, _page: &Page) -> Result<Box<dyn View>, String> {
            Err("Tang has no web view for this system".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;

    type Log = Rc<RefCell<Vec<String>>>;

    /// A view that writes down what it is told, and when it is let go of.
    struct FakeView(Log);

    impl View for FakeView {
        fn set_bounds(&self, size: GuiSize, scale: f64) -> Result<(), String> {
            self.0.borrow_mut().push(format!("bounds {}x{} @{scale}", size.width, size.height));
            Ok(())
        }

        fn set_visible(&self, visible: bool) -> Result<(), String> {
            self.0.borrow_mut().push(format!("visible {visible}"));
            Ok(())
        }

        fn load(&self, page: &Page) -> Result<(), String> {
            self.0.borrow_mut().push(format!("load {}", name(page)));
            Ok(())
        }
    }

    impl Drop for FakeView {
        fn drop(&mut self) {
            self.0.borrow_mut().push("view dropped".to_string());
        }
    }

    fn name(page: &Page) -> String {
        match page {
            Page::Url(url) => url.clone(),
            Page::Waiting => "waiting".to_string(),
        }
    }

    /// A machine that the test says what it can do.
    struct FakeMaker {
        log: Log,
        available: Rc<RefCell<Result<(), String>>>,
        ready: Rc<Cell<bool>>,
        fail: Rc<Cell<bool>>,
    }

    impl Maker for FakeMaker {
        fn available(&self) -> Result<(), String> {
            self.available.borrow().clone()
        }

        fn ready(&self, _parent: Parent) -> bool {
            self.ready.get()
        }

        fn make(&self, _parent: Parent, size: GuiSize, scale: f64, page: &Page) -> Result<Box<dyn View>, String> {
            if self.fail.get() {
                return Err("the web view would not be made".to_string());
            }
            self.log.borrow_mut().push(format!("make {}x{} @{scale} {}", size.width, size.height, name(page)));
            Ok(Box::new(FakeView(self.log.clone())))
        }
    }

    struct Rig {
        gui: Gui,
        log: Log,
        available: Rc<RefCell<Result<(), String>>>,
        ready: Rc<Cell<bool>>,
        fail: Rc<Cell<bool>>,
    }

    impl Rig {
        fn new() -> Rig {
            Rig::counting(false)
        }

        /// A host that counts pixels of the screen (`physical`: Windows) or points.
        fn counting(physical: bool) -> Rig {
            let log = Log::default();
            let (available, ready, fail) = (Rc::new(RefCell::new(Ok(()))), Rc::new(Cell::new(true)), Rc::new(Cell::new(false)));
            let maker = FakeMaker { log: log.clone(), available: available.clone(), ready: ready.clone(), fail: fail.clone() };
            Rig { gui: Gui::new(Box::new(maker), physical), log, available, ready, fail }
        }

        /// What happened since this was last asked.
        fn said(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.borrow_mut())
        }
    }

    fn native() -> GuiApiType<'static> {
        GuiApiType::default_for_current_platform().expect("this system has a window API")
    }

    fn other() -> GuiApiType<'static> {
        if native() == GuiApiType::X11 { GuiApiType::WIN32 } else { GuiApiType::X11 }
    }

    fn embedded() -> GuiConfiguration<'static> {
        GuiConfiguration { api_type: native(), is_floating: false }
    }

    fn floating() -> GuiConfiguration<'static> {
        GuiConfiguration { api_type: native(), is_floating: true }
    }

    const PARENT: Parent = Parent(8 as *mut c_void);

    fn url(address: &str) -> Page {
        Page::Url(address.to_string())
    }

    #[test]
    fn an_embedded_window_is_for_this_systems_own_api_and_a_machine_that_can_show_a_web_view() {
        let rig = Rig::new();
        assert!(rig.gui.is_api_supported(embedded()));
        assert!(!rig.gui.is_api_supported(GuiConfiguration { api_type: other(), is_floating: false }), "not another system's windows");
        assert!(rig.gui.is_api_supported(GuiConfiguration { api_type: other(), is_floating: true }), "a floating window is the browser, whichever API the host names");
        assert_eq!(rig.gui.preferred(), Some(GuiConfiguration { api_type: native(), is_floating: false }), "a window of the host's is what Tang would rather");
        *rig.available.borrow_mut() = Err("no web view".to_string());
        assert!(!rig.gui.is_api_supported(embedded()), "no web view on the machine, no embedded window");
        assert!(rig.gui.is_api_supported(floating()));
        assert_eq!(rig.gui.preferred(), Some(GuiConfiguration { api_type: native(), is_floating: true }), "and the browser is what is asked for instead");
    }

    #[test]
    fn a_window_is_made_given_a_parent_shown_hidden_and_let_go_of_in_the_order_that_a_host_does_it() {
        let mut rig = Rig::new();
        rig.gui.create(embedded()).unwrap();
        rig.gui.set_scale(2.0).unwrap();
        assert_eq!(rig.gui.size(), DEFAULT_SIZE);
        assert!(rig.said().is_empty(), "no view before there is a parent");
        rig.gui.set_parent(PARENT, Page::Waiting).unwrap();
        assert_eq!(rig.said(), ["make 1100x720 @2 waiting"], "the view has the size and the scale that the host said, and the page that there is");
        assert_eq!(rig.gui.show(url("http://127.0.0.1:1/")), Ok(Shown::Embedded));
        assert_eq!(rig.said(), ["load http://127.0.0.1:1/", "visible true"], "the page that the instance has now, and then the view is shown");
        rig.gui.hide().unwrap();
        assert_eq!(rig.said(), ["visible false"]);
        rig.gui.show(url("http://127.0.0.1:1/")).unwrap();
        assert_eq!(rig.said(), ["visible true"], "shown again, with the page that it has");
        rig.gui.destroy();
        assert_eq!(rig.said(), ["view dropped"]);
        assert!(rig.gui.show(Page::Waiting).unwrap_err().contains("create"), "there is no window after `destroy`: it is a window that has to be made again");
        assert_eq!(rig.gui.hide(), Ok(()), "and a hide of nothing is nothing");
    }

    #[test]
    fn the_size_is_the_hosts_between_the_least_and_the_most_and_the_view_follows_it() {
        let mut rig = Rig::new();
        rig.gui.create(embedded()).unwrap();
        rig.gui.set_parent(PARENT, Page::Waiting).unwrap();
        rig.said();
        rig.gui.set_size(GuiSize { width: 900, height: 600 }).unwrap();
        assert_eq!(rig.gui.size(), GuiSize { width: 900, height: 600 });
        assert_eq!(rig.said(), ["bounds 900x600 @1"]);
        rig.gui.set_size(GuiSize { width: 10, height: 20 }).unwrap();
        assert_eq!(rig.gui.size(), LEAST, "not smaller than the least");
        rig.gui.set_size(GuiSize { width: 100_000, height: 200_000 }).unwrap();
        assert_eq!(rig.gui.size(), MOST, "not larger than the most");
        rig.said();
        assert_eq!(rig.gui.adjust_size(GuiSize { width: 10, height: 700 }), GuiSize { width: LEAST.width, height: 700 });
        assert_eq!(rig.gui.adjust_size(GuiSize { width: 700, height: 100_000 }), GuiSize { width: 700, height: MOST.height });
        assert!(rig.said().is_empty(), "to adjust a size is to ask, and the view is not told");
        rig.gui.set_scale(1.5).unwrap();
        assert_eq!(rig.said(), [format!("bounds {}x{} @1.5", MOST.width, MOST.height)], "a scale that comes late is told to the view");
        rig.gui.destroy();
        assert_eq!(rig.gui.size(), DEFAULT_SIZE, "a window that is made again starts at the size that it first had");
    }

    #[test]
    fn a_size_is_not_told_to_a_view_that_is_not_there_and_a_scale_that_is_no_number_is_refused() {
        let mut rig = Rig::new();
        rig.gui.create(embedded()).unwrap();
        rig.gui.set_size(GuiSize { width: 800, height: 500 }).unwrap();
        assert_eq!(rig.gui.size(), GuiSize { width: 800, height: 500 }, "the size is kept for the view that comes");
        rig.gui.set_parent(PARENT, Page::Waiting).unwrap();
        assert_eq!(rig.said(), ["make 800x500 @1 waiting"]);
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(rig.gui.set_scale(scale).is_err(), "{scale}");
        }
        assert!(rig.said().is_empty(), "and the view was not told");
    }

    #[test]
    fn the_view_shows_the_page_that_the_instance_has_and_follows_it_when_it_changes() {
        let mut rig = Rig::new();
        rig.gui.create(embedded()).unwrap();
        rig.gui.set_parent(PARENT, Page::Waiting).unwrap();
        rig.gui.show(Page::Waiting).unwrap();
        assert_eq!(rig.said(), ["make 1100x720 @1 waiting", "visible true"], "the waiting page is shown while there is no other, and is not loaded twice");
        rig.gui.refresh(url("http://a/")).unwrap();
        assert_eq!(rig.said(), ["load http://a/"], "the instance started: the view has its page");
        rig.gui.refresh(url("http://a/")).unwrap();
        assert!(rig.said().is_empty(), "the page that is shown is not loaded again");
        rig.gui.refresh(url("http://b/")).unwrap();
        assert_eq!(rig.said(), ["load http://b/"], "the instance was made again at another address");
        rig.gui.refresh(Page::Waiting).unwrap();
        assert_eq!(rig.said(), ["load waiting"]);
    }

    #[test]
    fn a_refresh_when_there_is_no_view_is_nothing_and_the_page_comes_when_the_view_does() {
        let mut rig = Rig::new();
        rig.gui.refresh(url("http://a/")).unwrap();
        rig.gui.create(embedded()).unwrap();
        rig.gui.refresh(url("http://a/")).unwrap();
        assert!(rig.said().is_empty());
        rig.gui.set_parent(PARENT, url("http://a/")).unwrap();
        assert_eq!(rig.said(), ["make 1100x720 @1 http://a/"], "the view is made with the page that there is");
    }

    #[test]
    fn a_parent_that_is_not_in_a_window_gets_its_view_when_the_window_is_shown() {
        let mut rig = Rig::new();
        rig.ready.set(false);
        rig.gui.create(embedded()).unwrap();
        assert_eq!(rig.gui.set_parent(PARENT, Page::Waiting), Ok(()), "a parent that is not ready is not a failure");
        assert!(rig.said().is_empty(), "no view yet");
        assert!(rig.gui.show(Page::Waiting).is_err(), "a window that is shown when its parent is still not in a window cannot show anything");
        assert!(rig.said().is_empty());
        rig.ready.set(true);
        assert_eq!(rig.gui.show(url("http://a/")), Ok(Shown::Embedded));
        assert_eq!(rig.said(), ["make 1100x720 @1 http://a/", "visible true"], "the view is made with the page of the moment, and shown");
    }

    #[test]
    fn a_view_that_cannot_be_made_says_why_and_is_tried_again_at_show() {
        let mut rig = Rig::new();
        rig.fail.set(true);
        rig.gui.create(embedded()).unwrap();
        assert_eq!(rig.gui.set_parent(PARENT, Page::Waiting), Err("the web view would not be made".to_string()));
        assert_eq!(rig.gui.show(Page::Waiting), Err("the web view would not be made".to_string()), "and again at show");
        rig.fail.set(false);
        assert_eq!(rig.gui.show(Page::Waiting), Ok(Shown::Embedded));
        assert_eq!(rig.said(), ["make 1100x720 @1 waiting", "visible true"]);
    }

    #[test]
    fn a_floating_window_is_the_page_in_the_browser_and_has_no_parent() {
        let mut rig = Rig::new();
        rig.gui.create(floating()).unwrap();
        assert!(rig.gui.set_parent(PARENT, Page::Waiting).is_err(), "a floating window is not put in the host's");
        assert_eq!(rig.gui.show(url("http://a/")), Ok(Shown::Floating), "the caller opens the browser");
        assert_eq!(rig.gui.hide(), Ok(()));
        assert!(rig.said().is_empty(), "no web view was made");
        *rig.available.borrow_mut() = Err("no web view".to_string());
        assert_eq!(rig.gui.create(floating()), Ok(()), "and it is what a machine with no web view has");
    }

    #[test]
    fn calls_that_come_in_the_wrong_order_or_for_what_the_machine_cannot_do_are_refused_in_words() {
        let mut rig = Rig::new();
        assert!(rig.gui.show(Page::Waiting).unwrap_err().contains("create"), "show before create");
        assert!(rig.gui.set_parent(PARENT, Page::Waiting).is_err(), "a parent before a window");
        assert!(rig.gui.create(GuiConfiguration { api_type: other(), is_floating: false }).is_err(), "another system's windows");
        *rig.available.borrow_mut() = Err("WebView2 is not installed".to_string());
        let why = rig.gui.create(embedded()).unwrap_err();
        assert!(why.contains("WebView2 is not installed"), "the reason that the machine gives is the one that is said: {why}");
        assert!(rig.said().is_empty());
    }

    #[test]
    fn a_second_parent_and_a_second_window_let_go_of_the_view_that_there_was() {
        let mut rig = Rig::new();
        rig.gui.create(embedded()).unwrap();
        rig.gui.set_parent(PARENT, Page::Waiting).unwrap();
        rig.gui.set_parent(Parent(16 as *mut c_void), Page::Waiting).unwrap();
        assert_eq!(rig.said(), ["make 1100x720 @1 waiting", "view dropped", "make 1100x720 @1 waiting"], "the first view goes when the second parent comes");
        rig.gui.create(embedded()).unwrap();
        assert_eq!(rig.said(), ["view dropped"], "a window that is made again lets go of the one before");
        assert!(rig.gui.show(Page::Waiting).is_err(), "and has no parent yet");
    }

    #[test]
    fn a_host_that_counts_pixels_is_given_the_default_size_at_its_scale_and_one_that_counts_points_is_not() {
        let mut pixels = Rig::counting(true);
        pixels.gui.create(embedded()).unwrap();
        assert_eq!(pixels.gui.size(), DEFAULT_SIZE, "at a scale of 1 the default is as it is");
        pixels.gui.set_scale(1.5).unwrap();
        assert_eq!(pixels.gui.size(), GuiSize { width: 1650, height: 1080 }, "at 150% the window is as large in what the person sees");
        pixels.gui.set_scale(2.0).unwrap();
        assert_eq!(pixels.gui.size(), GuiSize { width: 2200, height: 1440 });
        pixels.gui.set_parent(PARENT, Page::Waiting).unwrap();
        assert_eq!(pixels.said(), ["make 2200x1440 @2 waiting"], "the view is made at that size");

        let mut points = Rig::counting(false);
        points.gui.create(embedded()).unwrap();
        points.gui.set_scale(2.0).unwrap();
        assert_eq!(points.gui.size(), DEFAULT_SIZE, "points are scaled by the system already");
    }

    #[test]
    fn a_size_that_the_host_has_made_the_window_stays_whatever_the_scale_becomes_and_the_least_is_scaled_too() {
        let mut rig = Rig::counting(true);
        rig.gui.create(embedded()).unwrap();
        rig.gui.set_scale(2.0).unwrap();
        rig.gui.set_size(GuiSize { width: 1500, height: 900 }).unwrap();
        rig.gui.set_scale(1.0).unwrap();
        assert_eq!(rig.gui.size(), GuiSize { width: 1500, height: 900 }, "what the host has made it is what it is");
        rig.gui.set_scale(2.0).unwrap();
        rig.gui.set_size(GuiSize { width: 10, height: 10 }).unwrap();
        assert_eq!(rig.gui.size(), GuiSize { width: LEAST.width * 2, height: LEAST.height * 2 }, "the least is the same to the eye: it is scaled");
        assert_eq!(rig.gui.adjust_size(GuiSize { width: 100_000, height: 100_000 }), MOST, "the most is the most that there is");
        assert_eq!(scaled(MOST, 2.0), MOST, "and a scaled side is not more than that");
        assert_eq!(scaled(GuiSize { width: 3, height: 5 }, 1.5), GuiSize { width: 5, height: 8 }, "a side is a whole number of pixels, rounded");
    }

    #[test]
    fn the_host_is_told_that_the_window_is_resized_both_ways_and_in_any_shape() {
        let hints = Gui::hints();
        assert!(hints.can_resize_horizontally && hints.can_resize_vertically);
        assert_eq!(hints.strategy, AspectRatioStrategy::Disregard);
    }

    #[test]
    fn a_page_is_the_address_that_there_is_or_the_waiting_one() {
        assert_eq!(Page::of(Some("http://127.0.0.1:5/".to_string())), url("http://127.0.0.1:5/"));
        assert_eq!(Page::of(None), Page::Waiting);
        assert!(WAITING.contains("has not started yet"), "the waiting page says what it waits for");
    }
}
