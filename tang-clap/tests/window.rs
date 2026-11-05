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

#[test]
fn the_window_is_the_page_of_the_instance_which_the_host_asks_to_show_and_which_opens_in_the_browser() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("opened.txt");
    // SAFETY: this is the one test of this program, and nothing else reads the environment while it runs.
    unsafe { std::env::set_var("TANG_BROWSER", opener(dir.path(), &file)) };

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
    assert!(!file.exists(), "nothing was opened");

    // Switched on, it shows the page: the browser is opened on an address on this machine, which answers.
    let processor = instance
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: RATE, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap();
    let mut handle = instance.plugin_handle();
    gui.show(&mut handle).expect("the window is shown");
    let url = opened(&file, 1).into_iter().next().expect("the browser was opened");
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
    assert_eq!(opened(&file, 2), [url.clone(), url], "the browser is opened again on the same address (and the test waits for it: the program that opens it is not left to find its directory gone)");
    gui.destroy(&mut handle);
    instance.deactivate(processor.stop_processing());
}
