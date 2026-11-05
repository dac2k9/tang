//! A check that is run from the command line and plays nothing: what a plugin's own window is like on this machine (`tang window`, step 112). (`tang listen` is the other of its kind, and is with the audio input that it checks.)

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};

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
}

/// Open the window of the plugin `source`, as `options` say, and say what it was.
pub fn window(source: &str, options: WindowOptions, sample_rate: u32) -> anyhow::Result<()> {
    // (Before anything is loaded: a session that has no screen is said so, and the plugin is not made to wait for nothing.)
    window::available().map_err(|why| anyhow!(why))?;
    // The plugin is loaded as `describe` loads it, on this thread, which is the main one: the window and the plugin's controller are its. No sound is made, and none is needed to draw a plugin's face.
    let plugin = plugin::load(source, sample_rate as f32, 512, &plugin::Runtime::default())?;
    let name = plugin.name().to_string();
    let editor = plugin.editor().ok_or_else(|| anyhow!("{name} has no window of its own that Tang can show (its plugin format has none yet)"))?;
    let mut open = editor.open(&name, options.focus).map_err(|why| anyhow!("{name}: {why}"))?;
    let (width, height) = open.size();
    println!("{name}: a window of {width} x {height}");

    let started = Instant::now();
    let limit = options.seconds.map(Duration::from_secs_f32);
    let still_open = loop {
        if !open.poll() {
            break false;
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            break true;
        }
        window::pump(window::PUMP_TURN);
    };

    if still_open && options.tree {
        println!("What the plugin made in the window:\n{}", open.describe().trim_end());
    }
    if let Some(path) = options.snapshot {
        if !still_open {
            println!("(the window was closed before there was a picture of it to take)");
        } else if let Some(png) = open.snapshot_png() {
            std::fs::write(path, &png).with_context(|| format!("writing {}", path.display()))?;
            println!("Snapshot: {} ({} bytes)", path.display(), png.len());
        } else {
            println!("(what is in the window cannot be had as a picture)");
        }
    }
    open.close();
    let seen = started.elapsed().as_secs_f32();
    if still_open {
        println!("Closed after {seen:.1} s.");
    } else {
        println!("The window was closed after {seen:.1} s.");
    }
    // The window goes before the plugin that it is a window of: a view of a plugin that is gone is a view of nothing.
    drop(open);
    drop(editor);
    drop(plugin);
    Ok(())
}
