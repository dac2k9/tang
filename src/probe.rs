//! A check that is run from the command line and plays nothing: what a plugin's own window is like on this machine (`tang window`, step 112; a CLAP plugin's too, step 114). (`tang listen` is the other of its kind, and is with the audio input that it checks.)

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};

use crate::plugin::editors;
use crate::{plugin, window};

/// What `tang window` is asked to do besides open the window.
pub struct WindowOptions<'a> {
    /// How long the window stays open; with none, until the person closes it.
    pub seconds: Option<f32>,
    /// Where a picture of what is in the window is written, a moment before it is closed.
    pub snapshot: Option<&'a Path>,
    /// Whether the window takes the keyboard from the terminal when it opens.
    pub focus: bool,
    /// Whether the views that the plugin made in the window are printed, a moment before it is closed.
    pub tree: bool,
    /// The size that the window is made once it is open, in points: what a host does that has kept the size that a person gave it (a CLAP plugin: a VST3 plugin's window is its view's).
    pub size: Option<(u32, u32)>,
}

/// Open the window of the plugin `source`, as `options` say, and say what it was.
pub fn window(source: &str, options: WindowOptions, sample_rate: u32) -> anyhow::Result<()> {
    // (Before anything is loaded: a session that has no screen is said so, and the plugin is not made to wait for nothing.)
    window::available().map_err(|why| anyhow!(why))?;
    // The plugin is loaded as `describe` loads it, on this thread, which is the main one: the window and the plugin's controller are its. No sound is made, and none is needed to draw a plugin's face.
    let plugin = plugin::load(source, sample_rate as f32, 512, &plugin::Runtime::default())?;
    let name = plugin.name().to_string();
    let editor = plugin.editor_id().ok_or_else(|| anyhow!("{name} has no window of its own that Tang can show (it has no `gui` extension, if it is a CLAP plugin, or no view, if it is a VST3 plugin; the other formats have none yet)"))?;
    editors::open(editor, &name, options.focus, &[]).map_err(|why| anyhow!("{name}: {why}"))?;
    let (width, height) = editors::with_open(editor, |open| open.size()).unwrap_or_default();
    println!("{name}: a window of {width} x {height}");
    // A host that has kept the size that a person gave the window makes it that, and tells the plugin.
    if let Some((wanted_width, wanted_height)) = options.size {
        match editors::with_open(editor, |open| open.resize(i32::try_from(wanted_width).unwrap_or(i32::MAX), i32::try_from(wanted_height).unwrap_or(i32::MAX))) {
            Some(Ok(())) => {
                let (width, height) = editors::with_open(editor, |open| open.size()).unwrap_or_default();
                println!("The window is {width} x {height}");
            }
            Some(Err(why)) => println!("(--size: {why})"),
            None => println!("(--size: the window is closed)"),
        }
    }

    let started = Instant::now();
    let limit = options.seconds.map(Duration::from_secs_f32);
    let still_open = loop {
        editors::poll();
        // What the person does to the plugin's knobs is said as it comes (there is no session to apply it to: it shows that the plugin tells the host).
        for (_, index, value) in editors::take_edits() {
            println!("The plugin says that its parameter {index} is {value}");
        }
        if !editors::is_open(editor) {
            break false;
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            break true;
        }
        window::pump(window::PUMP_TURN);
    };

    if still_open && options.tree {
        let tree = editors::with_open(editor, |open| open.describe()).unwrap_or_default();
        println!("What the plugin made in the window:\n{}", tree.trim_end());
    }
    if let Some(path) = options.snapshot {
        if !still_open {
            println!("(the window was closed before there was a picture of it to take)");
        } else if let Some(png) = editors::with_open(editor, |open| open.snapshot_png()).flatten() {
            std::fs::write(path, &png).with_context(|| format!("writing {}", path.display()))?;
            println!("Snapshot: {} ({} bytes)", path.display(), png.len());
        } else {
            println!("(what is in the window cannot be had as a picture)");
        }
    }
    editors::close(editor);
    let seen = started.elapsed().as_secs_f32();
    if still_open {
        println!("Closed after {seen:.1} s.");
    } else {
        println!("The window was closed after {seen:.1} s.");
    }
    // The plugin goes after its window: letting go of it forgets its editor too.
    drop(plugin);
    Ok(())
}
