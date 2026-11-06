//! The wire protocol: the types that cross the socket between the controller and
//! a browser, and the TypeScript that is generated from them; see
//! `docs/web-ui.md` §7.
//!
//! The types themselves live next to what they describe (`NodeId` in `model`,
//! `Intent` and `Event` in `intent`, ...) and derive `serde` always and `ts_rs::TS`
//! under the `web` feature. This module lists the roots of the protocol and
//! writes their TypeScript, into `web/src/protocol/generated/`, which is committed.
//! A test fails when what is committed is not what the types generate now:
//!
//! ```text
//! TANG_UPDATE_TYPES=1 cargo test --features web typescript
//! ```
//!
//! rewrites the directory.

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime speaks the protocol"))]

use serde::{Deserialize, Serialize};

use crate::app::catalog::CatalogEntry;
use crate::app::intent::{Intent, IntentError, StampedEvent};
use crate::app::layout::TelemetryLayout;
use crate::app::model::NodeId;
use crate::app::snapshot::Snapshot;
use crate::app::sounds::{SoundEntry, SoundLibrary};

/// The version of the protocol, in `Hello`: it goes up when a message changes in a
/// way an old client cannot read, and a client refuses a server that says another.
pub const PROTOCOL: u32 = 1;

/// What a client sends, as a text frame of JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClientMessage {
    /// The token a server that wants one (`Hello.needs_token`) is sent first: nothing
    /// else is processed before it.
    Auth { token: String },
    /// Start hearing about the session: a snapshot now, and the events after it.
    Subscribe,
    /// Which telemetry frames to send (each channel at about 30 Hz); none to stop.
    Telemetry { channels: Vec<TelemetryChannel> },
    /// The nodes whose signal this client wants to see (what it wanted before it does not; none to stop): the signal at the output of an instrument, an effect, a group or a
    /// bus effect, sent as frames of its own (`probe_frame`, about 30 Hz) for as long as the node is watched. The answer is `Probes`.
    Probes { nodes: Vec<NodeId> },
    /// Apply an intent; the answer (`Ack` or `Rejected`) carries `req_id`.
    Request { req_id: u32, intent: Intent },
    /// What the library `library` (one of those the hello lists) has: the answer is `Sounds`, or `Rejected` with the reason (a folder that is gone, a server that does not answer). It is not an intent: it changes nothing in
    /// the session, and a library that is far away can take its time.
    ListSounds { req_id: u32, library: String },
    /// Open the sound `id` of `library` in place of the session, as `Intent::OpenSession` does with the text of a file that the page read: the server reads it (or downloads it) and goes on as for that intent, and the answer
    /// (`Ack` or `Rejected`) carries `req_id`. Only for a program that can open another session (the hello says `openSession`).
    OpenSound { req_id: u32, library: String, id: String },
    /// A note, straight to the audio thread (it does not pass the controller).
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8 },
    /// Let go of everything that sounds (`NoteInput::panic`): the pedal up, every note that is held ended, and all notes off and all sound off on every channel. Not only the
    /// notes this client holds: a hung note is as likely to be a key of the terminal's or a hardware keyboard's, and it is what the button is for.
    Panic,
}

/// What the server sends, as a text frame of JSON (telemetry goes as binary
/// frames, see `telemetry_frame`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ServerMessage {
    /// The first message of every connection.
    Hello {
        /// `PROTOCOL`: a client that was built for another refuses to go on.
        protocol: u32,
        /// The session's revision now.
        rev: u64,
        /// What the server can do: `notes`, `telemetry`, and `windows` when the machine that runs it can show a plugin's own window (step 112).
        features: Vec<String>,
        /// The next message must be `Auth`.
        needs_token: bool,
        /// The libraries of sounds that the program has, which a client can list (`ListSounds`) and open a sound of (`OpenSound`), in the order to show them (step 116); none for a program that cannot open another session.
        libraries: Vec<SoundLibrary>,
    },
    /// The answer to `Subscribe`: where to start from.
    Snapshot { snapshot: Snapshot },
    /// Something an intent changed, from this client or another front-end.
    Event { event: StampedEvent },
    /// What the telemetry slots mean now: sent when it changes, so that frames and
    /// layout stay in step (a frame names the `rev` of the layout it follows).
    Layout { layout: TelemetryLayout },
    /// The plugins installed on this machine, which a client can add to the session by `id`: sent
    /// after the snapshot, and again whenever the scan finds more or ends (`scanning` goes false).
    /// It replaces what the client had; `rev` goes up with it.
    Catalog { rev: u64, scanning: bool, entries: Vec<CatalogEntry> },
    /// What Undo and Redo would do: the words of the step each would put back or do again ("set cutoff"), or nothing if it
    /// would do nothing. Sent after the snapshot and the catalog, and again whenever it changes (after the events of the intent
    /// that changed it, and before its answer). It is not part of the session: it has no revision, and a client that has been told
    /// nothing has nothing to undo.
    History { undo: Option<String>, redo: Option<String> },
    /// Which of the nodes that a `Probes` request named are watched now: those that have a signal and a ring (the engine has a few, shared by every client). A node that
    /// is left out may be watched later, if a ring comes free, and its frames come then.
    Probes { nodes: Vec<NodeId> },
    /// The MIDI inputs that are open, by name, in the order they were opened: what a client can make a source of (`AddSource` with a device of that name). Sent after the history, and
    /// again whenever the list changes (a device is plugged in or pulled out); it replaces what the client had. It is not part of the session (a session names devices that are not
    /// plugged in too), and a program that has no MIDI subsystem tells an empty list.
    Inputs { names: Vec<String> },
    /// The answer to `ListSounds` `req_id`: what `library` has, newest first; `truncated` when it has more than it lists.
    Sounds { req_id: u32, library: String, entries: Vec<SoundEntry>, truncated: bool },
    /// The intent of the request `req_id` was applied; the session is at `rev`.
    Ack { req_id: u32, rev: u64 },
    /// The intent of the request `req_id` was not applied.
    Rejected { req_id: u32, error: WireError },
    /// The connection is about to close: what was wrong with it.
    Closing { error: WireError },
}

/// A group of telemetry frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum TelemetryChannel {
    /// The peaks and clip counts (`TelemetryLayout::meters`).
    Meters,
    /// The modulators' outputs and the parameters' effective values
    /// (`TelemetryLayout::modulation`).
    Modulation,
    /// Which notes are held, by whichever source holds them (a MIDI keyboard, the virtual piano, a
    /// browser): 128 values, the note numbers being their places, 1 for a note that is held and 0
    /// for the rest. It follows no layout (its `layout_rev` is 0).
    Held,
    /// The last frames of the output mix, for a scope: `SCOPE_FRAME_LENGTH` values of the left side, the latest last, and then as many of the
    /// right side (`telemetry::ScopeRing`). It follows no layout (its `layout_rev` is 0).
    Scope,
}

/// How many frames of each side a scope frame has: what a reader may take of the engine's ring at once (`telemetry::SCOPE_READ_MAX`), 43 ms at 48 kHz.
#[cfg_attr(not(feature = "web"), expect(dead_code, reason = "only the web runtime sends scope frames"))]
pub const SCOPE_FRAME_LENGTH: usize = crate::telemetry::SCOPE_READ_MAX;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct WireError {
    pub code: ErrorCode,
    /// A sentence for a person, not for a program.
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    /// No node has the id the intent names.
    NotFound,
    /// The intent does not make sense for what it names.
    Invalid,
    /// A file could not be written.
    Io,
    /// The message was not one the server understands.
    BadRequest,
    /// The token is wrong or missing.
    Unauthorized,
    /// Too many requests too fast.
    RateLimited,
    /// The client falls behind: it is dropped and should reconnect.
    Overloaded,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        WireError { code, message: message.into() }
    }
}

impl From<&IntentError> for WireError {
    fn from(error: &IntentError) -> Self {
        let code = match error {
            IntentError::NotFound(_) | IntentError::EdgeNotFound(_) => ErrorCode::NotFound,
            IntentError::Invalid(_) => ErrorCode::Invalid,
            IntentError::Io(_) => ErrorCode::Io,
        };
        WireError::new(code, error.to_string())
    }
}

/// Bytes before the values of a telemetry frame.
pub const FRAME_HEADER_LEN: usize = 8;

/// A telemetry frame, the binary message that carries the current values of one
/// channel: `u8 channel | u8 flags | u16 n | u32 layout_rev | n × f32`, all little
/// endian. `channel` is 0 for the meters, 1 for the modulation, 2 for the held notes and 3 for the scope, `flags` is 0
/// (reserved), `layout_rev` is the `rev` of the `TelemetryLayout` the values follow:
/// value *i* is that of entry *i* of the channel's list in it (the held notes and the scope follow none: 0).
pub fn telemetry_frame(channel: TelemetryChannel, layout_rev: u32, values: &[f32]) -> Vec<u8> {
    frame_of(
        match channel {
            TelemetryChannel::Meters => 0,
            TelemetryChannel::Modulation => 1,
            TelemetryChannel::Held => 2,
            TelemetryChannel::Scope => 3,
        },
        layout_rev,
        values,
    )
}

/// The frame of what a node that is watched has at its output (`ClientMessage::Probes`): a telemetry frame of channel 4 whose `u32` is the node's id (it follows no layout) and whose
/// values are the left side and then the right, as the scope's are, `SCOPE_FRAME_LENGTH` of each.
#[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends probe frames"))]
pub fn probe_frame(node: NodeId, values: &[f32]) -> Vec<u8> {
    frame_of(4, node.0, values)
}

fn frame_of(channel: u8, tag: u32, values: &[f32]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + values.len() * 4);
    frame.push(channel);
    frame.push(0);
    frame.extend_from_slice(&(values.len().min(u16::MAX as usize) as u16).to_le_bytes());
    frame.extend_from_slice(&tag.to_le_bytes());
    for value in values.iter().take(u16::MAX as usize) {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame
}

#[cfg(test)]
mod tests {
    #[test]
    fn node_ids_are_plain_numbers_on_the_wire() {
        use crate::app::model::NodeId;
        assert_eq!(serde_json::to_string(&NodeId(7)).unwrap(), "7");
        assert_eq!(serde_json::from_str::<NodeId>("7").unwrap(), NodeId(7));
    }
}

#[cfg(test)]
mod message_tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::app::intent::{Event, Gain};
    use crate::app::model::NodeId;

    fn wire<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    fn client_messages() -> Vec<(ClientMessage, Value)> {
        vec![
            (ClientMessage::Auth { token: "s3cret".to_string() }, json!({"type": "auth", "token": "s3cret"})),
            (ClientMessage::Subscribe, json!({"type": "subscribe"})),
            (
                ClientMessage::Telemetry { channels: vec![TelemetryChannel::Meters, TelemetryChannel::Modulation] },
                json!({"type": "telemetry", "channels": ["meters", "modulation"]}),
            ),
            (
                ClientMessage::Request { req_id: 7, intent: Intent::SetGain { node: NodeId(3), gain: Gain::Volume, value: 0.5 } },
                json!({"type": "request", "reqId": 7, "intent": {"type": "setGain", "node": 3, "gain": "volume", "value": 0.5}}),
            ),
            (ClientMessage::Probes { nodes: vec![NodeId(3), NodeId(9)] }, json!({"type": "probes", "nodes": [3, 9]})),
            (ClientMessage::Probes { nodes: vec![] }, json!({"type": "probes", "nodes": []})),
            (ClientMessage::ListSounds { req_id: 3, library: "sessions".to_string() }, json!({"type": "listSounds", "reqId": 3, "library": "sessions"})),
            (
                ClientMessage::OpenSound { req_id: 4, library: "folder1".to_string(), id: "drums/kick.toml".to_string() },
                json!({"type": "openSound", "reqId": 4, "library": "folder1", "id": "drums/kick.toml"}),
            ),
            (ClientMessage::NoteOn { note: 60, velocity: 100 }, json!({"type": "noteOn", "note": 60, "velocity": 100})),
            (ClientMessage::NoteOff { note: 60 }, json!({"type": "noteOff", "note": 60})),
            (ClientMessage::Panic, json!({"type": "panic"})),
        ]
    }

    #[test]
    fn a_client_message_is_an_object_tagged_with_its_camel_case_name() {
        for (message, json) in client_messages() {
            assert_eq!(wire(&message), json, "{message:?}");
            assert_eq!(serde_json::from_value::<ClientMessage>(json).unwrap(), message);
        }
    }

    #[test]
    fn what_a_client_sends_that_is_not_a_message_is_an_error() {
        let bad = [
            ("no type", json!({"token": "x"})),
            ("an unknown type", json!({"type": "shout"})),
            ("a Rust name", json!({"type": "NoteOn", "note": 60, "velocity": 100})),
            ("a note that is not a number", json!({"type": "noteOn", "note": "C4", "velocity": 100})),
            ("a note past a byte", json!({"type": "noteOn", "note": 300, "velocity": 100})),
            ("a request with no id", json!({"type": "request", "intent": {"type": "setParam", "node": 1, "param": 0, "value": 0.5}})),
            ("a request whose intent is nonsense", json!({"type": "request", "reqId": 1, "intent": {"type": "explode"}})),
            ("a channel that does not exist", json!({"type": "telemetry", "channels": ["spectrum"]})),
            ("probes of nodes that are not numbers", json!({"type": "probes", "nodes": ["Filter"]})),
            ("probes with no nodes", json!({"type": "probes"})),
            ("a listing of no library", json!({"type": "listSounds", "reqId": 1})),
            ("a listing with no request id", json!({"type": "listSounds", "library": "sessions"})),
            ("a sound with no id", json!({"type": "openSound", "reqId": 1, "library": "sessions"})),
            ("a sound of no library", json!({"type": "openSound", "reqId": 1, "id": "a.toml"})),
            ("a sound that is a number", json!({"type": "openSound", "reqId": 1, "library": "sessions", "id": 7})),
        ];
        for (what, json) in bad {
            assert!(serde_json::from_value::<ClientMessage>(json).is_err(), "{what}");
        }
    }

    #[test]
    fn a_server_message_is_an_object_tagged_with_its_camel_case_name() {
        let event = StampedEvent { rev: 9, event: Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 } };
        let cases = vec![
            (
                ServerMessage::Hello { protocol: PROTOCOL, rev: 4, features: vec!["notes".to_string()], needs_token: true, libraries: vec![] },
                json!({"type": "hello", "protocol": PROTOCOL, "rev": 4, "features": ["notes"], "needsToken": true, "libraries": []}),
            ),
            (
                ServerMessage::Hello {
                    protocol: PROTOCOL,
                    rev: 4,
                    features: vec!["notes".to_string(), "openSession".to_string()],
                    needs_token: false,
                    libraries: vec![SoundLibrary { id: "sessions".to_string(), label: "Saved sessions".to_string(), place: "/home/me/.config/tang/sessions".to_string() }],
                },
                json!({
                    "type": "hello", "protocol": PROTOCOL, "rev": 4, "features": ["notes", "openSession"], "needsToken": false,
                    "libraries": [{"id": "sessions", "label": "Saved sessions", "place": "/home/me/.config/tang/sessions"}],
                }),
            ),
            (
                ServerMessage::Sounds {
                    req_id: 3,
                    library: "sessions".to_string(),
                    entries: vec![
                        SoundEntry { id: "pad.toml".to_string(), name: "pad".to_string(), detail: "Helm · 1 effect".to_string(), openable: true, modified: Some(1_704_067_200) },
                        SoundEntry { id: "broken.toml".to_string(), name: "broken".to_string(), detail: "not a session: expected an equals sign".to_string(), openable: false, modified: None },
                    ],
                    truncated: false,
                },
                json!({
                    "type": "sounds", "reqId": 3, "library": "sessions", "truncated": false,
                    "entries": [
                        {"id": "pad.toml", "name": "pad", "detail": "Helm · 1 effect", "openable": true, "modified": 1_704_067_200},
                        {"id": "broken.toml", "name": "broken", "detail": "not a session: expected an equals sign", "openable": false, "modified": null},
                    ],
                }),
            ),
            (
                ServerMessage::Event { event },
                json!({"type": "event", "event": {"rev": 9, "type": "paramSet", "node": 3, "param": 2, "value": 0.5}}),
            ),
            (ServerMessage::Ack { req_id: 7, rev: 10 }, json!({"type": "ack", "reqId": 7, "rev": 10})),
            (
                ServerMessage::Rejected { req_id: 7, error: WireError::new(ErrorCode::NotFound, "there is no node 9") },
                json!({"type": "rejected", "reqId": 7, "error": {"code": "notFound", "message": "there is no node 9"}}),
            ),
            (
                ServerMessage::Closing { error: WireError::new(ErrorCode::Overloaded, "too slow") },
                json!({"type": "closing", "error": {"code": "overloaded", "message": "too slow"}}),
            ),
            (
                ServerMessage::Catalog {
                    rev: 3,
                    scanning: true,
                    entries: vec![CatalogEntry { id: "builtin:osc".to_string(), name: "Oscillator".to_string(), format: "Built-in".to_string(), is_instrument: true, params: 8, presets: 0 }],
                },
                json!({
                    "type": "catalog",
                    "rev": 3,
                    "scanning": true,
                    "entries": [{"id": "builtin:osc", "name": "Oscillator", "format": "Built-in", "isInstrument": true, "params": 8, "presets": 0}],
                }),
            ),
            (
                ServerMessage::History { undo: Some("set cutoff".to_string()), redo: None },
                json!({"type": "history", "undo": "set cutoff", "redo": null}),
            ),
            (ServerMessage::History { undo: None, redo: Some("move an effect".to_string()) }, json!({"type": "history", "undo": null, "redo": "move an effect"})),
            (ServerMessage::Probes { nodes: vec![NodeId(3)] }, json!({"type": "probes", "nodes": [3]})),
            (
                ServerMessage::Inputs { names: vec!["Launchkey Mini MK4 37 MIDI Out".to_string(), "Seaboard RISE".to_string()] },
                json!({"type": "inputs", "names": ["Launchkey Mini MK4 37 MIDI Out", "Seaboard RISE"]}),
            ),
            (ServerMessage::Inputs { names: vec![] }, json!({"type": "inputs", "names": []})),
        ];
        for (message, json) in cases {
            assert_eq!(wire(&message), json, "{message:?}");
            assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), message);
        }
    }

    #[test]
    fn snapshots_and_layouts_are_nested_under_their_own_names() {
        use crate::app::testutil::{GROUPS, load};
        let model = load(GROUPS).model;
        let layout = model.telemetry_layout();
        let message = ServerMessage::Layout { layout: layout.clone() };
        let json = wire(&message);
        assert_eq!(json["type"], "layout");
        assert_eq!(json["layout"]["rev"], json!(layout.rev));
        assert_eq!(serde_json::from_value::<ServerMessage>(json).unwrap(), message);
    }

    #[test]
    fn every_error_code_has_its_own_camel_case_name() {
        let names: Vec<Value> = [
            ErrorCode::NotFound,
            ErrorCode::Invalid,
            ErrorCode::Io,
            ErrorCode::BadRequest,
            ErrorCode::Unauthorized,
            ErrorCode::RateLimited,
            ErrorCode::Overloaded,
        ]
        .iter()
        .map(wire)
        .collect();
        assert_eq!(names, ["notFound", "invalid", "io", "badRequest", "unauthorized", "rateLimited", "overloaded"].map(Value::from));
    }

    #[test]
    fn the_telemetry_channels_are_named_by_what_they_carry() {
        let names: Vec<Value> = [TelemetryChannel::Meters, TelemetryChannel::Modulation, TelemetryChannel::Held, TelemetryChannel::Scope].iter().map(wire).collect();
        assert_eq!(names, ["meters", "modulation", "held", "scope"].map(Value::from));
    }

    #[test]
    fn an_intent_error_says_what_it_was_in_a_code_and_a_sentence() {
        let missing = WireError::from(&IntentError::NotFound(NodeId(9)));
        assert_eq!((missing.code, missing.message.as_str()), (ErrorCode::NotFound, "there is no node 9"));
        let invalid = WireError::from(&IntentError::Invalid("no good".to_string()));
        assert_eq!((invalid.code, invalid.message.as_str()), (ErrorCode::Invalid, "no good"));
        let io = WireError::from(&IntentError::Io("Permission denied".to_string()));
        assert_eq!((io.code, io.message.as_str()), (ErrorCode::Io, "Permission denied"));
    }

    #[test]
    fn a_telemetry_frame_is_a_small_header_and_little_endian_floats() {
        let frame = telemetry_frame(TelemetryChannel::Modulation, 0x0102_0304, &[1.0, -2.5, 0.25]);
        assert_eq!(frame.len(), FRAME_HEADER_LEN + 3 * 4);
        assert_eq!(frame[0], 1, "the modulation channel");
        assert_eq!(frame[1], 0, "no flags");
        assert_eq!(&frame[2..4], &[3, 0], "three values");
        assert_eq!(&frame[4..8], &[4, 3, 2, 1], "the layout's revision");
        let values: Vec<f32> = frame[8..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(values, [1.0, -2.5, 0.25]);
        assert_eq!(telemetry_frame(TelemetryChannel::Meters, 0, &[])[0], 0, "the meters channel");
        assert_eq!(telemetry_frame(TelemetryChannel::Held, 0, &[0.0, 1.0])[0], 2, "the held notes' channel");
        assert_eq!(telemetry_frame(TelemetryChannel::Scope, 0, &[0.0, 1.0])[0], 3, "the scope's channel");
        assert_eq!(telemetry_frame(TelemetryChannel::Meters, 0, &[]).len(), FRAME_HEADER_LEN);
    }

    #[test]
    fn a_probe_frame_is_a_telemetry_frame_of_channel_four_with_the_node_where_the_layouts_revision_is() {
        let frame = probe_frame(NodeId(0x0102_0304), &[1.0, -2.5, 0.25]);
        assert_eq!(frame.len(), FRAME_HEADER_LEN + 3 * 4);
        assert_eq!(frame[0], 4, "the probe channel");
        assert_eq!(frame[1], 0, "no flags");
        assert_eq!(&frame[2..4], &[3, 0], "three values");
        assert_eq!(&frame[4..8], &[4, 3, 2, 1], "the node");
        let values: Vec<f32> = frame[8..].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(values, [1.0, -2.5, 0.25]);
        assert_eq!(probe_frame(NodeId(0), &[]).len(), FRAME_HEADER_LEN);
        assert_eq!(probe_frame(NodeId(5), &vec![0.5f32; 70_000]).len(), FRAME_HEADER_LEN + u16::MAX as usize * 4, "and as many values as the header can say");
    }

    #[test]
    fn a_telemetry_frame_never_claims_more_values_than_it_carries() {
        let values = vec![0.5f32; 70_000];
        let frame = telemetry_frame(TelemetryChannel::Meters, 1, &values);
        let n = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        assert_eq!(n, u16::MAX as usize);
        assert_eq!(frame.len(), FRAME_HEADER_LEN + n * 4);
    }
}

#[cfg(all(test, feature = "web"))]
mod typescript {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use ts_rs::{Config, TS};

    use serde::Serialize;

    use crate::app::graph::{Graph, Waveform};
    use crate::app::intent::{Event, Gain, Intent, ModulatorField, PatternField, StampedEvent};
    use crate::app::layout::TelemetryLayout;
    use crate::app::model::{BPM_MAX, BPM_MIN, CABLE_GAIN_MAX, DEPTH_MAX, DEPTH_MIN, NEW_CABLE_GAIN, STEP_MAX, STEP_MIN};
    use crate::plugin::chain::{COUNT_IN_BEATS, KEY_CENTRE, KEY_SPAN, MAX_STEPS};
    use crate::app::snapshot::Snapshot;
    use super::{ClientMessage, ServerMessage};

    /// Every gain, every modulator setting (`ModulatorField::ALL`) and every pattern setting, to write the range of each. (One left out
    /// is a key missing in the `Record` that is generated, which the web's typecheck refuses.)
    const GAINS: [Gain; 6] = [Gain::Volume, Gain::Mix, Gain::GroupVolume, Gain::GroupPan, Gain::MasterVolume, Gain::LimiterCeiling];
    const PATTERN_FIELDS: [PatternField; 4] = [PatternField::Length, PatternField::Enabled, PatternField::Looping, PatternField::Transpose];

    /// The entries of a TypeScript object that says `[min, max]` for each of `all`, by the
    /// name it has on the wire (the variants are serialized as strings).
    fn range_entries<T: Serialize + Copy>(all: &[T], range: fn(T) -> (f32, f32)) -> String {
        all.iter()
            .map(|&item| {
                let name = serde_json::to_value(item).unwrap();
                let (min, max) = range(item);
                format!("  {}: [{min}, {max}],\n", name.as_str().expect("a unit variant is a string"))
            })
            .collect()
    }

    /// The entries of a TypeScript object that says what each setting of a modulator is when the
    /// modulator is new: what a new LFO, a new envelope, a new macro and a new step sequencer start as (the first
    /// that has the setting: a step sequencer's rate, which is another, is in `new_steps`).
    fn default_entries() -> String {
        use crate::app::model::ModSourceSlot;
        let (lfo, envelope, knob, steps) = (ModSourceSlot::new_lfo(), ModSourceSlot::new_envelope(), ModSourceSlot::new_macro(), ModSourceSlot::new_steps());
        ModulatorField::ALL
            .iter()
            .map(|&field| {
                let name = serde_json::to_value(field).unwrap();
                let value = lfo
                    .setting(field)
                    .or_else(|| envelope.setting(field))
                    .or_else(|| knob.setting(field))
                    .or_else(|| steps.setting(field))
                    .expect("a new LFO, a new envelope, a new macro or a new step sequencer has every setting");
                format!("  {}: {value},\n", name.as_str().expect("a unit variant is a string"))
            })
            .collect()
    }

    /// The settings of a modulator whose value is a whole number, which the controller rounds it to (`ModulatorField::is_whole`), by the name they have on the wire, as the elements
    /// of a TypeScript array.
    fn whole_fields() -> String {
        let names: Vec<String> = ModulatorField::ALL.iter().filter(|field| field.is_whole()).map(|&field| serde_json::to_string(&field).unwrap()).collect();
        names.join(", ")
    }

    /// What a new step sequencer starts as, as a TypeScript object: its steps, its length, its rate, its glide, whether a note starts it again and its note length to follow
    /// the tempo by (`ModSourceSlot::new_steps`).
    fn new_steps() -> String {
        use crate::app::model::ModSourceSlot;
        let ModSourceSlot::Steps { values, length, rate, glide, retrigger, sync } = ModSourceSlot::new_steps() else { unreachable!("a new step sequencer is one") };
        let values: Vec<String> = values.iter().map(|value| value.to_string()).collect();
        format!("{{ values: [{}], length: {length}, rate: {rate}, glide: {glide}, retrigger: {retrigger}, sync: {sync} }}", values.join(", "))
    }

    /// The note lengths that a modulator can follow the tempo by (`SYNC_DIVISIONS`), as the lines of a TypeScript array of objects: the name a person reads and a session
    /// file says, and how many beats of the tempo it is, written as the shortest number that is the 32 bits the controller has.
    fn sync_divisions() -> String {
        crate::app::model::SYNC_DIVISIONS
            .iter()
            .map(|&(name, beats)| format!("  {{ name: {}, beats: {beats} }},\n", serde_json::to_string(name).unwrap()))
            .collect()
    }

    /// The entries of a TypeScript object that says how each setting of a modulator is turned, `"linear"` or `"log"`
    /// (`ModulatorField::taper`).
    fn taper_entries() -> String {
        use crate::app::intent::Taper;
        ModulatorField::ALL
            .iter()
            .map(|&field| {
                let name = serde_json::to_value(field).unwrap();
                let taper = match field.taper() {
                    Taper::Linear => "linear",
                    Taper::Log => "log",
                };
                format!("  {}: \"{taper}\",\n", name.as_str().expect("a unit variant is a string"))
            })
            .collect()
    }

    /// The names an LFO's waveforms have on the wire, in the order of their index (the value
    /// of `ModulatorField::Waveform`: `LfoWaveform::ALL`), as the elements of a TypeScript array.
    fn waveform_names() -> String {
        let names: Vec<String> = crate::plugin::chain::LfoWaveform::ALL
            .iter()
            .map(|&waveform| serde_json::to_string(&Waveform::from(waveform)).unwrap())
            .collect();
        names.join(", ")
    }

    /// The names of the roots a scale can have, by pitch class (0 C to 11 B), as the elements of a TypeScript
    /// array: what `setScale` and a session file call them (`scale::NOTE_NAMES`).
    fn scale_root_names() -> String {
        let names: Vec<String> = crate::scale::NOTE_NAMES.iter().map(|name| serde_json::to_string(name).unwrap()).collect();
        names.join(", ")
    }

    /// The kinds of scale, in the order the TUI lists them, as the lines of a TypeScript array of objects: the name a
    /// person reads, the short name that `setScale` and a session file say, and the semitones from the root of each of
    /// its notes (`scale::SCALES`).
    fn scale_kinds() -> String {
        crate::scale::SCALES
            .iter()
            .map(|scale| {
                let name = serde_json::to_string(scale.name).unwrap();
                let short = serde_json::to_string(scale.short).unwrap();
                let intervals = serde_json::to_string(scale.intervals).unwrap().replace(',', ", ");
                format!("  {{ name: {name}, short: {short}, intervals: {intervals} }},\n")
            })
            .collect()
    }

    /// Where the generated TypeScript is committed.
    fn generated_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/protocol/generated")
    }

    /// Write the TypeScript of every root type of the protocol, and of what each
    /// uses, into `dir`: one file per type.
    fn export_typescript(dir: &Path) -> anyhow::Result<()> {
        // Not `Config::from_env()`: what is generated must not depend on the
        // environment of whoever runs the test. `u64` counts (revisions) stay far
        // below 2^53, so they are plain numbers rather than `bigint`.
        let cfg = Config::new().with_large_int("number").with_out_dir(dir);
        Intent::export_all(&cfg)?;
        Event::export_all(&cfg)?;
        StampedEvent::export_all(&cfg)?;
        Graph::export_all(&cfg)?;
        TelemetryLayout::export_all(&cfg)?;
        Snapshot::export_all(&cfg)?;
        ClientMessage::export_all(&cfg)?;
        ServerMessage::export_all(&cfg)?;
        // The constants a client needs that are not types.
        fs::create_dir_all(dir)?;
        fs::write(
            dir.join("constants.ts"),
            format!(
                "// This file was generated by `app::protocol` (`TANG_UPDATE_TYPES=1 cargo test --features web typescript`). Do not edit this file manually.\n\
                 import type {{ Gain }} from \"./Gain\";\n\
                 import type {{ ModulatorField }} from \"./ModulatorField\";\n\
                 import type {{ PatternField }} from \"./PatternField\";\n\
                 import type {{ Waveform }} from \"./Waveform\";\n\n\
                 /** The version of the protocol: `Hello.protocol` says what the server speaks. */\n\
                 export const PROTOCOL = {};\n\n\
                 /** Bytes before the values of a binary telemetry frame. */\n\
                 export const FRAME_HEADER_LEN = {};\n\n\
                 /** The lowest and the highest value of each host-side gain: what the controller clamps to (`Gain::range`). */\n\
                 export const GAIN_RANGES: Record<Gain, readonly [number, number]> = {{\n{}}};\n\n\
                 /** The same for each setting of a modulator (`ModulatorField::range`). */\n\
                 export const MODULATOR_FIELD_RANGES: Record<ModulatorField, readonly [number, number]> = {{\n{}}};\n\n\
                 /** What each setting of a modulator is when the modulator is new (`ModSourceSlot::new_lfo`, `new_envelope`): where a double click puts a knob. */\n\
                 export const MODULATOR_FIELD_DEFAULTS: Record<ModulatorField, number> = {{\n{}}};\n\n\
                 /** How a setting of a modulator is turned: a rate and a time run over decades, which a knob turns by ratios (`ModulatorField::taper`). */\n\
                 export const MODULATOR_FIELD_SCALES: Record<ModulatorField, \"linear\" | \"log\"> = {{\n{}}};\n\n\
                 /** The settings of a modulator whose value is a whole number, which the controller rounds it to: a place in a list, a count, a switch (`ModulatorField::is_whole`). */\n\
                 export const MODULATOR_WHOLE_FIELDS: readonly ModulatorField[] = [{}];\n\n\
                 /** The lowest and the highest value of each setting of a pattern: what the controller clamps to (`PatternField::range`); a switch is 0 or 1. */\n\
                 export const PATTERN_FIELD_RANGES: Record<PatternField, readonly [number, number]> = {{\n{}}};\n\n\
                 /** How many beats of the metronome a recording is counted in with, before it takes the length of the pattern (`COUNT_IN_BEATS`). */\n\
                 export const PATTERN_COUNT_IN_BEATS = {COUNT_IN_BEATS};\n\n\
                 /** How long a pattern is, in beats, when a lane is given one that has nothing recorded (`PatternState::empty`). */\n\
                 export const PATTERN_DEFAULT_LENGTH = {};\n\n\
                 /** The note that a key control is centred on (middle C), and how many semitones either way make the whole of its depth (`KEY_CENTRE`, `KEY_SPAN`). */\n\
                 export const KEY_CENTRE = {KEY_CENTRE};\n\
                 export const KEY_SPAN = {KEY_SPAN};\n\n\
                 /** The lowest and the highest depth of a modulation cable (`DEPTH_MIN`, `DEPTH_MAX`). */\n\
                 export const DEPTH_RANGE: readonly [number, number] = [{DEPTH_MIN}, {DEPTH_MAX}];\n\n\
                 /** The lowest and the highest level of an audio cable, and the level a cable that is drawn without one starts at (`CABLE_GAIN_MAX`, `NEW_CABLE_GAIN`). */\n\
                 export const CABLE_GAIN_RANGE: readonly [number, number] = [0, {CABLE_GAIN_MAX}];\n\
                 export const NEW_CABLE_GAIN = {NEW_CABLE_GAIN};\n\n\
                 /** How many steps a step sequencer has, the bars of its node (`MAX_STEPS`), and the lowest and the highest value of a step (`STEP_MIN`, `STEP_MAX`). */\n\
                 export const MAX_STEPS = {MAX_STEPS};\n\
                 export const STEP_RANGE: readonly [number, number] = [{STEP_MIN}, {STEP_MAX}];\n\n\
                 /** What a new step sequencer starts as (`ModSourceSlot::new_steps`): the staircase of its steps, how many are in use, its rate (not the 1 of an LFO that the defaults of the settings say), its glide and whether a note starts it again. */\n\
                 export const NEW_STEPS = {} as const;\n\n\
                 /** The slowest and the fastest tempo, in beats a minute: what the controller holds a tempo to (`BPM_MIN`, `BPM_MAX`). */\n\
                 export const BPM_RANGE: readonly [number, number] = [{BPM_MIN}, {BPM_MAX}];\n\n\
                 /** The waveforms of an LFO by the index that is the value of its `waveform` setting (`LfoWaveform::ALL`). */\n\
                 export const WAVEFORMS: readonly Waveform[] = [{}];\n\n\
                 /** The note lengths that a cycle of an LFO, or a step of a step sequencer, can follow the tempo by (`SYNC_DIVISIONS`): the name a person reads and a session file says, and how many beats of the tempo it is. A modulator's `sync` is a place in this list counted from 1, and 0 is none, which is called `SYNC_FREE`. */\n\
                 export const SYNC_DIVISIONS: readonly {{ name: string; beats: number }}[] = [\n{}];\n\
                 export const SYNC_FREE = {};\n\n\
                 /** The roots a scale can have by pitch class (0 C to 11 B), as `setScale` and a session file name them (`scale::NOTE_NAMES`). */\n\
                 export const SCALE_ROOTS: readonly string[] = [{}];\n\n\
                 /** The kinds of scale, in the order the TUI lists them: the name a person reads, the short name that `setScale` and a session file say, and the semitones above the root of each of its notes (`scale::SCALES`). */\n\
                 export const SCALE_KINDS: readonly {{ name: string; short: string; intervals: readonly number[] }}[] = [\n{}];\n\n\
                 /** How far from the origin a node may be drawn, either way: what the controller clamps a place to (`NodeUi::LIMIT`). */\n\
                 export const NODE_UI_LIMIT = {};\n\n\
                 /** A place is kept to this many parts of a pixel: a tenth (`NodeUi::PARTS`). */\n\
                 export const NODE_UI_PARTS = {};\n\n\
                 /** How many parameters of a plugin can be pinned to its node: what the controller refuses more than (`MAX_PINNED`). */\n\
                 export const PINNED_LIMIT = {};\n\n\
                 /** How many characters a group's name can have: what the controller refuses more than (`MAX_NAME`). */\n\
                 export const NAME_LIMIT = {};\n\n\
                 /** How many characters the name of a MIDI device, as a source of notes is called by it, can have: what the controller refuses more than (`MAX_DEVICE_NAME`). */\n\
                 export const DEVICE_NAME_LIMIT = {};\n\n\
                 /** How many frames of each side a frame of the scope channel has (`SCOPE_FRAME_LENGTH`): the left side and then the right, the latest frame of each last. */\n\
                 export const SCOPE_FRAME_LENGTH = {};\n\n\
                 /** How long after one change of a thing, in seconds, the next change of it is still one step of the history (`history::SAME_STEP`). */\n\
                 export const HISTORY_SAME_STEP = {};\n\n\
                 /** How many steps the history keeps, the oldest going (`history::MAX_STEPS`). */\n\
                 export const HISTORY_MAX_STEPS = {};\n",
                super::PROTOCOL,
                super::FRAME_HEADER_LEN,
                range_entries(&GAINS, Gain::range),
                range_entries(&ModulatorField::ALL, ModulatorField::range),
                default_entries(),
                taper_entries(),
                whole_fields(),
                range_entries(&PATTERN_FIELDS, PatternField::range),
                crate::app::model::PatternState::empty(120.0).length_beats,
                new_steps(),
                waveform_names(),
                sync_divisions(),
                serde_json::to_string(crate::app::model::FREE).unwrap(),
                scale_root_names(),
                scale_kinds(),
                crate::session::NodeUi::LIMIT,
                crate::session::NodeUi::PARTS,
                crate::app::model::MAX_PINNED,
                crate::app::model::MAX_NAME,
                crate::session::MAX_DEVICE_NAME,
                super::SCOPE_FRAME_LENGTH,
                crate::app::history::SAME_STEP.as_secs_f64(),
                crate::app::history::MAX_STEPS,
            ),
        )?;
        Ok(())
    }

    /// The `.ts` files in a directory, by name.
    fn typescript_files(dir: &Path) -> BTreeMap<String, String> {
        let Ok(entries) = fs::read_dir(dir) else { return BTreeMap::new() };
        entries
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "ts"))
            .map(|path| (path.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(&path).unwrap()))
            .collect()
    }

    /// How the `.ts` files in `committed` differ from those in `fresh`, one line
    /// each, by file name. Other files in `committed` are not looked at.
    fn differences(committed: &Path, fresh: &Path) -> Vec<String> {
        let (committed, fresh) = (typescript_files(committed), typescript_files(fresh));
        let mut names: Vec<&String> = committed.keys().chain(fresh.keys()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| match (committed.get(name), fresh.get(name)) {
                (Some(a), Some(b)) if a == b => None,
                (Some(_), Some(_)) => Some(format!("{name} is out of date")),
                (Some(_), None) => Some(format!("{name} is not generated any more")),
                (None, _) => Some(format!("{name} is not committed")),
            })
            .collect()
    }

    fn write(dir: &Path, name: &str, text: &str) {
        fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn the_range_of_a_gain_is_written_by_the_gains_wire_name() {
        assert_eq!(
            range_entries(&GAINS, Gain::range),
            "  volume: [0, 4],\n  mix: [0, 1],\n  groupVolume: [0, 4],\n  groupPan: [-1, 1],\n  masterVolume: [0, 2],\n  limiterCeiling: [-24, 0],\n"
        );
    }

    #[test]
    fn the_range_of_a_pattern_setting_is_written_by_its_wire_name() {
        assert_eq!(
            range_entries(&PATTERN_FIELDS, PatternField::range),
            "  length: [1, 32],\n  enabled: [0, 1],\n  looping: [0, 1],\n  transpose: [0, 1],\n"
        );
    }

    #[test]
    fn the_range_of_a_modulator_setting_is_written_the_way_a_person_writes_the_number() {
        // (Not `0.009999999776482582`: an f32 is written as the shortest number that is it.)
        assert_eq!(
            range_entries(&ModulatorField::ALL, ModulatorField::range),
            "  rate: [0.01, 50],\n  waveform: [0, 5],\n  attack: [0.001, 10],\n  decay: [0.001, 10],\n  sustain: [0, 1],\n  release: [0.001, 10],\n  value: [0, 1],\n  glide: [0, 1],\n  length: [1, 16],\n  retrigger: [0, 1],\n  fade: [0, 5],\n  sync: [0, 16],\n"
        );
    }

    #[test]
    fn the_taper_of_a_modulator_setting_is_written_for_every_setting() {
        assert_eq!(
            taper_entries(),
            "  rate: \"log\",\n  waveform: \"linear\",\n  attack: \"log\",\n  decay: \"log\",\n  sustain: \"linear\",\n  release: \"log\",\n  value: \"linear\",\n  glide: \"linear\",\n  length: \"linear\",\n  retrigger: \"linear\",\n  fade: \"linear\",\n  sync: \"linear\",\n"
        );
    }

    #[test]
    fn what_a_new_modulator_starts_as_is_written_for_every_setting() {
        // (The retrigger is a switch that an LFO and a step sequencer both have, off for the one and on for the other: the table has the first that has it, the LFO's, and
        // the page's switch of a step sequencer starts from `NEW_STEPS`. The fade is an LFO's alone, and the sync is both's, none for either.)
        assert_eq!(
            default_entries(),
            "  rate: 1,\n  waveform: 0,\n  attack: 0.01,\n  decay: 0.3,\n  sustain: 0.7,\n  release: 0.5,\n  value: 0,\n  glide: 0,\n  length: 16,\n  retrigger: 0,\n  fade: 0,\n  sync: 0,\n"
        );
    }

    #[test]
    fn the_settings_that_are_whole_numbers_are_listed_by_their_wire_names() {
        assert_eq!(whole_fields(), "\"waveform\", \"length\", \"retrigger\", \"sync\"");
    }

    #[test]
    fn what_a_new_step_sequencer_starts_as_is_written_whole() {
        assert_eq!(
            new_steps(),
            "{ values: [-1, -0.875, -0.75, -0.625, -0.5, -0.375, -0.25, -0.125, 0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875], length: 16, rate: 8, glide: 0, retrigger: true, sync: 0 }"
        );
    }

    #[test]
    fn the_note_lengths_are_written_with_their_names_and_beats_in_the_order_of_their_place_and_free_is_what_none_is_called() {
        let written = sync_divisions();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), crate::app::model::SYNC_DIVISIONS.len());
        assert_eq!(lines[0], "  { name: \"4 bars\", beats: 16 },");
        assert_eq!(lines[7], "  { name: \"1/4\", beats: 1 },");
        assert_eq!(lines[5], "  { name: \"1/2 triplet\", beats: 1.3333334 },", "a number is written as the shortest one that is its 32 bits");
        assert_eq!(lines[15], "  { name: \"1/32\", beats: 0.125 },");
        assert_eq!(crate::app::model::FREE, "free");
    }

    #[test]
    fn the_waveforms_are_written_in_the_order_of_their_index() {
        assert_eq!(waveform_names(), "\"sine\", \"triangle\", \"saw\", \"square\", \"random\", \"smooth\"");
        // (The index is what a modulator's `waveform` setting is.)
        for (index, name) in crate::plugin::chain::LfoWaveform::ALL.iter().enumerate() {
            assert_eq!(name.to_index(), index);
        }
    }

    #[test]
    fn the_roots_of_a_scale_are_written_in_the_order_of_their_pitch_class() {
        assert_eq!(scale_root_names(), "\"C\", \"C#\", \"D\", \"D#\", \"E\", \"F\", \"F#\", \"G\", \"G#\", \"A\", \"A#\", \"B\"");
    }

    #[test]
    fn the_kinds_of_scale_are_written_with_the_name_a_person_reads_and_the_short_name_a_session_file_says() {
        let written = scale_kinds();
        assert_eq!(written.lines().count(), crate::scale::SCALES.len());
        assert!(
            written.starts_with(
                "  { name: \"Major\", short: \"major\", intervals: [0, 2, 4, 5, 7, 9, 11] },\n  { name: \"Natural Minor\", short: \"minor\", intervals: [0, 2, 3, 5, 7, 8, 10] },\n"
            ),
            "{written}"
        );
        assert!(written.contains("{ name: \"Minor Pentatonic\", short: \"min pent\", intervals: [0, 3, 5, 7, 10] },\n"), "{written}");
        assert!(written.ends_with("{ name: \"Chromatic\", short: \"chromatic\", intervals: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] },\n"), "{written}");
    }

    #[test]
    fn the_limits_of_a_place_are_in_the_constants_as_the_controller_keeps_a_place() {
        let dir = tempfile::tempdir().unwrap();
        export_typescript(dir.path()).unwrap();
        let constants = fs::read_to_string(dir.path().join("constants.ts")).unwrap();
        assert!(constants.contains("export const NODE_UI_LIMIT = 100000;\n"), "{constants}");
        assert!(constants.contains("export const NODE_UI_PARTS = 10;\n"), "{constants}");
    }

    #[test]
    fn the_length_of_what_a_scope_frame_has_of_each_side_is_in_the_constants() {
        let dir = tempfile::tempdir().unwrap();
        export_typescript(dir.path()).unwrap();
        let constants = fs::read_to_string(dir.path().join("constants.ts")).unwrap();
        assert!(constants.contains("export const SCOPE_FRAME_LENGTH = 2048;\n"), "{constants}");
    }

    #[test]
    fn the_limits_of_the_history_are_in_the_constants_as_the_controller_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        export_typescript(dir.path()).unwrap();
        let constants = fs::read_to_string(dir.path().join("constants.ts")).unwrap();
        assert!(constants.contains("export const HISTORY_SAME_STEP = 1;\n"), "{constants}");
        assert!(constants.contains("export const HISTORY_MAX_STEPS = 200;\n"), "{constants}");
    }

    #[test]
    fn two_equal_directories_have_no_differences() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for dir in [&a, &b] {
            write(dir.path(), "One.ts", "export type One = number;\n");
            write(dir.path(), "Two.ts", "export type Two = string;\n");
        }
        assert_eq!(differences(a.path(), b.path()), Vec::<String>::new());
    }

    #[test]
    fn a_changed_missing_and_extra_file_are_each_reported() {
        let (committed, fresh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(committed.path(), "Same.ts", "a");
        write(fresh.path(), "Same.ts", "a");
        write(committed.path(), "Changed.ts", "old");
        write(fresh.path(), "Changed.ts", "new");
        write(fresh.path(), "Missing.ts", "m");
        write(committed.path(), "Extra.ts", "e");
        assert_eq!(
            differences(committed.path(), fresh.path()),
            [
                "Changed.ts is out of date",
                "Extra.ts is not generated any more",
                "Missing.ts is not committed",
            ]
        );
    }

    #[test]
    fn a_file_that_is_not_typescript_is_left_alone() {
        let (committed, fresh) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        write(committed.path(), "README.md", "notes");
        assert_eq!(differences(committed.path(), fresh.path()), Vec::<String>::new());
    }

    #[test]
    fn the_generated_typescript_is_up_to_date() {
        let committed = generated_dir();
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            fs::create_dir_all(&committed).unwrap();
            for entry in fs::read_dir(&committed).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|ext| ext == "ts") {
                    fs::remove_file(path).unwrap();
                }
            }
            export_typescript(&committed).unwrap();
            return;
        }
        let fresh = tempfile::tempdir().unwrap();
        export_typescript(fresh.path()).unwrap();
        let differences = differences(&committed, fresh.path());
        assert!(
            differences.is_empty(),
            "web/src/protocol/generated is stale:\n  {}\nrun `TANG_UPDATE_TYPES=1 cargo test --features web typescript` and commit the result",
            differences.join("\n  ")
        );
    }
}
