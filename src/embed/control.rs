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
    /// Where to serve it: this machine and a port that the system picks.
    pub bind: std::net::SocketAddr,
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

/// Whether the scan of the installed plugins starts now: it is wanted (`TANG_NO_SCAN` is not set), it has not been started, and someone is looking at the page (a scan loads every plugin that is installed, which is a thing to do in a host's process only when someone
/// asks for the list).
#[cfg(any(test, feature = "web"))]
fn should_scan(wanted: bool, started: bool, browsers: usize) -> bool {
    wanted && !started && browsers > 0
}

/// Say why a plugin's own window cannot be opened here: the window is the main thread's, and the main thread is the host's.
fn no_windows() -> Result<(), String> {
    Err("a plugin's own window is not available inside a host yet".to_string())
}

/// The thread's body. The audio half of the engine goes to the host's side first (`audio`), so that it can take the commands that the load sends while it goes on; then the session is loaded and the page is served, and `started` says how it went.
pub(super) fn run(setup: Setup, audio: Sender<(AudioGraph, Receiver<MidiEvent>)>, started: Sender<Result<Started, String>>, control: Receiver<Control>) {
    let Setup { text, sample_rate, max_frames, serve, bind, input } = setup;
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
    let mut ctl = embedded(model, handles);

    #[cfg(feature = "web")]
    if serve {
        match serve_session(&mut ctl, &started, &control, bind) {
            Ok(()) => {
                ctl.shutdown();
                return;
            }
            // (A plugin that cannot serve its page is still a plugin that plays: the host's sound is not for the page's sake.)
            Err(error) => log::error!("The page could not be served ({error:#}): the plugin plays without one"),
        }
    }
    #[cfg(not(feature = "web"))]
    let _ = (serve, bind);

    let _ = started.send(Ok(Started { url: None }));
    run_quietly(&mut ctl, &control);
    ctl.shutdown();
}

/// The controller of a session that is in a host: the library of modules, no window of a plugin's own, and the host as a device.
fn embedded(model: crate::app::model::SessionModel, handles: crate::app::controller::EngineHandles) -> Controller {
    let mut ctl = Controller::new(model, handles, None);
    ctl.modules_dir = crate::modules_dir();
    ctl.refresh_modules();
    ctl.window_check = no_windows;
    ctl.watch_devices(Box::new(HostDevice));
    ctl
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
fn serve_session(ctl: &mut Controller, started: &Sender<Result<Started, String>>, control: &Receiver<Control>, bind: std::net::SocketAddr) -> anyhow::Result<()> {
    use crate::web::runtime::Runtime;
    use crate::web::server::Config;

    let mut runtime = Runtime::start(ctl, Config::new(bind))?;
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
                if should_scan(scan, scanned, runtime.clients()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_the_one_device_there_is_and_the_source_of_what_it_sends_is_its_own() {
        let mut host = HostDevice;
        assert_eq!(host.sources_of(HOST_DEVICE), SourceSet::NONE.with(HOST_SOURCE));
        assert!(host.sources_of(HOST_DEVICE).contains(HOST_SOURCE) && !host.sources_of(HOST_DEVICE).contains(crate::source::Source::KEYS), "the keys are another source");
        assert_eq!(host.sources_of("Some other keyboard"), SourceSet::NONE, "no other device is open");
        assert_eq!(host.open_names(), [HOST_DEVICE]);
        assert_eq!(host.poll_new_devices(), DeviceChanges::default(), "nothing plugs in or out");
    }

    #[test]
    fn a_plugin_of_a_host_has_no_window_of_its_own_and_says_why() {
        assert!(no_windows().unwrap_err().contains("not available inside a host"));
    }

    #[test]
    fn the_controller_of_a_host_has_no_windows_and_has_the_host_as_its_device() {
        let (handles, _engine) = crate::app::testutil::stub_engine();
        let ctl = embedded(crate::app::testutil::load("[[instrument]]\nplugin = \"builtin:osc\"\n").model, handles);
        assert!((ctl.window_check)().unwrap_err().contains("not available inside a host"), "the check that every window is asked is the host's");
        assert_eq!(ctl.inputs(), [HOST_DEVICE], "the host is the one input that is open");
        assert!(ctl.session_path.is_none(), "the host's project is where the session is kept, not a file");
    }

    /// The thread's own: a controller that is started on `text`, with `bind` for its page, whose engine's other end is drained as the host's idle thread does.
    #[cfg(feature = "web")]
    fn started(bind: std::net::SocketAddr) -> (Receiver<Result<Started, String>>, Sender<Control>, std::thread::JoinHandle<()>) {
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(1);
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        let setup = Setup { text: "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string(), sample_rate: 48_000.0, max_frames: 256, serve: true, bind, input: Arc::new(HostInput::new(256)) };
        let thread = std::thread::spawn(move || run(setup, audio_tx, started_tx, control_rx));
        // (The engine's audio half is kept and drained, so that the load goes through.)
        let (mut graph, _notes) = audio_rx.recv_timeout(std::time::Duration::from_secs(30)).expect("the audio half");
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drain = std::thread::spawn({
            let done = done.clone();
            move || {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    graph.drain_commands();
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
        });
        let result = started_rx.recv_timeout(std::time::Duration::from_secs(30));
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = drain.join();
        // (Put back for the caller, which reads it: the channel has one place, and `recv_timeout` took it.)
        let (again_tx, again_rx) = crossbeam_channel::bounded(1);
        again_tx.send(result.expect("it said how it went")).unwrap();
        (again_rx, control_tx, thread)
    }

    #[cfg(feature = "web")]
    #[test]
    fn a_page_that_cannot_be_served_leaves_a_plugin_that_plays_without_one() {
        // The port is taken by this test, so that the page cannot be served there.
        let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let (started, control, thread) = started(taken.local_addr().unwrap());
        match started.recv().unwrap() {
            Ok(Started { url: None }) => {}
            Ok(Started { url }) => panic!("a page was served on a port that is taken: {url:?}"),
            Err(why) => panic!("a plugin that cannot serve its page does not fail: {why}"),
        }
        // It runs: it answers what the host asks.
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        control.send(Control::Session(reply_tx)).unwrap();
        let (text, changed) = reply_rx.recv_timeout(std::time::Duration::from_secs(10)).expect("it answers").expect("the session");
        assert!(text.contains("builtin:osc") && !changed);
        control.send(Control::Quit).unwrap();
        thread.join().unwrap();
    }

    #[cfg(feature = "web")]
    #[test]
    fn a_page_is_served_where_it_is_asked_to_be() {
        let (started, control, thread) = started(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
        let started = started.recv().unwrap().expect("it started");
        assert!(started.url.is_some_and(|url| url.starts_with("http://127.0.0.1:")));
        control.send(Control::Quit).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn the_scan_starts_once_when_someone_looks_and_not_when_it_is_not_wanted() {
        assert!(should_scan(true, false, 1));
        assert!(should_scan(true, false, 3));
        assert!(!should_scan(true, false, 0), "not before a browser has come");
        assert!(!should_scan(true, true, 1), "not twice");
        assert!(!should_scan(false, false, 1), "not when it is not wanted");
    }
}
