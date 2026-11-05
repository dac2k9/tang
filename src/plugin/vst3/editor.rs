//! A VST3 plugin's own window (step 112): the view protocol, `IPlugView` and `IPlugFrame`.
//!
//! The controller makes the plugin's view (`createView("editor")`) and the host puts it in a window. The order is the one the format says: ask whether the view is for this platform (`isPlatformTypeSupported`) and how big
//! it is (`getSize`, `canResize`) before there is a window at all, so that the window is made the size of the view; give the view the host's frame (`setFrame`) BEFORE it is attached, since a view may ask for another
//! size while it is attached; attach it to the window's content view (`attached`); and when the window goes, `removed` and then `setFrame(null)`, before the view is let go of.
//!
//! Everything here happens on the main thread, which is the thread of the window and of the plugin's controller. The frame that the plugin is given is a COM object that lives as long as the plugin keeps hold of it, so it
//! says no to a call from any other thread (a plugin that calls it from one is wrong, and the state that the frame shares with the editor is not made for two threads).

use std::any::Any;
use std::cell::Cell;
use std::ffi::CStr;
use std::rc::Rc;
use std::sync::Arc;
use std::thread::ThreadId;

use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait as _, ViewType};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait as _, ViewRect, kInvalidArgument, kResultFalse, kResultOk, kResultTrue, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use super::Vst3Module;
use crate::plugin::{Editor, OpenEditor};
use crate::window::{self, Opener, Spec, Window};

/// What the plugin is told that the window it is attached to is (the platform's own name for its parent).
#[cfg(target_os = "macos")]
use vst3::Steinberg::kPlatformTypeNSView as PLATFORM_TYPE;
#[cfg(target_os = "windows")]
use vst3::Steinberg::kPlatformTypeHWND as PLATFORM_TYPE;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use vst3::Steinberg::kPlatformTypeX11EmbedWindowID as PLATFORM_TYPE;

/// How big a window a plugin may ask for, in points a side. A plugin that asks for more is wrong, and a window that size is a window that cannot be got rid of.
const MAX_SIDE: i32 = 16_384;

/// The size of a window for a plugin that does not say how big its view is.
const DEFAULT_SIZE: (i32, i32) = (640, 480);

/// A plugin's window, ready to be opened: what makes its view, and what has to stay loaded for as long as a view of it may be.
pub(super) struct Vst3Editor {
    /// Makes the plugin's view (`IEditController::createView`), each time a window is opened: a view is made for a window, and let go of with it.
    make_view: Box<dyn Fn() -> Option<ComPtr<IPlugView>>>,
    /// Whatever has to be alive for as long as a view of the plugin may be: the library, for a plugin that is loaded. Last, so that it goes after what came out of the library.
    _keep: Box<dyn Any>,
}

impl Vst3Editor {
    /// The editor of a plugin that is loaded: its controller makes the views, and the module stays loaded for as long as the editor is held. The editor has to be let go of, and its window closed, before the plugin is
    /// terminated (`Vst3Plugin`'s `Drop` terminates the controller): a view of a terminated plugin is a view of nothing.
    pub(super) fn of(controller: ComPtr<IEditController>, module: Arc<Vst3Module>) -> Self {
        Self::new(
            Box::new(move || {
                // Safety: the controller is alive (see above) and is asked for its view on the main thread, as the format says.
                let view = unsafe { controller.createView(ViewType::kEditor) };
                // Safety: `createView` hands over one reference of the view it made, or none.
                unsafe { ComPtr::from_raw(view) }
            }),
            Box::new(module),
        )
    }

    fn new(make_view: Box<dyn Fn() -> Option<ComPtr<IPlugView>>>, keep: Box<dyn Any>) -> Self {
        Vst3Editor { make_view, _keep: keep }
    }

    /// `open`, with the window made by `open_window` (`window::open`, and in a test a window that is on no screen).
    fn open_in(&self, title: &str, focus: bool, open_window: Opener) -> Result<Box<dyn OpenEditor>, String> {
        let view = (self.make_view)().ok_or_else(|| "this plugin has no window of its own".to_string())?;

        // Safety: the view is the plugin's own and is asked its questions on the main thread, before it is attached to anything.
        if unsafe { view.isPlatformTypeSupported(PLATFORM_TYPE) } != kResultTrue {
            // Safety: as above.
            let wanted = unsafe { CStr::from_ptr(PLATFORM_TYPE) }.to_string_lossy();
            return Err(format!("this plugin's window is not one that can be shown here (it does not take a {wanted})"));
        }
        let mut rect = ViewRect { left: 0, top: 0, right: 0, bottom: 0 };
        // Safety: as above; the rectangle is ours.
        let said = unsafe { view.getSize(&mut rect) };
        let size = (rect.right.saturating_sub(rect.left), rect.bottom.saturating_sub(rect.top));
        let (width, height) = if said == kResultOk && (1..=MAX_SIDE).contains(&size.0) && (1..=MAX_SIDE).contains(&size.1) {
            size
        } else {
            log::warn!("VST3 plugin view says its size is {size:?} (result={said}): a window of {DEFAULT_SIZE:?} is used");
            DEFAULT_SIZE
        };
        // Safety: as above.
        let resizable = unsafe { view.canResize() } == kResultTrue;

        let window = open_window(&Spec { title, width, height, resizable, focus })?;
        let shared = Rc::new(Shared {
            window,
            view: view.as_ptr(),
            owner: std::thread::current().id(),
            size: Cell::new((width, height)),
            telling: Cell::new(false),
            open: Cell::new(true),
        });
        let frame = ComWrapper::new(TangPlugFrame { shared: shared.clone() });
        let Some(frame_ptr) = frame.to_com_ptr::<IPlugFrame>() else {
            shared.window.close();
            return Err("the host's frame for the plugin's window could not be made".to_string());
        };
        // Safety: the frame is alive for as long as the open editor is, which is as long as the plugin holds on to it (`close` takes it back).
        unsafe { view.setFrame(frame_ptr.as_ptr()) };
        // Safety: the parent is the window's content view, which is alive for as long as the window is.
        let attached = unsafe { view.attached(shared.window.parent(), PLATFORM_TYPE) };
        if attached != kResultOk {
            // (A view that was not attached has nothing to take off: `removed` is for one that was.)
            shared.open.set(false);
            // Safety: as above.
            unsafe { view.setFrame(std::ptr::null_mut()) };
            shared.window.close();
            return Err(format!("this plugin's window did not attach to the window that was made for it (result={attached})"));
        }
        Ok(Box::new(OpenVst3Editor { view, _frame: frame, shared, closed: false }))
    }
}

impl Editor for Vst3Editor {
    fn open(&self, title: &str, focus: bool) -> Result<Box<dyn OpenEditor>, String> {
        self.open_in(title, focus, &window::open)
    }
}

/// What the open editor and the frame that the plugin has hold of share.
struct Shared {
    window: Rc<dyn Window>,
    /// The plugin's view, good for as long as `open` is set: the open editor owns it, and the frame (which the plugin may hold for longer) tells it its size through this.
    view: *mut IPlugView,
    /// The thread that opened the window: the frame answers a call from it and no other.
    owner: ThreadId,
    /// The size that the plugin has been told its window is, in points, so that it is told again only of a change.
    size: Cell<(i32, i32)>,
    /// Set while the plugin is being told a size: it may ask for one in the call (a plugin that is told its window is another size and asks for the size it wants), and is not told again, so the two do not go round.
    telling: Cell<bool>,
    /// Whether the view is in the window: set from before the view is attached until it is taken off. A plugin that asks for a size outside of that is refused, since the view may be gone.
    open: Cell<bool>,
}

impl Shared {
    /// Tell the plugin's view that what it is in is this size (`onSize`), which the format says to do after every change that is not the plugin's own doing, and after the ones that are: `resizeView` is a request.
    fn tell(&self, (width, height): (i32, i32)) {
        if self.telling.replace(true) {
            return;
        }
        let mut rect = ViewRect { left: 0, top: 0, right: width, bottom: height };
        // Safety: the view is the plugin's own, held by the open editor from before it was attached until after it is removed, and `open` is set in between.
        if let Some(view) = unsafe { ComRef::from_raw(self.view) } {
            // Safety: as above; the rectangle is ours.
            unsafe { view.onSize(&mut rect) };
        }
        self.telling.set(false);
    }

    /// The plugin asks for a window of another size (`IPlugFrame::resizeView`): it gets it if it is a size that a window can be, and is told that it has it.
    fn resize(&self, new_size: *mut ViewRect) -> tresult {
        if std::thread::current().id() != self.owner {
            log::warn!("VST3 plugin asked for a new window size from a thread that is not the main one: refused");
            return kResultFalse;
        }
        if !self.open.get() {
            return kResultFalse;
        }
        // Safety: a plugin gives a pointer to a rectangle of its own, or none.
        let Some(rect) = (unsafe { new_size.as_ref() }) else {
            return kInvalidArgument;
        };
        let (width, height) = (rect.right.saturating_sub(rect.left), rect.bottom.saturating_sub(rect.top));
        if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
            return kInvalidArgument;
        }
        self.window.set_size(width, height);
        self.size.set((width, height));
        self.tell((width, height));
        kResultTrue
    }
}

/// The host's end of the conversation about the window's size, which the plugin is given before it is attached.
struct TangPlugFrame {
    shared: Rc<Shared>,
}

impl Class for TangPlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for TangPlugFrame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        // (The view that the plugin names is the one it was given a frame for: the editor tells its own.)
        self.shared.resize(new_size)
    }
}

/// A plugin's view in a window, attached.
struct OpenVst3Editor {
    // (The view goes first, then the frame that it was given, and what they shared.)
    view: ComPtr<IPlugView>,
    _frame: ComWrapper<TangPlugFrame>,
    shared: Rc<Shared>,
    closed: bool,
}

impl OpenEditor for OpenVst3Editor {
    fn poll(&mut self) -> bool {
        if self.closed {
            return false;
        }
        if !self.shared.window.is_open() {
            // The person has closed it: the view comes out of it before it is let go of, as when the program closes it.
            self.close();
            return false;
        }
        let size = self.shared.window.size();
        if size != self.shared.size.get() {
            self.shared.size.set(size);
            self.shared.tell(size);
        }
        true
    }

    fn close(&mut self) {
        if std::mem::replace(&mut self.closed, true) {
            return;
        }
        // (From here on the frame says no to a plugin that asks for a size.)
        self.shared.open.set(false);
        // Safety: the view was attached, and is taken off (`removed`) and let go of the frame (`setFrame(null)`), in that order, while the window it was attached to is still there.
        unsafe {
            self.view.removed();
            self.view.setFrame(std::ptr::null_mut());
        }
        self.shared.window.close();
    }

    fn size(&self) -> (i32, i32) {
        self.shared.window.size()
    }

    fn snapshot_png(&self) -> Option<Vec<u8>> {
        self.shared.window.snapshot_png()
    }

    fn describe(&self) -> String {
        self.shared.window.describe()
    }
}

impl Drop for OpenVst3Editor {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ffi::c_void;

    use vst3::Steinberg::{FIDString, IPlugViewTrait, char16, int16};

    use super::*;

    /// What a fake view and a fake window say they were asked, in the order they were asked it: one log for both, which is how the order between them is seen.
    type Log = Rc<RefCell<Vec<String>>>;

    fn said(log: &Log) -> Vec<String> {
        log.borrow().clone()
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
        fn close(&self) {
            self.log.borrow_mut().push("window.close".to_string());
            self.open.set(false);
        }
        fn snapshot_png(&self) -> Option<Vec<u8>> {
            Some(vec![1, 2, 3])
        }
        fn describe(&self) -> String {
            "a fake window\n".to_string()
        }
    }

    /// A plugin's view, written the way a plugin writes one: a COM object that answers the format's questions, and says how it was asked.
    struct FakeView {
        log: Log,
        size: (i32, i32),
        /// Whether it takes this platform's parent.
        platform_ok: bool,
        resizable: bool,
        /// What `attached` answers.
        attach_result: tresult,
        /// A size that the view asks the frame for while it is being attached.
        asks_on_attach: Option<(i32, i32)>,
        /// A size that the view asks the frame for while it is told a size (`onSize`).
        asks_on_size: Option<(i32, i32)>,
        /// The frame that the host gave the view, which the test calls as the plugin would.
        frame: RefCell<Option<ComPtr<IPlugFrame>>>,
        /// The first frame that the host gave it, which it never lets go of: a plugin that is wrong about that, as a plugin may be.
        kept: RefCell<Option<ComPtr<IPlugFrame>>>,
        /// The parent that the view was attached to.
        parent: Cell<*mut c_void>,
    }

    impl Class for FakeView {
        type Interfaces = (IPlugView,);
    }

    impl FakeView {
        fn new(log: &Log, size: (i32, i32)) -> Self {
            FakeView {
                log: log.clone(),
                size,
                platform_ok: true,
                resizable: true,
                attach_result: kResultOk,
                asks_on_attach: None,
                asks_on_size: None,
                frame: RefCell::new(None),
                kept: RefCell::new(None),
                parent: Cell::new(std::ptr::null_mut()),
            }
        }

        fn ask_the_frame(&self, (width, height): (i32, i32)) -> tresult {
            let frame = self.frame.borrow().clone().expect("the view has a frame");
            let mut rect = ViewRect { left: 0, top: 0, right: width, bottom: height };
            // Safety: the frame is the host's, and alive while the view holds it.
            unsafe { frame.resizeView(std::ptr::null_mut(), &mut rect) }
        }
    }

    impl IPlugViewTrait for FakeView {
        unsafe fn isPlatformTypeSupported(&self, r#type: FIDString) -> tresult {
            // Safety: the host gives a name that ends in a zero.
            let name = unsafe { CStr::from_ptr(r#type) }.to_string_lossy().into_owned();
            self.log.borrow_mut().push(format!("view.isPlatformTypeSupported({name})"));
            if self.platform_ok { kResultTrue } else { kResultFalse }
        }
        unsafe fn attached(&self, parent: *mut c_void, r#type: FIDString) -> tresult {
            // Safety: as above.
            let name = unsafe { CStr::from_ptr(r#type) }.to_string_lossy().into_owned();
            self.log.borrow_mut().push(format!("view.attached({name})"));
            self.parent.set(parent);
            if let Some(size) = self.asks_on_attach {
                self.ask_the_frame(size);
            }
            self.attach_result
        }
        unsafe fn removed(&self) -> tresult {
            self.log.borrow_mut().push("view.removed".to_string());
            kResultOk
        }
        unsafe fn onWheel(&self, _distance: f32) -> tresult {
            kResultFalse
        }
        unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
            kResultFalse
        }
        unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
            kResultFalse
        }
        unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
            self.log.borrow_mut().push("view.getSize".to_string());
            // Safety: the host gives a rectangle of its own.
            unsafe { *size = ViewRect { left: 0, top: 0, right: self.size.0, bottom: self.size.1 } };
            kResultOk
        }
        unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
            // Safety: as above.
            let rect = unsafe { *new_size };
            self.log.borrow_mut().push(format!("view.onSize({}, {})", rect.right - rect.left, rect.bottom - rect.top));
            if let Some(size) = self.asks_on_size {
                self.ask_the_frame(size);
            }
            kResultOk
        }
        unsafe fn onFocus(&self, _state: vst3::Steinberg::TBool) -> tresult {
            kResultOk
        }
        unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
            // Safety: the host gives its frame, or none.
            let held = unsafe { ComRef::from_raw(frame) }.map(|frame| frame.to_com_ptr());
            self.log.borrow_mut().push(format!("view.setFrame({})", if held.is_some() { "frame" } else { "none" }));
            if self.kept.borrow().is_none() {
                *self.kept.borrow_mut() = held.clone();
            }
            *self.frame.borrow_mut() = held;
            kResultOk
        }
        unsafe fn canResize(&self) -> tresult {
            self.log.borrow_mut().push("view.canResize".to_string());
            if self.resizable { kResultTrue } else { kResultFalse }
        }
        unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
            kResultTrue
        }
    }

    /// An editor for `view` and what it gets from the host: the window, which the test keeps hold of to look at and to change as a person would.
    struct Setup {
        log: Log,
        view: ComWrapper<FakeView>,
        window: Rc<FakeWindow>,
        editor: Vst3Editor,
    }

    fn setup(configure: impl FnOnce(&mut FakeView)) -> Setup {
        let log: Log = Rc::default();
        let mut view = FakeView::new(&log, (800, 600));
        configure(&mut view);
        let view = ComWrapper::new(view);
        let window = Rc::new(FakeWindow { log: log.clone(), size: Cell::new((0, 0)), open: Cell::new(true), content: 0 });
        let handed_out = view.clone();
        let editor = Vst3Editor::new(Box::new(move || handed_out.to_com_ptr::<IPlugView>()), Box::new(()));
        Setup { log, view, window, editor }
    }

    impl Setup {
        fn open(&self) -> Result<Box<dyn OpenEditor>, String> {
            let (log, window) = (self.log.clone(), self.window.clone());
            self.editor.open_in("A plugin", true, &move |spec: &Spec| {
                log.borrow_mut().push(format!(
                    "window.open({} {}x{}{}{})",
                    spec.title,
                    spec.width,
                    spec.height,
                    if spec.resizable { " resizable" } else { "" },
                    if spec.focus { " focus" } else { "" }
                ));
                window.size.set((spec.width, spec.height));
                Ok(window.clone() as Rc<dyn Window>)
            })
        }

        /// The platform's own name for a window's parent, as the plugin hears it.
        fn platform() -> String {
            // Safety: a constant that ends in a zero.
            unsafe { CStr::from_ptr(PLATFORM_TYPE) }.to_string_lossy().into_owned()
        }
    }

    #[test]
    fn a_plugin_is_asked_what_it_is_and_how_big_before_there_is_a_window_and_has_its_frame_before_it_is_attached() {
        let setup = setup(|_| {});
        let editor = setup.open().expect("opens");
        let platform = Setup::platform();
        assert_eq!(
            said(&setup.log),
            [
                format!("view.isPlatformTypeSupported({platform})"),
                "view.getSize".to_string(),
                "view.canResize".to_string(),
                "window.open(A plugin 800x600 resizable focus)".to_string(),
                "view.setFrame(frame)".to_string(),
                format!("view.attached({platform})"),
            ],
            "the order that the format says: what and how big (so that the window can be that size), the frame, and only then attached"
        );
        assert_eq!(editor.size(), (800, 600));
        assert_eq!(setup.view.parent.get(), setup.window.parent(), "attached to what is inside the window");
    }

    #[test]
    fn a_plugin_that_cannot_be_resized_gets_a_window_that_cannot_be() {
        let setup = setup(|view| view.resizable = false);
        setup.open().expect("opens");
        assert!(said(&setup.log).contains(&"window.open(A plugin 800x600 focus)".to_string()), "{:?}", said(&setup.log));
    }

    #[test]
    fn a_window_that_does_not_ask_for_the_keyboard_does_not_say_so_to_the_screen() {
        let setup = setup(|_| {});
        let (log, window) = (setup.log.clone(), setup.window.clone());
        setup
            .editor
            .open_in("Quiet", false, &move |spec: &Spec| {
                log.borrow_mut().push(format!("focus={}", spec.focus));
                Ok(window.clone() as Rc<dyn Window>)
            })
            .expect("opens");
        assert!(said(&setup.log).contains(&"focus=false".to_string()));
    }

    #[test]
    fn a_plugin_that_has_no_view_says_so_and_no_window_is_made() {
        let log: Log = Rc::default();
        let editor = Vst3Editor::new(Box::new(|| None), Box::new(()));
        let refused = |_: &Spec| -> Result<Rc<dyn Window>, String> {
            log.borrow_mut().push("window.open".to_string());
            Err("no".to_string())
        };
        let error = editor.open_in("A plugin", true, &refused).err().expect("refused");
        assert_eq!(error, "this plugin has no window of its own");
        assert!(said(&log).is_empty(), "{:?}", said(&log));
    }

    #[test]
    fn a_view_that_is_not_for_this_platform_gets_no_window() {
        let setup = setup(|view| view.platform_ok = false);
        let error = setup.open().err().expect("refused");
        assert!(error.contains("not one that can be shown here") && error.contains(&Setup::platform()), "{error}");
        assert_eq!(said(&setup.log), [format!("view.isPlatformTypeSupported({})", Setup::platform())], "nothing else is asked of it, and no window is opened");
    }

    #[test]
    fn a_view_that_does_not_attach_is_let_go_of_without_being_removed_and_its_window_is_taken_away() {
        let setup = setup(|view| view.attach_result = kResultFalse);
        let error = setup.open().err().expect("refused");
        assert!(error.contains("did not attach"), "{error}");
        let log = said(&setup.log);
        let tail: Vec<&str> = log.iter().rev().take(2).map(String::as_str).collect();
        assert_eq!(tail, ["window.close", "view.setFrame(none)"], "the frame is taken back and the window closed: {log:?}");
        assert!(!log.contains(&"view.removed".to_string()), "a view that was never attached has nothing to remove: {log:?}");
        assert!(!setup.window.is_open());
    }

    #[test]
    fn a_view_that_does_not_say_how_big_it_is_gets_a_window_of_the_usual_size() {
        // (A size of none, a side that is none or less, and a side that is more than a window can be are all the same to it: no size.)
        for size in [(0, 0), (0, 300), (300, 0), (-5, 300), (300, -5), (MAX_SIDE + 1, 300), (300, MAX_SIDE + 1)] {
            let setup = setup(|view| view.size = size);
            assert_eq!(setup.open().expect("opens").size(), (640, 480), "{size:?}");
        }
        for size in [(MAX_SIDE, 1), (1, MAX_SIDE)] {
            let setup = setup(|view| view.size = size);
            assert_eq!(setup.open().expect("opens").size(), size, "the largest side and the smallest that a window can have are not refused");
        }
    }

    #[test]
    fn a_plugin_that_asks_for_another_size_gets_it_and_is_told_so() {
        let setup = setup(|_| {});
        let mut editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        assert_eq!(setup.view.ask_the_frame((1000, 500)), kResultTrue);
        assert_eq!(said(&setup.log), ["window.set_size(1000, 500)", "view.onSize(1000, 500)"], "the window first, and then the plugin is told what it has, which the format says to do");
        assert_eq!(setup.window.size(), (1000, 500));
        // The next look at the window finds the size that the plugin was told, and tells it nothing.
        assert!(editor.poll());
        assert_eq!(said(&setup.log).len(), 2, "{:?}", said(&setup.log));
    }

    #[test]
    fn the_largest_side_and_the_smallest_that_a_window_can_have_are_asked_for_and_given() {
        let setup = setup(|_| {});
        let _editor = setup.open().expect("opens");
        for size in [(MAX_SIDE, 1), (1, MAX_SIDE)] {
            assert_eq!(setup.view.ask_the_frame(size), kResultTrue, "{size:?}");
            assert_eq!(setup.window.size(), size);
        }
    }

    #[test]
    fn a_plugin_that_asks_for_a_size_while_it_is_attached_gets_that_too() {
        let setup = setup(|view| view.asks_on_attach = Some((900, 700)));
        let editor = setup.open().expect("opens");
        let log = said(&setup.log);
        let attached = log.iter().position(|line| line.starts_with("view.attached")).expect("attached");
        assert_eq!(&log[attached + 1..], ["window.set_size(900, 700)", "view.onSize(900, 700)"], "{log:?}");
        assert_eq!(editor.size(), (900, 700));
    }

    #[test]
    fn a_plugin_that_asks_for_a_size_when_it_is_told_one_is_not_told_again() {
        let setup = setup(|view| view.asks_on_size = Some((300, 200)));
        let _editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        // The first ask is told (`onSize`), and the plugin asks again in that call: the window takes the new size, and the plugin is not told it a second time.
        assert_eq!(setup.view.ask_the_frame((1000, 500)), kResultTrue);
        assert_eq!(
            said(&setup.log),
            ["window.set_size(1000, 500)", "view.onSize(1000, 500)", "window.set_size(300, 200)"],
            "told once: it would go on for ever otherwise"
        );
        assert_eq!(setup.window.size(), (300, 200));
    }

    #[test]
    fn a_size_that_a_window_cannot_be_is_refused_and_the_window_is_left_as_it_is() {
        let setup = setup(|_| {});
        let _editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        for (width, height) in [(0, 100), (100, 0), (-5, 100), (100, -5), (MAX_SIDE + 1, 100), (100, MAX_SIDE + 1)] {
            assert_eq!(setup.view.ask_the_frame((width, height)), kInvalidArgument, "{width}x{height}");
        }
        let mut backwards = ViewRect { left: 500, top: 0, right: 100, bottom: 100 };
        let frame = setup.view.frame.borrow().clone().expect("a frame");
        // Safety: the frame is the host's, and alive while the view holds it.
        assert_eq!(unsafe { frame.resizeView(std::ptr::null_mut(), &mut backwards) }, kInvalidArgument);
        // Safety: as above; the plugin may give no rectangle, and is told it was wrong.
        assert_eq!(unsafe { frame.resizeView(std::ptr::null_mut(), std::ptr::null_mut()) }, kInvalidArgument);
        // Even a rectangle whose sides overflow does not take the program down.
        let mut huge = ViewRect { left: i32::MIN, top: i32::MIN, right: i32::MAX, bottom: i32::MAX };
        // Safety: as above.
        assert_eq!(unsafe { frame.resizeView(std::ptr::null_mut(), &mut huge) }, kInvalidArgument);
        assert!(said(&setup.log).is_empty(), "{:?}", said(&setup.log));
        assert_eq!(setup.window.size(), (800, 600));
    }

    #[test]
    fn a_plugin_that_calls_the_frame_from_another_thread_is_told_no_and_the_window_is_left_alone() {
        let setup = setup(|_| {});
        let _editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        let frame = setup.view.frame.borrow().clone().expect("a frame").as_ptr() as usize;
        let answer = std::thread::spawn(move || {
            let mut rect = ViewRect { left: 0, top: 0, right: 1000, bottom: 500 };
            // Safety: the frame is alive while the editor is, which outlives this thread (it is joined below); the call is the plugin's mistake, which the frame must survive.
            unsafe { ComRef::from_raw(frame as *mut IPlugFrame).expect("a frame").resizeView(std::ptr::null_mut(), &mut rect) }
        })
        .join()
        .expect("the thread ends");
        assert_eq!(answer, kResultFalse);
        assert!(said(&setup.log).is_empty(), "{:?}", said(&setup.log));
        assert_eq!(setup.window.size(), (800, 600));
    }

    #[test]
    fn a_person_who_drags_the_edge_of_the_window_is_told_to_the_plugin_once_for_each_size() {
        let setup = setup(|_| {});
        let mut editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        assert!(editor.poll());
        assert!(said(&setup.log).is_empty(), "nothing has changed: {:?}", said(&setup.log));
        setup.window.size.set((850, 620));
        assert!(editor.poll());
        assert!(editor.poll());
        assert_eq!(said(&setup.log), ["view.onSize(850, 620)"], "told once, not on every look");
        setup.window.size.set((800, 600));
        assert!(editor.poll());
        assert_eq!(said(&setup.log), ["view.onSize(850, 620)", "view.onSize(800, 600)"], "and told of the size that it is back at");
    }

    #[test]
    fn a_window_that_the_person_closed_is_seen_and_the_view_is_taken_out_of_it_before_it_is_let_go_of() {
        let setup = setup(|_| {});
        let mut editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        setup.window.open.set(false);
        assert!(!editor.poll());
        assert_eq!(said(&setup.log), ["view.removed", "view.setFrame(none)", "window.close"], "taken off, the frame given back, and then the window");
        assert!(!editor.poll());
        editor.close();
        drop(editor);
        assert_eq!(said(&setup.log).len(), 3, "and a window that is closed is not closed again: {:?}", said(&setup.log));
        assert!(setup.view.frame.borrow().is_none(), "the plugin holds no frame of ours any more");
    }

    #[test]
    fn closing_the_editor_closes_the_window_once_and_so_does_dropping_it() {
        let setup = setup(|_| {});
        let mut editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        editor.close();
        assert_eq!(said(&setup.log), ["view.removed", "view.setFrame(none)", "window.close"]);
        assert!(!setup.window.is_open());
        drop(editor);
        assert_eq!(said(&setup.log).len(), 3, "{:?}", said(&setup.log));

        let setup = self::setup(|_| {});
        let editor = setup.open().expect("opens");
        setup.log.borrow_mut().clear();
        drop(editor);
        assert_eq!(said(&setup.log), ["view.removed", "view.setFrame(none)", "window.close"], "an editor that is dropped is closed");
    }

    #[test]
    fn a_plugin_that_keeps_the_frame_of_a_window_that_did_not_attach_is_told_no_when_it_asks_for_a_size() {
        let setup = setup(|view| view.attach_result = kResultFalse);
        setup.open().err().expect("refused");
        let frame = setup.view.kept.borrow().clone().expect("the plugin kept the frame");
        setup.log.borrow_mut().clear();
        let mut rect = ViewRect { left: 0, top: 0, right: 1000, bottom: 500 };
        // Safety: the frame is alive, since the plugin and this test hold a reference of it.
        assert_eq!(unsafe { frame.resizeView(std::ptr::null_mut(), &mut rect) }, kResultFalse);
        assert!(said(&setup.log).is_empty(), "{:?}", said(&setup.log));
    }

    #[test]
    fn a_plugin_that_asks_for_a_size_after_its_window_is_closed_is_told_no_and_nothing_is_touched() {
        let setup = setup(|_| {});
        let mut editor = setup.open().expect("opens");
        // A plugin that keeps a frame it was told to let go of (a mistake of its own) and calls it: the view it names may be gone.
        let frame = setup.view.frame.borrow().clone().expect("a frame");
        editor.close();
        drop(editor);
        setup.log.borrow_mut().clear();
        let mut rect = ViewRect { left: 0, top: 0, right: 1000, bottom: 500 };
        // Safety: the frame is alive, since this test holds a reference of it.
        assert_eq!(unsafe { frame.resizeView(std::ptr::null_mut(), &mut rect) }, kResultFalse);
        assert!(said(&setup.log).is_empty(), "{:?}", said(&setup.log));
    }

    #[test]
    fn what_is_in_the_window_can_be_had_as_a_picture_and_as_text() {
        let setup = setup(|_| {});
        let editor = setup.open().expect("opens");
        assert_eq!(editor.snapshot_png(), Some(vec![1, 2, 3]));
        assert_eq!(editor.describe(), "a fake window\n");
    }
}
