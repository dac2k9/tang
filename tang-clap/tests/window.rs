//! The plugin's window: the host asks for a floating window, and the plugin's window is the page of this instance, opened in the browser that the person has. A test of its own, because it says which program opens the page by
//! an environment variable (`TANG_BROWSER`), which no other test of the process should see.

mod common;

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use clack_extensions::gui::{GuiApiType, GuiConfiguration, PluginGui};
use clack_host::prelude::*;
use common::*;

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

#[test]
fn the_window_is_the_page_of_the_instance_which_the_host_asks_to_show_and_which_opens_in_the_browser() {
    let file = stand_in();
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = so_far(file);

    let bundle = bundle();
    let mut instance = instance(&bundle, instrument_id());
    let mut handle = instance.plugin_handle();
    let gui = handle.get_extension::<PluginGui>().expect("the plugin has a window");

    // The window is a floating one, which the plugin shows itself: one that is to be put in a window of the host's is refused.
    let preferred = gui.get_preferred_api(&mut handle).expect("it has a preferred api");
    assert!(preferred.is_floating, "{preferred:?}");
    let floating = GuiConfiguration { api_type: GuiApiType::default_for_current_platform().unwrap(), is_floating: true };
    let embedded = GuiConfiguration { is_floating: false, ..floating };
    assert!(gui.is_api_supported(&mut handle, floating));
    assert!(!gui.is_api_supported(&mut handle, embedded), "Tang's window is not put in the host's");
    assert!(gui.create(&mut handle, embedded).is_err());
    gui.create(&mut handle, floating).expect("a floating window is made");
    assert!(gui.get_size(&mut handle).is_none(), "it has no size of its own that the host could give it");

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
