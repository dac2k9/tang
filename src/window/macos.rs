//! The window on macOS: an `NSWindow` by objc2-app-kit, and an event pump that is a loop of ours over `nextEventMatchingMask` (`NSApplication.run` does not come back, and the program has its own loops).

use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapImageFileType, NSEventMask, NSView, NSWindow,
    NSWindowAnimationBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSDictionary, NSPoint, NSRect, NSSize, NSString};

use super::{Spec, Window};

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// The dictionary of the login session that the program runs in, which the window server made: null when there is none to be had (a program started over ssh, in a container, by CI).
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}

/// Whether this machine can show a window, and if it cannot, why. Asked before anything of AppKit is touched, since the first touch of it with no window server to talk to ends the program.
pub fn available() -> Result<(), String> {
    // Safety: no arguments; what comes back is ours to let go of (a "Copy" function), which is done here.
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    if session.is_null() {
        return Err("this session has no screen to open a window on (a login over ssh, a container, CI)".to_string());
    }
    // Safety: a CoreFoundation object that was just made, and is not used again.
    unsafe { CFRelease(session.cast_mut()) };
    Ok(())
}

/// The application, made ready the first time it is asked for. A program that is not an application bundle starts with no right to a window (the policy is `Prohibited`); an accessory one has windows and no icon in
/// the Dock or menu bar, which is what a terminal program that opens a window for a while should have.
fn application(mtm: MainThreadMarker) -> Retained<NSApplication> {
    static LAUNCHED: Once = Once::new();
    let app = NSApplication::sharedApplication(mtm);
    LAUNCHED.call_once(|| {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        // (What `run` does before its loop, and a loop of our own has to do for itself.)
        app.finishLaunching();
    });
    app
}

/// Open a window that has `spec`'s size inside it, on the screen.
pub fn open(spec: &Spec) -> Result<Rc<dyn Window>, String> {
    available()?;
    let mtm = MainThreadMarker::new().ok_or_else(|| "a window is opened on the main thread".to_string())?;
    let app = application(mtm);
    let mut style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    if spec.resizable {
        style |= NSWindowStyleMask::Resizable;
    }
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(f64::from(spec.width), f64::from(spec.height)));
    // Safety: a window is made on the main thread (the marker says so) with a frame, a style and a backing store that AppKit knows, and shown where its owner asks.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            content,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // Safety: the `Retained` is the window's owner, and `close` (the red button, or ours) must not give up a reference that is not a second one.
    unsafe { window.setReleasedWhenClosed(false) };
    // (No zoom when it opens and no fade when it closes: a fade is finished by the turns of the loop that come after the close, and the one turn that `close` makes is not for that.)
    window.setAnimationBehavior(NSWindowAnimationBehavior::None);
    window.setTitle(&NSString::from_str(spec.title));
    window.center();
    show(&window, &app, spec.focus);
    Ok(Rc::new(MacWindow { window }))
}

/// Put the window in front. It is made the key window of the program, whether or not the program is the active one: a plugin (an iPlug2 one, Gateway) makes no view in a window that has not been, and an empty window is what it
/// is given otherwise. The program is made the active one only when `focus` says so, which is what leaves the keyboard where it was.
fn show(window: &NSWindow, app: &NSApplication, focus: bool) {
    window.makeKeyAndOrderFront(None);
    if focus {
        // (Deprecated for `activate`, which a macOS before 14 does not have, and which is no more than the polite form of this.)
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    } else {
        // (The window of a program that is not the active one is in front of the other programs' only when it is told to be.)
        window.orderFrontRegardless();
    }
}

/// Let the windows have their events, for `limit` if there are none to have and a little longer if there are a lot: wait for an event and hand it to AppKit, until the time is up. The run loop's other sources
/// (the timers of a plugin's editor, what it posts to the main queue) are served while it waits.
pub fn pump(limit: Duration) {
    let Some(mtm) = MainThreadMarker::new() else {
        // (There are no windows anywhere else, and nothing for another thread to serve.)
        std::thread::sleep(limit);
        return;
    };
    let app = application(mtm);
    let deadline = Instant::now() + limit;
    loop {
        // A pool for every turn: the main thread has none of its own, and what AppKit hands out while it works would be kept for as long as the program runs.
        let handled = autoreleasepool(|_| {
            let until = NSDate::dateWithTimeIntervalSinceNow(deadline.saturating_duration_since(Instant::now()).as_secs_f64());
            // Safety: the run loop mode is a constant that Foundation defines.
            let mode = unsafe { NSDefaultRunLoopMode };
            match app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&until), mode, true) {
                Some(event) => {
                    app.sendEvent(&event);
                    true
                }
                None => false,
            }
        });
        if !handled || Instant::now() >= deadline {
            break;
        }
    }
    app.updateWindows();
}

/// How deep a view tree is described: a plugin's views are a few levels, and this is for a person to read.
const DESCRIBE_DEPTH: usize = 12;

/// A view and what is in it, one to a line, each indented by how deep it is: its class, its size, and whether it is hidden or has a layer.
fn describe_view(view: &NSView, depth: usize, text: &mut String) {
    let size = view.frame().size;
    let indent = "  ".repeat(depth);
    text.push_str(&format!(
        "{indent}{} {}x{}{}{}\n",
        view.class().name().to_string_lossy(),
        size.width.round(),
        size.height.round(),
        if view.isHidden() { " (hidden)" } else { "" },
        if view.wantsLayer() { " (layer)" } else { "" },
    ));
    if depth == DESCRIBE_DEPTH {
        text.push_str(&format!("{indent}  ...\n"));
        return;
    }
    for inner in view.subviews().iter() {
        describe_view(&inner, depth + 1, text);
    }
}

struct MacWindow {
    window: Retained<NSWindow>,
}

impl Window for MacWindow {
    fn parent(&self) -> *mut c_void {
        // (The window keeps its content view, so the pointer is good for as long as the window is, though the reference that is made here is let go of.)
        match self.window.contentView() {
            Some(view) => Retained::as_ptr(&view).cast_mut().cast(),
            None => std::ptr::null_mut(),
        }
    }

    fn size(&self) -> (i32, i32) {
        match self.window.contentView() {
            Some(view) => {
                let size = view.frame().size;
                (size.width.round() as i32, size.height.round() as i32)
            }
            None => (0, 0),
        }
    }

    fn set_size(&self, width: i32, height: i32) {
        self.window.setContentSize(NSSize::new(f64::from(width), f64::from(height)));
    }

    fn is_open(&self) -> bool {
        self.window.isVisible()
    }

    fn raise(&self, focus: bool) {
        show(&self.window, &application(self.window.mtm()), focus);
    }

    fn close(&self) {
        self.window.close();
        // AppKit keeps what it tells the window server until its run loop turns, and the loops of the program turn it only while a window is open: without this turn the window of the last one to close would stay on the screen,
        // shut and frozen, until the next one was opened. (No window is open while the scan of the installed plugins runs, which is when the loop must not be turned: f179.)
        pump(Duration::from_millis(1));
    }

    fn describe(&self) -> String {
        let mut text = String::new();
        match self.window.contentView() {
            Some(view) => describe_view(&view, 0, &mut text),
            None => text.push_str("(no content view)\n"),
        }
        let others: Vec<String> = application(self.window.mtm())
            .windows()
            .iter()
            .filter(|other| !std::ptr::eq(Retained::as_ptr(other), Retained::as_ptr(&self.window)))
            .map(|other| {
                let size = other.frame().size;
                format!("{} \"{}\" {}x{}{}", other.class().name().to_string_lossy(), other.title(), size.width.round(), size.height.round(), if other.isVisible() { "" } else { " (not on the screen)" })
            })
            .collect();
        if others.is_empty() {
            text.push_str("no other windows\n");
        } else {
            text.push_str(&format!("other windows: {}\n", others.join(", ")));
        }
        text
    }

    fn snapshot_png(&self) -> Option<Vec<u8>> {
        let view = self.window.contentView()?;
        let bounds = view.bounds();
        let picture = view.bitmapImageRepForCachingDisplayInRect(bounds)?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &picture);
        // Safety: no properties are given, so the dictionary is empty, which is any type that it is asked for.
        let png = unsafe { picture.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
        Some(png.to_vec())
    }
}

impl Drop for MacWindow {
    fn drop(&mut self) {
        // A window that nobody holds is not left on the screen.
        self.window.close();
    }
}
