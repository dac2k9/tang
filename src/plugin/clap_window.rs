//! The window of a CLAP plugin, for `tang window` (step 114): the plugin's `gui` extension, put in a window of Tang's own.
//!
//! It is a check that is run from the command line and plays nothing, like the one for a VST3 plugin (`probe`): the plugin is loaded and **activated** (a plugin such as Tang starts its page when it is switched on), asked for a window
//! that goes in the host's (`create` for the system's own API, `set_scale`, `get_size`, `can_resize`), given the content view of a window of ours (`set_parent`) and shown. The window's events are turned by the caller's loop
//! (`window::pump`), the size that a person drags the window's edge to is told to the plugin (`set_size`, after `adjust_size`), and what the plugin asks of the host (`request_resize`, `request_show`, `request_hide`, `closed`)
//! is answered from that loop too.
//!
//! This is not the registry of the windows (`plugin::editors`) that VST3 plugins have in a session: a CLAP plugin that is in a session is in the hands of the audio thread (`ClapPlugin` is moved to it, by value), and its `gui`
//! extension is the main thread's, so a button on its node needs the main thread to hold a handle that this crate does not give it yet.

use std::ffi::CString;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, HostGui, HostGuiImpl, PluginGui, Window as ClapWindow};
use clack_host::prelude::*;

use crate::probe::WindowOptions;
use crate::window;

/// What a plugin asks of the host while its window is open: written down by whichever thread the plugin asks from, and answered by the loop that turns the window's events.
#[derive(Default)]
struct Asked {
    /// The size that the plugin would like the host's window to be.
    resize: Mutex<Option<GuiSize>>,
    /// The plugin has closed its window, or lost it.
    closed: AtomicBool,
}

struct ProbeHost;
struct ProbeShared(std::sync::Arc<Asked>);
struct ProbeMainThread;

impl HostHandlers for ProbeHost {
    type Shared<'a> = ProbeShared;
    type MainThread<'a> = ProbeMainThread;
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostGui>();
    }
}

impl<'a> SharedHandler<'a> for ProbeShared {
    fn request_restart(&self) {
        log::debug!("The plugin asked for a restart (ignored)");
    }

    fn request_process(&self) {
        log::debug!("The plugin asked for a process (ignored)");
    }

    fn request_callback(&self) {
        log::debug!("The plugin asked for a callback (ignored)");
    }
}

impl<'a> MainThreadHandler<'a> for ProbeMainThread {}

impl HostGuiImpl for ProbeShared {
    fn resize_hints_changed(&self) {
        log::debug!("The plugin's resize hints changed (ignored)");
    }

    fn request_resize(&self, new_size: GuiSize) -> Result<(), HostError> {
        *self.0.resize.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(new_size);
        Ok(())
    }

    fn request_show(&self) -> Result<(), HostError> {
        // (The window is shown from the start and stays.)
        Ok(())
    }

    fn request_hide(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, was_destroyed: bool) {
        log::debug!("The plugin closed its window (destroyed: {was_destroyed})");
        self.0.closed.store(true, Ordering::Release);
    }
}

/// What the window of a plugin that has none says, and a machine that cannot put it in the host's.
const NO_WINDOW: &str = "has no window that can be put in a window of Tang's";

/// The parent that the plugin is given: the window's content view, as the system's own kind of window.
fn parent_of(content: *mut std::ffi::c_void) -> anyhow::Result<ClapWindow<'static>> {
    #[cfg(target_os = "macos")]
    return Ok(ClapWindow::from_cocoa_nsview(content));
    #[cfg(windows)]
    return Ok(ClapWindow::from_win32_hwnd(content));
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = content;
        Err(anyhow!("Tang has no window for a CLAP plugin to be put in on this system"))
    }
}

/// Open the window of the CLAP plugin `source` (a path to a `.clap` file, which is its first plugin, or `clap:<id>`), as `options` say, and say what it was.
pub fn window(source: &str, options: &WindowOptions, sample_rate: u32) -> anyhow::Result<()> {
    // (Before anything is loaded: a session that has no screen is said so, and the plugin is not made to wait for nothing.)
    window::available().map_err(|why| anyhow!(why))?;
    let (bundle, id, name, _) = super::clap::find_plugin(source)?;
    let host_info = HostInfo::new("tang window", "tang", "https://github.com/apanloco/tang", env!("CARGO_PKG_VERSION"))?;
    let id = CString::new(id.as_str()).context("the plugin's id has a NUL in it")?;
    let asked = std::sync::Arc::new(Asked::default());
    let shared = asked.clone();
    let mut instance = PluginInstance::<ProbeHost>::new(move |_| ProbeShared(shared), |_| ProbeMainThread, &bundle, &id, &host_info).map_err(|why| anyhow!("{name} could not be made: {why}"))?;

    let gui: PluginGui = instance.plugin_shared_handle().get_extension().ok_or_else(|| anyhow!("{name} has no window of its own (its `gui` extension)"))?;
    // Switched on, without any audio being asked of it: what it makes of that (Tang starts its page, and a plugin that shows its state shows it) is what its window is of.
    let stopped = instance
        .activate(|_, _| (), PluginAudioConfiguration { sample_rate: f64::from(sample_rate), min_frames_count: 1, max_frames_count: 512 })
        .map_err(|why| anyhow!("{name} could not be switched on: {why}"))?;

    let mut handle = instance.plugin_handle();
    let api_type = GuiApiType::default_for_current_platform().ok_or_else(|| anyhow!("this system has no window API that CLAP knows"))?;
    let configuration = GuiConfiguration { api_type, is_floating: false };
    if !gui.is_api_supported(&mut handle, configuration) {
        return Err(anyhow!("{name} {NO_WINDOW} (it asks for a floating window that it shows itself, or has none for this system)"));
    }
    gui.create(&mut handle, configuration).map_err(|why| anyhow!("{name}: the window could not be made: {why}"))?;
    // (macOS counts points and has already scaled them, as every host there does: the scale is 1.)
    let _ = gui.set_scale(&mut handle, 1.0);
    let size = gui.get_size(&mut handle).unwrap_or(GuiSize { width: 640, height: 480 });
    let resizable = gui.can_resize(&mut handle);
    let width = i32::try_from(size.width).unwrap_or(640);
    let height = i32::try_from(size.height).unwrap_or(480);
    let host_window = window::open(&window::Spec { title: &name, width, height, resizable, focus: options.focus }).map_err(|why| anyhow!(why))?;

    // SAFETY: the content view is the window's, which is kept until after `destroy` below, and is the main thread's, which is where this runs.
    let attached = unsafe { gui.set_parent(&mut handle, parent_of(host_window.parent())?) };
    if let Err(why) = attached {
        gui.destroy(&mut handle);
        host_window.close();
        return Err(anyhow!("{name}: the window could not be put in Tang's: {why}"));
    }
    gui.show(&mut handle).map_err(|why| anyhow!("{name}: the window could not be shown: {why}"))?;
    println!("{name}: a window of {width} x {height}{}", if resizable { ", which it can be resized" } else { "" });

    // A host that has kept the size that a person gave the window makes it that, and tells the plugin.
    if let Some((wanted_width, wanted_height)) = options.size {
        let wanted = GuiSize { width: wanted_width, height: wanted_height };
        let size = gui.adjust_size(&mut handle, wanted).unwrap_or(wanted);
        host_window.set_size(i32::try_from(size.width).unwrap_or(width), i32::try_from(size.height).unwrap_or(height));
        match gui.set_size(&mut handle, size) {
            Ok(()) => println!("The window is {} x {}", size.width, size.height),
            Err(why) => println!("The plugin did not take a window of {} x {}: {why}", size.width, size.height),
        }
    }

    let started = Instant::now();
    let limit = options.seconds.map(Duration::from_secs_f32);
    let mut told = host_window.size();
    let still_open = loop {
        window::pump(window::PUMP_TURN);
        if !host_window.is_open() || asked.closed.load(Ordering::Acquire) {
            break false;
        }
        // What the plugin asked of the window's size.
        if let Some(wanted) = asked.resize.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            host_window.set_size(i32::try_from(wanted.width).unwrap_or(width), i32::try_from(wanted.height).unwrap_or(height));
        }
        // What a person made of it by dragging its edge: the plugin is told once for each size, the closest that it can be.
        let now = host_window.size();
        if now != told {
            told = now;
            let wanted = GuiSize { width: u32::try_from(now.0).unwrap_or(0), height: u32::try_from(now.1).unwrap_or(0) };
            let size = gui.adjust_size(&mut handle, wanted).unwrap_or(wanted);
            if size != wanted {
                host_window.set_size(i32::try_from(size.width).unwrap_or(now.0), i32::try_from(size.height).unwrap_or(now.1));
                told = host_window.size();
            }
            match gui.set_size(&mut handle, size) {
                Ok(()) => println!("The window is {} x {}", size.width, size.height),
                Err(why) => println!("The plugin did not take a window of {} x {}: {why}", size.width, size.height),
            }
        }
        if limit.is_some_and(|limit| started.elapsed() >= limit) {
            break true;
        }
    };

    if still_open && options.tree {
        println!("What is in the window:\n{}", host_window.describe().trim_end());
    }
    if let Some(path) = options.snapshot {
        if !still_open {
            println!("(the window was closed before there was a picture of it to take)");
        } else if let Some(png) = host_window.snapshot_png() {
            std::fs::write(path, &png).with_context(|| format!("writing {}", path.display()))?;
            println!("Snapshot: {} ({} bytes)", path.display(), png.len());
        } else {
            println!("(what is in the window cannot be had as a picture)");
        }
    }
    let _ = gui.hide(&mut handle);
    gui.destroy(&mut handle);
    host_window.close();
    let seen = started.elapsed().as_secs_f32();
    if still_open {
        println!("Closed after {seen:.1} s.");
    } else {
        println!("The window was closed after {seen:.1} s.");
    }
    // The plugin goes after its window: switched off, and then let go of.
    instance.deactivate(stopped);
    Ok(())
}
