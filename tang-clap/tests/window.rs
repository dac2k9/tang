//! The plugin's window: a window of the host's with the page in a web view (the size that the host negotiates is tried here; the web view itself is tried with a real window, `tang window Tang.clap`), or, for a host that asks for a floating window, the page of this instance, opened in the browser that the person has. A test of its own, because it says which program opens the page by
//! an environment variable (`TANG_BROWSER`), which no other test of the process should see.

// (Linux has no window of the plugin's, so the tests below that put one on a screen are not there, and what only they use is dead code and an unused import there.)
#![cfg_attr(not(any(windows, target_os = "macos")), expect(dead_code, unused_imports, reason = "only the tests of a window on a screen use them, and Linux has none"))]

mod common;

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window};
use clack_extensions::state::PluginState;
use clack_host::prelude::*;
use clack_host::process::StartedPluginAudioProcessor;
use common::*;
use tang_clap::gui::{Maker, Page, Parent, View};

/// A program that writes the address that it is given at the end of `file`: what stands in for the browser.
fn opener(dir: &std::path::Path, file: &std::path::Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("open-page.sh");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\n", file.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }
    #[cfg(windows)]
    {
        let script = dir.join("open-page.cmd");
        std::fs::write(&script, format!("@echo %1>>\"{}\"\r\n", file.display())).unwrap();
        script
    }
}

/// The file that the stand-in writes in, made once for the tests of this program (they say which program opens the page by an environment variable that every test of the process sees), and the lock that the tests take so that one at a time has the page
/// opened: a test counts what the stand-in has been given.
fn stand_in() -> &'static std::path::Path {
    static FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    FILE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("opened.txt");
        // SAFETY: nothing else of this program reads or writes the environment while this runs: every test comes through here first (and waits for it), and the plugin reads it only when a page is opened.
        unsafe { std::env::set_var("TANG_BROWSER", opener(dir.path(), &file)) };
        // (The directory is left where it is: the stand-in is run by the system after a test is over, and needs it.)
        let kept = dir.path().to_path_buf();
        std::mem::forget(dir);
        kept.join("opened.txt")
    })
}

static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// What the stand-in has been given, once it has been given `count` things (it is a program of its own, started and left to itself: a moment each).
fn opened(file: &std::path::Path, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lines: Vec<String> = std::fs::read_to_string(file).unwrap_or_default().lines().map(|line| line.trim().to_string()).filter(|line| !line.is_empty()).collect();
        if lines.len() >= count || Instant::now() >= deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What the stand-in has been given so far.
fn so_far(file: &std::path::Path) -> usize {
    opened(file, 0).len()
}

/// Whether this system has a web view that Tang puts in a window of the host's (`gui`): macOS and Windows.
const HAS_WEB_VIEW: bool = cfg!(any(windows, target_os = "macos"));

#[test]
fn the_window_is_the_page_of_the_instance_which_the_host_asks_to_show_and_which_opens_in_the_browser() {
    let file = stand_in();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = so_far(file);

    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("the plugin has a window");

    // A system that has a web view for it (macOS, Windows) is asked for a window of the host's, and the page is shown in it (the next test); anywhere else the window is a floating one, which the plugin shows itself. This
    // test is of the floating one, which the plugin has on every system, and is what a host that asks for it is given.
    let preferred = gui.get_preferred_api(&mut handle).expect("it has a preferred api");
    assert_eq!(preferred.is_floating, !HAS_WEB_VIEW, "{preferred:?}");
    let floating = GuiConfiguration { api_type: GuiApiType::default_for_current_platform().unwrap(), is_floating: true };
    let embedded = GuiConfiguration { is_floating: false, ..floating };
    assert!(gui.is_api_supported(&mut handle, floating));
    assert_eq!(gui.is_api_supported(&mut handle, embedded), HAS_WEB_VIEW, "a window of the host's is for a system that has a web view");
    if !HAS_WEB_VIEW {
        assert!(gui.create(&mut handle, embedded).is_err());
    }
    gui.create(&mut handle, floating).expect("a floating window is made");

    // The plugin has not been switched on: there is no page to show yet, and no browser is opened for nothing.
    assert!(gui.show(&mut handle).is_err());
    assert_eq!(so_far(file), before, "nothing was opened");

    // Switched on, it shows the page: the browser is opened on an address on this machine, which answers.
    let processor = instance
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: RATE, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap();
    let mut handle = instance.plugin_handle();
    gui.show(&mut handle).expect("the window is shown");
    let url = opened(file, before + 1).get(before).cloned().expect("the browser was opened");
    let address = url.strip_prefix("http://127.0.0.1:").unwrap_or_else(|| panic!("an address on this machine: {url}")).trim_end_matches('/');
    let port: u16 = address.parse().unwrap_or_else(|why| panic!("{url}: {why}"));
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("the page is served");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(stream, "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "the page answers: {}", answer.lines().next().unwrap_or(""));

    // Hiding it does nothing to the page, and it can be shown again.
    gui.hide(&mut handle).expect("it is hidden");
    gui.show(&mut handle).expect("and shown again");
    assert_eq!(opened(file, before + 2)[before..], [url.clone(), url], "the browser is opened again on the same address");
    gui.destroy(&mut handle);
    instance.deactivate(processor.stop_processing());
}

#[test]
fn an_embedded_window_has_a_size_that_the_host_asks_for_and_changes_and_is_made_again_after_it_is_destroyed() {
    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("the plugin has a window");
    let embedded = GuiConfiguration { api_type: GuiApiType::default_for_current_platform().unwrap(), is_floating: false };
    if !HAS_WEB_VIEW {
        assert!(!gui.is_api_supported(&mut handle, embedded), "no web view, no window of the host's");
        assert!(gui.create(&mut handle, embedded).is_err());
        return;
    }
    assert!(gui.is_api_supported(&mut handle, embedded));
    assert!(!gui.is_api_supported(&mut handle, GuiConfiguration { api_type: GuiApiType::X11, is_floating: false }) || cfg!(target_os = "linux"), "not another system's windows");
    assert!(gui.get_preferred_api(&mut handle).is_some_and(|preferred| !preferred.is_floating), "a window of the host's is what it asks to be made");
    gui.create(&mut handle, embedded).expect("an embedded window is made");

    // The size that the host starts from, what it may do with it, and what it asks the plugin to make of a size.
    assert_eq!(gui.get_size(&mut handle), Some(GuiSize { width: 1100, height: 720 }));
    assert!(gui.can_resize(&mut handle));
    let hints = gui.get_resize_hints(&mut handle).expect("it says how it is resized");
    assert!(hints.can_resize_horizontally && hints.can_resize_vertically);
    assert_eq!(gui.adjust_size(&mut handle, GuiSize { width: 10, height: 100_000 }), Some(GuiSize { width: 480, height: 16_384 }));
    assert_eq!(gui.adjust_size(&mut handle, GuiSize { width: 100_000, height: 10 }), Some(GuiSize { width: 16_384, height: 320 }), "the least is 480 x 320");
    // The scale: a host that counts pixels (Windows) is told a window that is as large to the eye, and one that counts points (macOS) is not.
    gui.set_scale(&mut handle, 2.0).expect("a scale of 2");
    let at_two = if cfg!(windows) { GuiSize { width: 2200, height: 1440 } } else { GuiSize { width: 1100, height: 720 } };
    assert_eq!(gui.get_size(&mut handle), Some(at_two));
    gui.set_scale(&mut handle, 1.0).expect("a scale of 1");
    assert_eq!(gui.get_size(&mut handle), Some(GuiSize { width: 1100, height: 720 }));
    assert!(gui.set_scale(&mut handle, 0.0).is_err(), "a scale of 0 is none");
    gui.set_size(&mut handle, GuiSize { width: 900, height: 600 }).expect("the host makes the window another size");
    assert_eq!(gui.get_size(&mut handle), Some(GuiSize { width: 900, height: 600 }), "and is told that it is");
    // Nothing is shown before there is a window of the host's to show it in.
    assert!(gui.show(&mut handle).is_err());

    // Destroyed, and made again: the window starts as it did.
    gui.destroy(&mut handle);
    gui.create(&mut handle, embedded).expect("and made again");
    assert_eq!(gui.get_size(&mut handle), Some(GuiSize { width: 1100, height: 720 }));
    gui.destroy(&mut handle);
}

// What the plugin does with a window of the host's, with no screen to put it on: a view that writes down what it is told, which the plugins made on this thread are given (`use_maker_on_this_thread`).
thread_local! {
    static VIEWS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn write(line: String) {
    VIEWS.with(|log| log.borrow_mut().push(line));
}

/// What the views have been told since this was last asked.
fn said() -> Vec<String> {
    VIEWS.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

fn named(page: &Page) -> String {
    match page {
        Page::Url(url) => url.clone(),
        Page::Waiting => "waiting".to_string(),
    }
}

struct FakeView;

impl View for FakeView {
    fn set_bounds(&self, size: GuiSize, scale: f64) -> Result<(), String> {
        write(format!("bounds {}x{} @{scale}", size.width, size.height));
        Ok(())
    }

    fn set_visible(&self, visible: bool) -> Result<(), String> {
        write(format!("visible {visible}"));
        Ok(())
    }

    fn load(&self, page: &Page) -> Result<(), String> {
        write(format!("load {}", named(page)));
        Ok(())
    }
}

impl Drop for FakeView {
    fn drop(&mut self) {
        write("view dropped".to_string());
    }
}

struct FakeMaker;

impl Maker for FakeMaker {
    fn available(&self) -> Result<(), String> {
        Ok(())
    }

    fn ready(&self, _parent: Parent) -> bool {
        true
    }

    fn make(&self, _parent: Parent, size: GuiSize, scale: f64, page: &Page) -> Result<Box<dyn View>, String> {
        write(format!("make {}x{} @{scale} {}", size.width, size.height, named(page)));
        Ok(Box::new(FakeView))
    }
}

fn fake_maker() -> Box<dyn Maker> {
    Box::new(FakeMaker)
}

/// A window of the host's, of this system's kind (a pointer that is not a window: the views here are not put anywhere).
fn host_window() -> Window<'static> {
    #[cfg(target_os = "macos")]
    return Window::from_cocoa_nsview(8 as *mut std::ffi::c_void);
    #[cfg(not(target_os = "macos"))]
    return Window::from_win32_hwnd(8 as *mut std::ffi::c_void);
}

fn embedded() -> GuiConfiguration<'static> {
    GuiConfiguration { api_type: GuiApiType::default_for_current_platform().unwrap(), is_floating: false }
}

fn switch_on(instance: &mut PluginInstance<Handlers>) -> StartedPluginAudioProcessor<Handlers> {
    instance
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: RATE, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap()
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn a_window_that_is_asked_for_before_the_plugin_is_on_waits_and_then_shows_the_page_and_follows_the_instance_to_the_next_address() {
    tang_clap::gui::use_maker_on_this_thread(fake_maker);
    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("the plugin has a window");

    // A host that opens the window of a plugin that has not been switched on (the person asked, and the audio engine is not running): there is no page yet, and the view says that it waits.
    gui.create(&mut handle, embedded()).expect("an embedded window is made");
    gui.set_scale(&mut handle, 1.0).unwrap();
    // SAFETY: the window is a pointer that nothing is done with: the view of this test does not put itself in it.
    unsafe { gui.set_parent(&mut handle, host_window()) }.expect("the view is made in the host's window");
    assert_eq!(said(), ["make 1100x720 @1 waiting"], "no page yet");
    gui.show(&mut handle).expect("and shown");
    assert_eq!(said(), ["visible true"]);

    // Switched on: the page that the instance serves comes into the view, by itself.
    let processor = switch_on(&mut instance);
    let told = said();
    assert_eq!(told.len(), 1, "{told:?}");
    let first = told[0].strip_prefix("load http://127.0.0.1:").unwrap_or_else(|| panic!("the page of the instance: {told:?}")).to_string();

    // A host that gives the plugin a session while it is on: the instance is served at another address, and the view goes there.
    let mut handle = instance.plugin_handle();
    let state = handle.get_extension::<PluginState>().expect("the plugin keeps a state");
    state.load(&mut handle, &mut std::io::Cursor::new(state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.5\n"))).expect("the session is taken");
    let told = said();
    assert_eq!(told.len(), 1, "{told:?}");
    let second = told[0].strip_prefix("load http://127.0.0.1:").unwrap_or_else(|| panic!("the page of the instance: {told:?}")).to_string();
    assert_ne!(first, second, "another address");

    // The host resizes the window, scales it, hides it and lets it go.
    let gui = handle.get_extension::<PluginGui>().unwrap();
    gui.set_size(&mut handle, GuiSize { width: 900, height: 600 }).unwrap();
    gui.set_scale(&mut handle, 2.0).unwrap();
    gui.hide(&mut handle).unwrap();
    gui.destroy(&mut handle);
    assert_eq!(said(), ["bounds 900x600 @1", "bounds 900x600 @2", "visible false", "view dropped"]);
    instance.deactivate(processor.stop_processing());
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn a_window_that_is_asked_for_when_the_plugin_is_on_is_made_with_its_page_and_a_window_that_is_not_the_systems_own_is_refused() {
    tang_clap::gui::use_maker_on_this_thread(fake_maker);
    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let processor = switch_on(&mut instance);
    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("the plugin has a window");
    gui.create(&mut handle, embedded()).unwrap();
    gui.set_scale(&mut handle, 2.0).unwrap();

    // A window of another system's kind is not one that the view can go in: none is made for it. (The host is not told so by the call: the clack that the plugin is built on answers `true` to `set_parent` and `set_size` whatever the
    // plugin returns, as it does the `false` of `create`, `show` and `hide`; the plugin says why in its log, and a view that is not there is refused again at `show`.)
    // SAFETY: the window is a handle that is only looked at, and refused.
    let _ = unsafe { gui.set_parent(&mut handle, Window::from_x11_handle(1)) };
    assert!(said().is_empty(), "no view was made for it");
    assert!(gui.show(&mut handle).is_err(), "and there is nothing to show");

    // SAFETY: as above, a pointer that nothing is done with.
    unsafe { gui.set_parent(&mut handle, host_window()) }.expect("the view is made");
    let told = said();
    assert_eq!(told.len(), 1, "{told:?}");
    // (At a scale of 2 a host that counts pixels, Windows, is made a window that is as large to the eye, and one that counts points is not.)
    let size = if cfg!(windows) { "2200x1440" } else { "1100x720" };
    assert!(told[0].starts_with(&format!("make {size} @2 http://127.0.0.1:")), "the page that the instance has, from the start: {told:?}");
    gui.show(&mut handle).unwrap();
    assert_eq!(said(), ["visible true"], "the page that it was made with is the one that there is: nothing is loaded again");
    gui.destroy(&mut handle);
    instance.deactivate(processor.stop_processing());
}

#[test]
fn a_press_of_the_switch_in_a_block_asks_the_host_to_call_back_and_the_page_is_opened_by_the_main_thread_and_the_switch_goes_back() {
    let file = stand_in();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = so_far(file);
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());

    // The switch that is set to 0 is not a press.
    assert!(rig.block_with(Some(0.0)).is_empty());
    assert_eq!(rig.callbacks(), 0);

    // Pressed in a block: the audio thread does not open a page, it asks the host to call the main thread back; nothing is put back yet.
    assert!(rig.block_with(Some(1.0)).is_empty());
    assert_eq!(rig.callbacks(), 1, "the host was asked to call back");
    assert_eq!(so_far(file), before, "and no page was opened by the audio thread");
    // Held at 1 is not another press.
    assert!(rig.block_with(Some(1.0)).is_empty());
    assert_eq!(rig.callbacks(), 1, "a switch that stays at 1 asks for nothing");

    // The host calls back: the main thread opens the page, on an address of this machine.
    rig.call_back();
    let urls = opened(file, before + 1);
    assert!(urls.get(before).is_some_and(|url| url.starts_with("http://127.0.0.1:")), "{urls:?}");
    // The next block says that the switch is back at 0, once.
    assert_eq!(rig.block_with(None), [0.0], "put back for the host to show");
    assert!(rig.block_with(None).is_empty(), "and said once");

    // And it can be pressed again: that is another opening.
    assert!(rig.block_with(Some(1.0)).is_empty());
    assert_eq!(rig.callbacks(), 2);
    rig.call_back();
    assert_eq!(opened(file, before + 2).len(), before + 2, "the page is opened again");
    assert_eq!(rig.block_with(None), [0.0]);

    // The host that is not processing (the plugin is active) sends the change without a block (`flush`): the same, asked for the same way.
    assert!(rig.flush_with(Some(1.0)).is_empty());
    assert_eq!(rig.callbacks(), 3, "a press in a flush asks the host to call back as well");
    rig.call_back();
    assert_eq!(opened(file, before + 3).len(), before + 3, "and the page is opened");
    assert_eq!(rig.flush_with(None), [0.0], "the flush tells the host that the switch is back");
    assert!(rig.flush_with(None).is_empty());
    rig.stop();
}

#[test]
fn a_press_when_the_plugin_has_not_started_opens_nothing_and_the_switch_goes_back_all_the_same() {
    use clack_extensions::params::PluginParams;
    let file = stand_in();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = so_far(file);
    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let params = instance.plugin_handle().get_extension::<PluginParams>().unwrap();
    let mut inactive = instance.inactive_plugin_handle().expect("not active");
    let mut input = EventBuffer::with_capacity(1);
    input.push(&clack_host::events::event_types::ParamValueEvent::new(0, ClapId::new(0), Pckn::match_all(), 1.0, clack_host::utils::Cookie::empty()));
    let mut output = EventBuffer::with_capacity(2);
    params.flush(&mut inactive, &input.as_input(), &mut output.as_output());
    let put_back: Vec<f64> = output
        .as_input()
        .iter()
        .filter_map(|event| match event.as_core_event() {
            Some(clack_host::events::spaces::CoreEventSpace::ParamValue(change)) => Some(change.value()),
            _ => None,
        })
        .collect();
    assert_eq!(put_back, [0.0], "the press was seen: the switch goes back");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(so_far(file), before, "and no page was opened: there is none, the plugin is not switched on");
}

/// A client of the page at `url`, as a browser is: the real socket, which the page's own code opens.
fn page_socket(url: &str) -> tungstenite::WebSocket<std::net::TcpStream> {
    let port: u16 = url.strip_prefix("http://127.0.0.1:").unwrap_or_else(|| panic!("an address on this machine: {url}")).trim_end_matches('/').parse().unwrap_or_else(|why| panic!("{url}: {why}"));
    let stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("the page is served");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let (socket, _) = tungstenite::client(format!("ws://127.0.0.1:{port}/ws"), stream).expect("the page takes the socket");
    socket
}

/// The next message that the page sends, as JSON.
fn heard(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> serde_json::Value {
    loop {
        match socket.read().expect("the page says something in ten seconds") {
            tungstenite::Message::Text(text) => return serde_json::from_str(text.as_str()).expect("JSON"),
            _ => continue,
        }
    }
}

/// What the page answers to the request `req_id`.
fn answer(socket: &mut tungstenite::WebSocket<std::net::TcpStream>, req_id: u64) -> serde_json::Value {
    loop {
        let message = heard(socket);
        if message["reqId"] == req_id {
            return message;
        }
    }
}

const OTHER_SESSION: &str = "[[instrument]]\nplugin = \"builtin:osc\"\nrange = \"C4-C5\"\n\n[master]\nlimiter = false\n";

#[test]
fn a_page_that_opens_another_session_has_the_host_called_back_and_the_engine_made_again_with_it_and_the_project_marked_changed() {
    let file = stand_in();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = so_far(file);
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());

    // The page: the switch that opens it is pressed, and the stand-in for the browser is given its address.
    assert!(rig.block_with(Some(1.0)).is_empty());
    rig.call_back();
    let url = opened(file, before + 1).get(before).cloned().expect("the browser was opened");
    assert_eq!(rig.block_with(None), [0.0], "the switch is put back");
    let asked = rig.callbacks();

    // A person picks a file in the page: it sends the text of the session as one request, and is told that it was taken.
    let mut socket = page_socket(&url);
    let hello = heard(&mut socket);
    assert!(hello["features"].as_array().unwrap().iter().any(|feature| feature == "openSession"), "the page is told that it can open a session: {hello}");
    socket.send(tungstenite::Message::text(serde_json::json!({"type": "request", "reqId": 1, "intent": {"type": "openSession", "text": OTHER_SESSION}}).to_string())).unwrap();
    assert_eq!(answer(&mut socket, 1)["type"], "ack");

    // The controller's thread asked the host to call back, and nothing is changed until the host does (the plugin's own session is what the host would save).
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.callbacks() == asked && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(rig.callbacks(), asked + 1, "the host was asked to call back, once");
    assert_eq!(rig.dirty_marks(), 0, "and has not been told that the project changed");
    assert!(!String::from_utf8_lossy(&rig.save()).contains("C4-C5"), "the session is as it was");

    // The host calls back: the engine is made again with the session, and the host is told that its project has changed.
    rig.call_back();
    assert_eq!(rig.dirty_marks(), 1, "the host is told that the project changed, once");
    let saved = String::from_utf8_lossy(&rig.save()).into_owned();
    assert!(saved.contains("range = \"C4-C5\""), "the session is the one that was opened:\n{saved}");
    // (The page that was served by the engine that is gone is gone.)
    assert!(socket.read().is_err() || socket.read().is_err(), "the old page's socket ends");

    // The new engine is served at another address, which the switch opens.
    assert!(rig.block_with(Some(1.0)).is_empty());
    rig.call_back();
    let urls = opened(file, before + 2);
    let now = urls.get(before + 1).cloned().expect("the browser was opened again");
    assert_ne!(now, url, "the page is at another address");
    // A call back with nothing to do does nothing: the project is not marked again.
    rig.call_back();
    assert_eq!(rig.dirty_marks(), 1);
    rig.stop();
}

#[test]
fn a_page_that_opens_a_text_that_is_not_a_session_is_refused_and_the_host_is_not_called_back() {
    let file = stand_in();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = so_far(file);
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(rig.block_with(Some(1.0)).is_empty());
    rig.call_back();
    let url = opened(file, before + 1).get(before).cloned().expect("the browser was opened");
    let asked = rig.callbacks();
    let mut socket = page_socket(&url);
    heard(&mut socket);
    socket.send(tungstenite::Message::text(serde_json::json!({"type": "request", "reqId": 1, "intent": {"type": "openSession", "text": "this is {not toml"}}).to_string())).unwrap();
    let refused = answer(&mut socket, 1);
    assert_eq!(refused["type"], "rejected", "{refused}");
    assert!(refused["error"]["message"].as_str().unwrap().starts_with("that is not a session: "), "{refused}");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.callbacks(), asked, "the host was not asked to call back");
    rig.call_back();
    assert_eq!(rig.dirty_marks(), 0, "and the project is as it was");
    rig.stop();
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn a_window_follows_the_instance_to_the_address_that_a_session_opened_by_its_page_is_served_at() {
    tang_clap::gui::use_maker_on_this_thread(fake_maker);
    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("the plugin has a window");
    gui.create(&mut handle, embedded()).expect("an embedded window is made");
    gui.set_scale(&mut handle, 1.0).unwrap();
    // SAFETY: the window is a pointer that nothing is done with: the view of this test does not put itself in it.
    unsafe { gui.set_parent(&mut handle, host_window()) }.expect("the view is made in the host's window");
    gui.show(&mut handle).expect("and shown");
    let processor = switch_on(&mut instance);
    let told = said();
    let first = told.iter().find_map(|line| line.strip_prefix("load ")).unwrap_or_else(|| panic!("the page of the instance: {told:?}")).to_string();

    // The page, in the window, opens another session (a person picked a file): the plugin asks the host to call back.
    let mut socket = page_socket(&first);
    heard(&mut socket);
    socket.send(tungstenite::Message::text(serde_json::json!({"type": "request", "reqId": 1, "intent": {"type": "openSession", "text": OTHER_SESSION}}).to_string())).unwrap();
    assert_eq!(answer(&mut socket, 1)["type"], "ack");
    let callbacks = |instance: &PluginInstance<Handlers>| instance.access_shared_handler(|shared| shared.callbacks.load(std::sync::atomic::Ordering::SeqCst));
    let deadline = Instant::now() + Duration::from_secs(10);
    while callbacks(&instance) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(callbacks(&instance), 1, "the host was asked to call back");
    assert!(said().is_empty(), "the window waits for the host: nothing is done on the controller's thread");

    // The host calls back: the engine is made again, and the window goes to the new address.
    instance.call_on_main_thread_callback();
    let told = said();
    assert_eq!(told.len(), 1, "{told:?}");
    let second = told[0].strip_prefix("load ").unwrap_or_else(|| panic!("the page of the instance: {told:?}"));
    assert_ne!(second, first, "another address");
    assert!(second.starts_with("http://127.0.0.1:"), "{second}");

    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().unwrap();
    gui.destroy(&mut handle);
    instance.deactivate(processor.stop_processing());
}
