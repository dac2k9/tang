//! The web runtime as the thread that owns the controller sees it; see
//! `docs/web-ui.md` §3.1 and §3.2.
//!
//! A `Runtime` is everything the browser side needs, built in one place: the hub the
//! clients are registered in, what the connections read, the owner that answers their
//! requests on the controller's thread, and the server with its telemetry pusher.
//! The owner's thread builds it with `start` and from then on, on every turn of its
//! loop, answers the requests that have come in and keeps what the connections read
//! current. `run_headless` is that loop for `tang serve`; the TUI loop of `tang --web`
//! (step 13) will do the same among its own business.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;

use crate::app::controller::Controller;
use crate::app::intent::Applied;
use crate::web::Shared;
use crate::web::assets::Embedded;
use crate::web::connection::OwnerRequest;
use crate::web::hub::Hub;
use crate::web::owner::Owner;
use crate::web::server::{Config, Parts, Server};

/// How many frames a client's queue holds: some seconds of telemetry at 30 Hz for two
/// channels, or a good many events. A client that fills it is not keeping up.
pub const CLIENT_QUEUE: usize = 256;

/// How many requests may wait for the owner at once. A drag on a slider is a few dozen a
/// second and the owner takes microseconds for one; this is room for every client's burst
/// (`Limits::burst`) with some to spare. A request beyond it is answered "busy".
pub const OWNER_QUEUE: usize = 1024;

/// How often the headless loop polls the controller (the plugins the audio thread hands
/// back, MIDI devices coming and going, what it reports) and refreshes what the
/// connections read.
const TICK: Duration = Duration::from_millis(50);

/// How long the headless loop waits between two turns of the plugins' windows while one is open (`Controller::pump_windows`, which itself waits for an event for a moment): short enough for a window that answers the mouse.
const WINDOW_TURN: Duration = Duration::from_millis(1);

pub struct Runtime {
    // (The server goes first when the runtime is dropped: the clients are closed before
    // the owner stops answering them.)
    server: Server,
    owner: Owner,
}

impl Runtime {
    /// Start serving `ctl`'s session. From now on what the controller applies, whichever
    /// front-end asked for it, is told to the clients that have subscribed.
    pub fn start(ctl: &mut Controller, mut config: Config) -> anyhow::Result<Runtime> {
        // (A page shows its Open button when the program that owns the controller can open another session: the hello says so.)
        config.open_session = ctl.open_session.is_some();
        let hub = Arc::new(Hub::new(CLIENT_QUEUE));
        let shared = Arc::new(Shared::new(ctl.rev, ctl.model.telemetry_layout()));
        let (owner_tx, owner_rx) = crossbeam_channel::bounded(OWNER_QUEUE);
        let owner = Owner::new(ctl, hub.clone(), shared.clone(), owner_rx);
        let parts =
            Parts { assets: Arc::new(Embedded), hub, shared, owner: owner_tx, notes: ctl.note_input(), telemetry: ctl.telemetry.clone() };
        let server = Server::start(config, parts)?;
        Ok(Runtime { server, owner })
    }

    /// Where the server listens.
    pub fn addr(&self) -> std::net::SocketAddr {
        self.server.addr()
    }

    /// The address to open in a browser (with the token in it, if there is one).
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Stop the server, as if its thread had died: nothing can send a request any more.
    #[cfg(test)]
    pub fn stop_server(&mut self) {
        self.server.stop();
    }

    /// How many browsers are connected.
    pub fn clients(&self) -> usize {
        self.owner.clients()
    }

    /// The requests the browsers make, for a loop that waits for them along with other
    /// things (a clone of the receiver can be selected on).
    pub fn requests(&self) -> Receiver<OwnerRequest> {
        self.owner.requests().clone()
    }

    /// Answer `request`, and the ones that are waiting behind it; the intents that were
    /// applied, for the loop to react to.
    pub fn serve(&mut self, ctl: &mut Controller, request: OwnerRequest) -> Vec<Applied> {
        let mut applied: Vec<Applied> = self.owner.handle(ctl, request).into_iter().collect();
        self.owner.serve_with(ctl, |intent| applied.push(intent));
        applied
    }

    /// Keep what the connections read current: call it on every turn of a loop that is
    /// not the headless one.
    pub fn refresh(&mut self, ctl: &mut Controller) {
        self.owner.refresh(ctl);
        self.owner.refresh_probes(ctl);
    }

    /// The loop of a headless server: answer requests as they come, and every tick poll
    /// the controller and refresh. Returns when `quit` says so (or its senders are gone).
    pub fn run_headless(&mut self, ctl: &mut Controller, quit: &Receiver<()>) {
        let requests = self.owner.requests().clone();
        let tick = crossbeam_channel::tick(TICK);
        loop {
            // While a plugin's own window is open (step 112) its events are turned between the rest, a few milliseconds apart; with none open, and while the plugin scan goes (f179), this never comes.
            let windows = if ctl.windows_open() { crossbeam_channel::after(WINDOW_TURN) } else { crossbeam_channel::never() };
            crossbeam_channel::select! {
                recv(requests) -> request => match request {
                    Ok(request) => {
                        self.owner.handle(ctl, request);
                        self.owner.serve(ctl);
                    }
                    Err(_) => break,
                },
                recv(tick) -> _ => {
                    ctl.poll();
                    self.refresh(ctl);
                }
                recv(windows) -> _ => {
                    // (What a knob in a window did is applied by the controller's `poll`, here and not only at the next tick, so that the sound follows the hand.)
                    ctl.pump_windows();
                    ctl.poll();
                }
                recv(quit) -> _ => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::thread::{JoinHandle, ThreadId};

    use serde_json::{Value, json};

    use super::*;
    use crate::app::intent::{Gain, Intent};
    use crate::app::testutil::{self, DropProbe, StubEngine, load};
    use crate::source::{MidiEvent, Source};
    use crate::web::testclient::{self, recv, said, tell, until_closed, wait_until};

    /// The tracer: an oscillator through a filter whose cutoff an LFO drives.
    const SESSION: &str = r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument.effect]]
plugin = "builtin:filter"

[[instrument.modulator]]
type = "lfo"
rate = 0.5

[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5
"#;

    /// A controller over a stub engine with a headless runtime on its own thread, the way
    /// `tang serve` runs one, and the engine's other end for the test to look at.
    struct Live {
        addr: SocketAddr,
        engine: StubEngine,
        /// What the (stub) audio thread would publish.
        telemetry: Arc<crate::telemetry::Telemetry>,
        owner_thread: ThreadId,
        quit: Option<crossbeam_channel::Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    fn live() -> Live {
        started(false)
    }

    /// As `live`, over a controller whose program can open another session (`Controller::open_session`: a plugin in a host).
    fn started(opens: bool) -> Live {
        started_with(opens, None, None)
    }

    /// As `started`, with libraries of sounds for the page (step 116), and a list that the sessions that the controller is asked to open are put in.
    fn started_with(opens: bool, sounds: Option<Arc<crate::app::sounds::Sounds>>, opened: Option<Arc<std::sync::Mutex<Vec<String>>>>) -> Live {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (quit_tx, quit_rx) = crossbeam_channel::bounded(1);
        let thread = std::thread::spawn(move || {
            let (handles, engine) = testutil::stub_engine();
            let telemetry = handles.telemetry.clone();
            let mut ctl = Controller::new(load(SESSION).model, handles, None);
            if opens {
                ctl.open_session = Some(Box::new(move |text| {
                    if let Some(opened) = &opened {
                        opened.lock().unwrap().push(text);
                    }
                    Ok(())
                }));
            }
            let mut config = Config::new("127.0.0.1:0".parse().unwrap());
            config.sounds = sounds;
            let mut runtime = Runtime::start(&mut ctl, config).unwrap();
            ready_tx.send((runtime.addr(), engine, telemetry)).unwrap();
            runtime.run_headless(&mut ctl, &quit_rx);
        });
        let (addr, engine, telemetry) = ready_rx.recv_timeout(Duration::from_secs(10)).expect("the runtime did not start");
        let owner_thread = thread.thread().id();
        Live { addr, engine, telemetry, owner_thread, quit: Some(quit_tx), thread: Some(thread) }
    }

    impl Live {
        /// Stop the loop, as `tang serve` does on Ctrl+C, and wait for it.
        fn stop(&mut self) {
            self.quit.take();
            if let Some(thread) = self.thread.take() {
                thread.join().expect("the owner's thread panicked");
            }
        }

        /// What the engine has been sent since the last look, leaving out the telemetry slots.
        fn commands(&self) -> Vec<String> {
            self.engine.drain().0
        }
    }

    impl Drop for Live {
        fn drop(&mut self) {
            self.quit.take();
            if let Some(thread) = self.thread.take() {
                // (Not `expect`: this may be running while a test is unwinding.)
                let _ = thread.join();
            }
        }
    }

    /// The node and parameter of `plugin`'s `param` in a snapshot message, and the value it has.
    fn find_param(message: &Value, node: &str, param: &str) -> (u64, u64, f64) {
        let nodes = message["snapshot"]["graph"]["nodes"].as_array().expect("a snapshot with nodes");
        let node = nodes.iter().find(|n| n["name"] == node).unwrap_or_else(|| panic!("no node {node}"));
        let param = node["plugin"]["params"].as_array().unwrap().iter().find(|p| p["name"] == param).unwrap_or_else(|| panic!("no {param}"));
        (node["id"].as_u64().unwrap(), param["index"].as_u64().unwrap(), param["value"].as_f64().unwrap())
    }

    fn set_param(req_id: u32, node: u64, param: u64, value: f64) -> Value {
        json!({"type": "request", "reqId": req_id, "intent": {"type": "setParam", "node": node, "param": param, "value": value}})
    }

    async fn subscribed(live: &Live) -> (testclient::Client, Value) {
        let mut client = testclient::connect(live.addr).await;
        assert_eq!(said(&mut client).await["type"], "hello");
        let snapshot = testclient::subscribe(&mut client).await;
        (client, snapshot)
    }

    #[tokio::test]
    async fn the_hello_says_that_the_program_can_open_another_session_only_when_its_controller_can() {
        let features = |hello: &Value| hello["features"].as_array().unwrap().iter().filter_map(Value::as_str).map(String::from).collect::<Vec<_>>();
        let plain = live();
        let mut client = testclient::connect(plain.addr).await;
        assert!(!features(&said(&mut client).await).contains(&"openSession".to_string()), "a program that is started with its file cannot");
        let opens = started(true);
        let mut client = testclient::connect(opens.addr).await;
        assert!(features(&said(&mut client).await).contains(&"openSession".to_string()), "a plugin in a host can");
    }

    /// A folder with two sounds in it, as libraries of a program: the folder is kept as long as the value is.
    fn folder_of_sounds() -> (Arc<crate::app::sounds::Sounds>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pad.toml"), "[[instrument]]\nplugin = \"builtin:osc\"\nrange = \"C4-C5\"\n").unwrap();
        std::fs::write(dir.path().join("broken.toml"), "this is {not toml").unwrap();
        let folder = crate::app::sounds::Folder::new("here", "Here", dir.path().to_path_buf());
        (Arc::new(crate::app::sounds::Sounds::new(vec![Box::new(folder)])), dir)
    }

    #[tokio::test]
    async fn a_page_lists_the_sounds_of_a_library_that_the_hello_names_and_opens_one_as_a_session() {
        let (sounds, _dir) = folder_of_sounds();
        let opened = Arc::new(std::sync::Mutex::new(Vec::new()));
        let live = started_with(true, Some(sounds), Some(opened.clone()));
        let mut client = testclient::connect(live.addr).await;
        let hello = said(&mut client).await;
        assert_eq!(hello["libraries"].as_array().unwrap().len(), 1, "{hello}");
        assert_eq!((hello["libraries"][0]["id"].as_str(), hello["libraries"][0]["label"].as_str()), (Some("here"), Some("Here")));
        let _ = testclient::subscribe(&mut client).await;

        tell(&mut client, json!({"type": "listSounds", "reqId": 5, "library": "here"})).await;
        let listing = said(&mut client).await;
        assert_eq!((listing["type"].as_str(), listing["reqId"].as_u64(), listing["library"].as_str()), (Some("sounds"), Some(5), Some("here")), "{listing}");
        let mut names: Vec<(&str, bool)> = listing["entries"].as_array().unwrap().iter().map(|entry| (entry["name"].as_str().unwrap(), entry["openable"].as_bool().unwrap())).collect();
        names.sort();
        assert_eq!(names, [("broken", false), ("pad", true)]);

        tell(&mut client, json!({"type": "openSound", "reqId": 6, "library": "here", "id": "pad.toml"})).await;
        assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 6, "rev": 1}));
        assert_eq!(*opened.lock().unwrap(), ["[[instrument]]\nplugin = \"builtin:osc\"\nrange = \"C4-C5\"\n"], "the controller was handed the sound's text, to open it");

        tell(&mut client, json!({"type": "openSound", "reqId": 7, "library": "here", "id": "broken.toml"})).await;
        let refused = said(&mut client).await;
        assert_eq!((refused["type"].as_str(), refused["reqId"].as_u64(), refused["error"]["code"].as_str()), (Some("rejected"), Some(7), Some("invalid")), "{refused}");
        assert!(refused["error"]["message"].as_str().unwrap().starts_with("that is not a session: "), "{refused}");
        assert_eq!(opened.lock().unwrap().len(), 1, "a sound that is not a session is not handed on");

        tell(&mut client, json!({"type": "openSound", "reqId": 8, "library": "here", "id": "../escape.toml"})).await;
        let refused = said(&mut client).await;
        assert_eq!((refused["type"].as_str(), refused["reqId"].as_u64()), (Some("rejected"), Some(8)), "{refused}");
        tell(&mut client, json!({"type": "listSounds", "reqId": 9, "library": "elsewhere"})).await;
        let refused = said(&mut client).await;
        assert_eq!((refused["error"]["code"].as_str(), refused["reqId"].as_u64()), (Some("notFound"), Some(9)), "{refused}");
    }

    /// A library that takes its time to be let go of, and says when it has been: the desk's thread lets go of it when it ends.
    #[derive(Debug)]
    struct SlowToLetGo(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SlowToLetGo {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_millis(300));
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl crate::app::sounds::Library for SlowToLetGo {
        fn info(&self) -> crate::app::sounds::SoundLibrary {
            crate::app::sounds::SoundLibrary { id: "slow".to_string(), label: "Slow".to_string(), place: "nowhere".to_string() }
        }
        fn list(&self) -> Result<crate::app::sounds::Listing, crate::app::protocol::WireError> {
            Ok(crate::app::sounds::Listing { entries: Vec::new(), truncated: false })
        }
        fn read(&self, _: &str) -> Result<String, crate::app::protocol::WireError> {
            Err(crate::app::protocol::WireError::new(crate::app::protocol::ErrorCode::NotFound, "none"))
        }
    }

    #[test]
    fn stopping_the_server_waits_for_the_thread_that_lists_sounds_which_a_plugin_that_is_unloaded_must_not_leave_running() {
        let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sounds = Arc::new(crate::app::sounds::Sounds::new(vec![Box::new(SlowToLetGo(gone.clone()))]));
        let mut live = started_with(true, Some(sounds), None);
        live.stop();
        assert!(gone.load(std::sync::atomic::Ordering::SeqCst), "the thread had ended, and let go of its libraries, by the time the server had stopped");
    }

    #[tokio::test]
    async fn a_program_with_no_libraries_lists_none_and_has_no_sounds_to_open() {
        for sounds in [None, Some(Arc::new(crate::app::sounds::Sounds::default()))] {
            let live = started_with(true, sounds, None);
            let mut client = testclient::connect(live.addr).await;
            assert_eq!(said(&mut client).await["libraries"], json!([]));
            let _ = testclient::subscribe(&mut client).await;
            tell(&mut client, json!({"type": "listSounds", "reqId": 1, "library": "here"})).await;
            let refused = said(&mut client).await;
            assert_eq!((refused["type"].as_str(), refused["error"]["message"].as_str()), (Some("rejected"), Some("this program has no sounds to list")), "{refused}");
        }
    }

    #[tokio::test]
    async fn a_parameter_set_in_the_browser_is_applied_told_to_every_client_and_sent_to_the_engine() {
        let live = live();
        let (mut a, snapshot) = subscribed(&live).await;
        let (mut b, _) = subscribed(&live).await;
        let (filter, cutoff, before) = find_param(&snapshot, "Filter", "cutoff");
        assert_ne!(before, 0.9);
        assert!(live.commands().is_empty(), "nothing has been sent to the engine yet");

        tell(&mut a, set_param(7, filter, cutoff, 0.9)).await;
        // The requester hears the event and then the answer; the other client the event.
        let event = said(&mut a).await;
        assert_eq!(event["type"], "event");
        assert_eq!(event["event"], json!({"rev": 1, "type": "paramSet", "node": filter, "param": cutoff, "value": 0.9}));
        let history = json!({"type": "history", "undo": "set cutoff", "redo": null});
        assert_eq!(said(&mut a).await, history, "what can be undone now, after the event");
        assert_eq!(said(&mut a).await, json!({"type": "ack", "reqId": 7, "rev": 1}));
        assert_eq!(said(&mut b).await, event);
        assert_eq!(said(&mut b).await, history);

        // The engine has it too: the filter is the effect in slot 1 of lane 0.
        assert_eq!(live.commands(), ["SetParameter inst=0 slot=1 param=0 value=0.900"]);

        // A client that comes late is told where the session is, and starts from there.
        let mut late = testclient::connect(live.addr).await;
        assert_eq!(said(&mut late).await["rev"], 1, "Hello");
        let snapshot = testclient::subscribe(&mut late).await;
        assert_eq!(snapshot["snapshot"]["rev"], 1);
        assert_eq!(find_param(&snapshot, "Filter", "cutoff").2, 0.9);
    }

    #[tokio::test]
    async fn the_engines_telemetry_reaches_the_clients_that_ask_for_it() {
        use crate::telemetry::MASTER;
        let live = live();
        let (mut a, _) = subscribed(&live).await;
        live.telemetry.store(MASTER.peak, 0.625);
        tell(&mut a, json!({"type": "telemetry", "channels": ["meters"]})).await;
        // (A frame or two from before the value was stored may come first.)
        let found = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match recv(&mut a).await {
                    Some(tokio_tungstenite::tungstenite::Message::Binary(bytes)) => {
                        assert_eq!(bytes[0], 0, "the meters");
                        if f32::from_le_bytes(bytes[8..12].try_into().unwrap()) == 0.625 {
                            return;
                        }
                    }
                    other => panic!("{other:?}"),
                }
            }
        })
        .await;
        assert!(found.is_ok(), "the master's peak never arrived");
    }

    #[tokio::test]
    async fn a_runtime_started_on_a_session_that_has_changed_says_where_it_is() {
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
        let node = ctl.model.instruments[0].node;
        ctl.apply(Intent::SetGain { node, gain: Gain::Volume, value: 0.5 }).unwrap();
        let runtime = Runtime::start(&mut ctl, Config::new("127.0.0.1:0".parse().unwrap())).unwrap();
        let mut client = testclient::connect(runtime.addr()).await;
        assert_eq!(said(&mut client).await["rev"], 1);
    }

    #[tokio::test]
    async fn a_request_that_cannot_be_applied_is_rejected_and_changes_nothing() {
        let live = live();
        let (mut a, _) = subscribed(&live).await;
        let (mut b, _) = subscribed(&live).await;
        tell(&mut a, set_param(3, 424_242, 0, 0.5)).await;
        let rejected = said(&mut a).await;
        assert_eq!((rejected["type"].clone(), rejected["reqId"].clone(), rejected["error"]["code"].clone()), (json!("rejected"), json!(3), json!("notFound")));
        assert!(live.commands().is_empty());
        assert!(tokio::time::timeout(Duration::from_millis(150), recv(&mut b)).await.is_err(), "nobody else is told");
    }

    #[tokio::test]
    async fn one_clients_requests_are_applied_in_the_order_they_were_made() {
        let live = live();
        let (mut a, snapshot) = subscribed(&live).await;
        let (filter, cutoff, _) = find_param(&snapshot, "Filter", "cutoff");
        for n in 0..30u32 {
            tell(&mut a, set_param(n, filter, cutoff, 0.01 * (n + 1) as f64)).await;
        }
        let mut acks = Vec::new();
        while acks.len() < 30 {
            let message = said(&mut a).await;
            if message["type"] == "ack" {
                acks.push((message["reqId"].as_u64().unwrap(), message["rev"].as_u64().unwrap()));
            }
        }
        assert_eq!(acks, (0..30).map(|n| (n, n + 1)).collect::<Vec<(u64, u64)>>());
        let sent = live.commands();
        assert_eq!(sent.len(), 30);
        assert_eq!(sent[0], "SetParameter inst=0 slot=1 param=0 value=0.010");
        assert_eq!(sent[29], "SetParameter inst=0 slot=1 param=0 value=0.300");
    }

    #[tokio::test]
    async fn a_note_from_the_browser_plays_in_the_engine_and_ends_with_the_connection() {
        let live = live();
        let (mut a, _) = subscribed(&live).await;
        tell(&mut a, json!({"type": "noteOn", "note": 64, "velocity": 90})).await;
        assert_eq!(live.engine.midi_rx.recv_timeout(Duration::from_secs(5)).unwrap(), (0, [0x90, 64, 90]));
        // The tab is closed with the key still down.
        drop(a);
        assert_eq!(live.engine.midi_rx.recv_timeout(Duration::from_secs(5)).unwrap(), (0, [0x80, 64, 0]));
    }

    #[tokio::test]
    async fn a_panic_from_the_browser_reaches_the_engine_as_the_messages_that_let_go_of_everything_and_closing_the_tab_ends_nothing_again() {
        let live = live();
        let (mut a, _) = subscribed(&live).await;
        tell(&mut a, json!({"type": "noteOn", "note": 64, "velocity": 90})).await;
        assert_eq!(live.engine.midi_rx.recv_timeout(Duration::from_secs(5)).unwrap(), (0, [0x90, 64, 90]));
        tell(&mut a, json!({"type": "panic"})).await;
        let mut heard = Vec::new();
        while heard.len() < 16 + 1 + 16 + 16 {
            heard.push(live.engine.midi_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        }
        // (Every lane hears a panic, whatever sources it hears: it is no source's.)
        let to_all = |bytes: [u8; 3]| MidiEvent::from_source(Source::BROADCAST, 0, bytes);
        assert_eq!(heard[0], to_all([0xB0, 64, 0]), "the pedal goes up first");
        assert_eq!(heard[16], to_all([0x80, 64, 0]), "then the note that is held");
        assert_eq!(heard[17], to_all([0xB0, 123, 0]), "then all notes off");
        assert_eq!(heard[33], to_all([0xB0, 120, 0]), "then all sound off");
        // The tab is closed after it: the note was ended, and is not ended a second time.
        drop(a);
        assert!(live.engine.midi_rx.recv_timeout(Duration::from_millis(300)).is_err(), "nothing more was sent");
    }

    #[tokio::test]
    async fn a_recording_the_audio_thread_reports_reaches_the_browsers_as_a_snapshot() {
        let live = live();
        let (mut a, snapshot) = subscribed(&live).await;
        assert_eq!(snapshot["snapshot"]["meta"]["dirty"], false);
        live.engine.pattern_tx.send(testutil::recording(0)).unwrap();
        let told = tokio::time::timeout(Duration::from_secs(5), said(&mut a)).await.expect("nothing was told");
        assert_eq!(told["type"], "snapshot");
        assert_eq!(told["snapshot"]["meta"]["dirty"], true, "the recording is part of the session");
        assert_eq!(told["snapshot"]["rev"], 1, "a revision of its own");
    }

    #[tokio::test]
    async fn the_clients_that_are_connected_are_counted() {
        let live = live();
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
        let runtime = Runtime::start(&mut ctl, Config::new("127.0.0.1:0".parse().unwrap())).unwrap();
        assert_eq!(runtime.clients(), 0);
        let a = testclient::connect(runtime.addr()).await;
        wait_until("the connection was not counted", || runtime.clients() == 1).await;
        let b = testclient::connect(runtime.addr()).await;
        wait_until("the second connection was not counted", || runtime.clients() == 2).await;
        drop(a);
        wait_until("the closed connection was still counted", || runtime.clients() == 1).await;
        drop(b);
        let _ = live;
    }

    #[tokio::test]
    async fn the_loop_reaps_what_the_audio_thread_hands_back_on_its_own_thread() {
        let live = live();
        let (probe, dropped_on) = DropProbe::recording();
        live.engine.return_tx.send(Box::new(probe)).unwrap();
        wait_until("the plugin was not dropped", || dropped_on.lock().unwrap().is_some()).await;
        assert_eq!(*dropped_on.lock().unwrap(), Some(live.owner_thread), "not on the audio thread's side, nor on the server's");
    }

    #[tokio::test]
    async fn quitting_closes_the_clients_and_stops_the_server() {
        let mut live = live();
        let (mut a, _) = subscribed(&live).await;
        let addr = live.addr;
        live.stop();
        let (_texts, code) = until_closed(&mut a).await;
        assert_eq!(code, Some(1001));
        assert!(std::net::TcpStream::connect(addr).is_err(), "nobody listens any more");
    }

    #[tokio::test]
    async fn the_url_is_the_address_the_runtime_listens_on() {
        let live = live();
        let mut ctl = {
            let (handles, _engine) = testutil::stub_engine();
            Controller::new(load(SESSION).model, handles, None)
        };
        let runtime = Runtime::start(&mut ctl, Config::new("127.0.0.1:0".parse().unwrap())).unwrap();
        assert_eq!(runtime.url(), format!("http://{}/", runtime.addr()));
        assert_ne!(runtime.addr(), live.addr);
    }

    #[tokio::test]
    async fn a_second_runtime_cannot_take_the_address_of_the_first() {
        let live = live();
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, None);
        let error = Runtime::start(&mut ctl, Config::new(live.addr)).err().expect("the address is taken");
        assert!(error.to_string().contains("cannot listen"), "{error}");
    }
}
