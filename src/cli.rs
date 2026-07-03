use clap::{Parser, Subcommand};

/// Default audio buffer size in frames.
///
/// On Linux, audio goes through PipeWire's JACK bridge, which runs every client
/// on one driver graph at a single global quantum equal to the *smallest* latency
/// any active client requests. A small buffer here would drag the whole system's
/// quantum down and make other apps (YouTube, etc.) chop. 1024 matches PipeWire's
/// typical default global quantum, so Tang doesn't disturb anything else; pass
/// `--buffer-size` to opt into lower latency. macOS (CoreAudio) and Windows
/// (WASAPI) don't share a global quantum, so they keep a low-latency default.
#[cfg(target_os = "linux")]
pub const DEFAULT_BUFFER_SIZE: &str = "1024";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_BUFFER_SIZE: &str = "256";

#[derive(Parser)]
#[command(name = "tang", about = "Minimal CLI LV2/CLAP instrument host")]
pub struct Cli {
    /// Session file path (.toml). Defaults to a new session.
    pub session: Option<String>,

    /// Audio output device name (default: system default)
    #[arg(long)]
    pub audio_device: Option<String>,

    /// MIDI input device name filter (default: open all)
    #[arg(long)]
    pub midi_device: Option<String>,

    /// Audio buffer size in frames
    #[arg(long, default_value = DEFAULT_BUFFER_SIZE)]
    pub buffer_size: u32,

    /// Sample rate in Hz
    #[arg(long, default_value = "48000")]
    pub sample_rate: u32,

    /// Show a live incoming-MIDI monitor at the bottom of the TUI (for
    /// diagnosing what a controller actually sends).
    #[arg(long)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// List available MIDI inputs, audio outputs, and plugins
    #[command(subcommand)]
    Enumerate(EnumerateTarget),
    /// Describe a plugin (parameters, presets, I/O)
    Describe {
        /// Plugin source (lv2:<URI>, clap:<ID>, or path)
        plugin: String,
    },
    /// Load a session and play via MIDI input with virtual piano (no TUI)
    Play {
        /// Path to session file (.toml). If omitted, creates a new session.
        session: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EnumerateTarget {
    /// List available MIDI input devices
    Midi,
    /// List available audio output devices
    Audio,
    /// List available LV2 and CLAP plugins
    Plugins,
    /// List built-in plugins
    Builtins,
}
