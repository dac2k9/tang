//! `tang serve` as a person runs it: the real binary, on the null audio backend (no
//! sound card is opened, so nothing is heard), with a WebSocket client in front of it.
//!
//! What the unit tests of `web::` show with a stub engine, this shows with the whole
//! program: a browser that plays a note and moves a parameter hears it in the master's
//! meter, because the note went through the WebSocket to the note path, the parameter
//! through the owner to the audio thread, and the peak back through the atomics and the
//! telemetry frames. See `docs/web-ui.md` §12, step 10.

#![cfg(feature = "web")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// A saw oscillator through a filter an LFO sweeps; quiet on purpose. The tests' own copy of the tracer scene, and not
/// `examples/tracer.toml`, which a person who runs the program on it can save over.
const TRACER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/web/src/client/fixtures/tracer.toml");

/// `NodeId::MASTER`, as a client sees it.
const MASTER: u64 = u32::MAX as u64;

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A child process that is killed when this goes, however the test ends: one that panics
/// while the program is starting (it never says where it serves, say) must not leave it running.
struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running `tang serve`.
struct Tang {
    /// (Where there is no Ctrl+C to send, nothing reads it: it is held so that the program is killed when this goes.)
    #[cfg_attr(not(unix), allow(dead_code))]
    child: Process,
    addr: SocketAddr,
    /// What it has logged (its stderr), for the message of a test that fails.
    log: Arc<Mutex<String>>,
    /// The library of modules it was started with (`TANG_MODULES_DIR`): a directory of the test's own, so that no test reads or writes the library of the person who runs them.
    library: std::path::PathBuf,
    /// (Kept so that a directory that was made for the program goes when the program does.)
    _own_library: Option<tempfile::TempDir>,
}

impl Tang {
    fn start() -> Tang {
        Tang::start_on(TRACER)
    }

    /// `tang serve` on the session in `session`.
    fn start_on(session: impl AsRef<std::ffi::OsStr>) -> Tang {
        Tang::start_in(None, session)
    }

    /// `tang serve` on the session in `session`, started in the directory `dir` (where the test is, if none).
    fn start_in(dir: Option<&std::path::Path>, session: impl AsRef<std::ffi::OsStr>) -> Tang {
        // No sound card and no MIDI input is opened: nothing is heard, nothing is listened to.
        Tang::start_with(dir, session, "no such device")
    }

    /// `tang serve` on the session in `session`, listening to the MIDI input called `midi_device` (and to no sound card).
    #[cfg(unix)]
    fn start_listening_to(session: impl AsRef<std::ffi::OsStr>, midi_device: &str) -> Tang {
        Tang::start_with(None, session, midi_device)
    }

    fn start_with(dir: Option<&std::path::Path>, session: impl AsRef<std::ffi::OsStr>, midi_device: &str) -> Tang {
        Tang::start_with_library(dir, session, midi_device, None)
    }

    /// `tang serve` on the session in `session`, with the library of modules in `library` (an empty directory that is made for it, if none).
    fn start_with_library(dir: Option<&std::path::Path>, session: impl AsRef<std::ffi::OsStr>, midi_device: &str, library: Option<&std::path::Path>) -> Tang {
        let own_library = library.is_none().then(|| tempfile::tempdir().expect("a directory for the library"));
        let library = library.map(std::path::Path::to_path_buf).unwrap_or_else(|| own_library.as_ref().expect("made above").path().to_path_buf());
        let mut command = Command::new(env!("CARGO_BIN_EXE_tang"));
        command
            .args(["--audio-device", "none", "--midi-device", midi_device, "serve"])
            .arg(session)
            .args(["--web-bind", "127.0.0.1:0"])
            .env("TANG_MODULES_DIR", &library)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = dir {
            command.current_dir(dir);
        }
        let mut child = Process(command.spawn().expect("tang starts"));

        let log = Arc::new(Mutex::new(String::new()));
        let stderr = child.0.stderr.take().unwrap();
        let sink = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut log = sink.lock().unwrap();
                log.push_str(&line);
                log.push('\n');
            }
        });

        // Where it serves is the first line on stdout.
        let stdout = child.0.stdout.take().unwrap();
        let (lines, first) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = lines.send(line);
            }
        });
        let line = first
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("tang did not say where it serves within a minute:\n{}", log.lock().unwrap()));
        let url = line.strip_prefix("Web interface: http://").unwrap_or_else(|| panic!("an unexpected first line: {line:?}"));
        let addr = url.trim_end_matches('/').parse().unwrap_or_else(|e| panic!("{url:?}: {e}"));
        Tang { child, addr, log, library, _own_library: own_library }
    }

    fn log(&self) -> String {
        self.log.lock().unwrap().clone()
    }

    /// Ask it to stop the way Ctrl+C does.
    #[cfg(unix)]
    fn interrupt(&self) {
        let status = Command::new("kill").args(["-INT", &self.child.0.id().to_string()]).status().expect("kill runs");
        assert!(status.success());
    }

    #[cfg(unix)]
    fn exit_within(&mut self, within: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Some(status) = self.child.0.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

/// A WebSocket the way a browser opens one from the page: with its origin.
async fn connect_as(tang: &Tang, origin: &str) -> Result<Client, tungstenite::Error> {
    let mut request = format!("ws://{}/ws", tang.addr).into_client_request().unwrap();
    request.headers_mut().insert("origin", origin.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.map(|(client, _response)| client)
}

async fn connect(tang: &Tang) -> Client {
    connect_as(tang, &format!("http://{}", tang.addr)).await.expect("the server takes the page's own origin")
}

async fn tell(client: &mut Client, message: Value) {
    client.send(Message::text(message.to_string())).await.unwrap();
}

/// The next JSON text (the telemetry frames that come between are skipped).
async fn next_json(client: &mut Client) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), client.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => return serde_json::from_str(text.as_str()).unwrap(),
            Ok(Some(Ok(_))) => {}
            other => panic!("the connection gave {other:?} while a message was awaited"),
        }
    }
    panic!("no message in twenty seconds");
}

/// The answer to request `req_id`: the next `ack` or `rejected` for it (`event`s come first).
async fn answer(client: &mut Client, req_id: u64) -> Value {
    loop {
        let message = next_json(client).await;
        if message["reqId"] == req_id {
            return message;
        }
    }
}

/// Where in a meters frame the master's peak is, from the layout of a snapshot.
fn master_peak_at(snapshot: &Value) -> usize {
    let meters = snapshot["snapshot"]["telemetry"]["meters"].as_array().expect("a meters list");
    meters.iter().position(|e| e["node"] == MASTER && e["kind"] == "peak").expect("the master has a peak")
}

/// A snapshot's node by name, and a parameter of its plugin by name: (node id, parameter index).
fn param_of(snapshot: &Value, node: &str, param: &str) -> (u64, u64) {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let node = nodes.iter().find(|n| n["name"] == node).unwrap_or_else(|| panic!("no node {node}"));
    let param = node["plugin"]["params"].as_array().unwrap().iter().find(|p| p["name"] == param).unwrap_or_else(|| panic!("no {param}"));
    (node["id"].as_u64().unwrap(), param["index"].as_u64().unwrap())
}

/// Read frames until the master's peak satisfies `done`, and return it. Fails, with what
/// the server logged, when it has not within `within`.
async fn peak_until(client: &mut Client, tang: &Tang, at: usize, within: Duration, what: &str, done: impl Fn(f32) -> bool) -> f32 {
    let deadline = Instant::now() + within;
    let mut last = f32::NAN;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), client.next()).await {
            Ok(Some(Ok(Message::Binary(frame)))) => {
                assert_eq!(frame[0], 0, "a meters frame");
                last = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
                if done(last) {
                    return last;
                }
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(Some(Err(_))) | Ok(None) => panic!("the connection ended while waiting for {what}\n{}", tang.log()),
        }
    }
    panic!("{what}: the master's peak was {last} after {within:?}\n{}", tang.log());
}

/// Connected, subscribed, and asking for the meters: the client, and where the master's peak is.
async fn listening(tang: &Tang) -> (Client, usize, Value) {
    let mut client = connect(tang).await;
    let hello = next_json(&mut client).await;
    assert_eq!((hello["type"].clone(), hello["protocol"].clone(), hello["needsToken"].clone()), (json!("hello"), json!(1), json!(false)));
    tell(&mut client, json!({"type": "subscribe"})).await;
    let snapshot = next_json(&mut client).await;
    assert_eq!(snapshot["type"], "snapshot", "{snapshot}");
    let at = master_peak_at(&snapshot);
    tell(&mut client, json!({"type": "telemetry", "channels": ["meters"]})).await;
    (client, at, snapshot)
}

const SOON: Duration = Duration::from_secs(10);

#[tokio::test]
async fn a_browser_plays_a_note_and_moves_a_parameter_and_hears_it_in_the_meters() {
    let tang = Tang::start();
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, volume) = param_of(&snapshot, "Oscillator", "volume");

    // Nothing is playing: the meter is at rest.
    peak_until(&mut client, &tang, at, SOON, "silence before any note", |peak| peak == 0.0).await;

    // A key pressed in the browser sounds; released, it dies away.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // The oscillator's volume turned to nothing: the same note is silent, and the browser is told it was done.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": osc, "param": volume, "value": 0.0}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    // (The oscillator eases its volume over a few milliseconds, and a note that starts in the
    // block where it changes is heard with the old one fading out, so give it a moment.)
    tokio::time::sleep(Duration::from_millis(100)).await;
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let quiet_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < quiet_until {
        match tokio::time::timeout(Duration::from_millis(200), client.next()).await {
            Ok(Some(Ok(Message::Binary(frame)))) => {
                let peak = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
                assert!(peak < 0.001, "the volume is 0 and the meter says {peak}");
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            other => panic!("{other:?}"),
        }
    }

    // And back up: it was the parameter that silenced it, not a dead engine.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setParam", "node": osc, "param": volume, "value": 1.0}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the note with the volume back", |peak| peak > 0.01).await;
}

#[tokio::test]
async fn a_browser_saves_the_session_to_its_own_file_and_the_file_has_what_it_changed() {
    // A copy of the tracer, not the example: the program writes where the session came from.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    assert_eq!(snapshot["snapshot"]["meta"]["file"], "jam.toml", "the session is known by its file's name, not its path");
    let (filter, resonance) = param_of(&snapshot, "Filter", "resonance");

    // A change is in the session and not in the file.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": filter, "param": resonance, "value": 7.5}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    let before = std::fs::read_to_string(&file).unwrap();
    assert!(!before.contains("resonance = 7.5"), "{before}");

    // Saving is a request like the others, and answered by the revision it made, and the event that says so.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    let mut said = Vec::new();
    let saved = loop {
        let message = next_json(&mut client).await;
        if message["reqId"] == 2 {
            break message;
        }
        said.push(message);
    };
    assert_eq!((saved["type"].clone(), saved["rev"].clone()), (json!("ack"), json!(2)), "{saved}");
    assert!(
        said.iter().any(|m| m["type"] == "event" && m["event"]["type"] == "saved" && m["event"]["file"] == "jam.toml" && m["event"]["rev"] == 2),
        "the client is told of the save: {said:?}"
    );
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.contains("resonance = 7.5"), "the file has the change:\n{after}");

    // A client that arrives now is given a session with nothing unsaved.
    let (_, _, again) = listening(&tang).await;
    assert_eq!(again["snapshot"]["meta"]["dirty"], false);
}

#[tokio::test]
async fn a_browser_undoes_and_redoes_what_it_turned_and_is_told_what_it_can_do_and_the_file_has_what_is_done() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    let (filter, resonance) = param_of(&snapshot, "Filter", "resonance");
    let history = |said: &[Value]| said.iter().rfind(|m| m["type"] == "history").cloned().expect("a message of the history");

    // Nothing has been done: nothing to undo or redo, in the words of the refusal, and what a client is told to begin with.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "undo"}})).await;
    let (refused, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((refused["type"].clone(), refused["error"]["code"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!("invalid"), json!("there is nothing to undo")), "{refused}");
    assert_eq!(history(&said), json!({"type": "history", "undo": null, "redo": null}));

    // A knob turned is something to undo, told with the change (after its event, before the answer).
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setParam", "node": filter, "param": resonance, "value": 7.5}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(answered["type"], "ack");
    let types: Vec<&str> = said.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["event", "history"], "{said:?}");
    assert_eq!(said[1], json!({"type": "history", "undo": "set resonance", "redo": null}));

    // Undo puts it back, as an event like the others, and the file, saved now, does not have it.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "undo"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 3).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(2)), "{answered}");
    let put_back = said.iter().find(|m| m["type"] == "event").expect("an event");
    assert_eq!((put_back["event"]["type"].clone(), put_back["event"]["node"].clone(), put_back["event"]["param"].clone()), (json!("paramSet"), json!(filter), json!(resonance)));
    assert_ne!(put_back["event"]["value"], json!(7.5), "{put_back}");
    assert_eq!(history(&said), json!({"type": "history", "undo": null, "redo": "set resonance"}));
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("resonance = 7.5"));

    // Redo does it again, and the file has it when it is saved.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "redo"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 5).await;
    assert_eq!(answered["type"], "ack");
    assert_eq!(said.iter().find(|m| m["type"] == "event").unwrap()["event"]["value"], json!(7.5));
    assert_eq!(history(&said), json!({"type": "history", "undo": "set resonance", "redo": null}));
    tell(&mut client, json!({"type": "request", "reqId": 6, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 6).await["type"], "ack");
    assert!(std::fs::read_to_string(&file).unwrap().contains("resonance = 7.5"));

    // A browser that comes now is told what can be undone, after the snapshot and the catalog.
    let mut late = connect(&tang).await;
    assert_eq!(next_json(&mut late).await["type"], "hello");
    tell(&mut late, json!({"type": "subscribe"})).await;
    let first = [next_json(&mut late).await, next_json(&mut late).await, next_json(&mut late).await, next_json(&mut late).await];
    let types: Vec<&str> = first.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(types, ["snapshot", "catalog", "history", "inputs"]);
    assert_eq!(first[2], json!({"type": "history", "undo": "set resonance", "redo": null}));
    assert!(first[3]["names"].is_array(), "the MIDI inputs that are open, whichever they are on this machine: {}", first[3]);
}

#[tokio::test]
async fn a_browser_is_told_what_plugins_can_be_added_and_never_where_they_are_installed() {
    let tang = Tang::start();
    let mut client = connect(&tang).await;
    assert_eq!(next_json(&mut client).await["type"], "hello");
    tell(&mut client, json!({"type": "subscribe"})).await;
    assert_eq!(next_json(&mut client).await["type"], "snapshot");

    // The catalog comes after the snapshot, as far as the scan has got, and again while it finds more: the
    // built-ins are the first batch (what else is installed depends on the machine).
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut told = Vec::new();
    let osc = loop {
        let message = next_json(&mut client).await;
        // (What can be undone follows the first catalog: there is nothing yet. So do the MIDI inputs that are open, whichever they are on this machine.)
        if message["type"] == "history" {
            assert_eq!(message, json!({"type": "history", "undo": null, "redo": null}));
            continue;
        }
        if message["type"] == "inputs" {
            continue;
        }
        assert_eq!(message["type"], "catalog", "nothing else is told to a client that only listens: {message}");
        let found = message["entries"].as_array().unwrap().iter().find(|entry| entry["id"] == "builtin:osc").cloned();
        told.push(message);
        if let Some(found) = found {
            break found;
        }
        assert!(Instant::now() < deadline, "the built-ins did not come in 30 s; told {told:?}\n{}", tang.log());
    };
    assert_eq!((osc["name"].clone(), osc["format"].clone(), osc["isInstrument"].clone()), (json!("Oscillator"), json!("Built-in"), json!(true)), "{osc}");
    assert!(osc["params"].as_u64().unwrap() > 0, "{osc}");

    // Which plugins are installed is the machine's business: where, nobody's but the program's.
    let last = told.last().unwrap();
    assert!(last["rev"].as_u64().unwrap() > 0, "{last}");
    for entry in last["entries"].as_array().unwrap() {
        let mut keys: Vec<_> = entry.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["format", "id", "isInstrument", "name", "params", "presets"], "{entry}");
    }
    assert!(!last.to_string().contains(env!("CARGO_MANIFEST_DIR")), "no path of the machine in the catalog");
}

#[tokio::test]
async fn a_session_named_by_a_relative_path_is_saved_to_that_file_wherever_the_program_is_by_then() {
    // Plugins that are scanned or loaded can change the working directory of the whole program (some
    // do), and a path that is relative to it then leads somewhere else: a save would write the
    // session into a plugin's directory. The program here stays where it is started, and says which file it writes.
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path();
    std::fs::copy(TRACER, here.join("jam.toml")).unwrap();
    let tang = Tang::start_in(Some(here), "jam.toml");
    let (mut client, _, _) = listening(&tang).await;

    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    let saved_to = || tang.log().lines().find_map(|line| line.split_once("Session saved to ").map(|(_, path)| path.to_string()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while saved_to().is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let said = saved_to().unwrap_or_else(|| panic!("it did not say where it saved the session:\n{}", tang.log()));
    // The path it says is an absolute one, and is the file that was asked for. (How a system spells the directory, `/private/var` for `/var` on a Mac and `\\?\C:\Users\runneradmin` for `C:\Users\RUNNER~1` on Windows, is
    // not the point: the two are compared as the files they are.)
    assert!(std::path::Path::new(&said).is_absolute(), "the file is named by where it is, not by how it was asked for: {said}\n{}", tang.log());
    assert_eq!(std::fs::canonicalize(&said).unwrap(), std::fs::canonicalize(here.join("jam.toml")).unwrap(), "it is the file that was asked for:\n{}", tang.log());
}

/// The master's peak a moment after `wait` has passed, as the frames say (the frames are read all the while).
async fn peak_after(client: &mut Client, at: usize, wait: Duration) -> f32 {
    let deadline = Instant::now() + wait;
    let mut last = f32::NAN;
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(100), client.next()).await {
            last = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
        }
    }
    last
}

#[tokio::test]
async fn a_browser_puts_a_filter_in_the_chain_and_hears_it_in_the_meter_and_the_saved_file_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    let (first_filter, _) = param_of(&snapshot, "Filter", "cutoff");

    // A note, held: the level the meter shows through the chain as it is.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    let before = peak_after(&mut client, at, Duration::from_millis(500)).await;
    assert!(before > 0.01, "the note is heard: {before}");

    // The browser puts a filter at the end of the lane's chain (the catalog names it: the built-ins are the first batch
    // the scan finds, folded in long before this): told by a snapshot, answered by the revision.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": osc, "plugin": "builtin:filter"}})).await;
    let mut said = Vec::new();
    let answered = loop {
        let message = next_json(&mut client).await;
        if message["reqId"] == 1 {
            break message;
        }
        said.push(message);
    };
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let names: Vec<_> = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Oscillator", "Filter", "Filter", "LFO", "Master"]);
    let (added, cutoff) = {
        let nodes = told["snapshot"]["graph"]["nodes"].as_array().unwrap();
        let added = nodes.iter().find(|n| n["name"] == "Filter" && n["id"] != json!(first_filter)).expect("a second filter");
        let cutoff = added["plugin"]["params"].as_array().unwrap().iter().find(|p| p["name"] == "cutoff").unwrap();
        (added["id"].as_u64().unwrap(), cutoff["index"].as_u64().unwrap())
    };

    // It has a node whose parameters are turned like the others', and the engine has it in the chain: closed down to nothing,
    // the note is hardly heard (the meter falls 40 dB a second, so a moment on it shows what is there).
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setParam", "node": added, "param": cutoff, "value": 0.0}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let after = peak_after(&mut client, at, Duration::from_secs(2)).await;
    assert!(after < before / 10.0, "the new filter is in the audio path: {after} against {before}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // And what is saved has it, with the value it was given.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved.matches("plugin = \"builtin:filter\"").count(), 2, "{saved}");
    assert!(saved.contains("cutoff = 0.0"), "{saved}");
}

/// The answer to request `req_id`, and what was said before it (the snapshot a change of structure is told as comes first).
async fn answer_with_what_came_before(client: &mut Client, req_id: u64) -> (Value, Vec<Value>) {
    let mut said = Vec::new();
    loop {
        let message = next_json(client).await;
        if message["reqId"] == req_id {
            return (message, said);
        }
        said.push(message);
    }
}

#[tokio::test]
async fn a_browser_adds_a_lane_with_an_instrument_and_hears_it_in_the_meter_and_the_saved_file_has_both_lanes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let before_meters = snapshot["snapshot"]["telemetry"]["meters"].as_array().unwrap().len();

    // A note, and the level the tracer's one lane makes of it (quiet on purpose: its volume is 0.1).
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    let before = peak_after(&mut client, at, Duration::from_millis(500)).await;
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // The browser adds a lane with an oscillator in it (the catalog names it: the built-ins are the first batch the scan finds):
    // told by a snapshot, with the lane's meters in its layout, answered by the revision.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addInstrument", "plugin": "builtin:osc"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let nodes = told["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let names: Vec<_> = nodes.iter().map(|n| n["name"].as_str().unwrap()).collect();
    // (The tracer's LFO is the session's own: after the lanes' nodes, and before the master.)
    assert_eq!(names, ["Oscillator", "Filter", "Oscillator", "LFO", "Master"]);
    assert_eq!(nodes[2]["parent"], json!(null), "a lane of its own, in no group");
    assert_eq!(told["snapshot"]["telemetry"]["meters"].as_array().unwrap().len(), before_meters + 2, "a peak and a clip count for the lane");

    // The next note is heard through it: the oscillator is at its own volume, ten times the tracer's lane.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let after = peak_until(&mut client, &tang, at, SOON, "the note through the new lane", |peak| peak > 3.0 * before).await;
    assert!(after > 3.0 * before, "{after} against {before}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // And what is saved has both lanes.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved.matches("[[instrument]]").count(), 2, "{saved}");
    assert_eq!(saved.matches("plugin = \"builtin:osc\"").count(), 2, "{saved}");
}

#[tokio::test]
async fn a_browser_replaces_the_instrument_of_a_lane_and_what_was_set_on_the_old_one_is_gone_from_the_sound_the_cables_and_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, volume) = param_of(&snapshot, "Oscillator", "volume");
    let (_, detune) = param_of(&snapshot, "Oscillator", "detune");
    let lfo = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "LFO").unwrap()["id"].as_u64().unwrap();

    // The old instrument is turned down to nothing, and has a cable of the lane's LFO on its detune.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": osc, "param": volume, "value": 0.0}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    let cable = json!({"type": "addModEdge", "modulator": lfo, "to": {"node": osc, "port": {"type": "param", "index": detune}}});
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": cable})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    // (The oscillator eases its volume over a few milliseconds.)
    tokio::time::sleep(Duration::from_millis(100)).await;
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let quiet_until = Instant::now() + Duration::from_millis(1000);
    while Instant::now() < quiet_until {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(100), client.next()).await {
            let peak = f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
            assert!(peak < 0.001, "the volume is 0 and the meter says {peak}");
        }
    }
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // The browser puts an oscillator in the lane, in place of that one: told by a snapshot, the lane's node the same,
    // the cable on the old one's detune gone with it (the filter's cable is not the old instrument's), and the new
    // instrument's parameters its own.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "replaceInstrument", "node": osc, "plugin": "builtin:osc"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 3).await;
    // (The revisions: the volume 1, the cable 2 and the snapshot it was told as 3, the replacement 4.)
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(4)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let lane = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == json!(osc)).expect("the lane keeps its node");
    let params: Vec<_> = lane["plugin"]["params"].as_array().unwrap().iter().map(|p| (p["name"].as_str().unwrap(), p["value"].as_f64().unwrap())).collect();
    assert!(params.contains(&("volume", 1.0)) && params.contains(&("waveform", 0.0)), "{params:?}");
    let cables = told["snapshot"]["graph"]["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == "modulation").count();
    assert_eq!(cables, 1, "the cable to the filter is left, and the one to the old detune is not");

    // The same note is heard again: it was the old instrument that was silent.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note through the new instrument", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // What is saved has no trace of the old instrument's edits or its cable, and the rest of the lane is as it was.
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("[instrument.params]") && !saved.contains("waveform = 3"), "the saw and the volume of 0 are gone: {saved}");
    assert!(!saved.contains("detune"), "the cable is gone: {saved}");
    assert!(!saved.contains("volume = 0.0"), "{saved}");
    assert!(saved.contains("plugin = \"builtin:filter\"") && saved.contains("cutoff"), "the lane has its filter and the LFO's cable to it: {saved}");
    assert!(saved.contains("volume = 0.1"), "and its own volume: {saved}");
}

#[tokio::test]
async fn a_browser_sets_the_keys_of_a_lane_and_a_note_outside_them_is_silent_and_one_inside_is_heard_and_the_saved_file_has_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");

    // Every note is heard to begin with (the tracer's one lane is quiet on purpose: its volume is 0.1).
    tell(&mut client, json!({"type": "noteOn", "note": 48, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 48})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // The browser gives the lane the keys C4 to C5: told as an event, answered by the revision.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setRange", "node": osc, "range": {"low": 60, "high": 72}}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "event").expect("the keys are told as an event");
    assert_eq!(
        (told["event"]["type"].clone(), told["event"]["node"].clone(), told["event"]["range"].clone()),
        (json!("rangeSet"), json!(osc), json!({"low": 60, "high": 72})),
        "{told}"
    );

    // A note below them is not played by the lane, and what is below is silent: the meter has nothing a moment after.
    tell(&mut client, json!({"type": "noteOn", "note": 48, "velocity": 100})).await;
    let outside = peak_after(&mut client, at, Duration::from_millis(800)).await;
    assert!(outside < 0.001, "a note outside the keys is silent: {outside}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 48})).await;
    // A note inside them is heard.
    tell(&mut client, json!({"type": "noteOn", "note": 64, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "a note inside the keys", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 64})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // What is saved has the keys, as the TUI writes them.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("range = \"C4-C5\""), "{saved}");

    // No range is every note again: the note that was silent is heard, and the file has no range.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "setRange", "node": osc, "range": null}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    tell(&mut client, json!({"type": "noteOn", "note": 48, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note that was outside the keys", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 48})).await;
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("range ="), "no keys are every key");

    // A range that is not one is refused in the controller's words, and the lane keeps the keys it had.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setRange", "node": osc, "range": {"low": 90, "high": 40}}})).await;
    let refused = answer(&mut client, 5).await;
    assert_eq!(refused["type"], "rejected", "{refused}");
    assert_eq!(refused["error"]["message"], "the range goes from F#6 down to E2: its low note has to be the lower", "{refused}");
}

/// Read frames until the held notes' (channel 2: 128 values, one for each note, 1 for a note that is held) satisfy `done`, and
/// return them. Fails, with what the server logged, when they have not within `within`.
async fn held_until(client: &mut Client, tang: &Tang, within: Duration, what: &str, done: impl Fn(&[f32]) -> bool) -> Vec<f32> {
    let deadline = Instant::now() + within;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(500), client.next()).await {
            if frame[0] != 2 {
                continue;
            }
            assert_eq!((frame[1], u16::from_le_bytes([frame[2], frame[3]]), u32::from_le_bytes(frame[4..8].try_into().unwrap())), (0, 128, 0), "128 notes, and no layout");
            last = frame[8..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            if done(&last) {
                return last;
            }
        }
    }
    let held: Vec<_> = last.iter().enumerate().filter(|(_, v)| **v > 0.0).map(|(note, _)| note).collect();
    panic!("{what}: the held notes were {held:?} after {within:?}\n{}", tang.log());
}

#[tokio::test]
async fn the_notes_that_are_held_are_told_to_every_browser_that_asks_whoever_holds_them() {
    let tang = Tang::start();
    let (mut player, _, _) = listening(&tang).await;
    let mut watcher = connect(&tang).await;
    next_json(&mut watcher).await;
    tell(&mut watcher, json!({"type": "subscribe"})).await;
    assert_eq!(next_json(&mut watcher).await["type"], "snapshot");
    tell(&mut watcher, json!({"type": "telemetry", "channels": ["held"]})).await;

    // Nothing is held to begin with, and the frame is a value for each of the 128 notes.
    let none = held_until(&mut watcher, &tang, SOON, "the first frame", |_| true).await;
    assert_eq!(none.len(), 128);
    assert!(none.iter().all(|v| *v == 0.0), "{none:?}");

    // What one browser plays, another sees, by the number of the note.
    tell(&mut player, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    tell(&mut player, json!({"type": "noteOn", "note": 64, "velocity": 100})).await;
    let two = held_until(&mut watcher, &tang, SOON, "the two notes", |v| v[60] == 1.0 && v[64] == 1.0).await;
    assert_eq!(two.iter().filter(|v| **v > 0.0).count(), 2, "and no other");
    tell(&mut player, json!({"type": "noteOff", "note": 60})).await;
    held_until(&mut watcher, &tang, SOON, "the first note ending", |v| v[60] == 0.0 && v[64] == 1.0).await;

    // A browser that goes with a note held leaves nothing held: the server ends its notes.
    drop(player);
    held_until(&mut watcher, &tang, SOON, "the note ending with the browser that held it", |v| v.iter().all(|x| *x == 0.0)).await;
}

#[tokio::test]
async fn a_browser_locks_the_piano_to_a_scale_and_a_note_outside_it_is_dropped_before_it_is_heard_or_held_and_the_saved_file_has_the_scale() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, _) = listening(&tang).await;
    // (Another browser watches the notes that are held.)
    let mut watcher = connect(&tang).await;
    next_json(&mut watcher).await;
    tell(&mut watcher, json!({"type": "subscribe"})).await;
    assert_eq!(next_json(&mut watcher).await["type"], "snapshot");
    tell(&mut watcher, json!({"type": "telemetry", "channels": ["held"]})).await;

    // The scale is told as an event, in the form the session file has it and with the notes that a page draws.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setScale", "scale": "Bb Natural Minor"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "event").expect("the scale is told as an event");
    assert_eq!(told["event"], json!({"rev": 1, "type": "scaleSet", "scale": "A# minor", "root": 10, "kind": "minor", "notes": [0, 1, 3, 5, 6, 8, 10]}), "{told}");
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setLocked", "locked": true}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(2)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "event").expect("the lock is told as an event");
    assert_eq!(told["event"], json!({"rev": 2, "type": "lockedSet", "locked": true}), "{told}");

    // D4 (62) is not in A# minor: it is dropped where it is sent, so the lane is silent...
    tell(&mut client, json!({"type": "noteOn", "note": 62, "velocity": 100})).await;
    let outside = peak_after(&mut client, at, Duration::from_millis(800)).await;
    assert!(outside < 0.001, "a note outside the scale is silent: {outside}\n{}", tang.log());
    // ...and C#4 (61) is, and is heard and held (the note before it was handled first: it is not held either).
    tell(&mut client, json!({"type": "noteOn", "note": 61, "velocity": 100})).await;
    let held = held_until(&mut watcher, &tang, SOON, "a note inside the scale", |v| v[61] == 1.0).await;
    assert_eq!(held[62], 0.0, "the note outside the scale was never held");
    peak_until(&mut client, &tang, at, SOON, "a note inside the scale", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 61})).await;
    peak_until(&mut client, &tang, at, Duration::from_secs(15), "the note dying away", |peak| peak < 0.001).await;

    // What is saved has the scale as the TUI writes it, and the lock.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("[piano]") && saved.contains("scale = \"A# minor\"") && saved.contains("locked = true"), "{saved}");

    // Let go, the note that was dropped is played, held and heard.
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "setLocked", "locked": false}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    tell(&mut client, json!({"type": "noteOn", "note": 62, "velocity": 100})).await;
    held_until(&mut watcher, &tang, SOON, "the note that was dropped", |v| v[62] == 1.0).await;
    peak_until(&mut client, &tang, at, SOON, "the note that was dropped", |peak| peak > 0.01).await;
    tell(&mut client, json!({"type": "noteOff", "note": 62})).await;

    // Text that is not a scale is refused in the controller's words, and the scale stays.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setScale", "scale": "Cé"}})).await;
    let refused = answer(&mut client, 5).await;
    assert_eq!(refused["type"], "rejected", "{refused}");
    assert_eq!(refused["error"]["message"], "\"Cé\" is not a scale: a root and a kind of scale, like C major or F# dorian", "{refused}");
}

/// The next value, in a frame of the modulation channel of the layout `rev`, at place `at` of it (frames of another layout, and of other channels, are skipped).
async fn modulation_value(client: &mut Client, rev: u32, at: usize) -> f32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(500), client.next()).await
            && frame[0] == 1
            && u32::from_le_bytes(frame[4..8].try_into().unwrap()) == rev
        {
            return f32::from_le_bytes(frame[8 + 4 * at..12 + 4 * at].try_into().unwrap());
        }
    }
    panic!("no frame of the modulation channel of the layout {rev} in ten seconds");
}

/// Throw away what the connection has already received and not been read: the frames that came while a test was waiting say what was, not what is.
async fn drain(client: &mut Client) {
    while let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_millis(10), client.next()).await {}
}

#[tokio::test]
async fn a_preset_loaded_into_a_plugin_that_a_cable_drives_is_what_the_cable_is_centred_on_in_the_audio_thread() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    let lfo = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "LFO").unwrap()["id"].as_u64().unwrap();
    // (The catalog follows the snapshot, as far as the scan has got: the built-ins are its first batch. A plugin is only added once it is told.)
    loop {
        let message = next_json(&mut client).await;
        if message["type"] == "catalog" && message["entries"].as_array().unwrap().iter().any(|e| e["id"] == "builtin:reverb") {
            break;
        }
    }

    // A reverb in the lane's chain, and the lane's LFO on its decay at a small depth: the decay is 0.88 as the reverb is made, and the
    // cable moves it by a tenth of its range (0.89) either way.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": osc, "plugin": "builtin:reverb"}})).await;
    let (acked, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(acked["type"], "ack", "{acked}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("a snapshot");
    let (reverb, decay) = param_of(told, "Reverb", "decay");
    let cable = json!({"type": "addModEdge", "modulator": lfo, "to": {"node": reverb, "port": {"type": "param", "index": decay}}});
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": cable})).await;
    let (acked, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(acked["type"], "ack", "{acked}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("a snapshot");
    let edge = told["snapshot"]["graph"]["edges"].as_array().unwrap().iter().find(|e| e["kind"] == "modulation" && e["to"]["node"] == json!(reverb)).expect("the cable")["id"].clone();
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "setEdgeDepth", "edge": edge, "depth": 0.1}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");

    // Where the value the cable puts on the decay is in the frames of the modulation channel.
    let layout = &told["snapshot"]["telemetry"];
    let at = layout["modulation"].as_array().unwrap().iter().position(|e| e["kind"] == "effective" && e["node"] == json!(reverb) && e["port"]["index"] == json!(decay)).expect("a slot for the decay");
    let rev = layout["rev"].as_u64().unwrap() as u32;
    tell(&mut client, json!({"type": "telemetry", "channels": ["modulation"]})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    drain(&mut client).await;
    for _ in 0..10 {
        let value = modulation_value(&mut client, rev, at).await;
        assert!((0.78..=0.99).contains(&value), "the decay is modulated around the reverb's own: {value}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }

    // The room has a decay of 0.55. The cable is centred on that now, and the decay moves around it (the old base, put back at the next buffer, would
    // have the decay at 0.79 or more for the preset that was loaded).
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "loadPreset", "node": reverb, "preset": "room"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    tokio::time::sleep(Duration::from_millis(300)).await;
    drain(&mut client).await;
    for _ in 0..20 {
        let value = modulation_value(&mut client, rev, at).await;
        assert!((0.4..=0.7).contains(&value), "the decay is modulated around the preset's: {value}\n{}", tang.log());
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

#[tokio::test]
async fn a_second_browser_hears_what_the_first_one_changes() {
    let tang = Tang::start();
    let (mut first, _, snapshot) = listening(&tang).await;
    let (mut second, _, _) = listening(&tang).await;
    let (filter, cutoff) = param_of(&snapshot, "Filter", "cutoff");

    tell(&mut first, json!({"type": "request", "reqId": 9, "intent": {"type": "setParam", "node": filter, "param": cutoff, "value": 0.8}})).await;
    assert_eq!(answer(&mut first, 9).await["rev"], 1);
    // (The second hears the event somewhere among its own telemetry frames.)
    loop {
        let message = next_json(&mut second).await;
        if message["type"] == "event" {
            assert_eq!(message["event"]["type"], "paramSet");
            assert_eq!((message["event"]["node"].clone(), message["event"]["param"].clone()), (json!(filter), json!(cutoff)));
            assert_eq!(message["event"]["rev"], 1);
            break;
        }
    }
}

/// The modulation cables of a snapshot message, as (the node that drives, the node it ends at, the port it ends at).
fn cables_of(snapshot: &Value) -> Vec<(u64, u64, Value)> {
    let edges = snapshot["snapshot"]["graph"]["edges"].as_array().unwrap();
    edges
        .iter()
        .filter(|e| e["kind"] == "modulation")
        .map(|e| (e["from"]["node"].as_u64().unwrap(), e["to"]["node"].as_u64().unwrap(), e["to"]["port"].clone()))
        .collect()
}

#[tokio::test]
async fn a_browser_draws_a_cable_from_one_modulator_to_the_rate_of_another_and_the_saved_file_has_it_and_a_program_started_on_it_draws_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let lfo = nodes.iter().find(|n| n["name"] == "LFO").unwrap()["id"].as_u64().unwrap();

    // A second LFO that hears the lane (what the "+" of the lane makes: the session's own, like the tracer's LFO, which the program has moved there when it loaded the file): told as a snapshot, since a node came.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addFreeModulator", "kind": "lfo", "hears": osc}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the structure is told as a snapshot");
    let second = told["snapshot"]["graph"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "modulator" && n["parent"].is_null() && n["hears"] == osc && n["id"] != lfo)
        .expect("the new modulator")["id"]
        .as_u64()
        .unwrap();

    // The new one drives the rate of the first: a cable to a setting of a modulator, which is the node's own port.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "addModEdge", "modulator": second, "to": {"node": lfo, "port": {"type": "rate"}}}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the cable is told as a snapshot");
    assert!(cables_of(told).contains(&(second, lfo, json!({"type": "rate"}))), "{:?}", cables_of(told));

    // A modulator does not drive itself, and the first LFO has no attack: refused in the controller's words.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "addModEdge", "modulator": second, "to": {"node": second, "port": {"type": "rate"}}}})).await;
    let refused = answer(&mut client, 3).await;
    assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!("a modulator cannot drive its own settings")), "{refused}");
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "addModEdge", "modulator": second, "to": {"node": lfo, "port": {"type": "attack"}}}})).await;
    let refused = answer(&mut client, 4).await;
    assert_eq!(refused["error"]["message"], "that modulator has no attack", "{refused}");

    // What is saved has the cable on the modulator that drives, as the TUI writes it: the index of the one it drives.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("mod_rate = 0"), "{saved}");

    // A program started on that file draws it: the first LFO is the first of the session's own, and the second is after it.
    drop(client);
    drop(tang);
    let again = Tang::start_on(&file);
    let (_, _, snapshot) = listening(&again).await;
    assert!(cables_of(&snapshot).contains(&(second, lfo, json!({"type": "rate"}))), "{:?}", cables_of(&snapshot));

    // And the cable is taken out by the intent that takes any cable out, the rate going back to what the file has.
    let edge = snapshot["snapshot"]["graph"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["to"]["port"]["type"] == "rate")
        .expect("the cable")["id"]
        .as_u64()
        .unwrap();
    let (mut browser, _, _) = listening(&again).await;
    tell(&mut browser, json!({"type": "request", "reqId": 1, "intent": {"type": "removeEdge", "edge": edge}})).await;
    let (answered, said) = answer_with_what_came_before(&mut browser, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("told as a snapshot");
    assert!(!cables_of(told).iter().any(|(_, to, port)| *to == lfo && port["type"] == "rate"), "{:?}", cables_of(told));
}

/// The modulators of a snapshot message that are in no lane and no group: the nodes of kind `modulator` that have no parent.
fn free_modulators_of(snapshot: &Value) -> Vec<u64> {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    nodes.iter().filter(|n| n["kind"] == "modulator" && n["parent"].is_null()).map(|n| n["id"].as_u64().unwrap()).collect()
}

#[tokio::test]
async fn a_browser_makes_a_modulator_that_is_in_no_lane_and_draws_a_cable_from_it_to_a_parameter_and_the_saved_file_has_it_and_a_program_started_on_it_draws_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("free.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, _, snapshot) = listening(&tang).await;
    let (osc, volume) = param_of(&snapshot, "Oscillator", "volume");
    let (filter, cutoff) = param_of(&snapshot, "Filter", "cutoff");
    // (The tracer's own LFO is the session's own: the program moves the modulators of a lane there when it loads the file.)
    let tracers = free_modulators_of(&snapshot);
    assert_eq!(tracers.len(), 1, "the tracer's LFO is in no lane since the program loaded it: {snapshot}");

    // A modulator of its own: told as a snapshot, since a node came, with no parent.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addFreeModulator", "kind": "envelope", "hears": null}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the structure is told as a snapshot");
    let both = free_modulators_of(told);
    assert_eq!(both.len(), 2, "{told}");
    let free = *both.iter().find(|id| !tracers.contains(id)).expect("the new one");
    let history = said.iter().rfind(|m| m["type"] == "history").cloned().expect("a message of the history");
    assert_eq!(history, json!({"type": "history", "undo": "add an envelope", "redo": null}));

    // It drives a parameter of the oscillator and one of the filter (the tracer's filter is in the same lane: a free modulator reaches any plugin of the session).
    for (req, node, param) in [(2, osc, volume), (3, filter, cutoff)] {
        tell(&mut client, json!({"type": "request", "reqId": req, "intent": {"type": "addModEdge", "modulator": free, "to": {"node": node, "port": {"type": "param", "index": param}}}})).await;
        let (answered, said) = answer_with_what_came_before(&mut client, req).await;
        assert_eq!(answered["type"], "ack", "{answered}");
        let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the cable is told as a snapshot");
        assert!(cables_of(told).contains(&(free, node, json!({"type": "param", "index": param}))), "{:?}", cables_of(told));
    }
    // The same parameter twice, a group's node and a modulator of a lane are refused, in the controller's words, and nothing changes.
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "addModEdge", "modulator": free, "to": {"node": osc, "port": {"type": "param", "index": volume}}}})).await;
    let refused = answer(&mut client, 4).await;
    assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!("that modulator drives that parameter already")), "{refused}");

    // What is saved has the modulator among the tables of the file, and its cables name the plugins by their places.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("[[modulator]]") && saved.contains("to = \"instrument[0]\"") && saved.contains("param = \"volume\""), "{saved}");
    assert!(saved.contains("to = \"instrument[0].effect[0]\"") && saved.contains("param = \"cutoff\""), "{saved}");

    // A program started on that file draws it again: the modulators with no parent (the tracer's, and this one after it), and the two cables of this one.
    drop(client);
    drop(tang);
    let again = Tang::start_on(&file);
    let (mut browser, _, snapshot) = listening(&again).await;
    let both = free_modulators_of(&snapshot);
    assert_eq!(both.len(), 2, "{snapshot}");
    let free = both[1];
    let to_the_oscillator = cables_of(&snapshot).iter().filter(|(from, to, _)| *from == free && *to == osc).count();
    let to_the_filter = cables_of(&snapshot).iter().filter(|(from, to, _)| *from == free && *to == filter).count();
    assert_eq!((to_the_oscillator, to_the_filter), (1, 1), "{:?}", cables_of(&snapshot));

    // It is taken out by the intent that takes any node out, and Undo puts it back with its cables; the history of this program begins with that.
    tell(&mut browser, json!({"type": "request", "reqId": 1, "intent": {"type": "removeNode", "node": free}})).await;
    let (answered, said) = answer_with_what_came_before(&mut browser, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("told as a snapshot");
    assert_eq!(free_modulators_of(told), vec![both[0]], "the tracer's LFO is left");
    assert!(!cables_of(told).iter().any(|(from, ..)| *from == free));
    tell(&mut browser, json!({"type": "request", "reqId": 2, "intent": {"type": "undo"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut browser, 2).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("told as a snapshot");
    assert_eq!(free_modulators_of(told), both, "the very node");
    assert_eq!(cables_of(told).iter().filter(|(from, ..)| *from == free).count(), 2, "with its cables");
}

/// The audio cables of a snapshot message (the edges of kind `cable`), in the order of its graph.
fn audio_cables_of(snapshot: &Value) -> Vec<Value> {
    snapshot["snapshot"]["graph"]["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == "cable").cloned().collect()
}

#[tokio::test]
async fn a_browser_draws_a_send_and_hears_it_in_the_meter_and_the_saved_file_has_it_and_a_program_started_on_it_draws_and_plays_it() {
    // One sine, at a level of 0.2: a steady tone, so that what a cable that sends it to the mix adds is a number that can be told. The lane is in a group, so that its way out goes to the group's bus and
    // a cable from its instrument to the mix is a cable beside it (a lane with no effect and no group has its way out from its instrument to the mix, which a cable between the two is: it gives the link back).
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sends.toml");
    std::fs::write(&file, "[[group]]\n\n[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.2\ngroup = 0\n").unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    assert!(audio_cables_of(&snapshot).is_empty());
    let lane = id_of(&snapshot, "Oscillator");

    tell(&mut client, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    let dry = peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.02).await;
    let dry = peak_until(&mut client, &tang, at, SOON, "the note, steady", |peak| (peak - dry).abs() < 0.01 * dry).await;
    assert!((0.03..0.08).contains(&dry), "a sine at a level of 0.2 peaks at a quarter of it, the oscillator's own level: {dry}");

    // The lane's instrument into the mix, as it is: what the lane puts out is in the mix twice. Told as a snapshot, since a cable came.
    let master = json!({"node": MASTER, "port": {"type": "audioIn"}});
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addCable", "from": {"node": lane, "port": {"type": "audioOut"}}, "to": master, "gain": null}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("a cable is told as a snapshot");
    let cables = audio_cables_of(told);
    assert_eq!(cables.len(), 1, "{cables:?}");
    assert_eq!((cables[0]["from"]["node"].clone(), cables[0]["to"]["node"].clone(), cables[0]["gain"].clone()), (json!(lane), json!(MASTER), json!(1.0)), "{cables:?}");
    let edge = cables[0]["id"].as_u64().unwrap();
    peak_until(&mut client, &tang, at, SOON, "the lane twice in the mix", |peak| (peak - 2.0 * dry).abs() < 0.1 * dry).await;

    // Its level is an event, and the mix follows it: half of the send is a half more.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setCableGain", "edge": edge, "gain": 0.5}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    assert!(said.iter().any(|m| m["type"] == "event" && m["event"]["type"] == "cableGainSet" && m["event"]["edge"] == edge && m["event"]["gain"] == 0.5), "{said:?}");
    peak_until(&mut client, &tang, at, SOON, "half of the send", |peak| (peak - 1.5 * dry).abs() < 0.1 * dry).await;

    // A cable is not drawn twice, nor a depth set on it: refused, in the controller's words.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "addCable", "from": {"node": lane, "port": {"type": "audioOut"}}, "to": master, "gain": 1.0}})).await;
    assert_eq!(answer(&mut client, 3).await["error"]["message"], "there is a cable between those two already");
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "setEdgeDepth", "edge": edge, "depth": 0.5}})).await;
    assert_eq!(answer(&mut client, 4).await["error"]["message"], "an audio cable has a level, and no depth");

    // Taken out, the lane is in the mix once; undone, the cable is back at the level it had.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "removeEdge", "edge": edge}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the lane once", |peak| (peak - dry).abs() < 0.1 * dry).await;
    tell(&mut client, json!({"type": "request", "reqId": 6, "intent": {"type": "undo"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 6).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("an undo that puts a cable back is told as a snapshot");
    assert_eq!(audio_cables_of(told).iter().map(|c| (c["id"].clone(), c["gain"].clone())).collect::<Vec<_>>(), [(json!(edge), json!(0.5))], "back under its id, at its level");
    peak_until(&mut client, &tang, at, SOON, "half of the send again", |peak| (peak - 1.5 * dry).abs() < 0.1 * dry).await;

    // What is saved has the cable in the file, and a program that is started on it draws it and plays it.
    tell(&mut client, json!({"type": "request", "reqId": 7, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 7).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\ngain = 0.5\n"), "{saved}");
    drop(client);
    drop(tang);
    let again = Tang::start_on(&file);
    let (mut browser, at, snapshot) = listening(&again).await;
    let cables = audio_cables_of(&snapshot);
    assert_eq!(cables.len(), 1, "{cables:?}");
    assert_eq!((cables[0]["from"]["node"].clone(), cables[0]["to"]["node"].clone(), cables[0]["gain"].clone()), (json!(id_of(&snapshot, "Oscillator")), json!(MASTER), json!(0.5)));
    tell(&mut browser, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    peak_until(&mut browser, &again, at, SOON, "the note with the send that the file has", |peak| (peak - 1.5 * dry).abs() < 0.1 * dry).await;
}

#[tokio::test]
async fn a_browser_draws_a_cable_between_the_ends_of_a_link_that_is_cut_and_the_link_is_given_back_and_heard_and_where_the_link_is_there_the_cable_is_refused() {
    // One sine, at a level of 0.2: a steady tone. The lane has no effect and no group, so its way out is the link from its instrument to the mix (step 88a: every audio link is a cable to a client).
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("links.toml");
    std::fs::write(&file, "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.2\n").unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let lane = id_of(&snapshot, "Oscillator");
    let (start, end) = (json!({"node": lane, "port": {"type": "audioOut"}}), json!({"node": MASTER, "port": {"type": "audioIn"}}));

    tell(&mut client, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    let dry = peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.02).await;
    let dry = peak_until(&mut client, &tang, at, SOON, "the note, steady", |peak| (peak - dry).abs() < 0.01 * dry).await;

    // The link is there: a cable between its two ends is refused, in the words of a second cable between two places, and nothing is heard of it.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addCable", "from": start, "to": end, "gain": null}})).await;
    assert_eq!(answer(&mut client, 1).await["error"]["message"], "there is a cable between those two already");

    // The way out is cut: the lane goes nowhere, and the note is silent.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setRoute", "node": lane, "route": "none"}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the note, with the way out cut", |peak| peak < 0.001).await;

    // A cable between the two ends gives the link back and makes no cable: the note is heard as it was, and the step that is there is the link's, which Undo cuts again.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "addCable", "from": start, "to": end, "gain": null}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 3).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("a link that is given back is told as a snapshot");
    assert!(audio_cables_of(told).is_empty(), "the link is given back and no cable is made: {:?}", audio_cables_of(told));
    assert_eq!(said.iter().rfind(|m| m["type"] == "history").cloned().expect("a message of the history"), json!({"type": "history", "undo": "give the lane of Oscillator its way out", "redo": null}));
    peak_until(&mut client, &tang, at, SOON, "the note, as it was", |peak| (peak - dry).abs() < 0.1 * dry).await;
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "undo"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the note, with the way out cut by Undo", |peak| peak < 0.001).await;

    // Saved with the way out given back, the file has no word of a way out and no cable (a link is no cable): redo, then save.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "redo"}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the note, with the way out given back by Redo", |peak| (peak - dry).abs() < 0.1 * dry).await;
    tell(&mut client, json!({"type": "request", "reqId": 6, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 6).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("route") && !saved.contains("[[cable]]"), "{saved}");
}

#[tokio::test]
async fn a_browser_adds_the_audio_input_and_it_is_silent_until_a_cable_is_drawn_from_it_and_then_the_null_backends_tone_is_heard_and_the_saved_file_has_the_lane_bare_and_a_program_started_on_it_has_it() {
    // One sine that is never played, so that nothing is heard but what the input makes. (The null backend has no sound card to listen to, so the input is a tone of 220 Hz at a level of 0.25 in place of a microphone.)
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("input.toml");
    std::fs::write(&file, "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.2\n").unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, _) = listening(&tang).await;
    until_installed(&mut client, &tang, "builtin:input").await;
    peak_until(&mut client, &tang, at, SOON, "the session, with nothing played", |peak| peak < 0.001).await;

    // The input is added in a lane of its own that starts bare: its way out is cut, so there is no link from it, and it is silent at the master though its tone is running.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addInstrument", "plugin": "builtin:input"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}\n{}", tang.log());
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    assert_eq!(node_names(told), ["Oscillator", "Audio Input", "Master"]);
    let (lane, osc) = (id_of(told, "Audio Input"), id_of(told, "Oscillator"));
    let node = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == lane).unwrap();
    assert_eq!((node["route"].clone(), node["kind"].clone(), node["volume"].clone()), (json!("none"), json!("instrument"), json!(1.0)), "{node}");
    assert_eq!(audio_path_of(told), [(osc, MASTER, "audio".to_string())], "the oscillator's link, and none for the input");
    peak_until(&mut client, &tang, at, SOON, "the input, bare", |peak| peak < 0.001).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    peak_until(&mut client, &tang, at, SOON, "the input, bare, a moment later", |peak| peak < 0.001).await;

    // A cable from it to the mix is the way out given back, and the tone is heard: a sine of 0.25 at a gain of 1 and the lane's volume of 1.
    let (start, end) = (json!({"node": lane, "port": {"type": "audioOut"}}), json!({"node": MASTER, "port": {"type": "audioIn"}}));
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "addCable", "from": start, "to": end, "gain": null}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    assert_eq!(said.iter().rfind(|m| m["type"] == "history").cloned().expect("a message of the history"), json!({"type": "history", "undo": "give the lane of Audio Input its way out", "redo": null}));
    let tone = peak_until(&mut client, &tang, at, SOON, "the tone through the cable", |peak| (peak - 0.25).abs() < 0.02).await;
    assert!((tone - 0.25).abs() < 0.02, "{tone}");

    // Its gain is the level (the plugin's own `gain`, 0 to 4): twice the gain is twice the tone.
    let (_, gain) = param_of(told, "Audio Input", "gain");
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "setParam", "node": lane, "param": gain, "value": 2.0}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the tone at a gain of 2", |peak| (peak - 0.5).abs() < 0.03).await;

    // The file has the lane with its way out (given back, so no word of one) and the gain; and no cable, since the link is no cable.
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved.matches("[[instrument]]").count(), 2, "{saved}");
    assert!(saved.contains("plugin = \"builtin:input\"") && saved.contains("gain = 2"), "{saved}");
    assert!(!saved.contains("route") && !saved.contains("[[cable]]"), "{saved}");

    // Cut again, the lane is silent; the file says so, and a program started on it has the lane bare and silent, and hears the tone through a cable at the gain that the file keeps.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setRoute", "node": lane, "route": "none"}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the input, with its way out cut", |peak| peak < 0.001).await;
    tell(&mut client, json!({"type": "request", "reqId": 6, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 6).await["type"], "ack");
    let bare = std::fs::read_to_string(&file).unwrap();
    assert!(bare.contains("plugin = \"builtin:input\"\n") && bare.contains("route = \"none\""), "{bare}");

    let again = Tang::start_on(&file);
    let (mut second, at, snapshot) = listening(&again).await;
    assert_eq!(node_names(&snapshot), ["Oscillator", "Audio Input", "Master"]);
    let lane = id_of(&snapshot, "Audio Input");
    peak_until(&mut second, &again, at, SOON, "the input that the file keeps bare", |peak| peak < 0.001).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    peak_until(&mut second, &again, at, SOON, "the input that the file keeps bare, a moment later", |peak| peak < 0.001).await;
    let (start, end) = (json!({"node": lane, "port": {"type": "audioOut"}}), json!({"node": MASTER, "port": {"type": "audioIn"}}));
    tell(&mut second, json!({"type": "request", "reqId": 1, "intent": {"type": "addCable", "from": start, "to": end, "gain": null}})).await;
    assert_eq!(answer(&mut second, 1).await["type"], "ack");
    peak_until(&mut second, &again, at, SOON, "the tone that the file's gain makes", |peak| (peak - 0.5).abs() < 0.03).await;
}

/// The entries of a `catalog` message whose format is `Module`, as (id, name, number of effects).
fn modules_of(catalog: &Value) -> Vec<(String, String, u64)> {
    let entries = catalog["entries"].as_array().unwrap();
    entries.iter().filter(|entry| entry["format"] == "Module").map(|entry| (entry["id"].as_str().unwrap().to_string(), entry["name"].as_str().unwrap().to_string(), entry["params"].as_u64().unwrap())).collect()
}

/// The next `catalog` message that lists all of `ids` (the catalog comes after the snapshot as far as the scan has got, and the library's modules are in it from the start: an intent that names a plugin that
/// the scan has not found yet is refused as for one that is not installed).
async fn until_catalog_has(client: &mut Client, tang: &Tang, ids: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let message = next_json(client).await;
        if message["type"] == "catalog" {
            let listed: Vec<&str> = message["entries"].as_array().unwrap().iter().map(|entry| entry["id"].as_str().unwrap()).collect();
            if ids.iter().all(|id| listed.contains(id)) {
                return message;
            }
        }
        assert!(Instant::now() < deadline, "{ids:?} were not all listed in 30 s\n{}", tang.log());
    }
}

#[tokio::test]
async fn a_browser_saves_a_chain_of_effects_as_a_module_and_adds_it_in_a_lane_of_its_own_and_hears_it_through_cables_and_the_library_is_the_programs_and_another_program_has_the_module() {
    // A lane that is an oscillator through a filter and a reverb, the way out of which is cut, so that nothing is heard until a cable says where.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("chain.toml");
    std::fs::write(&file, "[[instrument]]\nplugin = \"builtin:osc\"\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.4\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.3\n").unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    until_installed(&mut client, &tang, "builtin:reverb").await;
    let lane = id_of(&snapshot, "Oscillator");

    // A note is held through it all, and nothing is heard: the lane's way out is cut.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    peak_until(&mut client, &tang, at, SOON, "the held note, with the way out cut", |peak| peak < 0.001).await;

    // The chain is saved as a module: the answer is the revision and the event, the clients are told the catalog again with the module in it (a format of its own, the number of its effects), and the file is in the library, which is this program's own.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "saveModule", "node": lane, "name": "Room"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}\n{}", tang.log());
    assert!(said.iter().any(|m| m["type"] == "event" && m["event"]["type"] == "moduleSaved" && m["event"]["name"] == "Room"), "the client is told of the module: {said:?}");
    let catalog = match said.iter().rfind(|m| m["type"] == "catalog") {
        Some(catalog) => catalog.clone(),
        None => until_catalog_has(&mut client, &tang, &["module:Room"]).await,
    };
    assert_eq!(modules_of(&catalog), [("module:Room".to_string(), "Room".to_string(), 2)], "{catalog}");
    assert!(!catalog.to_string().contains(&dir.path().display().to_string()) && !catalog.to_string().contains(&tang.library.display().to_string()), "where the library is, is not told: {catalog}");
    let module = std::fs::read_to_string(tang.library.join("Room.toml")).unwrap();
    assert!(module.contains("plugin = \"builtin:filter\"") && module.contains("plugin = \"builtin:reverb\"") && module.contains("cutoff = 0.4") && module.contains("mix = 0.3"), "{module}");
    assert!(!module.contains("builtin:osc"), "a module is the chain and not the instrument:\n{module}");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("Room"), "and the session's own file has nothing of it");

    // It is added as an effect that stands by itself: a lane of its own with its two effects, bare, and the step of the history says so.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "addFreeEffect", "plugin": "module:Room"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(answered["type"], "ack", "{answered}\n{}", tang.log());
    assert_eq!(said.iter().rfind(|m| m["type"] == "history").cloned().expect("a message of the history"), json!({"type": "history", "undo": "add Room", "redo": null}));
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    let nodes = told["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let new_lane = nodes.iter().rfind(|n| n["kind"] == "instrument").expect("a new lane");
    assert_eq!((new_lane["route"].clone(), new_lane["plugin"].clone()), (json!("none"), json!(null)), "bare, with no instrument: {new_lane}");
    let effects: Vec<&Value> = nodes.iter().filter(|n| n["kind"] == "effect" && n["parent"] == new_lane["id"]).collect();
    assert_eq!(effects.iter().map(|n| n["name"].as_str().unwrap()).collect::<Vec<_>>(), ["Filter", "Reverb"]);
    let (first, last) = (effects[0]["id"].as_u64().unwrap(), effects[1]["id"].as_u64().unwrap());
    peak_until(&mut client, &tang, at, SOON, "the held note, with the module added and not cabled", |peak| peak < 0.001).await;

    // Cables say where: the oscillator into the module's first effect, and the module's last effect to the mix (which gives the lane its way out). The note is heard through the module, and only now.
    let from_the_lane = json!({"node": lane, "port": {"type": "audioOut"}});
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "addCable", "from": from_the_lane, "to": {"node": first, "port": {"type": "audioIn"}}, "gain": null}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "addCable", "from": {"node": last, "port": {"type": "audioOut"}}, "to": {"node": MASTER, "port": {"type": "audioIn"}}, "gain": null}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    peak_until(&mut client, &tang, at, SOON, "the held note, heard through the module", |peak| peak > 0.01).await;

    // The session's file has the lane that the module made as any lane is written (no instrument, its effects with what they were saved with) and the cable; the module is not in it.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(saved.matches("[[instrument]]").count(), 2, "{saved}");
    assert_eq!(saved.matches("plugin = \"builtin:filter\"").count(), 2, "{saved}");
    assert_eq!(saved.matches("cutoff = 0.4").count(), 2, "the module's cutoff is in the lane that it made: {saved}");
    assert!(saved.contains("from = \"instrument[0]\"") && saved.contains("to = \"instrument[1].effect[0]\""), "{saved}");

    // A program that is started on another session and on this library has the module: the catalog says so at the start, and the module is added there as well.
    let other = dir.path().join("other.toml");
    std::fs::write(&other, "[[instrument]]\nplugin = \"builtin:osc\"\n").unwrap();
    let again = Tang::start_with_library(None, &other, "no such device", Some(&tang.library));
    let (mut second, _, _) = listening(&again).await;
    let catalog = until_catalog_has(&mut second, &again, &["module:Room", "builtin:filter", "builtin:reverb"]).await;
    assert_eq!(modules_of(&catalog), [("module:Room".to_string(), "Room".to_string(), 2)]);
    tell(&mut second, json!({"type": "request", "reqId": 1, "intent": {"type": "addFreeEffect", "plugin": "module:Room"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut second, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}\n{}", again.log());
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    assert_eq!(told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().filter(|n| n["kind"] == "effect").count(), 2);

    // A program that has no module of that name refuses it, in words, and has no more lanes for it.
    tell(&mut second, json!({"type": "request", "reqId": 2, "intent": {"type": "addFreeEffect", "plugin": "module:Nothing"}})).await;
    let refused = answer(&mut second, 2).await;
    assert_eq!(refused["type"], "rejected", "{refused}");
    assert!(refused["error"]["message"].as_str().unwrap_or_default().contains("no module called Nothing"), "{refused}");
}

/// Where in a meters frame the master's cell of this kind (`peak`, `clips` or `reduction`) is, from the layout of a snapshot.
fn master_cell_at(snapshot: &Value, kind: &str) -> usize {
    let meters = snapshot["snapshot"]["telemetry"]["meters"].as_array().expect("a meters list");
    meters.iter().position(|e| e["node"] == MASTER && e["kind"] == kind).unwrap_or_else(|| panic!("the master has a {kind}"))
}

/// Read frames until the cells at `at` of a meters frame satisfy `done`, and return them. Fails, with what the server logged, when they have not within `within`.
async fn cells_until(client: &mut Client, tang: &Tang, at: &[usize], within: Duration, what: &str, done: impl Fn(&[f32]) -> bool) -> Vec<f32> {
    let deadline = Instant::now() + within;
    let mut last = Vec::new();
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), client.next()).await {
            Ok(Some(Ok(Message::Binary(frame)))) => {
                assert_eq!(frame[0], 0, "a meters frame");
                last = at.iter().map(|&cell| f32::from_le_bytes(frame[8 + 4 * cell..12 + 4 * cell].try_into().unwrap())).collect();
                if done(&last) {
                    return last;
                }
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(Some(Err(_))) | Ok(None) => panic!("the connection ended while waiting for {what}\n{}", tang.log()),
        }
    }
    panic!("{what}: the cells were {last:?} after {within:?}\n{}", tang.log());
}

/// The master as a snapshot's graph says it: its volume, whether its limiter is in the sound and its ceiling.
fn master_of(snapshot: &Value) -> (f64, bool, f64) {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let master = nodes.iter().find(|n| n["kind"] == "master").expect("the master is a node");
    (master["volume"].as_f64().unwrap(), master["limiter"].as_bool().unwrap(), master["ceiling"].as_f64().unwrap())
}

/// A sine through a lane at a volume of 4: one note of it is exactly full scale (the oscillator's own level is a quarter of it), and a chord of three is up to three times that, which clips unless something holds it.
const HOT: &str = "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 4.0\n\n[instrument.params]\nvolume = 1.0\n";

#[tokio::test]
async fn a_browser_turns_the_limiter_and_the_masters_volume_and_ceiling_and_a_hot_chord_is_held_under_the_ceiling_the_light_stays_dark_and_the_saved_file_has_the_master_and_a_program_started_on_it_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("hot.toml");
    std::fs::write(&file, HOT).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, peak, snapshot) = listening(&tang).await;
    let (clips, reduction) = (master_cell_at(&snapshot, "clips"), master_cell_at(&snapshot, "reduction"));
    let cells = [peak, clips, reduction];

    // A session that says nothing of its master has the limiter in the sound, at a ceiling of -1 dB and a volume of 1.
    assert_eq!(master_of(&snapshot), (1.0, true, -1.0), "{snapshot}");
    let ceiling = 10f32.powf(-1.0 / 20.0);

    // A chord that is up to three times full scale is held at the ceiling and never over it, the limiter says that it brings it down by a good deal, and nothing clips.
    for note in [60, 64, 67] {
        tell(&mut client, json!({"type": "noteOn", "note": note, "velocity": 100})).await;
    }
    let held = cells_until(&mut client, &tang, &cells, SOON, "the hot chord, held at the ceiling", |c| (c[0] - ceiling).abs() < 0.005 && c[2] > 3.0).await;
    assert!(held[2] > 3.0, "the limiter brings the sound down: {held:?}");
    let until = Instant::now() + Duration::from_millis(700);
    while Instant::now() < until {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(200), client.next()).await {
            let at = |cell: usize| f32::from_le_bytes(frame[8 + 4 * cell..12 + 4 * cell].try_into().unwrap());
            assert!(at(peak) <= ceiling + 1e-4, "the output is over the ceiling: {} > {ceiling}", at(peak));
            assert_eq!(at(clips), 0.0, "the limited sound clips");
        }
    }

    // Taken out of the sound, the same chord is the mix as it is, over full scale, and it clips (the engine counts the buffers over full scale).
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setLimiter", "on": false}})).await;
    let answered = answer(&mut client, 1).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    cells_until(&mut client, &tang, &cells, SOON, "the hot chord, let through", |c| c[0] > 1.5 && c[1] > 0.0 && c[2] == 0.0).await;

    // Put in the sound again with a ceiling of -12 dB (the intent holds it to -24 to 0): the chord is held there.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setLimiter", "on": true}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "setGain", "node": MASTER, "gain": "limiterCeiling", "value": -12.0}})).await;
    assert_eq!(answer(&mut client, 3).await["type"], "ack");
    let low = 10f32.powf(-12.0 / 20.0);
    cells_until(&mut client, &tang, &cells, SOON, "the hot chord, held at -12 dB", |c| (c[0] - low).abs() < 0.005).await;

    // The volume is in front of the limiter: one note is full scale and, at a tenth of the volume, 0.1, which is under a ceiling of -6 dB (0.5012), so it is let through as it is and the limiter has nothing to do.
    for note in [64, 67] {
        tell(&mut client, json!({"type": "noteOff", "note": note})).await;
    }
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "setGain", "node": MASTER, "gain": "limiterCeiling", "value": -6.0}})).await;
    assert_eq!(answer(&mut client, 4).await["type"], "ack");
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setGain", "node": MASTER, "gain": "masterVolume", "value": 0.1}})).await;
    assert_eq!(answer(&mut client, 5).await["type"], "ack");
    cells_until(&mut client, &tang, &cells, SOON, "the note at a tenth of the volume", |c| (c[0] - 0.1).abs() < 0.01 && c[2] == 0.0).await;

    // What is out of range is held to it (a ceiling over full scale is full scale) and what is not a number is refused with nothing changed.
    tell(&mut client, json!({"type": "request", "reqId": 6, "intent": {"type": "setGain", "node": MASTER, "gain": "masterVolume", "value": 9.0}})).await;
    let held_to = answer(&mut client, 6).await;
    assert_eq!(held_to["type"], "ack", "{held_to}");
    tell(&mut client, json!({"type": "request", "reqId": 7, "intent": {"type": "setGain", "node": MASTER, "gain": "volume", "value": 0.5}})).await;
    assert_eq!(answer(&mut client, 7).await["type"], "rejected", "a lane's volume is not the master's");
    tell(&mut client, json!({"type": "request", "reqId": 8, "intent": {"type": "setGain", "node": MASTER, "gain": "masterVolume", "value": 0.1}})).await;
    assert_eq!(answer(&mut client, 8).await["type"], "ack");

    // The file has the master as it is, and nothing that is the default: the volume and the ceiling (the limiter is in the sound, which is no key).
    tell(&mut client, json!({"type": "request", "reqId": 9, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 9).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("[master]\nvolume = 0.1\nceiling = -6.0\n") && !saved.contains("limiter"), "{saved}");
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // A program started on that file has the master the file says, and the note is what the volume makes it: 0.1, under the ceiling.
    let again = Tang::start_on(&file);
    let (mut second, peak, snapshot) = listening(&again).await;
    let (volume, limiter, ceiling) = master_of(&snapshot);
    assert!((volume - 0.1).abs() < 1e-6 && limiter && (ceiling + 6.0).abs() < 1e-6, "{snapshot}");
    tell(&mut second, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut second, &again, peak, SOON, "the note through the master that the file keeps", |p| (p - 0.1).abs() < 0.01).await;

    // And a program started on a file that takes the limiter out of the sound lets the hot chord clip, as the audio thread was told when it loaded.
    let unlimited = dir.path().join("unlimited.toml");
    std::fs::write(&unlimited, format!("[master]\nlimiter = false\n\n{HOT}")).unwrap();
    let third = Tang::start_on(&unlimited);
    let (mut other, peak, snapshot) = listening(&third).await;
    assert_eq!(master_of(&snapshot), (1.0, false, -1.0), "{snapshot}");
    let clips = master_cell_at(&snapshot, "clips");
    for note in [60, 64, 67] {
        tell(&mut other, json!({"type": "noteOn", "note": note, "velocity": 100})).await;
    }
    cells_until(&mut other, &third, &[peak, clips], SOON, "the hot chord, with no limiter in the sound", |c| c[0] > 1.5 && c[1] > 0.0).await;
}

/// The edges of a snapshot message that carry notes: the cables of notes that were drawn, and what a lane that has none hears (`hears`).
fn notes_edges_of(snapshot: &Value) -> Vec<Value> {
    snapshot["snapshot"]["graph"]["edges"].as_array().unwrap().iter().filter(|e| e["kind"] == "events" || e["kind"] == "hears").cloned().collect()
}

/// The sources of a snapshot message, as (node, what it is): the nodes that are of kind `source`.
fn sources_of(snapshot: &Value) -> Vec<(u64, Value)> {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    nodes.iter().filter(|n| n["kind"] == "source").map(|n| (n["id"].as_u64().unwrap(), n["source"].clone())).collect()
}

#[tokio::test]
async fn a_browser_wires_a_lane_to_a_device_that_is_not_there_and_the_lane_goes_quiet_and_undo_gives_it_every_source_and_the_saved_file_has_the_cable_and_a_program_started_on_it_has_it() {
    // One sine, at a level of 0.2: a steady tone. The program listens to no MIDI input (`Tang::start_on`), so that a device that a cable starts at is not there.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sources.toml");
    std::fs::write(&file, "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.2\n").unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    assert!(sources_of(&snapshot).is_empty() && notes_edges_of(&snapshot).is_empty(), "a session that has no source has none, and nothing that carries notes");
    let lane = id_of(&snapshot, "Oscillator");

    tell(&mut client, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    let dry = peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.02).await;
    let dry = peak_until(&mut client, &tang, at, SOON, "the note, steady", |peak| (peak - dry).abs() < 0.01 * dry).await;

    // The keys and a device are put in as sources: told as a snapshot, with a link from each to the lane, which hears every source (it has no cable). Nothing that is heard changes.
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addSource", "source": {"type": "keys"}}})).await;
    assert_eq!(answer(&mut client, 1).await["type"], "ack");
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "addSource", "source": {"type": "device", "name": " Nothing at all "}}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("a source is told as a snapshot");
    let sources = sources_of(told);
    assert_eq!(sources.iter().map(|(_, source)| source.clone()).collect::<Vec<_>>(), [json!({"type": "keys"}), json!({"type": "device", "name": "Nothing at all"})], "the name without the spaces round it");
    let (keys, device) = (sources[0].0, sources[1].0);
    let hears = notes_edges_of(told);
    assert_eq!(hears.iter().map(|e| (e["kind"].clone(), e["from"]["node"].clone(), e["to"]["node"].clone())).collect::<Vec<_>>(), [(json!("hears"), json!(keys), json!(lane)), (json!("hears"), json!(device), json!(lane))]);
    peak_until(&mut client, &tang, at, SOON, "the note as it was", |peak| (peak - dry).abs() < 0.1 * dry).await;

    // A source is not put in twice, nor a cable of notes given a level: refused, in the controller's words.
    tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "addSource", "source": {"type": "keys"}}})).await;
    assert_eq!(answer(&mut client, 3).await["error"]["message"], "the keys are a source already");
    let (out, into) = (|node: u64| json!({"node": node, "port": {"type": "eventsOut"}}), |node: u64| json!({"node": node, "port": {"type": "eventsIn"}}));
    tell(&mut client, json!({"type": "request", "reqId": 4, "intent": {"type": "addCable", "from": out(device), "to": into(lane), "gain": 0.5}})).await;
    assert_eq!(answer(&mut client, 4).await["error"]["message"], "a cable of notes has no level: it carries notes, which are heard or not");

    // The lane is wired to the device, which is not there: it hears nothing from now on, and lets go of the key that it holds, which is held still.
    tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "addCable", "from": out(device), "to": into(lane), "gain": null}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 5).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("a cable of notes is told as a snapshot");
    let edges = notes_edges_of(told);
    assert_eq!(edges.iter().map(|e| (e["kind"].clone(), e["from"]["node"].clone(), e["to"]["node"].clone())).collect::<Vec<_>>(), [(json!("events"), json!(device), json!(lane))], "the lane has a cable, and hears the sources it starts at and no others");
    peak_until(&mut client, &tang, at, SOON, "the lane has let go of the key", |peak| peak < 0.001).await;
    tell(&mut client, json!({"type": "noteOff", "note": 69})).await;
    tell(&mut client, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    let quiet = peak_after(&mut client, at, Duration::from_millis(600)).await;
    assert!(quiet < 0.001, "the keys are not heard by a lane that is wired to a device that is not there: {quiet}");

    // Undone, the cable is gone and the lane hears every source again: a key that is hit now is heard.
    tell(&mut client, json!({"type": "request", "reqId": 6, "intent": {"type": "undo"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 6).await;
    assert_eq!(answered["type"], "ack", "{answered}");
    let told = said.iter().rev().find(|m| m["type"] == "snapshot").expect("an undo that takes a cable out is told as a snapshot");
    assert!(notes_edges_of(told).iter().all(|e| e["kind"] == "hears"), "no cable is left");
    tell(&mut client, json!({"type": "noteOff", "note": 69})).await;
    tell(&mut client, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the key that is hit once the cable is undone", |peak| peak > 0.02).await;

    // Done again, and saved: the file has the sources and the cable, and a program that is started on it has them and the lane is quiet.
    tell(&mut client, json!({"type": "request", "reqId": 7, "intent": {"type": "redo"}})).await;
    assert_eq!(answer(&mut client, 7).await["type"], "ack");
    tell(&mut client, json!({"type": "request", "reqId": 8, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 8).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.starts_with("[keys]\n\n[[device]]\nname = \"Nothing at all\"\n\n[[instrument]]\n"), "{saved}");
    assert!(saved.contains("[[cable]]\nfrom = \"device[0]\"\nto = \"instrument[0]\"\n"), "{saved}");
    drop(client);
    drop(tang);
    let again = Tang::start_on(&file);
    let (mut browser, at, snapshot) = listening(&again).await;
    assert_eq!(sources_of(&snapshot).iter().map(|(_, source)| source.clone()).collect::<Vec<_>>(), [json!({"type": "keys"}), json!({"type": "device", "name": "Nothing at all"})]);
    assert_eq!(notes_edges_of(&snapshot).iter().map(|e| e["kind"].clone()).collect::<Vec<_>>(), [json!("events")], "the cable that the file has, and no link of a lane that hears every source");
    tell(&mut browser, json!({"type": "noteOn", "note": 69, "velocity": 100})).await;
    let quiet = peak_after(&mut browser, at, Duration::from_millis(600)).await;
    assert!(quiet < 0.001, "the lane is wired to a device that is not there: {quiet}");
}

/// The names of the nodes of a snapshot message, in the order of its graph.
fn node_names(snapshot: &Value) -> Vec<&str> {
    snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap()).collect()
}

/// The id of the first node of a snapshot message that has this name.
fn id_of(snapshot: &Value, name: &str) -> u64 {
    snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == name).unwrap_or_else(|| panic!("no node {name}"))["id"].as_u64().unwrap()
}

/// The links of the audio path of a snapshot message, in order, as (the node it leaves, the node it goes to, its kind).
fn audio_path_of(snapshot: &Value) -> Vec<(u64, u64, String)> {
    let edges = snapshot["snapshot"]["graph"]["edges"].as_array().unwrap();
    edges
        .iter()
        .filter(|e| e["kind"] != "modulation")
        .map(|e| (e["from"]["node"].as_u64().unwrap(), e["to"]["node"].as_u64().unwrap(), e["kind"].as_str().unwrap().to_string()))
        .collect()
}

/// Wait until the server has told the client that it can add the plugin `id` (the catalog comes after the snapshot, as far as the scan
/// has got: the built-ins are the first batch, but an intent that comes before they are in is refused as for a plugin that is not
/// installed).
async fn until_installed(client: &mut Client, tang: &Tang, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let message = next_json(client).await;
        if message["type"] == "catalog" && message["entries"].as_array().unwrap().iter().any(|entry| entry["id"] == id) {
            return;
        }
        assert!(Instant::now() < deadline, "{id} was not told in 30 s\n{}", tang.log());
    }
}

/// The next `snapshot` message a client is told (an `event` or anything else that comes before it is passed over).
async fn next_snapshot(client: &mut Client) -> Value {
    loop {
        let message = next_json(client).await;
        if message["type"] == "snapshot" {
            return message;
        }
    }
}

#[tokio::test]
async fn a_browser_moves_an_effect_along_its_chain_and_a_second_one_is_told_and_the_note_is_still_heard_and_the_saved_file_has_the_new_order_and_a_program_started_on_it_draws_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut client, at, snapshot) = listening(&tang).await;
    let (mut other, _, _) = listening(&tang).await;
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    let (filter, cutoff) = param_of(&snapshot, "Filter", "cutoff");
    let lfo = id_of(&snapshot, "LFO");

    // A reverb at the end of the lane's chain: the oscillator, the filter, then the reverb.
    until_installed(&mut client, &tang, "builtin:reverb").await;
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": osc, "plugin": "builtin:reverb"}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new structure is told as a snapshot");
    assert_eq!(node_names(told), ["Oscillator", "Filter", "Reverb", "LFO", "Master"]);
    let reverb = id_of(told, "Reverb");
    assert_eq!(audio_path_of(told), [(osc, filter, "audio".to_string()), (filter, reverb, "audio".to_string()), (reverb, MASTER, "audio".to_string())]);

    // A note, held: it is heard through the chain as it is.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    peak_until(&mut client, &tang, at, SOON, "the note", |peak| peak > 0.01).await;

    // The browser moves the reverb to the first place of the chain: told as a snapshot, with the effects in the new order, the audio
    // path through them in it, and the LFO's cable on the same filter, which is in the second place now.
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "moveEffect", "node": reverb, "index": 0}})).await;
    let (answered, said) = answer_with_what_came_before(&mut client, 2).await;
    // (The revisions: the effect 1 and the snapshot it was told as 2, the move 3.)
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(3)), "{answered}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("the new order is told as a snapshot");
    assert_eq!(node_names(told), ["Oscillator", "Reverb", "Filter", "LFO", "Master"]);
    assert_eq!(audio_path_of(told), [(osc, reverb, "audio".to_string()), (reverb, filter, "audio".to_string()), (filter, MASTER, "audio".to_string())]);
    assert!(cables_of(told).contains(&(lfo, filter, json!({"type": "param", "index": cutoff}))), "{:?}", cables_of(told));
    assert_eq!(told["snapshot"]["meta"]["dirty"], true);

    // The other browser is told the same: the snapshot of the effect, then the one of the move.
    let first = next_snapshot(&mut other).await;
    assert_eq!(node_names(&first), ["Oscillator", "Filter", "Reverb", "LFO", "Master"]);
    let moved = next_snapshot(&mut other).await;
    assert_eq!(node_names(&moved), ["Oscillator", "Reverb", "Filter", "LFO", "Master"]);
    assert_eq!(audio_path_of(&moved), audio_path_of(told));

    // The note is heard through the chain in its new order, the engine having been told it: the audio path is whole.
    let after = peak_after(&mut client, at, Duration::from_millis(500)).await;
    assert!(after > 0.01, "the note is heard through the reordered chain: {after}\n{}", tang.log());
    tell(&mut client, json!({"type": "noteOff", "note": 60})).await;

    // What the controller refuses, it says in its own words, and nothing changes (no revision is used up).
    for (req_id, node, index, message) in [
        (3, reverb, 0, "that effect is at that place already"),
        (4, reverb, 2, "the chain has no place 2"),
        (5, osc, 0, "only an effect can be moved"),
        (6, lfo, 0, "only an effect can be moved"),
        (7, 9999, 0, "there is no node 9999"),
    ] {
        tell(&mut client, json!({"type": "request", "reqId": req_id, "intent": {"type": "moveEffect", "node": node, "index": index}})).await;
        let refused = answer(&mut client, req_id).await;
        assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!(message)), "{refused}");
    }

    // What is saved has the effects in the new order, and the LFO's cable on the filter's new place in the chain.
    tell(&mut client, json!({"type": "request", "reqId": 8, "intent": {"type": "save"}})).await;
    let saved_answer = answer(&mut client, 8).await;
    assert_eq!((saved_answer["type"].clone(), saved_answer["rev"].clone()), (json!("ack"), json!(5)), "the refusals used no revision: {saved_answer}");
    let saved = std::fs::read_to_string(&file).unwrap();
    let (reverb_at, filter_at) = (saved.find("plugin = \"builtin:reverb\"").expect("a reverb"), saved.find("plugin = \"builtin:filter\"").expect("a filter"));
    assert!(reverb_at < filter_at, "the reverb comes first in the file: {saved}");
    assert!(saved.contains("to = \"instrument[0].effect[1]\""), "the cable is on the second effect of the lane, which is the filter's: {saved}");

    // A program started on that file draws the chain in that order, and the cable on the filter.
    drop((client, other));
    drop(tang);
    let again = Tang::start_on(&file);
    let (_, _, snapshot) = listening(&again).await;
    assert_eq!(node_names(&snapshot), ["Oscillator", "Reverb", "Filter", "LFO", "Master"]);
    let (filter, lfo) = (id_of(&snapshot, "Filter"), id_of(&snapshot, "LFO"));
    assert!(cables_of(&snapshot).contains(&(lfo, filter, json!({"type": "param", "index": cutoff}))), "{:?}", cables_of(&snapshot));
    assert_eq!(snapshot["snapshot"]["meta"]["dirty"], false);
}

/// The place a snapshot gives a node, by the node's id: its `ui`, or `None` if it has none (a node that is not placed has no `ui` at all).
fn place_of(snapshot: &Value, node: u64) -> Option<Value> {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    nodes.iter().find(|n| n["id"] == node).unwrap_or_else(|| panic!("no node {node}")).get("ui").cloned()
}

#[tokio::test]
async fn a_browser_places_a_node_and_a_second_one_sees_it_and_the_saved_file_has_it_and_a_program_started_on_the_file_draws_it_there() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut first, _, snapshot) = listening(&tang).await;
    let (mut second, _, _) = listening(&tang).await;
    let (filter, _) = param_of(&snapshot, "Filter", "resonance");
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    assert_eq!((place_of(&snapshot, filter), place_of(&snapshot, MASTER)), (None, None), "the tracer has no places");

    // The first browser drags the filter (a place with more decimals than are kept) and the master: told as an event to
    // the other browser, which is given the place as the controller keeps it, and answered by the revision.
    tell(&mut first, json!({"type": "request", "reqId": 1, "intent": {"type": "setNodeUi", "node": filter, "ui": {"x": 376.38211, "y": -40.04}}})).await;
    let (answered, _) = answer_with_what_came_before(&mut first, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    tell(&mut first, json!({"type": "request", "reqId": 2, "intent": {"type": "setNodeUi", "node": MASTER, "ui": {"x": 1200.0, "y": 300.0}}})).await;
    assert_eq!(answer(&mut first, 2).await["rev"], 2);
    for (rev, node, ui) in [(1, filter, json!({"x": 376.4, "y": -40.0})), (2, MASTER, json!({"x": 1200.0, "y": 300.0}))] {
        let told = loop {
            let message = next_json(&mut second).await;
            if message["type"] == "event" {
                break message;
            }
        };
        assert_eq!(
            (told["event"]["type"].clone(), told["event"]["node"].clone(), told["event"]["ui"].clone(), told["event"]["rev"].clone()),
            (json!("nodeUiSet"), json!(node), ui, json!(rev)),
            "{told}"
        );
    }

    // A browser that arrives now is given the places in the session, and a node nobody placed has none.
    let (_, _, now) = listening(&tang).await;
    assert_eq!(place_of(&now, filter), Some(json!({"x": 376.4, "y": -40.0})));
    assert_eq!(place_of(&now, MASTER), Some(json!({"x": 1200.0, "y": 300.0})));
    assert_eq!(place_of(&now, osc), None);
    assert_eq!(now["snapshot"]["meta"]["dirty"], true, "a place is a change the session has not saved");

    // A node that is not there is refused in the controller's words, and the session is as it was.
    tell(&mut first, json!({"type": "request", "reqId": 3, "intent": {"type": "setNodeUi", "node": 9999, "ui": {"x": 1.0, "y": 1.0}}})).await;
    let refused = answer(&mut first, 3).await;
    assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!("there is no node 9999")), "{refused}");

    // What is saved has them, as a person would write them, and only on the nodes that were placed.
    tell(&mut first, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut first, 4).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("ui = { x = 376.4, y = -40 }"), "{saved}");
    assert!(saved.contains("[ui]\nmaster = { x = 1200, y = 300 }"), "{saved}");
    assert_eq!(saved.matches("ui = {").count(), 1, "only the filter has a place of its own besides the master: {saved}");

    // A program started on that file draws every node where it was left.
    drop((first, second));
    drop(tang);
    let again = Tang::start_on(&file);
    let (mut browser, _, snapshot) = listening(&again).await;
    assert_eq!(place_of(&snapshot, filter), Some(json!({"x": 376.4, "y": -40.0})), "the filter has the same node id: it is the same lane");
    assert_eq!(place_of(&snapshot, MASTER), Some(json!({"x": 1200.0, "y": 300.0})));
    assert_eq!(place_of(&snapshot, osc), None);
    assert_eq!(snapshot["snapshot"]["meta"]["dirty"], false);

    // And a node given back to the layout is gone from the file the next time it is saved.
    tell(&mut browser, json!({"type": "request", "reqId": 1, "intent": {"type": "setNodeUi", "node": filter, "ui": null}})).await;
    assert_eq!(answer(&mut browser, 1).await["type"], "ack");
    tell(&mut browser, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut browser, 2).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(!saved.contains("ui = {"), "{saved}");
    assert!(saved.contains("[ui]\nmaster = { x = 1200, y = 300 }"), "{saved}");
}

/// The parameters a snapshot says are pinned to a node's plugin, by the node's id (the plugin's `pinned`).
fn pinned_of(snapshot: &Value, node: u64) -> Value {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    let node = nodes.iter().find(|n| n["id"] == node).unwrap_or_else(|| panic!("no node {node}"));
    node["plugin"]["pinned"].clone()
}

#[tokio::test]
async fn a_browser_pins_parameters_to_a_node_and_a_second_one_sees_it_and_the_saved_file_has_them_and_a_program_started_on_the_file_shows_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut first, _, snapshot) = listening(&tang).await;
    let (mut second, _, _) = listening(&tang).await;
    let (filter, resonance) = param_of(&snapshot, "Filter", "resonance");
    let (_, cutoff) = param_of(&snapshot, "Filter", "cutoff");
    let (osc, _) = param_of(&snapshot, "Oscillator", "volume");
    assert_eq!((pinned_of(&snapshot, filter), pinned_of(&snapshot, osc)), (json!([]), json!([])), "the tracer has nothing pinned");

    // The first browser pins two parameters of the filter, in an order of its own: told as an event to the other browser, and answered by
    // the revision.
    tell(&mut first, json!({"type": "request", "reqId": 1, "intent": {"type": "setPinned", "node": filter, "params": [resonance, cutoff]}})).await;
    let (answered, _) = answer_with_what_came_before(&mut first, 1).await;
    assert_eq!((answered["type"].clone(), answered["rev"].clone()), (json!("ack"), json!(1)), "{answered}");
    let told = loop {
        let message = next_json(&mut second).await;
        if message["type"] == "event" {
            break message;
        }
    };
    assert_eq!(
        (told["event"]["type"].clone(), told["event"]["node"].clone(), told["event"]["params"].clone(), told["event"]["rev"].clone()),
        (json!("pinnedSet"), json!(filter), json!([resonance, cutoff]), json!(1)),
        "{told}"
    );

    // A browser that arrives now is given what is pinned, and a plugin nobody pinned anything to has none.
    let (_, _, now) = listening(&tang).await;
    assert_eq!(pinned_of(&now, filter), json!([resonance, cutoff]));
    assert_eq!(pinned_of(&now, osc), json!([]));
    assert_eq!(now["snapshot"]["meta"]["dirty"], true, "pinning is a change the session has not saved");

    // What the controller refuses it refuses in its own words, and the session is as it was.
    let refusals = [
        (json!({"type": "setPinned", "node": filter, "params": [resonance, resonance]}), format!("parameter {resonance} is listed twice")),
        (json!({"type": "setPinned", "node": filter, "params": [4000]}), "the plugin has no parameter 4000".to_string()),
        (json!({"type": "setPinned", "node": 9999, "params": []}), "there is no node 9999".to_string()),
    ];
    for (i, (intent, why)) in refusals.into_iter().enumerate() {
        let id = 10 + i as u64;
        tell(&mut first, json!({"type": "request", "reqId": id, "intent": intent})).await;
        let refused = answer(&mut first, id).await;
        assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!(why)), "{refused}");
    }

    // What is saved has them by name, in the order they were pinned, and on the plugin that has them.
    tell(&mut first, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut first, 4).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("pinned = [\"resonance\", \"cutoff\"]"), "{saved}");
    assert_eq!(saved.matches("pinned = ").count(), 1, "only the filter has pins: {saved}");

    // A program started on that file shows them again, on the same node.
    drop((first, second));
    drop(tang);
    let again = Tang::start_on(&file);
    let (mut browser, _, snapshot) = listening(&again).await;
    assert_eq!(pinned_of(&snapshot, filter), json!([resonance, cutoff]), "the filter has the same node id: it is the same lane");
    assert_eq!(snapshot["snapshot"]["meta"]["dirty"], false);

    // And a node given back to the front-end's choice is gone from the file the next time it is saved.
    tell(&mut browser, json!({"type": "request", "reqId": 1, "intent": {"type": "setPinned", "node": filter, "params": []}})).await;
    assert_eq!(answer(&mut browser, 1).await["type"], "ack");
    tell(&mut browser, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut browser, 2).await["type"], "ack");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("pinned"));
}

/// The pattern a snapshot says a lane has (null while it has none), by the lane's id.
fn pattern_of(snapshot: &Value, node: u64) -> Value {
    let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
    nodes.iter().find(|n| n["id"] == node).unwrap_or_else(|| panic!("no node {node}"))["pattern"].clone()
}

/// The next snapshot a client is told that has the lane's pattern recorded (not recording, with notes): what the controller gives the browsers when the audio thread
/// reports a recording that is done, which is no event. Fails, with what the server logged, when it does not come within `within`.
async fn recorded_pattern(client: &mut Client, tang: &Tang, lane: u64, within: Duration) -> Value {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        let snapshot = tokio::time::timeout(Duration::from_secs(2), next_snapshot(client)).await;
        if let Ok(snapshot) = snapshot {
            let pattern = pattern_of(&snapshot, lane);
            if pattern["recording"] == false && pattern["notes"].as_u64().is_some_and(|notes| notes > 0) {
                return snapshot;
            }
        }
    }
    panic!("no recorded pattern was told in {within:?}\n{}", tang.log());
}

#[tokio::test]
async fn a_browser_records_a_pattern_on_a_lane_and_every_browser_and_the_saved_file_and_a_program_started_on_it_have_it() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("loop.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_on(&file);
    let (mut first, _, snapshot) = listening(&tang).await;
    let (mut second, _, _) = listening(&tang).await;
    let (lane, _) = param_of(&snapshot, "Oscillator", "volume");
    assert_eq!(pattern_of(&snapshot, lane), Value::Null, "the tracer has no pattern");

    // A short one: a beat at 300 beats a minute, which is counted in with four (a second all in all). The length is asked of a lane that has no pattern, which is given an empty one.
    tell(&mut first, json!({"type": "request", "reqId": 1, "intent": {"type": "setBpm", "bpm": 300.0}})).await;
    assert_eq!(answer(&mut first, 1).await["rev"], 1);
    tell(&mut first, json!({"type": "request", "reqId": 2, "intent": {"type": "setPatternField", "node": lane, "field": "length", "value": 1.0}})).await;
    assert_eq!(answer(&mut first, 2).await["rev"], 2);
    tell(&mut first, json!({"type": "request", "reqId": 3, "intent": {"type": "setRecording", "node": lane, "recording": true}})).await;
    assert_eq!(answer(&mut first, 3).await["rev"], 3);
    // (A note that is played during the count-in is recorded at the start of the pattern, with its end.)
    tell(&mut first, json!({"type": "noteOn", "note": 64, "velocity": 100})).await;
    tell(&mut first, json!({"type": "noteOff", "note": 64})).await;

    // The second browser was told each of them as an event: the tempo, the empty pattern with its length, and the recording that has begun.
    let mut told = Vec::new();
    while told.len() < 3 {
        let message = next_json(&mut second).await;
        if message["type"] == "event" {
            told.push(message);
        }
    }
    let empty = json!({"recording": false, "enabled": false, "looping": true, "inKey": false, "lengthBeats": 1.0, "bpm": 300.0, "notes": 0});
    assert_eq!((told[0]["event"]["type"].clone(), told[0]["event"]["bpm"].clone()), (json!("bpmSet"), json!(300.0)), "{}", told[0]);
    assert_eq!((told[1]["event"]["type"].clone(), told[1]["event"]["node"].clone(), told[1]["event"]["pattern"].clone()), (json!("patternSet"), json!(lane), empty), "{}", told[1]);
    assert_eq!(told[2]["event"]["pattern"], json!({"recording": true, "enabled": false, "looping": true, "inKey": false, "lengthBeats": 1.0, "bpm": 300.0, "notes": 0}), "{}", told[2]);

    // When the audio thread is done the lane has the pattern that was recorded, in every browser: it plays when a key is held, goes round, and has the one note.
    let done = json!({"recording": false, "enabled": true, "looping": true, "inKey": false, "lengthBeats": 1.0, "bpm": 300.0, "notes": 1});
    let told_first = recorded_pattern(&mut first, &tang, lane, Duration::from_secs(20)).await;
    assert_eq!(pattern_of(&told_first, lane), done, "{told_first}");
    let told_second = recorded_pattern(&mut second, &tang, lane, Duration::from_secs(20)).await;
    assert_eq!(pattern_of(&told_second, lane), done, "{told_second}");
    assert_eq!(told_second["snapshot"]["meta"]["dirty"], true, "a pattern is a change the session has not saved");

    // What is saved has it, with the tempo, and a program started on the file has the pattern in the same lane.
    tell(&mut first, json!({"type": "request", "reqId": 4, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut first, 4).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("[instrument.pattern]\nbpm = 300.0\nlength_beats = 1.0\nbase_note = \"E4\"\nenabled = true\n"), "{saved}");
    // (The tempo of the session is the pattern's, which a load comes to anyway: the file has no tempo of its own.)
    assert!(!saved.starts_with("bpm"), "{saved}");
    drop((first, second));
    drop(tang);
    let again = Tang::start_on(&file);
    let (mut browser, _, snapshot) = listening(&again).await;
    assert_eq!(pattern_of(&snapshot, lane), done, "the lane has the same node id: it is the same lane");
    assert_eq!((snapshot["snapshot"]["meta"]["dirty"].clone(), snapshot["snapshot"]["meta"]["bpm"].clone()), (json!(false), json!(300.0)));

    // A setting of it is the lane's in the file the next time it is saved, and a pattern that is cleared is gone from it: asked twice, the second is refused in the controller's words.
    tell(&mut browser, json!({"type": "request", "reqId": 1, "intent": {"type": "setPatternField", "node": lane, "field": "looping", "value": 0.0}})).await;
    assert_eq!(answer(&mut browser, 1).await["type"], "ack");
    tell(&mut browser, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut browser, 2).await["type"], "ack");
    assert!(std::fs::read_to_string(&file).unwrap().contains("looping = false"));
    tell(&mut browser, json!({"type": "request", "reqId": 3, "intent": {"type": "clearPattern", "node": lane}})).await;
    assert_eq!(answer(&mut browser, 3).await["type"], "ack");
    tell(&mut browser, json!({"type": "request", "reqId": 4, "intent": {"type": "clearPattern", "node": lane}})).await;
    let refused = answer(&mut browser, 4).await;
    assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!("that lane has no pattern")), "{refused}");
    tell(&mut browser, json!({"type": "request", "reqId": 5, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut browser, 5).await["type"], "ack");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("[instrument.pattern]"));

    // A node that is no lane is refused too.
    let (filter, _) = param_of(&snapshot, "Filter", "cutoff");
    tell(&mut browser, json!({"type": "request", "reqId": 6, "intent": {"type": "setRecording", "node": filter, "recording": true}})).await;
    let refused = answer(&mut browser, 6).await;
    assert_eq!((refused["type"].clone(), refused["error"]["message"].clone()), (json!("rejected"), json!("a pattern belongs to a lane")), "{refused}");
}

/// The latest probe frame (channel 4: the node's id where the layout's revision is, then the left side and the right, 2048 frames of each) that has arrived for each node:
/// read until `done` is satisfied by what has arrived so far, and return it. Fails, with what the server logged, when it is not within `within`.
async fn probes_until(
    client: &mut Client,
    tang: &Tang,
    within: Duration,
    what: &str,
    done: impl Fn(&std::collections::HashMap<u64, (Vec<f32>, Vec<f32>)>) -> bool,
) -> std::collections::HashMap<u64, (Vec<f32>, Vec<f32>)> {
    let mut seen = std::collections::HashMap::new();
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok(Some(Ok(Message::Binary(frame)))) = tokio::time::timeout(Duration::from_millis(500), client.next()).await {
            if frame[0] != 4 {
                continue;
            }
            let n = u16::from_le_bytes([frame[2], frame[3]]) as usize;
            assert_eq!((frame[1], n), (0, 4096), "no flags, and 2048 frames of each side");
            let node = u32::from_le_bytes(frame[4..8].try_into().unwrap()) as u64;
            let values: Vec<f32> = frame[8..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            seen.insert(node, (values[..2048].to_vec(), values[2048..].to_vec()));
            if done(&seen) {
                return seen;
            }
        }
    }
    panic!("{what}: the frames of {:?} had come after {within:?}\n{}", seen.keys().collect::<Vec<_>>(), tang.log());
}

#[tokio::test]
async fn a_browser_watches_the_signal_at_nodes_and_is_sent_what_the_engine_stores_there() {
    let tang = Tang::start();
    let (mut client, _, snapshot) = listening(&tang).await;
    let (osc, filter) = (id_of(&snapshot, "Oscillator"), id_of(&snapshot, "Filter"));

    // The answer says which of the nodes are watched: not the master (its scope is a channel of its own) and not a node that is not there.
    tell(&mut client, json!({"type": "probes", "nodes": [osc, filter, MASTER, 99_999]})).await;
    let answered = loop {
        let message = next_json(&mut client).await;
        if message["type"] == "probes" {
            break message;
        }
    };
    assert_eq!(answered["nodes"], json!([osc, filter]), "{answered}");

    // A note: the oscillator's saw is at its output (the same on both sides), and the filter that follows it has changed it.
    tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let loudest = |side: &[f32]| side.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
    let seen = probes_until(&mut client, &tang, SOON, "both nodes' signal", |seen| {
        [osc, filter].iter().all(|node| seen.get(node).is_some_and(|(left, _)| loudest(left) > 0.005))
    })
    .await;
    let (osc_left, osc_right) = &seen[&osc];
    let (filter_left, _) = &seen[&filter];
    assert_eq!(osc_left, osc_right, "an oscillator with no pan puts out one signal on both sides");
    assert!(loudest(osc_left) <= 0.1 + 1e-4, "the tracer's oscillator is at a volume of 0.1: {}", loudest(osc_left));
    assert_ne!(osc_left, filter_left, "the filter makes something else of it");

    // A client that stops watching is sent no more of them, and one that watches a node is not sent another's.
    tell(&mut client, json!({"type": "probes", "nodes": [filter]})).await;
    loop {
        let message = next_json(&mut client).await;
        if message["type"] == "probes" {
            assert_eq!(message["nodes"], json!([filter]));
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let later = probes_until(&mut client, &tang, SOON, "the filter's signal again", |seen| seen.contains_key(&filter)).await;
    assert!(!later.contains_key(&osc), "the oscillator is not watched any more");
}

/// The answer to a `probes` request: the nodes that are watched.
async fn probes_answer(client: &mut Client) -> Value {
    loop {
        let message = next_json(client).await;
        if message["type"] == "probes" {
            return message["nodes"].clone();
        }
    }
}

#[tokio::test]
async fn the_rings_of_a_browser_that_has_gone_are_given_to_a_node_that_had_none_and_its_frames_come_without_asking_again() {
    // Nine lanes, one more than the engine has rings for.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lanes.toml");
    std::fs::write(&file, "[[instrument]]\nplugin = \"builtin:osc\"\n".repeat(telemetry_rings() + 1)).unwrap();
    let tang = Tang::start_on(&file);
    let (mut first, _, snapshot) = listening(&tang).await;
    let lanes: Vec<u64> = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().filter(|n| n["name"].as_str().is_some_and(|name| name.starts_with("Oscillator"))).map(|n| n["id"].as_u64().unwrap()).collect();
    assert_eq!(lanes.len(), telemetry_rings() + 1);

    // The first browser takes every ring. A second one that asks for the ninth node is told that none is watched.
    tell(&mut first, json!({"type": "probes", "nodes": &lanes[..telemetry_rings()]})).await;
    assert_eq!(probes_answer(&mut first).await, json!(&lanes[..telemetry_rings()]));
    let (mut second, _, _) = listening(&tang).await;
    tell(&mut second, json!({"type": "probes", "nodes": [lanes[telemetry_rings()]]})).await;
    assert_eq!(probes_answer(&mut second).await, json!([]), "no ring is left");

    // When the first has gone, the ring comes to the ninth node, and its frames come to the second browser, which does not have to ask again.
    drop(first);
    tell(&mut second, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
    let ninth = lanes[telemetry_rings()];
    probes_until(&mut second, &tang, SOON, "the ninth lane's signal", |seen| seen.get(&ninth).is_some_and(|(left, _)| left.iter().any(|s| s.abs() > 0.001))).await;
}

/// How many nodes the engine can have probed at once (`telemetry::PROBES`), which the tests of the program cannot read.
fn telemetry_rings() -> usize {
    8
}

#[tokio::test]
async fn a_browser_that_vanishes_with_a_key_held_does_not_leave_the_note_sounding() {
    let tang = Tang::start();
    let (mut watcher, at, _) = listening(&tang).await;
    let mut player = connect(&tang).await;
    next_json(&mut player).await;
    tell(&mut player, json!({"type": "noteOn", "note": 67, "velocity": 100})).await;
    peak_until(&mut watcher, &tang, at, SOON, "the note", |peak| peak > 0.01).await;

    // The tab is closed: no note off, no close frame, the socket is simply gone.
    drop(player);
    peak_until(&mut watcher, &tang, at, Duration::from_secs(15), "the note ending with its connection", |peak| peak < 0.001).await;
}

/// A plain HTTP request: the status line and the whole answer.
fn http(tang: &Tang, host: &str, extra: &str) -> (u16, String) {
    let mut stream = std::net::TcpStream::connect(tang.addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(stream, "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n{extra}\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, response)
}

#[tokio::test]
async fn pages_and_hosts_that_are_not_this_servers_are_refused() {
    let tang = Tang::start();
    let own = tang.addr.to_string();

    let (status, page) = http(&tang, &own, "");
    assert_eq!(status, 200);
    // (The interface if it has been built, and what says how to build it if not: both are called Tang.)
    assert!(page.contains("<title>Tang</title>"), "{page}");

    // DNS rebinding: a name of somebody else's that points here.
    assert_eq!(http(&tang, &format!("evil.example:{}", tang.addr.port()), "").0, 403);
    // A page on another origin, for the page and for the WebSocket.
    assert_eq!(http(&tang, &own, "Origin: http://evil.example\r\n").0, 403);
    match connect_as(&tang, "http://evil.example").await {
        Err(tungstenite::Error::Http(response)) => assert_eq!(response.status().as_u16(), 403),
        other => panic!("{:?}", other.map(|_| ())),
    }
    // Nobody was let in: the session has not been touched.
    let (mut client, _, snapshot) = listening(&tang).await;
    assert_eq!(snapshot["snapshot"]["rev"], 0);
    let _ = client.close(None).await;
}

/// A port that is taken is found out at once, not after the session has loaded (which takes
/// seconds with a heavy instrument): the session here is one that cannot be loaded, so what the
/// program says first is what it found out first. `tang serve` and `tang --web` alike.
#[test]
fn a_port_that_is_taken_is_reported_before_the_session_is_loaded() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap();
    let web_bind = addr.to_string();
    let web = format!("--web={addr}");
    for args in [
        vec!["serve", "/no/such/session.toml", "--web-bind", web_bind.as_str()],
        vec![web.as_str(), "/no/such/session.toml"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tang"))
            .args(["--audio-device", "none", "--midi-device", "no such device"])
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .expect("tang runs");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}: {said}");
        assert!(said.contains(&format!("cannot listen on {addr}")), "{args:?}: the program said: {said}");
        // (And why, the way an error that is returned from `main` says it: the whole chain.)
        // (The words are the system's; Windows has others, which end in the same number whatever the language.)
        let in_use = if cfg!(windows) { "os error 10048" } else { "Address already in use" };
        assert!(said.contains("Caused by:") && said.contains(in_use), "{args:?}: the program said: {said}");
    }
}

/// The program interrupted as soon as it says where it serves, which is while the scan of the installed plugins is still on its
/// thread (native code: a machine with plugins that take a while to scan shows it). It crashed at its end, with a signal, about
/// one time in twenty: the process's exit ran what the plugins' libraries have to run at exit while the scan was still in them.
/// It ends itself now (`main`). Forty runs take three seconds, and found the crash three times of three before that was done.
#[cfg(unix)]
#[test]
fn interrupting_the_program_early_ends_it_with_status_0_every_time() {
    for run in 1..=40 {
        let mut tang = Tang::start();
        tang.interrupt();
        let status = tang.exit_within(Duration::from_secs(10)).unwrap_or_else(|| panic!("run {run}: tang did not stop\n{}", tang.log()));
        assert!(status.success(), "run {run}: {status:?}\n{}", tang.log());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_closes_the_browsers_and_stops_the_program() {
    let mut tang = Tang::start();
    let (mut client, _, _) = listening(&tang).await;
    tang.interrupt();
    // The browser is told the server is going away.
    let mut code = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), client.next()).await {
            Ok(Some(Ok(Message::Close(frame)))) => {
                code = frame.map(|frame| u16::from(frame.code));
                break;
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(Some(Err(_))) | Ok(None) => break,
        }
    }
    assert_eq!(code, Some(1001), "{}", tang.log());
    let status = tang.exit_within(Duration::from_secs(10)).unwrap_or_else(|| panic!("tang did not stop\n{}", tang.log()));
    assert!(status.success(), "{status:?}\n{}", tang.log());
    assert!(std::net::TcpStream::connect(tang.addr).is_err(), "nobody listens any more");
}

/// A MIDI controller that exists only here: a virtual output that the program can open as an input, as it opens a keyboard's.
/// (CoreMIDI on a Mac and ALSA's sequencer on Linux make them; Windows has none.)
#[cfg(unix)]
fn virtual_controller(name: &str) -> midir::MidiOutputConnection {
    use midir::os::unix::VirtualOutput;
    midir::MidiOutput::new("tang test").expect("a MIDI system").create_virtual(name).unwrap_or_else(|e| panic!("a virtual MIDI port called {name:?}: {e}"))
}

/// How many times `needle` is in the program's log so far.
#[cfg(unix)]
fn times_in_the_log(tang: &Tang, needle: &str) -> usize {
    tang.log().matches(needle).count()
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs a MIDI system that can make virtual ports (CoreMIDI on a Mac, ALSA's sequencer on Linux): cargo test --features web -- --ignored"]
async fn a_controller_that_is_unplugged_is_noticed_and_one_that_is_plugged_in_again_is_opened_again_and_heard() {
    let name = format!("tang replug test {}", std::process::id());
    let mut controller = virtual_controller(&name);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_listening_to(&file, &name);
    let opened = format!("Opened MIDI input: {name}");
    let gone = format!("MIDI input gone: {name}");
    // The program opens the inputs that it finds as it starts, and this one is there.
    let wait_for = |what: &'static str, done: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done() {
            assert!(Instant::now() < deadline, "{what}\n{}", tang.log());
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    wait_for("the program did not open the controller", &|| times_in_the_log(&tang, &opened) == 1);
    controller.send(&[0x90, 60, 100]).unwrap();
    wait_for("the program did not hear the controller", &|| times_in_the_log(&tang, "NoteOn ") == 1);

    // Unplugged with a key down: the program looks once a second, and says that the device has gone.
    drop(controller);
    wait_for("the program did not notice that the controller was gone", &|| times_in_the_log(&tang, &gone) == 1);

    // Plugged in again: it is opened again, and what it plays is heard.
    let mut controller = virtual_controller(&name);
    wait_for("the program did not open the controller again", &|| times_in_the_log(&tang, &opened) == 2);
    controller.send(&[0x90, 62, 100]).unwrap();
    wait_for("the program did not hear the controller that was plugged in again", &|| times_in_the_log(&tang, "NoteOn ") == 2);
    assert_eq!(times_in_the_log(&tang, &gone), 1, "it has not gone twice\n{}", tang.log());
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs a MIDI system that can make virtual ports (CoreMIDI on a Mac, ALSA's sequencer on Linux): cargo test --features web -- --ignored"]
async fn a_controller_that_is_plugged_in_after_the_program_has_started_is_opened_and_heard() {
    let name = format!("tang late test {}", std::process::id());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_listening_to(&file, &name);
    // (CoreMIDI tells a program that has no run loop of the ports that were there when it started, and no others: the program turns the loop once each time it looks.)
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(times_in_the_log(&tang, "Opened MIDI input"), 0, "it is not there yet\n{}", tang.log());
    let mut controller = virtual_controller(&name);
    let deadline = Instant::now() + Duration::from_secs(15);
    while times_in_the_log(&tang, &format!("Opened MIDI input: {name}")) == 0 {
        assert!(Instant::now() < deadline, "the program did not open the controller that was plugged in after it started\n{}", tang.log());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    controller.send(&[0x90, 60, 100]).unwrap();
    while times_in_the_log(&tang, "NoteOn ") == 0 {
        assert!(Instant::now() < deadline, "the program did not hear it\n{}", tang.log());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The modulator of a snapshot message that is a MIDI control: its node and what it is bound to (`null` while it learns), and whether it learns.
#[cfg(unix)]
fn midi_control_of(snapshot: &Value) -> Option<(u64, Value, bool)> {
    snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["kind"] == "modulator" && n["source"]["type"] == "midiLearn").map(|n| {
        (n["id"].as_u64().unwrap(), n["source"]["control"].clone(), n["source"]["learning"].as_bool().unwrap())
    })
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs a MIDI system that can make virtual ports (CoreMIDI on a Mac, ALSA's sequencer on Linux): cargo test --features web -- --ignored"]
async fn a_control_that_learns_is_bound_to_the_controller_that_is_moved_and_drives_the_parameter_and_every_browser_and_the_saved_file_have_it() {
    let name = format!("tang learn test {}", std::process::id());
    let mut controller = virtual_controller(&name);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("jam.toml");
    std::fs::copy(TRACER, &file).unwrap();
    let tang = Tang::start_listening_to(&file, &name);
    // (The program opens the inputs that it finds as it starts, and this one is there.)
    let deadline = Instant::now() + SOON;
    while !tang.log().contains("MIDI inputs connected: 1") {
        assert!(Instant::now() < deadline, "the program did not open the controller\n{}", tang.log());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (mut client, _, snapshot) = listening(&tang).await;
    // (The oscillator's pan: the tracer's LFO sweeps the filter's cutoff, which would not keep still for the check below.)
    let (osc, pan) = param_of(&snapshot, "Oscillator", "pan");

    // A control that learns, made for the pan, at 0.4 of its range: armed, and bound to nothing.
    let to = json!({"node": osc, "port": {"type": "param", "index": pan}});
    tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "modulate", "to": to, "kind": "midiLearn", "depth": 0.4}})).await;
    let (acked, said) = answer_with_what_came_before(&mut client, 1).await;
    assert_eq!(acked["type"], "ack", "{acked}");
    let told = said.iter().find(|m| m["type"] == "snapshot").expect("a snapshot");
    let (control, bound_to, learning) = midi_control_of(told).expect("a control that learns");
    assert_eq!((bound_to, learning), (Value::Null, true), "it waits for a controller");

    // The controller is moved (a person turns a knob, which sends a stream of values, and the program binds the first it hears: CC 74).
    let deadline = Instant::now() + SOON;
    let bound = loop {
        controller.send(&[0xB0, 74, 100]).unwrap();
        if let Ok(snapshot) = tokio::time::timeout(Duration::from_millis(150), next_snapshot(&mut client)).await
            && let Some((id, bound_to, learning)) = midi_control_of(&snapshot)
            && !learning
        {
            assert_eq!((id, bound_to), (control, json!({"type": "cc", "number": 74})), "{snapshot}");
            break snapshot;
        }
        assert!(Instant::now() < deadline, "the control was not bound within {SOON:?}\n{}", tang.log());
    };

    // The pan follows the knob: its own value is the centre, 0, and a controller at rest adds nothing; all the way it adds 0.4 of the range of 2.
    let layout = &bound["snapshot"]["telemetry"];
    let at = layout["modulation"].as_array().unwrap().iter().position(|e| e["kind"] == "effective" && e["node"] == json!(osc) && e["port"]["index"] == json!(pan)).expect("a slot for the pan");
    let rev = layout["rev"].as_u64().unwrap() as u32;
    tell(&mut client, json!({"type": "telemetry", "channels": ["modulation"]})).await;
    for (value, expected) in [(0u8, 0.0f32), (127, 0.8), (0, 0.0)] {
        controller.send(&[0xB0, 74, value]).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        drain(&mut client).await;
        let seen = modulation_value(&mut client, rev, at).await;
        assert!((seen - expected).abs() < 0.03, "the controller at {value}: the pan is {seen}, and should be about {expected}\n{}", tang.log());
    }

    // A second browser is told what the control is bound to, and the saved file has it.
    let (_second, _, told) = listening(&tang).await;
    assert_eq!(midi_control_of(&told).map(|(_, bound_to, learning)| (bound_to, learning)), Some((json!({"type": "cc", "number": 74}), false)));
    tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "save"}})).await;
    assert_eq!(answer(&mut client, 2).await["type"], "ack");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("type = \"midi\"") && saved.contains("midi_cc = 74"), "{saved}");
}

/// How many windows of the process `pid` are on the screen, from the window server's list (what a program that has windows shows every other one: the owner and the size need no permission).
#[cfg(target_os = "macos")]
fn windows_on_screen(pid: u32) -> usize {
    let script = format!(
        "ObjC.import('CoreGraphics'); ObjC.import('Foundation'); \
         const list = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly, 0))); \
         String(list.filter(w => w.kCGWindowOwnerPID == {pid}).length);"
    );
    let output = Command::new("osascript").args(["-l", "JavaScript", "-e", &script]).output().expect("osascript runs");
    String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or_else(|_| panic!("osascript said {output:?}"))
}

/// Wait until `expected` windows of the program are on the screen.
#[cfg(target_os = "macos")]
fn wait_for_windows(tang: &Tang, expected: usize, what: &str) {
    let pid = tang.child.0.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = windows_on_screen(pid);
        if seen == expected {
            return;
        }
        assert!(Instant::now() < deadline, "{what}: {seen} window(s) on the screen, and {expected} expected\n{}", tang.log());
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A plugin's own window, through the whole program (step 112, part B): the hello says that the machine can show windows and the plugin that it has one, a request opens it, and it leaves the screen with its plugin, when the
/// instrument is taken out of the lane, and is there again for the plugin that an undo brings back. *What found it:* the window of a plugin that was taken out was closed by the controller and stayed on the screen, shut and
/// frozen, since nothing turned the loop that makes AppKit tell the window server once the last window was closed (`window::macos`, `close`). For a person at a Mac that has a VST3 plugin installed, since a window is not for a
/// machine that has no screen and no plugin; the window takes the keyboard from the terminal for a few seconds. `cargo test --features web --test serve -- --ignored`.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "opens a plugin's window on the screen for a few seconds, and takes the keyboard: cargo test --features web --test serve -- --ignored"]
async fn a_plugins_own_window_opens_through_the_server_and_leaves_the_screen_with_its_plugin() {
    let mut bundles: Vec<_> = std::fs::read_dir("/Library/Audio/Plug-Ins/VST3")
        .map(|entries| entries.filter_map(Result::ok).map(|entry| entry.path()).filter(|path| path.extension().is_some_and(|ext| ext == "vst3")).collect())
        .unwrap_or_default();
    bundles.sort();
    let Some(bundle) = bundles.first() else {
        eprintln!("skipped: no VST3 plugin is installed");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("window.toml");
    std::fs::write(&file, format!("[[instrument]]\nplugin = {:?}\n", bundle.to_string_lossy())).unwrap();
    let tang = Tang::start_on(&file);
    let mut client = connect(&tang).await;
    let hello = next_json(&mut client).await;
    if !hello["features"].as_array().unwrap().iter().any(|feature| feature == "windows") {
        eprintln!("skipped: the server says that this machine cannot show a window: {hello}");
        return;
    }
    tell(&mut client, json!({"type": "subscribe"})).await;
    let snapshot = next_json(&mut client).await;
    let node = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|node| node["plugin"]["hasEditor"] == true).unwrap_or_else(|| panic!("the plugin does not say that it has a window: {snapshot}"))["id"].as_u64().unwrap();
    let mut req = 0;
    let mut ask = async |intent: Value| {
        req += 1;
        tell(&mut client, json!({"type": "request", "reqId": req, "intent": intent})).await;
        answer(&mut client, req).await
    };

    assert_eq!(windows_on_screen(tang.child.0.id()), 0, "nothing is open until it is asked for");
    // (A window waits for the scan of the installed plugins, which has only just begun: that is said, and it is asked for again.)
    let scan_over = Instant::now() + Duration::from_secs(60);
    loop {
        let opened = ask(json!({"type": "openEditor", "node": node})).await;
        if opened["type"] == "ack" {
            break;
        }
        let why = opened["error"]["message"].as_str().unwrap_or_default();
        assert!(why.contains("still being looked for") && Instant::now() < scan_over, "{opened}\n{}", tang.log());
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    wait_for_windows(&tang, 1, "the window that was asked for");
    // Asked for again, the one that is open is brought to the front: there is still one.
    assert_eq!(ask(json!({"type": "openEditor", "node": node})).await["type"], "ack");
    tokio::time::sleep(Duration::from_millis(300)).await;
    wait_for_windows(&tang, 1, "the window that was asked for twice");

    // The instrument is taken out of its lane: the window goes with the plugin.
    assert_eq!(ask(json!({"type": "clearInstrument", "node": node})).await["type"], "ack");
    wait_for_windows(&tang, 0, "the window of a plugin that left the session");
    let refused = ask(json!({"type": "openEditor", "node": node})).await;
    assert_eq!(refused["type"], "rejected", "{refused}");

    // An undo brings the plugin back, shut: a window is opened again by asking, for the plugin that is there now.
    assert_eq!(ask(json!({"type": "undo"})).await["type"], "ack");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(windows_on_screen(tang.child.0.id()), 0, "the plugin is back, and its window is not");
    assert_eq!(ask(json!({"type": "openEditor", "node": node})).await["type"], "ack");
    wait_for_windows(&tang, 1, "the window of the plugin that an undo brought back");
    assert!(!tang.log().contains("let go of on a thread that is not the main one"), "{}", tang.log());
}
