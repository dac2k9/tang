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

/// What the controller asks of the host that owns it (the plugin's main thread, which makes the engine again): the page opened another session (`Intent::OpenSession`).
pub(super) enum Request {
    /// Make the session this text of a session file.
    OpenSession(String),
}

/// How the controller's thread reaches its host: a queue that the host's main thread reads (`Host::serve_requests`), and what wakes the main thread up to look (a plugin: `request_callback`, which may be called from any thread).
#[derive(Clone)]
pub(super) struct Owner {
    pub requests: Sender<Request>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
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
    /// How the controller reaches the host that owns it.
    pub owner: Owner,
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

/// Whether the scan of the installed plugins starts now: it is wanted (`TANG_SCAN` is set: **it is not by default**), it has not been started, and someone is looking at the page. A scan loads every plugin that is installed, on a thread
/// of its own, and lets go of it; in a host's process the host's main thread turns the run loop all the time, and a plugin of the JUCE kind (ROLI's players) that is loaded and let go of while it turns crashes the process (f179, found
/// again with the plugin's window: `tang window Tang.clap` died in `juce::MessageQueue::runLoopSourceCallback`). So a plugin lists the built-ins and the modules, and the plugins that are installed only for a person who has said so.
#[cfg(any(test, feature = "web"))]
fn should_scan(wanted: bool, started: bool, browsers: usize) -> bool {
    wanted && !started && browsers > 0
}

/// Whether `TANG_SCAN` asks for the scan: it is set to something that is not empty and not `0`.
#[cfg(any(test, feature = "web"))]
fn scan_asked_for(variable: Option<&std::ffi::OsStr>) -> bool {
    variable.is_some_and(|value| !value.is_empty() && value != "0")
}

/// Say why a plugin's own window cannot be opened here: the window is the main thread's, and the main thread is the host's.
fn no_windows() -> Result<(), String> {
    Err("a plugin's own window is not available inside a host yet".to_string())
}

/// The thread's body. The audio half of the engine goes to the host's side first (`audio`), so that it can take the commands that the load sends while it goes on; then the session is loaded and the page is served, and `started` says how it went.
pub(super) fn run(setup: Setup, audio: Sender<(AudioGraph, Receiver<MidiEvent>)>, started: Sender<Result<Started, String>>, control: Receiver<Control>) {
    let Setup { text, sample_rate, max_frames, serve, bind, input, owner } = setup;
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
    let mut ctl = embedded(model, handles, owner);

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

/// The controller of a session that is in a host: the library of modules, no window of a plugin's own, the host as a device, and the host to ask when the page opens another session (`Intent::OpenSession`: the engine is made
/// again by the host's main thread, as it is for a project that the host opens, so the controller hands the text over and has the main thread called back).
fn embedded(model: crate::app::model::SessionModel, handles: crate::app::controller::EngineHandles, owner: Owner) -> Controller {
    let mut ctl = Controller::new(model, handles, None);
    let Owner { requests, wake } = owner;
    ctl.open_session = Some(Box::new(move |text| {
        requests.send(Request::OpenSession(text)).map_err(|_| "the plugin that holds this session has stopped".to_string())?;
        wake();
        Ok(())
    }));
    ctl.modules_dir = crate::modules_dir();
    ctl.refresh_modules();
    // (What can be listed without loading a plugin: the built-ins.)
    ctl.catalog.builtins();
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
/// installed plugins loads every one of them, which is not safe in a host's process (`should_scan`): it starts when the first browser has come, and only if `TANG_SCAN` is set.
#[cfg(feature = "web")]
fn serve_session(ctl: &mut Controller, started: &Sender<Result<Started, String>>, control: &Receiver<Control>, bind: std::net::SocketAddr) -> anyhow::Result<()> {
    use crate::web::runtime::Runtime;
    use crate::web::server::Config;

    let mut runtime = Runtime::start(ctl, Config::new(bind))?;
    log::info!("Web interface: {}", runtime.url());
    let _ = started.send(Ok(Started { url: Some(runtime.url()) }));

    let requests = runtime.requests();
    let tick = crossbeam_channel::tick(TICK);
    let scan = scan_asked_for(std::env::var_os("TANG_SCAN").as_deref());
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
                    ctl.catalog.scan_installed();
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

    /// An owner that a test reads: what the controller asked, how many times it was woken, and how many of those times the request was already there to be found (a host's main thread looks the moment it is called).
    fn owner() -> (Owner, Receiver<Request>, Arc<std::sync::atomic::AtomicUsize>, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (requests, asked) = crossbeam_channel::unbounded();
        let (woken, found) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (counter, finds, looking) = (woken.clone(), found.clone(), asked.clone());
        let wake = move || {
            if !looking.is_empty() {
                finds.fetch_add(1, Ordering::SeqCst);
            }
            counter.fetch_add(1, Ordering::SeqCst);
        };
        (Owner { requests, wake: Arc::new(wake) }, asked, woken, found)
    }

    #[test]
    fn the_page_opening_another_session_is_handed_to_the_host_which_is_woken_when_the_request_is_there_to_be_found() {
        use crate::app::intent::Intent;
        use std::sync::atomic::Ordering;
        let (handles, _engine) = crate::app::testutil::stub_engine();
        let (owner, asked, woken, found) = owner();
        let mut ctl = embedded(crate::app::testutil::load("[[instrument]]\nplugin = \"builtin:osc\"\n").model, handles, owner);
        let text = "[[instrument]]\nplugin = \"builtin:osc\"\nrange = \"C4-C5\"\n";
        ctl.apply(Intent::OpenSession { text: text.to_string() }).expect("taken");
        assert!(matches!(asked.try_recv(), Ok(Request::OpenSession(handed)) if handed == text), "the text as it came");
        assert!(asked.try_recv().is_err(), "once");
        assert_eq!((woken.load(Ordering::SeqCst), found.load(Ordering::SeqCst)), (1, 1), "the host was woken once, and when the request was in its queue: a main thread that looks at once finds it");
    }

    #[test]
    fn a_host_that_has_gone_is_not_woken_and_the_page_is_told_why_in_words() {
        use crate::app::intent::{Intent, IntentError};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (handles, _engine) = crate::app::testutil::stub_engine();
        let (requests, queue) = crossbeam_channel::unbounded();
        drop(queue);
        let woken = Arc::new(AtomicUsize::new(0));
        let counter = woken.clone();
        let owner = Owner { requests, wake: Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }) };
        let mut ctl = embedded(crate::app::testutil::load("[[instrument]]\nplugin = \"builtin:osc\"\n").model, handles, owner);
        let refused = ctl.apply(Intent::OpenSession { text: "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string() });
        assert_eq!(refused, Err(IntentError::Invalid("the plugin that holds this session has stopped".to_string())));
        assert_eq!(woken.load(Ordering::SeqCst), 0, "nobody to wake");
    }

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
        let ctl = embedded(crate::app::testutil::load("[[instrument]]\nplugin = \"builtin:osc\"\n").model, handles, owner().0);
        assert!((ctl.window_check)().unwrap_err().contains("not available inside a host"), "the check that every window is asked is the host's");
        assert_eq!(ctl.inputs(), [HOST_DEVICE], "the host is the one input that is open");
        assert!(ctl.session_path.is_none(), "the host's project is where the session is kept, not a file");
    }

    #[test]
    fn the_scan_of_the_installed_plugins_is_asked_for_by_tang_scan_and_by_nothing_else() {
        use std::ffi::OsStr;
        assert!(!scan_asked_for(None), "it is not what a plugin does by itself");
        assert!(!scan_asked_for(Some(OsStr::new(""))), "an empty variable is not a yes");
        assert!(!scan_asked_for(Some(OsStr::new("0"))), "and a 0 is a no");
        assert!(scan_asked_for(Some(OsStr::new("1"))));
        assert!(scan_asked_for(Some(OsStr::new("yes"))));
    }

    #[test]
    fn the_controller_of_a_host_lists_the_built_ins_at_once_and_loads_no_plugin_to_do_it() {
        let (handles, _engine) = crate::app::testutil::stub_engine();
        let ctl = embedded(crate::app::testutil::load("[[instrument]]\nplugin = \"builtin:osc\"\n").model, handles, owner().0);
        assert!(!ctl.catalog.is_scanning(), "no scan of the installed plugins is started: that loads every one of them in the host's process");
        let ids: Vec<&str> = ctl.catalog.plugins().iter().map(|plugin| plugin.id.as_str()).collect();
        assert!(ids.contains(&"builtin:osc") && ids.contains(&"builtin:input"), "the built-ins are in the list, so that a lane can be added: {ids:?}");
        assert!(ids.iter().all(|id| id.starts_with("builtin:")), "and nothing else is: {ids:?}");
        assert!(ctl.catalog.rev() > 0, "the clients are told that there is a list");
    }

    /// The thread's own: a controller that is started on `text`, with `bind` for its page, whose engine's other end is drained as the host's idle thread does.
    #[cfg(feature = "web")]
    fn started(bind: std::net::SocketAddr) -> (Receiver<Result<Started, String>>, Sender<Control>, std::thread::JoinHandle<()>) {
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(1);
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        let setup = Setup { text: "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string(), sample_rate: 48_000.0, max_frames: 256, serve: true, bind, input: Arc::new(HostInput::new(256)), owner: owner().0 };
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
        assert!(!should_scan(false, false, 1), "not when it is not wanted, which is how it is unless `TANG_SCAN` says so");
    }
}
