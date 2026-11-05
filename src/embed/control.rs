//! The controller's thread of Tang inside a host: the session is loaded onto the engine whose audio half the host's thread renders, the page is served, and what the page asks is answered, until the host says to stop. The controller is
//! made on this thread and stays on it, as it stays on the main thread in `tang serve`; the host's own threads never touch it.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::app::controller::{Controller, DeviceWatcher};
use crate::app::wiring::Wiring;
use crate::audio::MidiEvent;
use crate::midi::DeviceChanges;
use crate::plugin::builtin::HostInput;
use crate::plugin::chain::AudioGraph;
use crate::source::SourceSet;

use super::engine::HOST_SOURCE;

/// The name of the one device that Tang inside a host has: what the host sends, as a source of notes that a lane can be wired to.
pub const HOST_DEVICE: &str = "Host";

/// How often the thread polls the controller (the plugins that the audio thread hands back, what it reports) and refreshes what the connections read.
const TICK: Duration = Duration::from_millis(50);

/// What the thread is asked.
pub(super) enum Control {
    /// Give the session as the text of a session file, and whether it has been changed since it was loaded.
    Session(Sender<Result<(String, bool), String>>),
    /// Stop: the page is closed, the controller is shut down, the thread ends.
    Quit,
}

/// What the thread is started with.
pub(super) struct Setup {
    /// The session to load, as the text of a session file.
    pub text: String,
    pub sample_rate: f32,
    pub max_frames: usize,
    /// Whether to serve the session to browsers.
    pub serve: bool,
    /// What the audio inputs of the session play: what the host's `process` is given to hear.
    pub input: Arc<HostInput>,
}

/// What the thread says when the session is loaded and the page is served.
pub(super) struct Started {
    /// Where the page is (with the token that it needs, if it needs one): none when it is not served.
    pub url: Option<String>,
}

/// The host as a device: what a lane can be wired to as a source of notes. It is always there, and nothing plugs in or out.
struct HostDevice;

impl DeviceWatcher for HostDevice {
    fn poll_new_devices(&mut self) -> DeviceChanges {
        DeviceChanges::default()
    }

    fn sources_of(&self, name: &str) -> SourceSet {
        if name == HOST_DEVICE { SourceSet::NONE.with(HOST_SOURCE) } else { SourceSet::NONE }
    }

    #[cfg(any(test, feature = "web"))]
    fn open_names(&self) -> Vec<String> {
        vec![HOST_DEVICE.to_string()]
    }
}

/// Say why a plugin's own window cannot be opened here: the window is the main thread's, and the main thread is the host's.
fn no_windows() -> Result<(), String> {
    Err("a plugin's own window is not available inside a host yet".to_string())
}

/// The thread's body. The audio half of the engine goes to the host's side first (`audio`), so that it can take the commands that the load sends while it goes on; then the session is loaded and the page is served, and `started` says how it went.
pub(super) fn run(setup: Setup, audio: Sender<(AudioGraph, Receiver<MidiEvent>)>, started: Sender<Result<Started, String>>, control: Receiver<Control>) {
    let Setup { text, sample_rate, max_frames, serve, input } = setup;
    let config = match crate::session::parse(&text) {
        Ok(config) => config,
        Err(error) => {
            let _ = started.send(Err(format!("the session could not be read: {error:#}")));
            return;
        }
    };

    // (Plugins are loaded with a runtime of their own, so that no instance shares one with another, and the audio input among them plays what the host gives it to hear: no sound card is opened.)
    #[cfg(feature = "lv2")]
    let runtime = crate::plugin::Runtime::with_lv2(max_frames);
    #[cfg(not(feature = "lv2"))]
    let runtime = crate::plugin::Runtime::default();
    let runtime = runtime.with_host_input(input);
    let Wiring { graph, midi_rx, handles } = Wiring::new(config.piano.as_ref(), sample_rate, max_frames, runtime, false);
    if audio.send((graph, midi_rx)).is_err() {
        return;
    }

    let model = match crate::app::load::load_session(&config, Path::new("."), sample_rate, max_frames, &handles.runtime, &handles.cmd_tx, &mut |_| {}) {
        Ok(model) => model,
        Err(error) => {
            let _ = started.send(Err(format!("the session could not be loaded: {error:#}")));
            return;
        }
    };
    let mut ctl = Controller::new(model, handles, None);
    ctl.modules_dir = crate::modules_dir();
    ctl.refresh_modules();
    ctl.window_check = no_windows;
    ctl.watch_devices(Box::new(HostDevice));

    #[cfg(feature = "web")]
    if serve {
        match serve_session(&mut ctl, &started, &control) {
            Ok(()) => {}
            Err(error) => {
                let _ = started.send(Err(format!("the page could not be served: {error:#}")));
            }
        }
        ctl.shutdown();
        return;
    }
    #[cfg(not(feature = "web"))]
    let _ = serve;

    let _ = started.send(Ok(Started { url: None }));
    run_quietly(&mut ctl, &control);
    ctl.shutdown();
}

/// The session as text, and whether it was changed, for `Control::Session`.
fn session_of(ctl: &mut Controller) -> Result<(String, bool), String> {
    ctl.session_text().map(|text| (text, ctl.dirty)).map_err(|error| format!("{error:#}"))
}

/// The loop of a controller that serves no page: answer what the host asks, and poll, until it says to stop.
fn run_quietly(ctl: &mut Controller, control: &Receiver<Control>) {
    let tick = crossbeam_channel::tick(TICK);
    loop {
        crossbeam_channel::select! {
            recv(tick) -> _ => {
                ctl.poll();
            }
            recv(control) -> message => match message {
                Ok(Control::Session(reply)) => {
                    let _ = reply.send(session_of(ctl));
                }
                Ok(Control::Quit) | Err(_) => break,
            },
        }
    }
}

/// Serve the session of `ctl` to browsers on a port that the system picks (on this machine only: there is no token), say where (`started`), and answer what the browsers and the host ask until the host says to stop. The scan of the
/// installed plugins loads every one of them, which is a thing to do in a host's process only when someone asks for the list: it starts when the first browser has come (and not at all if `TANG_NO_SCAN` is set).
#[cfg(feature = "web")]
fn serve_session(ctl: &mut Controller, started: &Sender<Result<Started, String>>, control: &Receiver<Control>) -> anyhow::Result<()> {
    use crate::web::runtime::Runtime;
    use crate::web::server::Config;

    let mut runtime = Runtime::start(ctl, Config::new(std::net::SocketAddr::from(([127, 0, 0, 1], 0))))?;
    log::info!("Web interface: {}", runtime.url());
    let _ = started.send(Ok(Started { url: Some(runtime.url()) }));

    let requests = runtime.requests();
    let tick = crossbeam_channel::tick(TICK);
    let scan = std::env::var_os("TANG_NO_SCAN").is_none();
    let mut scanned = false;
    loop {
        crossbeam_channel::select! {
            recv(requests) -> request => match request {
                Ok(request) => {
                    runtime.serve(ctl, request);
                }
                Err(_) => break,
            },
            recv(tick) -> _ => {
                ctl.poll();
                runtime.refresh(ctl);
                if scan && !scanned && runtime.clients() > 0 {
                    scanned = true;
                    ctl.catalog.ensure_scan();
                }
            }
            recv(control) -> message => match message {
                Ok(Control::Session(reply)) => {
                    let _ = reply.send(session_of(ctl));
                }
                Ok(Control::Quit) | Err(_) => break,
            },
        }
    }
    log::info!("Stopping the web interface");
    Ok(())
}
