#![allow(clippy::collapsible_if)]

#[cfg(test)]
mod alloc_guard;
mod app;
mod audio;
mod chord;
mod cli;
mod config;
mod enumerate;
mod held_notes;
mod ids;
mod midi;
mod midi_monitor;
mod note_input;
#[cfg(test)]
mod oracle;
mod piano;
mod piano_filter;
mod plugin;
mod scale;
mod session;
mod source;
mod telemetry;
mod tty;
mod tui;
#[cfg(feature = "web")]
mod web;

use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use clap::Parser;
use cli::{Cli, Command, EnumerateTarget};

/// Convert a MIDI note number to a human-readable name (e.g. 60 → "C4").
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) as i8 - 1;
    let name = NAMES[(note % 12) as usize];
    format!("{name}{octave}")
}
use crossterm::event::{
    self, Event, KeyCode, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};

fn main() {
    let result = run();
    let code = match result {
        Ok(()) => 0,
        // (What returning the error from `main` would say.)
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    };
    // `run` has stopped everything that is ours, in order. What is not is the native code of the plugins, which the scan of
    // the installed ones may still be in, on a thread of its own (a quit a moment after the start finds it there): the
    // process's ordinary exit runs what those libraries have to run at exit while that thread is in them, and the
    // program crashed on the way out about one time in twenty. The process ends here instead, at once: what it has to say is
    // flushed first, as an exit that is not this would.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    // Safety: `_exit` ends the process; nothing after it runs, and nothing that matters is left to do.
    unsafe {
        libc::_exit(code)
    }
    #[cfg(not(unix))]
    std::process::exit(code)
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Err(e) = cli.check() {
        e.exit();
    }
    // What the terminal interface is to serve to browsers (`--web`), if it is to.
    let tui_web = cli.tui_web();

    // Load application config (extra plugin paths, etc.)
    if let Ok(config_dir) = dirs_config() {
        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(text) => match toml::from_str::<config::Config>(&text) {
                    Ok(cfg) => config::init(cfg),
                    Err(e) => eprintln!("Warning: failed to parse {}: {e}", config_path.display()),
                },
                Err(e) => eprintln!("Warning: failed to read {}: {e}", config_path.display()),
            }
        }
    }

    // Set LV2_PATH for extra LV2 search directories (before any LV2 world is created)
    let extra_lv2 = config::extra_lv2_paths();
    if !extra_lv2.is_empty() {
        let current = std::env::var("LV2_PATH").unwrap_or_default();
        let extra: String = extra_lv2
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        let new = if current.is_empty() {
            extra
        } else {
            format!("{extra}:{current}")
        };
        // Safety: called before any threads are spawned.
        unsafe {
            std::env::set_var("LV2_PATH", new);
        }
    }

    let Cli {
        session,
        audio_device,
        jack,
        midi_device,
        buffer_size,
        sample_rate,
        debug,
        command,
        ..
    } = cli;

    match command {
        None => {
            #[cfg(not(feature = "web"))]
            if tui_web.is_some() {
                anyhow::bail!("this build of tang has no web interface: build it with `--features web`");
            }
            // A port that is taken is better found out now than after the session has loaded.
            #[cfg(feature = "web")]
            if let Some(args) = &tui_web {
                web::preflight(args)?;
            }
            run_session(session, audio_device, jack, midi_device, buffer_size, sample_rate, debug, Mode::Tui(tui_web))
        }
        Some(Command::Enumerate(target)) => {
            env_logger::init();
            match target {
                EnumerateTarget::Midi => enumerate::midi(),
                EnumerateTarget::Audio => enumerate::audio(),
                EnumerateTarget::Plugins => enumerate::plugins(),
                EnumerateTarget::Builtins => enumerate::builtins(),
            }
        }
        Some(Command::Describe { plugin: source }) => {
            env_logger::init();
            let p = plugin::load(&source, 48000.0, 512, &plugin::Runtime::default())?;
            println!("{}", p.name());
            println!(
                "  Type:          {}",
                if p.is_instrument() {
                    "instrument"
                } else {
                    "effect"
                }
            );
            println!("  Audio outputs: {}", p.audio_output_count());
            // An effect that has a second input says what it calls the two (a cable can end at either: see "An effect with a second input").
            if let Some(inputs) = p.side_input() {
                println!("  Audio inputs:  two: {} (what the chain feeds) and {} (cables only)", inputs.main, inputs.side);
            }
            // (The sample rate that the plugin is loaded at above, which is what its latency is in frames of.)
            match p.latency() {
                0 => println!("  Latency:       none"),
                frames => println!("  Latency:       {frames} frames ({:.2} ms at 48000 Hz)", f64::from(frames) * 1000.0 / 48000.0),
            }
            let params = p.parameters();
            println!("  Parameters:    {}", params.len());
            for param in &params {
                // A parameter that has an effect only sometimes says when.
                let when = param.used_when.as_ref().map(|used| format!(": {}", used.why)).unwrap_or_default();
                if let Some(labels) = &param.labels {
                    println!(
                        "    [{}] {} ({}, default={}){when}",
                        param.index,
                        param.name,
                        labels.join("|"),
                        labels
                            .get(param.default.round() as usize)
                            .map(|s| s.as_str())
                            .unwrap_or("?"),
                    );
                } else {
                    println!(
                        "    [{}] {} (min={}, max={}, default={}){when}",
                        param.index, param.name, param.min, param.max, param.default
                    );
                }
            }
            let presets = p.presets();
            if presets.is_empty() {
                println!("  Presets:       (none)");
            } else {
                println!("  Presets:       {}", presets.len());
                for preset in &presets {
                    println!("    {} ({})", preset.name, preset.id);
                }
            }
            Ok(())
        }
        Some(Command::Listen { device, seconds }) => {
            env_logger::init();
            plugin::builtin::listen_to_the_input(device.as_deref(), seconds, sample_rate)
        }
        Some(Command::Play { session: play_session }) => {
            run_session(play_session, audio_device, jack, midi_device, buffer_size, sample_rate, debug, Mode::Play)
        }
        #[cfg(feature = "web")]
        Some(Command::Serve { session: serve_session, web }) => {
            web::preflight(&web)?;
            run_session(serve_session, audio_device, jack, midi_device, buffer_size, sample_rate, debug, Mode::Serve(web))
        }
        #[cfg(not(feature = "web"))]
        Some(Command::Serve { .. }) => {
            anyhow::bail!("this build of tang has no web interface: build it with `--features web`")
        }
    }
}

/// What a session is run for.
enum Mode {
    /// The terminal interface, which also serves the session to browsers if it has
    /// the arguments for that (`--web`).
    Tui(Option<cli::WebArgs>),
    /// No interface: the virtual piano and the MIDI devices play the session.
    Play,
    /// No interface at all: a browser controls the session.
    #[cfg(feature = "web")]
    Serve(cli::WebArgs),
}

/// Custom logger that writes to stderr with \r\n line endings for raw mode.
struct RawModeLogger;

impl log::Log for RawModeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs() % 86400; // time of day
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            let ms = now.subsec_millis();
            let _ = write!(
                std::io::stderr(),
                "[{h:02}:{m:02}:{s:02}.{ms:03} {}] {}\r\n",
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

static RAW_MODE_LOGGER: RawModeLogger = RawModeLogger;

/// Create a default in-memory session config and a path for saving later.
/// The file is NOT written to disk — only created on Ctrl+S.
fn default_session() -> anyhow::Result<(session::SessionConfig, std::path::PathBuf)> {
    let dir = dirs_config_sessions()?;

    // Generate a short unique ID from the current timestamp.
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let id = format!("{:x}", ts.as_millis());
    let path = dir.join(format!("session-{id}.toml"));

    let config = session::SessionConfig {
        instruments: vec![session::InstrumentSlotConfig {
            range: None,
            transpose: 0,
            muted: false,
            solo: false,
            route: session::Route::Out,
            instrument: Some(session::PluginConfig {
                plugin: "builtin:osc".into(),
                preset: None,
                volume: 1.0,
                pitch_bend_range: 2.0,
                remap: Default::default(),
                params: Default::default(),
                pinned: Vec::new(),
            }),
            effects: vec![],
            modulators: vec![],
            pattern: None,
            group: None,
            ui: None,
        }],
        groups: vec![],
        piano: None,
        master_ui: None,
        master: None,
        bpm: None,
        cables: vec![],
        keys: None,
        devices: vec![],
        free_modulators: vec![],
        modules: vec![],
    };

    log::info!("New session (will save to {} on Ctrl+S)", path.display());
    Ok((config, path))
}

/// Where the library of modules is: `TANG_MODULES_DIR` if it says, else `modules` in the config directory (`~/.config/tang/modules`). None if neither can be found.
fn modules_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("TANG_MODULES_DIR") {
        Some(dir) if !dir.is_empty() => Some(std::path::PathBuf::from(dir)),
        _ => dirs_config().ok().map(|config| config.join("modules")),
    }
}

fn dirs_config_sessions() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs_config()?;
    Ok(config.join("sessions"))
}

fn dirs_config() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Path::new(&home).join(".config").join("tang"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(Path::new(&config).join("tang"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Path::new(&home).join(".config").join("tang"));
        }
    }
    anyhow::bail!("could not determine config directory")
}

#[allow(clippy::too_many_arguments)]
fn run_session(
    session: Option<String>,
    audio_device: Option<String>,
    jack: bool,
    midi_device: Option<String>,
    buffer_size: u32,
    sample_rate: u32,
    debug: bool,
    mode: Mode,
) -> anyhow::Result<()> {
    let use_tui = matches!(mode, Mode::Tui(_));
    // With no sound card (`--audio-device none`: CI, headless runs) there is no microphone to listen to either: the audio input plays a quiet test tone, so that what a cable does to it is heard.
    plugin::builtin::use_test_tone(audio_device.as_deref() == Some(audio::NULL_DEVICE));
    // The audio input (a plugin, made far from here) opens its stream on the host that the output uses.
    audio::use_jack(jack);
    #[cfg(feature = "web")]
    let serving = matches!(mode, Mode::Serve(_));
    #[cfg(not(feature = "web"))]
    let serving = false;

    // Set up the logger early so plugin loading messages are visible. The terminal is in
    // raw mode (and wants \r\n) in the TUI and in `play`; a server's log is plain text.
    if serving {
        #[cfg(feature = "web")]
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    } else {
        log::set_logger(&RAW_MODE_LOGGER).ok();
        log::set_max_level(
            std::env::var("RUST_LOG")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(log::LevelFilter::Info),
        );
    }

    let sample_rate_f = sample_rate as f32;
    let max_block_size = buffer_size as usize;

    // Load or create session config.
    let (config, source) = match session {
        // (Every modulator is the session's own as it is loaded: a file that has them in its lanes and its groups is read as it was written, and saved the new way: `session::parse`.)
        Some(s) => (session::load(&s)?, s),
        None => {
            let (config, path) = default_session()?;
            (config, path.to_string_lossy().to_string())
        }
    };

    // From here on the session's file is known by a path that does not depend on where the process is:
    // plugins that are loaded or scanned can change its working directory (some do), and a relative
    // path would then lead somewhere else, for the save and for the plugins the session names relative
    // to its own directory.
    let source = session::absolute_path(&source);
    let session_dir = source.parent().unwrap_or_else(|| Path::new("."));

    // Splash screen while plugins load (TUI mode only). Plugin loads can take
    // seconds for heavyweight instruments; the splash thread owns the terminal
    // and animates while this thread loads. Dropped (= terminal restored)
    // before tui::run sets up its own terminal — also on the error path.
    let total_slots: usize = config
        .instruments
        .iter()
        .map(|i| i.instrument.is_some() as usize + i.effects.len())
        .sum();
    let splash = if use_tui {
        match tui::splash::Splash::start(total_slots) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("Splash screen unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    // While the splash owns the screen, suppress logging if stderr is the
    // terminal (it would corrupt the alternate screen). Logging to a
    // redirected stderr (`tang 2> debug.log`) is unaffected.
    let prev_log_level = log::max_level();
    if splash.is_some() && std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        log::set_max_level(log::LevelFilter::Off);
    }
    // Create shared LV2 world (scans system plugins once, reused for all LV2 loads)
    #[cfg(feature = "lv2")]
    let runtime = plugin::Runtime::with_lv2(max_block_size);
    #[cfg(not(feature = "lv2"))]
    let runtime = plugin::Runtime::default();

    // Create channels
    let (midi_tx, midi_rx) = crossbeam_channel::bounded::<audio::MidiEvent>(audio::MIDI_CHANNEL_CAPACITY);
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<plugin::chain::GraphCommand>(64);
    let (return_tx, return_rx) = crossbeam_channel::bounded::<Box<dyn plugin::Plugin>>(16);

    // Shared "currently held notes" state — populated by both the hardware MIDI
    // thread and the virtual piano, read by the TUI Piano tab.
    let held = std::sync::Arc::new(held_notes::HeldNotes::new());

    // Shared piano-tab filter: current scale + Highlight/Locked mode. Read by
    // both senders to drop off-scale notes when locked; written by the TUI.
    let piano_initial_scale = config
        .piano
        .as_ref()
        .and_then(|p| p.scale.as_deref().and_then(scale::ScaleSetting::parse))
        .unwrap_or_default();
    let piano_initial_mode = if config.piano.as_ref().is_some_and(|p| p.locked) {
        piano_filter::PianoMode::Locked
    } else {
        piano_filter::PianoMode::Highlight
    };
    let piano_filter = std::sync::Arc::new(piano_filter::PianoFilter::new(
        piano_initial_scale,
        piano_initial_mode,
    ));

    // Create empty audio graph (outputs silence until instruments are added)
    let mut graph = plugin::chain::AudioGraph::new(plugin::chain::CHAIN_CHANNELS, cmd_rx, return_tx);

    // Pattern recording completion channel
    let (pattern_tx, pattern_rx) = crossbeam_channel::bounded::<plugin::chain::PatternNotification>(64);
    graph.set_pattern_tx(pattern_tx.clone());
    // MIDI-learn captures flow back to the TUI on this channel.
    let (learn_tx, learn_rx) =
        crossbeam_channel::bounded::<plugin::chain::MidiLearnedNotification>(64);
    graph.set_learn_tx(learn_tx);
    // Give the audio thread the live piano scale for in-key pattern transposition.
    graph.set_piano_filter(piano_filter.clone());
    // And the cells it publishes its measurements in (meters, for now).
    let telemetry = std::sync::Arc::new(telemetry::Telemetry::new());
    graph.set_telemetry(telemetry.clone());

    // Start MIDI input
    if let Some(sp) = &splash {
        sp.status("Opening MIDI inputs…");
    }
    // Live incoming-MIDI monitor for `--debug` (a no-op when disabled).
    let midi_monitor = std::sync::Arc::new(midi_monitor::MidiMonitor::new(debug));
    let notes = note_input::NoteInput::new(midi_tx.clone(), held.clone(), piano_filter.clone());
    let mut midi_mgr = midi::MidiManager::new(notes, midi_device, midi_monitor.clone());
    // A server may well be somewhere with no MIDI at all (a container, CI): the
    // browser's keyboard still plays, and nobody should be warned about it every second.
    let midi_available = match midi_mgr.open_ports() {
        Ok(_) => true,
        Err(e) if serving => {
            log::warn!("No MIDI inputs: {e}");
            false
        }
        Err(e) => return Err(e),
    };
    log::info!("MIDI inputs connected: {}", midi_mgr.connection_count());

    // Build audio engine (not playing yet — will start after initial commands are queued)
    if let Some(sp) = &splash {
        sp.status("Starting audio engine…");
    }
    let mut engine = audio::AudioEngine::build(
        graph,
        midi_rx,
        audio_device.as_deref(),
        jack,
        sample_rate,
        buffer_size,
    )?;

    // Load the session: plugins into the audio thread, the model for the front-end.
    let model = app::load::load_session(
        &config,
        session_dir,
        sample_rate_f,
        max_block_size,
        &runtime,
        &cmd_tx,
        &mut |p| {
            if let Some(sp) = &splash {
                match p {
                    app::load::Progress::Loading(plugin) => {
                        sp.status(format!("Loading {}…", tui::splash::display_name(plugin)))
                    }
                    app::load::Progress::Loaded { done } => sp.progress(done),
                }
            }
        },
    )?;

    // All initial commands queued — now start the audio stream
    engine.play()?;

    // Loading done: stop the splash (restores the terminal) and re-enable
    // logging before the TUI applies its own suppression logic.
    drop(splash);
    log::set_max_level(prev_log_level);

    // From here on the controller owns the session and the engine's handles. It
    // reaps plugins the audio thread hands back and watches for MIDI devices
    // being plugged in, in both the TUI and `play`.
    let mut ctl = app::controller::Controller::new(
        model,
        app::controller::EngineHandles {
            cmd_tx,
            midi_tx,
            return_rx,
            pattern_rx,
            learn_rx,
            runtime,
            sample_rate: sample_rate_f,
            max_block_size,
            held,
            piano_filter,
            midi_monitor,
            telemetry,
            // (The audio thread takes a command within a block or two, 5 to 20 ms: this is for one that is stalled, which an undo then does without.)
            recovery_wait: std::time::Duration::from_millis(250),
        },
        Some(source.clone()),
    );
    // The library of modules (step 104): chains of effects that are saved in files, which every session can add. Where it is can be said (`TANG_MODULES_DIR`: the tests of the real program say it).
    ctl.modules_dir = modules_dir();
    ctl.refresh_modules();
    if midi_available {
        ctl.watch_devices(Box::new(midi_mgr));
    }

    // --- Branch: TUI view, a server for a browser, or plain play mode ---
    let ctl = if let Mode::Tui(web) = &mode {
        tui::run(ctl, web.as_ref())?
    } else if serving {
        #[cfg(feature = "web")]
        if let Mode::Serve(args) = &mode {
            web::serve(&mut ctl, args)?;
        }
        ctl
    } else {
        // --- Plain play mode (original) ---

        // Probe keyboard enhancement support (must be done before entering raw mode)
        let kitty_supported =
            crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

        // Install a panic hook that restores terminal state before the panic
        // message prints, so the user's terminal isn't left in raw mode.
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if kitty_supported {
                let _ = crossterm::execute!(std::io::stderr(), PopKeyboardEnhancementFlags);
            }
            let _ = crossterm::terminal::disable_raw_mode();
            original_hook(info);
        }));

        // Enter raw mode
        crossterm::terminal::enable_raw_mode()?;

        // Push Kitty keyboard flags if supported
        if kitty_supported {
            crossterm::execute!(
                std::io::stderr(),
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                        | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                )
            )?;
            log::info!("Kitty keyboard protocol enabled (press/release detection active)");
        } else {
            log::warn!(
                "Terminal does not support Kitty keyboard protocol — virtual piano disabled (hardware MIDI still works)"
            );
        }

        // Create virtual piano
        let mut virt_piano = piano::VirtualPiano::new(ctl.note_input(), kitty_supported);

        log::info!("Playing. Ctrl+Q or Ctrl+C to quit.");

        loop {
            // Poll crossterm events with 10ms timeout
            if event::poll(Duration::from_millis(10))? {
                if let Event::Key(key_event) = event::read()? {
                    // Ctrl+C or Ctrl+Q → quit
                    if key_event
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        match key_event.code {
                            KeyCode::Char('c') | KeyCode::Char('q') => break,
                            _ => {}
                        }
                    }
                    // Pass to virtual piano
                    virt_piano.handle_key_event(key_event);
                }
            }

            // Reap returned plugins and look for new MIDI devices.
            ctl.poll();
        }

        // Cleanup
        virt_piano.all_notes_off();

        if kitty_supported {
            crossterm::execute!(std::io::stderr(), PopKeyboardEnhancementFlags).ok();
        }
        crossterm::terminal::disable_raw_mode()?;
        ctl
    };

    log::info!("Stopping...");

    // Shutdown order matters: stop audio first (so callback can't call plugin),
    // then drop MIDI connections, then returned plugins are dropped last.
    engine.stop();
    ctl.shutdown();

    Ok(())
}

