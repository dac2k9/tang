//! What the server does with one connection's messages, apart from the sockets; see
//! `docs/web-ui.md` §3.3, §7 and §8.
//!
//! A `Connection` is the logic of a client's side of the protocol: it checks the
//! token, parses and validates each message, limits how fast they come, hands
//! intents to the owner of the controller, plays notes straight into the note path
//! and keeps track of which notes it holds so that a closed tab cannot leave one
//! sounding. It reaches the client through the hub. The socket glue that feeds it
//! frames and writes out the queue is in `web::server`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crossbeam_channel::{Sender, TrySendError};

use crate::app::intent::Intent;
use crate::app::model::NodeId;
use crate::app::protocol::{ClientMessage, ErrorCode, PROTOCOL, ServerMessage, WireError};
use crate::note_input::{Delivery, NoteInput};
use crate::web::Shared;
use crate::web::hub::{ClientId, Hub, Outgoing};
use crate::web::security::Policy;

/// The longest message the server reads from a connection that has not been let in, in bytes. A session's worth of intents is a
/// few dozen bytes each; this is generous.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// The longest message that a connection which has been let in may send (step 115): the text of a session file goes through as one (`Intent::OpenSession`), and a session that keeps the state of a plugin
/// (a sampler's, a drum kit's) is some hundreds of kilobytes. The frame that carries it is read whole before this is looked at, so the transport takes as much (`server::upgrade`) and a connection that
/// has not been let in is cut at `MAX_MESSAGE_BYTES` all the same, the moment that it is read.
pub const MAX_OPEN_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// The most nodes a connection may watch the signal of at once (`ClientMessage::Probes`): far more than the engine has rings for, which are shared by every client; a longer list is cut.
pub const MAX_PROBES: usize = 64;

/// What a connection asks of the owner of the controller, over a channel.
#[derive(Debug, PartialEq)]
pub enum OwnerRequest {
    /// Send the client a snapshot, and then the events after it.
    Subscribe { client: ClientId },
    /// Apply an intent and answer `req_id`.
    Apply { client: ClientId, req_id: u32, intent: Intent },
    /// Watch the signal at these nodes for the client, and tell it which are watched.
    Probes { client: ClientId, nodes: Vec<NodeId> },
}

/// Whether the connection goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// The client has been told why (best effort); close the socket.
    Close,
}

/// How many messages a connection may send: a burst, then a steady rate.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub burst: f64,
    pub per_second: f64,
}

impl Default for Limits {
    /// A drag on a few sliders and a played chord fit with room to spare.
    fn default() -> Self {
        Limits { burst: 300.0, per_second: 200.0 }
    }
}

/// A token bucket.
struct RateLimiter {
    limits: Limits,
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    fn new(limits: Limits, now: Instant) -> Self {
        RateLimiter { limits, tokens: limits.burst, last: now }
    }

    /// Spend a token if there is one.
    fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.limits.per_second).min(self.limits.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

enum State {
    /// The server wants a token, and has not had it.
    Unauthorized,
    Open,
}

/// What a connection works with: the same for every connection of a server.
pub struct Context {
    pub hub: Arc<Hub>,
    pub policy: Arc<Policy>,
    pub shared: Arc<Shared>,
    /// Where requests for the owner of the controller go.
    pub owner: Sender<OwnerRequest>,
    pub notes: NoteInput,
    pub limits: Limits,
    /// Whether this machine can show a plugin's own window (`Config::windows`): said in the hello.
    pub windows: bool,
    /// Whether the program can open another session while it runs (`Config::open_session`): said in the hello.
    pub open_session: bool,
}

pub struct Connection {
    id: ClientId,
    hub: Arc<Hub>,
    policy: Arc<Policy>,
    shared: Arc<Shared>,
    owner: Sender<OwnerRequest>,
    notes: NoteInput,
    windows: bool,
    open_session: bool,
    state: State,
    /// The notes this connection has started and not ended, one bit each.
    held: u128,
    limiter: RateLimiter,
}

impl Connection {
    pub fn new(id: ClientId, context: Context, now: Instant) -> Connection {
        let Context { hub, policy, shared, owner, notes, limits, windows, open_session } = context;
        let state = if policy.needs_token() { State::Unauthorized } else { State::Open };
        Connection { id, hub, policy, shared, owner, notes, windows, open_session, state, held: 0, limiter: RateLimiter::new(limits, now) }
    }

    /// Whether the connection may go on without the token: it has none to give, or it
    /// has given it.
    pub fn is_open(&self) -> bool {
        matches!(self.state, State::Open)
    }

    /// Say hello: the first message of the connection.
    pub fn open(&self) {
        self.reply(&ServerMessage::Hello {
            protocol: PROTOCOL,
            rev: self.shared.rev.load(Ordering::Relaxed),
            features: ["notes", "telemetry"].into_iter().chain(self.windows.then_some("windows")).chain(self.open_session.then_some("openSession")).map(String::from).collect(),
            needs_token: self.policy.needs_token(),
        });
    }

    /// A text frame arrived.
    pub fn on_text(&mut self, text: &str, now: Instant) -> Flow {
        let longest = if self.is_open() { MAX_OPEN_MESSAGE_BYTES } else { MAX_MESSAGE_BYTES };
        if text.len() > longest {
            return self.close_with(ErrorCode::BadRequest, "that message is too long");
        }
        let message = match serde_json::from_str::<ClientMessage>(text) {
            Ok(message) => message,
            Err(e) => return self.close_with(ErrorCode::BadRequest, format!("not a message the server understands: {e}")),
        };
        match (&self.state, message) {
            (State::Unauthorized, ClientMessage::Auth { token }) => {
                if self.policy.token_matches(&token) {
                    self.state = State::Open;
                    Flow::Continue
                } else {
                    self.close_with(ErrorCode::Unauthorized, "the token is wrong")
                }
            }
            (State::Unauthorized, _) => self.close_with(ErrorCode::Unauthorized, "send the token first"),
            (State::Open, message) => self.on_message(message, now),
        }
    }

    /// A binary frame arrived: the client has none to send.
    pub fn on_binary(&mut self) -> Flow {
        self.close_with(ErrorCode::BadRequest, "the server takes text frames only")
    }

    fn on_message(&mut self, message: ClientMessage, now: Instant) -> Flow {
        match message {
            // Already in: nothing to do, and no harm.
            ClientMessage::Auth { .. } => Flow::Continue,
            // A client says each of these once per connection; one that says them over
            // and over (each subscribe makes the owner build a snapshot) is not one to go on with.
            ClientMessage::Subscribe => {
                if !self.limiter.allow(now) {
                    return self.close_with(ErrorCode::RateLimited, "too many requests: slow down");
                }
                self.ask_owner(OwnerRequest::Subscribe { client: self.id });
                Flow::Continue
            }
            ClientMessage::Telemetry { channels } => {
                if !self.limiter.allow(now) {
                    return self.close_with(ErrorCode::RateLimited, "too many requests: slow down");
                }
                self.hub.set_channels(self.id, &channels);
                Flow::Continue
            }
            ClientMessage::Probes { mut nodes } => {
                if !self.limiter.allow(now) {
                    return self.close_with(ErrorCode::RateLimited, "too many requests: slow down");
                }
                nodes.truncate(MAX_PROBES);
                // (The hub is told at once, so that the frames of a node that has a ring are sent from the next turn; the owner says which of them have one.)
                self.hub.set_probes(self.id, &nodes);
                self.ask_owner(OwnerRequest::Probes { client: self.id, nodes });
                Flow::Continue
            }
            ClientMessage::Request { req_id, intent } => {
                if !self.limiter.allow(now) {
                    self.reject(req_id, ErrorCode::RateLimited, "too many requests: slow down");
                } else if !self.ask_owner(OwnerRequest::Apply { client: self.id, req_id, intent }) {
                    self.reject(req_id, ErrorCode::RateLimited, "the server is busy: try again");
                }
                Flow::Continue
            }
            ClientMessage::NoteOn { note, velocity } => {
                if note > 127 || velocity == 0 || velocity > 127 {
                    return self.close_with(ErrorCode::BadRequest, "a note is 0 to 127, its velocity 1 to 127");
                }
                if self.limiter.allow(now) && self.notes.note_on(note, velocity) == Delivery::Sent {
                    self.held |= 1u128 << note;
                }
                Flow::Continue
            }
            ClientMessage::NoteOff { note } => {
                if note > 127 {
                    return self.close_with(ErrorCode::BadRequest, "a note is 0 to 127");
                }
                // An end is never dropped for being too fast: a note left on is worse.
                self.notes.note_off(note);
                self.held &= !(1u128 << note);
                Flow::Continue
            }
            // Never dropped for being too fast, as an end is not: it is what a person does when a note hangs, and every one of them is let go of, the server's and not only
            // this connection's. What this connection held is let go of with the rest (closing it ends none of them a second time).
            ClientMessage::Panic => {
                let lost = self.notes.panic();
                if lost > 0 {
                    log::warn!("Panic: the channel to the audio thread was full, and {lost} messages were lost");
                }
                self.held = 0;
                Flow::Continue
            }
        }
    }

    /// The connection is over, however it ended: end the notes it holds and leave
    /// the hub. Safe to call twice: the second time there is nothing left to end.
    pub fn close(&mut self) {
        for note in 0..128u8 {
            if self.held & (1u128 << note) != 0 {
                self.notes.note_off(note);
            }
        }
        self.held = 0;
        self.hub.disconnect(self.id);
    }

    fn ask_owner(&self, request: OwnerRequest) -> bool {
        match self.owner.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn reject(&self, req_id: u32, code: ErrorCode, message: &str) {
        self.reply(&ServerMessage::Rejected { req_id, error: WireError::new(code, message) });
    }

    fn close_with(&mut self, code: ErrorCode, message: impl Into<String>) -> Flow {
        self.reply(&ServerMessage::Closing { error: WireError::new(code, message) });
        Flow::Close
    }

    fn reply(&self, message: &ServerMessage) {
        let json = serde_json::to_string(message).expect("a message is serializable");
        self.hub.send(self.id, Outgoing::Text(json.into()));
    }
}

/// However the task that owns a connection ends (it returns, it is cancelled with its
/// runtime, it panics), the notes the client holds are ended.
impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::mpsc::Receiver;

    use super::*;
    use crate::app::model::{NodeId, SessionModel};
    use crate::audio::MidiEvent;
    use crate::held_notes::HeldNotes;
    use crate::piano_filter::{PianoFilter, PianoMode};
    use crate::scale::ScaleSetting;
    use crate::web::security::Token;

    struct Rig {
        conn: Connection,
        id: ClientId,
        out: Receiver<Outgoing>,
        owner: crossbeam_channel::Receiver<OwnerRequest>,
        midi: crossbeam_channel::Receiver<MidiEvent>,
        hub: Arc<Hub>,
        filter: Arc<PianoFilter>,
        held: Arc<HeldNotes>,
        t0: Instant,
    }

    fn rig_with(policy: Policy, limits: Limits, owner_capacity: usize) -> Rig {
        let hub = Arc::new(Hub::new(64));
        let (id, out) = hub.connect();
        let (owner_tx, owner) = crossbeam_channel::bounded(owner_capacity);
        let (midi_tx, midi) = crossbeam_channel::bounded(256);
        let held = Arc::new(HeldNotes::new());
        let filter = Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight));
        let notes = NoteInput::new(midi_tx, held.clone(), filter.clone());
        let t0 = Instant::now();
        let shared = Arc::new(Shared::new(7, SessionModel::default().telemetry_layout()));
        let context = Context { hub: hub.clone(), policy: Arc::new(policy), shared, owner: owner_tx, notes, limits, windows: false, open_session: false };
        let conn = Connection::new(id, context, t0);
        Rig { conn, id, out, owner, midi, hub, filter, held, t0 }
    }

    fn open_policy() -> Policy {
        Policy::new("127.0.0.1:8080".parse().unwrap(), None, vec![]).unwrap()
    }

    fn guarded_policy() -> Policy {
        Policy::new("192.168.1.5:8080".parse().unwrap(), Some(Token::new("secret")), vec![]).unwrap()
    }

    fn rig() -> Rig {
        rig_with(open_policy(), Limits::default(), 16)
    }

    impl Rig {
        fn send(&mut self, message: Value) -> Flow {
            let now = self.t0;
            self.conn.on_text(&message.to_string(), now)
        }

        fn send_at(&mut self, message: Value, after: Duration) -> Flow {
            let now = self.t0 + after;
            self.conn.on_text(&message.to_string(), now)
        }

        fn said(&mut self) -> Vec<Value> {
            let mut said = Vec::new();
            while let Ok(Outgoing::Text(text)) = self.out.try_recv() {
                said.push(serde_json::from_str(&text).unwrap());
            }
            said
        }

        fn asked(&self) -> Vec<OwnerRequest> {
            self.owner.try_iter().collect()
        }

        fn played(&self) -> Vec<MidiEvent> {
            self.midi.try_iter().collect()
        }
    }

    fn set_param() -> Value {
        json!({"type": "request", "reqId": 5, "intent": {"type": "setParam", "node": 3, "param": 1, "value": 0.5}})
    }

    // -- Hello and the token --

    #[test]
    fn a_connection_opens_with_hello() {
        let mut rig = rig();
        rig.conn.open();
        assert_eq!(
            rig.said(),
            [json!({"type": "hello", "protocol": PROTOCOL, "rev": 7, "features": ["notes", "telemetry"], "needsToken": false})]
        );
    }

    #[test]
    fn hello_says_windows_only_when_this_machine_can_show_a_plugins_own_window() {
        let mut rig = rig();
        rig.conn.windows = true;
        rig.conn.open();
        assert_eq!(
            rig.said(),
            [json!({"type": "hello", "protocol": PROTOCOL, "rev": 7, "features": ["notes", "telemetry", "windows"], "needsToken": false})],
            "after the others, so that a client that reads the first two reads them as it did"
        );
    }

    #[test]
    fn hello_says_when_a_token_is_needed() {
        let mut rig = rig_with(guarded_policy(), Limits::default(), 16);
        rig.conn.open();
        assert_eq!(rig.said()[0]["needsToken"], json!(true));
    }

    #[test]
    fn a_connection_is_open_when_it_needs_no_token_or_has_given_it() {
        assert!(rig().conn.is_open());
        let mut guarded = rig_with(guarded_policy(), Limits::default(), 16);
        assert!(!guarded.conn.is_open());
        guarded.send(json!({"type": "auth", "token": "wrong"}));
        assert!(!guarded.conn.is_open());
        let mut guarded = rig_with(guarded_policy(), Limits::default(), 16);
        guarded.send(json!({"type": "auth", "token": "secret"}));
        assert!(guarded.conn.is_open());
    }

    #[test]
    fn with_a_token_nothing_is_processed_before_it_and_a_wrong_first_message_ends_the_connection() {
        for first in [
            json!({"type": "subscribe"}),
            set_param(),
            json!({"type": "noteOn", "note": 60, "velocity": 100}),
            json!({"type": "telemetry", "channels": ["meters"]}),
        ] {
            let mut rig = rig_with(guarded_policy(), Limits::default(), 16);
            assert_eq!(rig.send(first.clone()), Flow::Close, "{first}");
            let said = rig.said();
            assert_eq!(said.len(), 1);
            assert_eq!(said[0]["type"], "closing");
            assert_eq!(said[0]["error"]["code"], "unauthorized");
            assert!(rig.asked().is_empty() && rig.played().is_empty());
            assert!(!rig.hub.wanted(crate::app::protocol::TelemetryChannel::Meters));
        }
    }

    #[test]
    fn a_wrong_token_ends_the_connection_and_the_right_one_opens_it() {
        let mut rig = rig_with(guarded_policy(), Limits::default(), 16);
        assert_eq!(rig.send(json!({"type": "auth", "token": "Secret"})), Flow::Close);
        assert_eq!(rig.said()[0]["error"]["code"], "unauthorized");

        let mut rig = rig_with(guarded_policy(), Limits::default(), 16);
        assert_eq!(rig.send(json!({"type": "auth", "token": "secret"})), Flow::Continue);
        assert!(rig.said().is_empty(), "no news is good news");
        assert_eq!(rig.send(json!({"type": "subscribe"})), Flow::Continue);
        assert_eq!(rig.asked(), [OwnerRequest::Subscribe { client: rig.id }]);
    }

    #[test]
    fn a_connection_that_has_got_in_may_send_the_token_again_harmlessly() {
        let mut rig = rig();
        assert_eq!(rig.send(json!({"type": "auth", "token": "anything"})), Flow::Continue);
        assert!(rig.said().is_empty());
        let mut guarded = rig_with(guarded_policy(), Limits::default(), 16);
        guarded.send(json!({"type": "auth", "token": "secret"}));
        assert_eq!(guarded.send(json!({"type": "auth", "token": "wrong now"})), Flow::Continue, "it is already in");
    }

    // -- Requests --

    #[test]
    fn a_request_goes_to_the_owner_with_its_id() {
        let mut rig = rig();
        assert_eq!(rig.send(set_param()), Flow::Continue);
        assert_eq!(
            rig.asked(),
            [OwnerRequest::Apply { client: rig.id, req_id: 5, intent: Intent::SetParam { node: NodeId(3), param: 1, value: 0.5 } }]
        );
        assert!(rig.said().is_empty(), "the owner answers");
    }

    #[test]
    fn a_busy_owner_is_answered_for_it() {
        let mut rig = rig_with(open_policy(), Limits::default(), 1);
        rig.send(set_param());
        assert_eq!(rig.send(set_param()), Flow::Continue);
        let said = rig.said();
        assert_eq!(said.len(), 1);
        assert_eq!(said[0]["type"], "rejected");
        assert_eq!(said[0]["reqId"], 5);
        assert_eq!(said[0]["error"]["code"], "rateLimited");
        assert_eq!(rig.asked().len(), 1);
    }

    #[test]
    fn requests_come_at_a_limited_rate_and_the_rate_recovers() {
        let mut rig = rig_with(open_policy(), Limits { burst: 5.0, per_second: 10.0 }, 64);
        for _ in 0..5 {
            rig.send(set_param());
        }
        assert_eq!(rig.asked().len(), 5, "a burst of five is fine");
        assert_eq!(rig.send(set_param()), Flow::Continue, "the sixth is refused, not fatal");
        let said = rig.said();
        assert_eq!((said[0]["type"].clone(), said[0]["error"]["code"].clone()), (json!("rejected"), json!("rateLimited")));
        assert!(rig.asked().is_empty());
        // A tenth of a second at ten a second is one more.
        rig.send_at(set_param(), Duration::from_millis(110));
        assert_eq!(rig.asked().len(), 1);
        rig.send_at(set_param(), Duration::from_millis(111));
        assert!(rig.asked().is_empty(), "and then it is spent again");
        // A long wait refills the burst, and no more than the burst.
        for _ in 0..8 {
            rig.send_at(set_param(), Duration::from_secs(60));
        }
        assert_eq!(rig.asked().len(), 5);
    }

    #[test]
    fn notes_share_the_rate_but_a_note_off_is_never_dropped_for_it() {
        let mut rig = rig_with(open_policy(), Limits { burst: 2.0, per_second: 1.0 }, 64);
        for note in [60, 62, 64] {
            rig.send(json!({"type": "noteOn", "note": note, "velocity": 100}));
        }
        assert_eq!(rig.played(), [(0, [0x90, 60, 100]), (0, [0x90, 62, 100])], "the third is too fast and dropped");
        rig.send(json!({"type": "noteOff", "note": 60}));
        rig.send(json!({"type": "noteOff", "note": 62}));
        rig.send(json!({"type": "noteOff", "note": 64}));
        assert_eq!(rig.played(), [(0, [0x80, 60, 0]), (0, [0x80, 62, 0]), (0, [0x80, 64, 0])]);
    }

    #[test]
    fn a_panic_lets_go_of_every_note_the_server_holds_not_only_this_connections_and_is_not_held_back_by_the_rate() {
        let mut rig = rig_with(open_policy(), Limits { burst: 2.0, per_second: 1.0 }, 64);
        // Two notes are held by this connection (the burst), and one by something else (a key of the terminal, a hardware keyboard).
        rig.send(json!({"type": "noteOn", "note": 60, "velocity": 100}));
        rig.send(json!({"type": "noteOn", "note": 62, "velocity": 100}));
        rig.held.note_on(72);
        rig.played();
        // The rate is spent, and a panic is sent all the same.
        rig.send(json!({"type": "panic"}));
        let played = rig.played();
        assert_eq!(played.len(), 16 + 3 + 16 + 16, "pedal up on every channel, the three held notes ended, all notes off and all sound off on every channel");
        let to_all = |bytes: [u8; 3]| MidiEvent::from_source(crate::source::Source::BROADCAST, 0, bytes);
        assert_eq!(played[16..19], [to_all([0x80, 60, 0]), to_all([0x80, 62, 0]), to_all([0x80, 72, 0])], "the notes held, the lowest first, as no source's: every lane hears a panic");
        assert!(rig.held.held().is_empty(), "and nothing is held after it");
        // What the connection held is let go of: closing it ends no note a second time.
        rig.conn.close();
        assert!(rig.played().is_empty(), "the notes it had are ended already");
    }

    // -- Subscribing and telemetry --

    #[test]
    fn subscribing_is_the_owners_to_do() {
        let mut rig = rig();
        rig.send(json!({"type": "subscribe"}));
        assert_eq!(rig.asked(), [OwnerRequest::Subscribe { client: rig.id }]);
        assert!(!rig.hub.broadcast(&Outgoing::Text("x".into())).contains(&rig.id));
        assert!(rig.said().is_empty(), "not subscribed until the owner says so");
    }

    #[test]
    fn the_telemetry_a_client_asks_for_is_what_the_hub_is_told() {
        use crate::app::protocol::TelemetryChannel::{Meters, Modulation};
        let mut rig = rig();
        rig.send(json!({"type": "telemetry", "channels": ["meters"]}));
        assert!(rig.hub.wanted(Meters) && !rig.hub.wanted(Modulation));
        rig.send(json!({"type": "telemetry", "channels": ["modulation"]}));
        assert!(!rig.hub.wanted(Meters) && rig.hub.wanted(Modulation));
        rig.send(json!({"type": "telemetry", "channels": []}));
        assert!(!rig.hub.wanted(Meters) && !rig.hub.wanted(Modulation));
    }

    #[test]
    fn the_probes_a_client_asks_for_are_what_the_hub_is_told_and_the_owner_is_asked_to_set_them() {
        let mut rig = rig();
        rig.send(json!({"type": "probes", "nodes": [3, 9]}));
        assert!(rig.hub.probe_wanted(NodeId(3)) && rig.hub.probe_wanted(NodeId(9)) && !rig.hub.probe_wanted(NodeId(4)));
        assert_eq!(rig.asked(), [OwnerRequest::Probes { client: rig.id, nodes: vec![NodeId(3), NodeId(9)] }]);
        rig.send(json!({"type": "probes", "nodes": []}));
        assert!(!rig.hub.probe_wanted(NodeId(3)));
        assert_eq!(rig.asked(), [OwnerRequest::Probes { client: rig.id, nodes: vec![] }], "and the owner is told that it is none");
    }

    #[test]
    fn a_client_that_asks_for_more_probes_than_a_connection_can_have_is_given_the_first() {
        let mut rig = rig();
        let nodes: Vec<u32> = (0..100).collect();
        rig.send(json!({"type": "probes", "nodes": nodes}));
        assert!(rig.hub.probe_wanted(NodeId(MAX_PROBES as u32 - 1)) && !rig.hub.probe_wanted(NodeId(MAX_PROBES as u32)));
        let asked = rig.asked();
        let [OwnerRequest::Probes { nodes, .. }] = &asked[..] else { panic!("{asked:?}") };
        assert_eq!(nodes.len(), MAX_PROBES);
    }

    #[test]
    fn asking_for_probes_over_and_over_is_too_fast_and_ends_the_connection() {
        let mut rig = rig_with(open_policy(), Limits { burst: 2.0, per_second: 1.0 }, 64);
        assert_eq!(rig.send(json!({"type": "probes", "nodes": [1]})), Flow::Continue);
        assert_eq!(rig.send(json!({"type": "probes", "nodes": [2]})), Flow::Continue);
        assert_eq!(rig.send(json!({"type": "probes", "nodes": [3]})), Flow::Close);
        assert_eq!(rig.said()[0]["error"]["code"], "rateLimited");
        assert!(rig.hub.probe_wanted(NodeId(2)) && !rig.hub.probe_wanted(NodeId(3)), "the last one that was taken stands");
    }

    #[test]
    fn subscribing_over_and_over_is_too_fast_and_ends_the_connection() {
        // Each one makes the owner build a snapshot, so each one costs a token.
        let mut rig = rig_with(open_policy(), Limits { burst: 3.0, per_second: 1.0 }, 64);
        for _ in 0..3 {
            assert_eq!(rig.send(json!({"type": "subscribe"})), Flow::Continue);
        }
        assert_eq!(rig.asked().len(), 3);
        assert_eq!(rig.send(json!({"type": "subscribe"})), Flow::Close);
        let said = rig.said();
        assert_eq!((said[0]["type"].clone(), said[0]["error"]["code"].clone()), (json!("closing"), json!("rateLimited")));
        assert!(rig.asked().is_empty(), "the owner is not asked for a fourth snapshot");
    }

    #[test]
    fn asking_for_telemetry_over_and_over_is_too_fast_and_ends_the_connection() {
        use crate::app::protocol::TelemetryChannel::{Meters, Modulation};
        let mut rig = rig_with(open_policy(), Limits { burst: 2.0, per_second: 1.0 }, 64);
        assert_eq!(rig.send(json!({"type": "telemetry", "channels": ["meters"]})), Flow::Continue);
        assert_eq!(rig.send(json!({"type": "telemetry", "channels": ["modulation"]})), Flow::Continue);
        assert_eq!(rig.send(json!({"type": "telemetry", "channels": ["meters"]})), Flow::Close);
        assert_eq!(rig.said()[0]["error"]["code"], "rateLimited");
        assert!(!rig.hub.wanted(Meters) && rig.hub.wanted(Modulation), "the last one that was taken stands");
    }

    #[test]
    fn what_a_client_sends_once_per_connection_costs_next_to_nothing() {
        // A client that subscribes, asks for its telemetry and then drags a slider is nowhere near the limit.
        let mut rig = rig_with(open_policy(), Limits { burst: 5.0, per_second: 1.0 }, 64);
        assert_eq!(rig.send(json!({"type": "subscribe"})), Flow::Continue);
        assert_eq!(rig.send(json!({"type": "telemetry", "channels": ["meters", "modulation"]})), Flow::Continue);
        for _ in 0..3 {
            assert_eq!(rig.send(set_param()), Flow::Continue);
        }
        assert!(rig.said().is_empty());
        assert_eq!(rig.asked().len(), 4);
    }

    // -- Notes --

    #[test]
    fn a_note_is_played_into_the_note_path_and_held_until_it_ends() {
        let mut rig = rig();
        rig.send(json!({"type": "noteOn", "note": 60, "velocity": 100}));
        assert_eq!(rig.played(), [(0, [0x90, 60, 100])]);
        assert_eq!(rig.held.held(), [60], "the Piano tab shows it");
        rig.send(json!({"type": "noteOff", "note": 60}));
        assert_eq!(rig.played(), [(0, [0x80, 60, 0])]);
        assert!(rig.held.held().is_empty());
    }

    #[test]
    fn closing_ends_every_note_the_connection_still_holds_and_leaves_the_hub() {
        let mut rig = rig();
        for note in [60, 64, 67, 127, 0] {
            rig.send(json!({"type": "noteOn", "note": note, "velocity": 90}));
        }
        rig.send(json!({"type": "noteOff", "note": 64}));
        rig.played();
        assert_eq!(rig.hub.client_count(), 1);
        rig.conn.close();
        assert_eq!(rig.played(), [(0, [0x80, 0, 0]), (0, [0x80, 60, 0]), (0, [0x80, 67, 0]), (0, [0x80, 127, 0])]);
        assert!(rig.held.held().is_empty());
        assert_eq!(rig.hub.client_count(), 0);
        rig.conn.close();
        assert!(rig.played().is_empty(), "closing twice ends nothing twice");
    }

    #[test]
    fn a_connection_that_is_dropped_is_closed() {
        let Rig { conn, hub, midi, held, .. } = {
            let mut rig = rig();
            rig.send(json!({"type": "noteOn", "note": 72, "velocity": 100}));
            rig
        };
        assert_eq!(held.held(), [72]);
        assert_eq!(hub.client_count(), 1);
        drop(conn);
        let played: Vec<MidiEvent> = midi.try_iter().collect();
        assert_eq!(played, [(0, [0x90, 72, 100]), (0, [0x80, 72, 0])]);
        assert!(held.held().is_empty());
        assert_eq!(hub.client_count(), 0);
    }

    #[test]
    fn a_note_the_locked_mode_dropped_is_not_held_so_closing_does_not_end_it() {
        let mut rig = rig();
        rig.filter.set_mode(PianoMode::Locked);
        rig.send(json!({"type": "noteOn", "note": 61, "velocity": 100}));
        assert!(rig.played().is_empty());
        rig.conn.close();
        assert!(rig.played().is_empty());
    }

    #[test]
    fn a_note_that_was_dropped_for_a_full_channel_is_not_held() {
        let hub = Arc::new(Hub::new(64));
        let (id, _out) = hub.connect();
        let (owner_tx, _owner) = crossbeam_channel::bounded(4);
        let (midi_tx, midi) = crossbeam_channel::bounded(1);
        let notes = NoteInput::new(
            midi_tx,
            Arc::new(HeldNotes::new()),
            Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight)),
        );
        let t0 = Instant::now();
        let context = Context {
            hub,
            policy: Arc::new(open_policy()),
            shared: Arc::new(Shared::new(0, SessionModel::default().telemetry_layout())),
            owner: owner_tx,
            notes,
            limits: Limits::default(),
            windows: false,
            open_session: false,
        };
        let mut conn = Connection::new(id, context, t0);
        conn.on_text(&json!({"type": "noteOn", "note": 60, "velocity": 100}).to_string(), t0);
        conn.on_text(&json!({"type": "noteOn", "note": 62, "velocity": 100}).to_string(), t0);
        assert_eq!(midi.try_iter().count(), 1);
        conn.close();
        let ended: Vec<MidiEvent> = midi.try_iter().collect();
        assert_eq!(ended, [(0, [0x80, 60, 0])], "only the note that sounded is ended (the channel had room for one)");
    }

    #[test]
    fn a_note_out_of_range_ends_the_connection() {
        for bad in [
            json!({"type": "noteOn", "note": 128, "velocity": 100}),
            json!({"type": "noteOn", "note": 60, "velocity": 0}),
            json!({"type": "noteOn", "note": 60, "velocity": 128}),
            json!({"type": "noteOff", "note": 200}),
        ] {
            let mut rig = rig();
            assert_eq!(rig.send(bad.clone()), Flow::Close, "{bad}");
            let said = rig.said();
            assert_eq!(said[0]["type"], "closing");
            assert_eq!(said[0]["error"]["code"], "badRequest");
            assert!(rig.played().is_empty());
        }
    }

    // -- Bad input --

    #[test]
    fn what_is_not_a_message_ends_the_connection_with_a_reason() {
        for bad in ["", "not json", "{}", r#"{"type":"explode"}"#, r#"{"type":"noteOn","note":"C4","velocity":100}"#, "[1,2]", "null"] {
            let mut rig = rig();
            let now = rig.t0;
            assert_eq!(rig.conn.on_text(bad, now), Flow::Close, "{bad:?}");
            let said = rig.said();
            assert_eq!(said[0]["type"], "closing", "{bad:?}");
            assert_eq!(said[0]["error"]["code"], "badRequest", "{bad:?}");
        }
    }

    #[test]
    fn a_message_that_is_too_long_ends_the_connection_unread() {
        // A connection that has not been let in (it has a token to give, and has not given it) reads no more than the small limit.
        let mut rig = rig_with(guarded_policy(), Limits::default(), 16);
        let now = rig.t0;
        let long = format!(r#"{{"type":"auth","token":"{}"}}"#, "x".repeat(MAX_MESSAGE_BYTES));
        assert_eq!(rig.conn.on_text(&long, now), Flow::Close);
        assert_eq!(rig.said()[0]["error"]["code"], "badRequest");
        // One that has been let in reads more (a session's file goes through as one message): exactly the longest, and one more is not.
        let padding = MAX_OPEN_MESSAGE_BYTES - r#"{"type":"auth","token":""}"#.len();
        let just_fits = format!(r#"{{"type":"auth","token":"{}"}}"#, "x".repeat(padding));
        assert_eq!(just_fits.len(), MAX_OPEN_MESSAGE_BYTES);
        let mut rig = rig_with(open_policy(), Limits::default(), 16);
        assert_eq!(rig.conn.on_text(&just_fits, now), Flow::Continue);
        let one_too_many = format!(r#"{{"type":"auth","token":"{}"}}"#, "x".repeat(padding + 1));
        assert_eq!(rig.conn.on_text(&one_too_many, now), Flow::Close);
    }

    #[test]
    fn a_connection_that_is_let_in_reads_what_one_that_is_not_would_be_cut_off_for() {
        // The same message, a little over the small limit: a connection that is open reads it, one that waits for its token does not.
        let now = Instant::now();
        let over = format!(r#"{{"type":"auth","token":"{}"}}"#, "x".repeat(MAX_MESSAGE_BYTES + 1));
        let mut open = rig_with(open_policy(), Limits::default(), 16);
        assert_eq!(open.conn.on_text(&over, now), Flow::Continue);
        let mut waiting = rig_with(guarded_policy(), Limits::default(), 16);
        assert_eq!(waiting.conn.on_text(&over, now), Flow::Close);
    }

    #[test]
    fn hello_says_open_session_only_when_the_program_can_open_another_session() {
        let mut rig = rig();
        rig.conn.open();
        assert_eq!(rig.said()[0]["features"], json!(["notes", "telemetry"]));
        rig.conn.open_session = true;
        rig.conn.open();
        assert_eq!(rig.said()[0]["features"], json!(["notes", "telemetry", "openSession"]));
        rig.conn.windows = true;
        rig.conn.open();
        assert_eq!(rig.said()[0]["features"], json!(["notes", "telemetry", "windows", "openSession"]), "after the others");
    }

    #[test]
    fn a_binary_frame_ends_the_connection() {
        let mut rig = rig();
        assert_eq!(rig.conn.on_binary(), Flow::Close);
        assert_eq!(rig.said()[0]["error"]["code"], "badRequest");
    }

    #[test]
    fn the_rate_limiter_does_not_go_below_empty_or_above_full() {
        let t0 = Instant::now();
        let mut limiter = RateLimiter::new(Limits { burst: 3.0, per_second: 1.0 }, t0);
        assert!(limiter.allow(t0) && limiter.allow(t0) && limiter.allow(t0));
        assert!(!limiter.allow(t0) && !limiter.allow(t0));
        assert!(!limiter.allow(t0 + Duration::from_millis(900)));
        assert!(limiter.allow(t0 + Duration::from_millis(1100)));
        let much_later = t0 + Duration::from_secs(3600);
        let allowed = (0..10).filter(|_| limiter.allow(much_later)).count();
        assert_eq!(allowed, 3);
        // Time going backwards is no time.
        assert!(!limiter.allow(t0));
    }
}
