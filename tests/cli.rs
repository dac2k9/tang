//! The program as it is run from a shell: what it prints, and the status it ends with.
//!
//! `main` ends the process itself, after everything has been shut down in order (see `main.rs`), so
//! that what it says and the status it leaves are the program's to keep: these tests hold them.

use std::process::{Command, Output};

fn tang(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tang")).args(args).output().expect("tang runs")
}

#[test]
fn a_command_that_works_says_all_it_has_to_say_and_ends_with_status_0() {
    let output = tang(&["describe", "builtin:osc"]);
    let said = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{said}{}", String::from_utf8_lossy(&output.stderr));
    assert!(said.starts_with("Oscillator\n"), "{said}");
    assert!(said.ends_with("  Presets:       (none)\n"), "the last line of it is there: {said}");
}

#[test]
fn describe_says_how_late_a_plugin_is() {
    // The drive is late by the 23 frames of its oversampling filters (0.48 ms at 48 kHz); the oscillator is not late.
    let said = |plugin: &str| String::from_utf8_lossy(&tang(&["describe", plugin]).stdout).to_string();
    assert!(said("builtin:drive").contains("\n  Latency:       23 frames (0.48 ms at 48000 Hz)\n"), "{}", said("builtin:drive"));
    assert!(said("builtin:osc").contains("\n  Latency:       none\n"), "{}", said("builtin:osc"));
}

#[test]
fn describe_says_which_effect_has_a_second_input_and_what_it_calls_the_two() {
    let said = |plugin: &str| String::from_utf8_lossy(&tang(&["describe", plugin]).stdout).to_string();
    assert!(said("builtin:math").contains("\n  Audio inputs:  two: A (what the chain feeds) and B (cables only)\n"), "{}", said("builtin:math"));
    assert!(said("builtin:math").contains("[0] mode (ring|am|fold|warp|compare|min|max|xor|and|or|hold|follow, default=ring)"), "{}", said("builtin:math"));
    assert!(!said("builtin:filter").contains("Audio inputs"), "{}", said("builtin:filter"));
}

#[test]
fn describe_says_when_a_parameter_has_an_effect_if_it_has_one_only_sometimes() {
    let said = |plugin: &str| String::from_utf8_lossy(&tang(&["describe", plugin]).stdout).to_string();
    let math = said("builtin:math");
    // (The parameters of the Math that depend on its mode or on what is patched at B say so; the mode and the level, which always have one, do not.)
    for line in [
        "[1] amount (min=0, max=1, default=0.5): only in the modes am, fold, warp, compare, hold, follow\n",
        "[2] bits (1|2|3|4|5|6|7|8|9|10|11|12, default=6): only in the modes xor, and, or\n",
        "[3] pitch (min=-48, max=120, default=57): only while nothing is patched at B\n",
        "[4] wave (sine|triangle|saw|square, default=sine): only while nothing is patched at B\n",
        "[5] attack (min=0.0005, max=0.2, default=0.005): only in the mode follow\n",
        "[6] release (min=0.005, max=1.5, default=0.12): only in the mode follow\n",
        "[7] level (min=0, max=1, default=0.7)\n",
    ] {
        assert!(math.contains(line), "{line:?} is not in: {math}");
    }
    assert!(math.contains("follow, default=ring)\n"), "the mode has no condition: {math}");
    assert!(!said("builtin:filter").contains("only "), "a plugin that says nothing says nothing: {}", said("builtin:filter"));
}

#[test]
fn describe_says_that_the_audio_input_is_an_instrument_with_a_gain_a_pan_and_a_device_to_listen_to() {
    // (The names of the devices are the machine's, so the choice is looked at by its first label and its default only.)
    let said = String::from_utf8_lossy(&tang(&["describe", "builtin:input"]).stdout).to_string();
    for line in ["Audio Input\n", "\n  Type:          instrument\n", "\n  Audio outputs: 2\n", "\n  Latency:       none\n", "\n    [0] gain (min=0, max=4, default=1)\n", "\n    [1] pan (min=-1, max=1, default=0)\n", "\n    [2] device (Default input", ", default=Default input)\n", "\n  Presets:       (none)\n"] {
        assert!(said.contains(line), "{line:?} is not in: {said}");
    }
}

#[test]
fn enumerate_builtins_lists_the_audio_input_after_the_oscillator_as_an_instrument() {
    let said = String::from_utf8_lossy(&tang(&["enumerate", "builtins"]).stdout).to_string();
    let osc = said.find("[instrument] Oscillator").expect("the oscillator is listed");
    let input = said.find("[instrument] Audio Input").expect("the audio input is listed");
    assert!(osc < input, "{said}");
    assert!(said[input..].starts_with("[instrument] Audio Input (") && said[input..].contains("ID:      builtin:input\n          Params:  3\n          Presets: 0\n"), "{said}");
}

#[test]
fn listen_to_an_input_that_is_not_there_says_so_and_ends_with_status_1_having_opened_nothing() {
    // (`tang listen` plays nothing and opens the microphone to say what it hears; a name that no input has opens nothing, so that nothing is heard and no permission is asked.)
    let output = tang(&["listen", "no such input, not a device", "--seconds", "0.1"]);
    assert_eq!(output.status.code(), Some(1));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.starts_with("Error: could not open the input: "), "{said}");
    let told = String::from_utf8_lossy(&output.stdout);
    assert_eq!(told, "Listening to \"no such input, not a device\" for 0.1 s (nothing is played) ...\n");
}

#[test]
fn a_command_that_fails_says_why_on_stderr_and_ends_with_status_1() {
    let output = tang(&["describe", "builtin:nope"]);
    assert_eq!(output.status.code(), Some(1));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.starts_with("Error: Unknown built-in plugin: \"nope\"\n"), "{said}");
    assert!(output.stdout.is_empty(), "it printed nothing but the error: {}", String::from_utf8_lossy(&output.stdout));
}

#[test]
fn a_command_line_that_is_not_one_says_so_and_ends_with_status_2() {
    let output = tang(&["--nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--nope'"));
}

#[test]
fn window_for_a_plugin_that_has_no_window_says_so_and_ends_with_status_1_having_opened_nothing() {
    // (A built-in plugin has no face of its own. On a machine that has no screen, or none that Tang can open a window on, it is that which is said first, and the status is the same.)
    let output = tang(&["window", "builtin:osc", "--seconds", "0.1", "--no-focus"]);
    assert_eq!(output.status.code(), Some(1));
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(said.starts_with("Error: "), "{said}");
    assert!(
        said.contains("Oscillator has no window of its own") || said.contains("has no screen") || said.contains("macOS only"),
        "{said}"
    );
    assert!(output.stdout.is_empty(), "it printed nothing but the error: {}", String::from_utf8_lossy(&output.stdout));
}

/// Opens the window of a plugin on the screen for a second, and takes a picture of it: for a person at a Mac that has a VST3 plugin installed, since a window is not for a machine that has no screen and no plugin.
/// `cargo test --test cli -- --ignored`.
#[test]
#[cfg(target_os = "macos")]
#[ignore = "opens a plugin's window on the screen for a second: cargo test --test cli -- --ignored"]
fn window_opens_a_plugin_that_is_installed_and_takes_a_picture_of_it_and_closes_it() {
    let mut bundles: Vec<_> = std::fs::read_dir("/Library/Audio/Plug-Ins/VST3")
        .map(|entries| entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "vst3")).collect())
        .unwrap_or_default();
    bundles.sort();
    let Some(bundle) = bundles.first() else {
        eprintln!("skipped: no VST3 plugin is installed");
        return;
    };
    let dir = tempfile::tempdir().expect("a directory");
    let picture = dir.path().join("window.png");
    let output = tang(&["window", &bundle.to_string_lossy(), "--seconds", "1", "--no-focus", "--tree", "--snapshot", &picture.to_string_lossy()]);
    let said = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    if err.contains("has no screen") {
        eprintln!("skipped: {err}");
        return;
    }
    assert_eq!(output.status.code(), Some(0), "{said}{err}");
    assert!(said.contains(": a window of "), "{said}");
    assert!(said.contains("What the plugin made in the window:\nNSView "), "{said}");
    assert!(said.contains("Snapshot: ") && said.trim_end().ends_with(" s."), "{said}");
    let png = std::fs::read(&picture).expect("the picture was written");
    assert!(png.starts_with(b"\x89PNG"), "{:?}", &png[..8.min(png.len())]);
}
