use std::net::SocketAddr;
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{Args, CommandFactory, Parser, Subcommand};

/// Default audio buffer size in frames.
///
/// On Linux, audio goes through PipeWire (via ALSA or the JACK bridge), which runs
/// every client on one driver graph at a single global quantum equal to the *smallest* latency
/// any active client requests. A small buffer here would drag the whole system's
/// quantum down and make other apps (YouTube, etc.) chop. 1024 matches PipeWire's
/// typical default global quantum, so Tang doesn't disturb anything else; pass
/// `--buffer-size` to opt into lower latency. macOS (CoreAudio) and Windows
/// (WASAPI) don't share a global quantum, so they keep a low-latency default.
#[cfg(target_os = "linux")]
pub const DEFAULT_BUFFER_SIZE: &str = "1024";
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_BUFFER_SIZE: &str = "256";

/// Where the web interface listens unless it is told otherwise: on this machine only.
pub const DEFAULT_WEB_BIND: &str = "127.0.0.1:8765";

// The flags are `global`: they are the same wherever they are written, before the
// subcommand or after it (`tang play x.toml --audio-device none`).
#[derive(Parser)]
#[command(name = "tang", about = "Minimal CLI LV2/CLAP instrument host")]
pub struct Cli {
    /// Session file path (.toml). Defaults to a new session.
    pub session: Option<String>,

    /// Audio output device name (default: system default). `none` runs the
    /// engine on a timer without a sound card, for CI and headless use.
    #[arg(long, global = true)]
    pub audio_device: Option<String>,

    /// Linux: play through PipeWire's JACK bridge instead of ALSA. Lower
    /// latency, but a late tang cycle glitches every other app's audio.
    #[arg(long, global = true)]
    pub jack: bool,

    /// MIDI input device name filter (default: open all)
    #[arg(long, global = true)]
    pub midi_device: Option<String>,

    /// Audio buffer size in frames
    #[arg(long, default_value = DEFAULT_BUFFER_SIZE, global = true)]
    pub buffer_size: u32,

    /// Sample rate in Hz
    #[arg(long, default_value = "48000", global = true)]
    pub sample_rate: u32,

    /// Show a live incoming-MIDI monitor at the bottom of the TUI (for
    /// diagnosing what a controller actually sends).
    #[arg(long, global = true)]
    pub debug: bool,

    /// Serve the session to a browser as well, while the terminal interface runs:
    /// what is changed in either shows in the other. Takes an address (an IP and a
    /// port) after an `=`, as in `--web=0.0.0.0:9000`; on its own, 127.0.0.1:8765.
    /// Beyond this machine (anything but 127.0.0.1 or ::1) it needs a token: one is
    /// made, and printed in the address to open.
    #[arg(
        long,
        value_name = "ADDR",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = DEFAULT_WEB_BIND
    )]
    pub web: Option<SocketAddr>,

    /// Another page that may connect to the web interface of `--web`, as
    /// scheme://host:port (a dev server, say). Can be given more than once.
    #[arg(long = "web-origin", value_name = "ORIGIN", requires = "web")]
    pub web_origins: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// What the terminal interface is to serve to browsers, if it is to: the
    /// arguments `serve` takes, from `--web` and `--web-origin`.
    pub fn tui_web(&self) -> Option<WebArgs> {
        self.web.map(|web_bind| WebArgs { web_bind, web_origins: self.web_origins.clone() })
    }

    /// What clap cannot say: `--web` is for the terminal interface, so a subcommand
    /// has no use for it (`serve` has `--web-bind`, and `play` no web interface).
    pub fn check(&self) -> Result<(), clap::Error> {
        if self.web.is_some() && self.command.is_some() {
            return Err(Cli::command().error(
                ErrorKind::ArgumentConflict,
                "--web is for the terminal interface (`tang --web`); `tang serve` has --web-bind, and the other subcommands have no web interface",
            ));
        }
        Ok(())
    }
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
    /// Listen to an audio input for a few seconds and say what it hears, playing nothing: a check that the microphone is heard
    Listen {
        /// The input's name, or a part of it (`tang enumerate audio` lists them); the system's default input if omitted
        device: Option<String>,

        /// How many seconds to listen
        #[arg(long, default_value_t = 3.0)]
        seconds: f32,
    },
    /// Open a plugin's own window by itself and play nothing: a check that the window works and what it looks like (`--snapshot` writes what is in it as a picture, for a person who cannot see the screen)
    Window {
        /// Plugin source (vst3:<name>, or the path of a .vst3 bundle: a path loads only that plugin; clap:<id>, or the path of a .clap file, which is its first plugin)
        plugin: String,

        /// Make the window this size, in points (`900x600`), once it is open, as a host does that has kept the size that a person gave the window (a CLAP plugin: a VST3 plugin's window is its view's)
        #[arg(long, value_name = "WxH", value_parser = parse_size)]
        size: Option<(u32, u32)>,

        /// Close the window after this many seconds; without it the window stays until it is closed
        #[arg(long)]
        seconds: Option<f32>,

        /// Write what is in the window, as a PNG, a moment before it is closed
        #[arg(long, value_name = "FILE")]
        snapshot: Option<PathBuf>,

        /// Do not take the keyboard from the terminal when the window opens
        #[arg(long)]
        no_focus: bool,

        /// Print the views that the plugin has made in the window, a moment before it is closed (for a plugin whose window stays empty)
        #[arg(long)]
        tree: bool,
    },
    /// Load a session and play via MIDI input with virtual piano (no TUI)
    Play {
        /// Path to session file (.toml). If omitted, creates a new session.
        session: Option<String>,
    },
    /// Load a session and serve it to a browser, with no TUI (needs a build with the
    /// `web` feature)
    Serve {
        /// Path to session file (.toml). If omitted, creates a new session.
        session: Option<String>,

        #[command(flatten)]
        web: WebArgs,
    },
}

/// How the web interface listens.
#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub struct WebArgs {
    /// Address (an IP and a port) for the web interface to listen on. Beyond this
    /// machine (anything but 127.0.0.1 or ::1) it needs a token: one is made, and
    /// printed in the address to open.
    #[arg(long, default_value = DEFAULT_WEB_BIND)]
    pub web_bind: SocketAddr,

    /// Another page that may connect, as scheme://host:port (a dev server, say). Can be
    /// given more than once.
    #[arg(long = "web-origin", value_name = "ORIGIN")]
    pub web_origins: Vec<String>,
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

/// A size for `tang window --size`: two whole numbers above 0, as `900x600`.
fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (width, height) = text.split_once(['x', 'X']).ok_or_else(|| format!("`{text}` is not a size: write it as 900x600"))?;
    let side = |side: &str| side.trim().parse::<u32>().ok().filter(|&length| length > 0).ok_or_else(|| format!("`{text}` is not a size: both sides are whole numbers above 0, as 900x600"));
    Ok((side(width)?, side(height)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every way of writing the same thing: the flags before the subcommand (where
    /// they always worked) and after it, or among its arguments.
    fn placements(subcommand: &[&str], flags: &[&str]) -> Vec<Vec<String>> {
        let owned = |parts: &[&[&str]]| parts.iter().flat_map(|p| p.iter()).map(|s| s.to_string()).collect::<Vec<String>>();
        vec![
            owned(&[&["tang"], flags, subcommand]),
            owned(&[&["tang"], subcommand, flags]),
            owned(&[&["tang"], &subcommand[..1], flags, &subcommand[1..]]),
        ]
    }

    #[test]
    fn the_flags_of_the_session_commands_work_wherever_they_are_written() {
        let flags = ["--audio-device", "none", "--midi-device", "keys", "--buffer-size", "128", "--sample-rate", "44100", "--debug", "--jack"];
        for subcommand in [["play", "x.toml"], ["serve", "x.toml"]] {
            for args in placements(&subcommand, &flags) {
                let cli = Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
                assert_eq!(cli.audio_device.as_deref(), Some("none"), "{args:?}");
                assert_eq!(cli.midi_device.as_deref(), Some("keys"), "{args:?}");
                assert_eq!((cli.buffer_size, cli.sample_rate, cli.debug, cli.jack), (128, 44100, true, true), "{args:?}");
            }
        }
    }

    #[test]
    fn serve_listens_on_loopback_unless_told_otherwise() {
        let Command::Serve { session, web } = Cli::try_parse_from(["tang", "serve", "x.toml"]).unwrap().command.unwrap() else {
            panic!("not serve")
        };
        assert_eq!(session.as_deref(), Some("x.toml"));
        assert_eq!(web, WebArgs { web_bind: "127.0.0.1:8765".parse().unwrap(), web_origins: vec![] });

        let args = ["tang", "serve", "--web-bind", "0.0.0.0:9000", "--web-origin", "http://localhost:5173", "--web-origin", "http://localhost:5174"];
        let Command::Serve { session, web } = Cli::try_parse_from(args).unwrap().command.unwrap() else { panic!("not serve") };
        assert_eq!(session, None, "a new session, like the others");
        assert_eq!(web.web_bind, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(web.web_origins, ["http://localhost:5173", "http://localhost:5174"]);
    }

    #[test]
    fn the_web_flags_belong_to_serve_alone() {
        assert!(Cli::try_parse_from(["tang", "play", "x.toml", "--web-bind", "127.0.0.1:1"]).is_err());
        assert!(Cli::try_parse_from(["tang", "--web-bind", "127.0.0.1:1"]).is_err());
        assert!(Cli::try_parse_from(["tang", "serve", "--web-bind", "localhost:8765"]).is_err(), "an address, not a name");
    }

    #[test]
    fn the_terminal_interface_serves_the_web_interface_when_asked_to() {
        let cli = Cli::try_parse_from(["tang"]).unwrap();
        assert_eq!((cli.web, cli.tui_web()), (None, None), "not unless asked for");

        let cli = Cli::try_parse_from(["tang", "--web"]).unwrap();
        assert_eq!(cli.web, Some("127.0.0.1:8765".parse().unwrap()), "on this machine, where `serve` listens");

        let cli = Cli::try_parse_from(["tang", "--web=0.0.0.0:9000", "x.toml"]).unwrap();
        assert_eq!((cli.web, cli.session.as_deref()), (Some("0.0.0.0:9000".parse().unwrap()), Some("x.toml")));

        // The address comes after an `=` only, so a session after `--web` is a session.
        let cli = Cli::try_parse_from(["tang", "--web", "x.toml"]).unwrap();
        assert_eq!((cli.web, cli.session.as_deref()), (Some("127.0.0.1:8765".parse().unwrap()), Some("x.toml")));
        let cli = Cli::try_parse_from(["tang", "x.toml", "--web"]).unwrap();
        assert_eq!((cli.web, cli.session.as_deref()), (Some("127.0.0.1:8765".parse().unwrap()), Some("x.toml")));
    }

    #[test]
    fn the_web_flag_of_the_terminal_interface_has_the_arguments_serve_has() {
        let args = ["tang", "--web=[::1]:9000", "--web-origin", "http://localhost:5173", "--web-origin", "http://localhost:5174"];
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!(
            cli.tui_web(),
            Some(WebArgs {
                web_bind: "[::1]:9000".parse().unwrap(),
                web_origins: vec!["http://localhost:5173".to_string(), "http://localhost:5174".to_string()],
            })
        );
        let plain = Cli::try_parse_from(["tang", "--web"]).unwrap();
        assert_eq!(plain.tui_web(), Some(WebArgs { web_bind: "127.0.0.1:8765".parse().unwrap(), web_origins: vec![] }));
    }

    #[test]
    fn the_web_origins_of_the_terminal_interface_need_the_web_flag() {
        assert!(Cli::try_parse_from(["tang", "--web-origin", "http://localhost:5173"]).is_err());
        assert!(Cli::try_parse_from(["tang", "x.toml", "--web-origin", "http://localhost:5173"]).is_err());
        // (`serve` has its own, with the address it listens on.)
        assert!(Cli::try_parse_from(["tang", "serve", "--web-origin", "http://localhost:5173"]).is_ok());
    }

    #[test]
    fn an_address_is_an_ip_and_a_port() {
        for bad in ["--web=localhost:8765", "--web=8765", "--web=127.0.0.1", "--web="] {
            assert!(Cli::try_parse_from(["tang", bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_web_flag_belongs_to_the_terminal_interface_alone() {
        // `play` has no web interface, and `serve` has `--web-bind`.
        assert!(Cli::try_parse_from(["tang", "play", "x.toml", "--web"]).is_err());
        assert!(Cli::try_parse_from(["tang", "serve", "x.toml", "--web"]).is_err());
        assert!(Cli::try_parse_from(["tang", "enumerate", "audio", "--web"]).is_err());
        // Written before a subcommand it parses, because it is the top level's, and `check` refuses it.
        for args in [["tang", "--web", "play", "x.toml"], ["tang", "--web", "serve", "x.toml"]] {
            let cli = Cli::try_parse_from(args).unwrap();
            let error = cli.check().expect_err("a subcommand and --web").to_string();
            assert!(error.contains("--web"), "{error}");
        }
        assert!(Cli::try_parse_from(["tang", "--web", "x.toml"]).unwrap().check().is_ok());
        assert!(Cli::try_parse_from(["tang", "play", "x.toml"]).unwrap().check().is_ok());
    }

    #[test]
    fn the_window_check_takes_a_plugin_and_says_how_long_and_where_to_write_what_it_sees() {
        let Command::Window { plugin, seconds, snapshot, no_focus, tree, size } = Cli::try_parse_from(["tang", "window", "vst3:Pianoteq"]).unwrap().command.unwrap() else {
            panic!("not window")
        };
        assert_eq!((plugin.as_str(), seconds, snapshot, no_focus, tree, size), ("vst3:Pianoteq", None, None, false, false, None), "open until it is closed, the keyboard taken, nothing written, the plugin's size");

        let args = ["tang", "window", "x.vst3", "--seconds", "2.5", "--snapshot", "x.png", "--no-focus", "--tree"];
        let Command::Window { seconds, snapshot, no_focus, tree, .. } = Cli::try_parse_from(args).unwrap().command.unwrap() else { panic!("not window") };
        assert_eq!((seconds, snapshot.as_deref(), no_focus, tree), (Some(2.5), Some(std::path::Path::new("x.png")), true, true));

        let Command::Window { size, .. } = Cli::try_parse_from(["tang", "window", "x.clap", "--size", "900x600"]).unwrap().command.unwrap() else { panic!("not window") };
        assert_eq!(size, Some((900, 600)));
        let Command::Window { size, .. } = Cli::try_parse_from(["tang", "window", "x.clap", "--size", "1100X720"]).unwrap().command.unwrap() else { panic!("not window") };
        assert_eq!(size, Some((1100, 720)), "an X is an x");
        for bad in ["900", "x600", "900x", "0x600", "900x0", "-1x5", "ax b", "900x600x3"] {
            assert!(Cli::try_parse_from(["tang", "window", "x.clap", "--size", bad]).is_err(), "{bad}");
        }

        assert!(Cli::try_parse_from(["tang", "window"]).is_err(), "a plugin is needed");
        // The flags that every command has are the same here, written where they are written.
        assert!(Cli::try_parse_from(["tang", "window", "x.vst3", "--sample-rate", "44100"]).is_ok());
    }

    #[test]
    fn the_flags_are_as_before_when_they_are_not_given() {
        let cli = Cli::try_parse_from(["tang", "play", "x.toml"]).unwrap();
        assert_eq!((cli.audio_device, cli.midi_device, cli.sample_rate, cli.debug, cli.jack), (None, None, 48000, false, false));
        assert_eq!(cli.buffer_size.to_string(), DEFAULT_BUFFER_SIZE);
        assert!(Cli::try_parse_from(["tang", "x.toml", "--audio-device", "none"]).is_ok(), "and without a subcommand");
    }
}
