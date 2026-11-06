//! The sockets of the web runtime, and nothing else; see `docs/web-ui.md` §3.2 and §8.
//!
//! The server runs on a thread of its own, on a single-threaded `tokio` runtime: a
//! few browsers that send a few messages and are sent a few kilobytes thirty times a
//! second are nowhere near what one core can do, and one thread keeps the rest of the
//! program, which is synchronous, from ever seeing a future. `GET /ws` is the protocol,
//! and every other GET is the interface (`web::assets`: the page at `/`, its files, or a
//! 404); a guard in front of all of it applies the `Host` / `Origin` policy to every
//! request, loopback included.
//!
//! Each WebSocket is a task that does three things for its `Connection`: it feeds it
//! the frames that arrive, it writes out what the hub has queued for the client, and
//! it watches the clock (a token has to come in time) and the stop signal. What a
//! message *means* is the `Connection`'s business and the owner's; this module is
//! the part that cannot be tested without a socket.
//!
//! A client is never waited for: a write that takes too long ends the connection, and
//! a client that the hub gave up on (its queue was full) is told and closed. Either
//! way the connection's notes are ended and its place is free again.

use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::app::protocol::{ErrorCode, ServerMessage, WireError};
use crate::note_input::NoteInput;
use crate::telemetry::Telemetry;
use crate::web::Shared;
use crate::web::assets::{self, Assets};
use crate::web::connection::{Connection, Context, Flow, Limits, MAX_OPEN_MESSAGE_BYTES, OwnerRequest};
use crate::web::hub::{Hub, Outgoing};
use crate::web::pusher;
use crate::web::security::{Policy, Refusal, Token};

/// How long to wait for a client to answer the server's close frame before the socket is
/// dropped: a socket closed while the client's messages are still unread is reset, and
/// the reset can swallow the close frame.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// How long a stopping server waits for its connections to say goodbye.
const STOP_GRACE: Duration = Duration::from_secs(3);

/// WebSocket close codes (RFC 6455 §7.4).
mod close_code {
    pub const GOING_AWAY: u16 = 1001;
    pub const POLICY: u16 = 1008;
    pub const TRY_AGAIN_LATER: u16 = 1013;
}

/// How the server listens, and how it treats its clients.
#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    /// What a client has to present first; required beyond loopback.
    pub token: Option<Token>,
    /// Other pages that may open a connection (a dev server, for instance).
    pub extra_origins: Vec<String>,
    /// How many connections at once: one more is turned away.
    pub max_clients: usize,
    /// How long a connection has to present the token.
    pub auth_timeout: Duration,
    /// How long one write to a client may take before the client is given up on.
    pub write_timeout: Duration,
    /// How often telemetry frames go out to the clients that want them.
    pub telemetry_period: Duration,
    /// How long a stopping server waits for its connections to end before it drops them.
    pub stop_grace: Duration,
    pub limits: Limits,
    /// Whether this machine can show a plugin's own window (step 112): the clients are told so in the hello (`windows`), and a page shows the button that opens one only then.
    pub windows: bool,
    /// Whether the program can open another session while it runs (step 115, `Controller::open_session`): the clients are told so in the hello (`openSession`), and a page shows its Open button only then.
    pub open_session: bool,
}

impl Config {
    pub fn new(bind: SocketAddr) -> Config {
        Config {
            bind,
            token: None,
            extra_origins: Vec::new(),
            max_clients: 32,
            auth_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(10),
            // About thirty frames a second.
            telemetry_period: Duration::from_millis(33),
            stop_grace: STOP_GRACE,
            limits: Limits::default(),
            windows: false,
            open_session: false,
        }
    }
}

/// What the server works with from the rest of the program.
pub struct Parts {
    /// The files of the interface it serves.
    pub assets: Arc<dyn Assets>,
    pub hub: Arc<Hub>,
    pub shared: Arc<Shared>,
    /// Where requests for the owner of the controller go.
    pub owner: crossbeam_channel::Sender<OwnerRequest>,
    pub notes: NoteInput,
    /// What the audio thread publishes, for the telemetry frames.
    pub telemetry: Arc<Telemetry>,
}

/// What every request and connection reads.
struct App {
    assets: Arc<dyn Assets>,
    policy: Arc<Policy>,
    hub: Arc<Hub>,
    shared: Arc<Shared>,
    owner: crossbeam_channel::Sender<OwnerRequest>,
    notes: NoteInput,
    limits: Limits,
    windows: bool,
    open_session: bool,
    auth_timeout: Duration,
    write_timeout: Duration,
    /// One permit per connection, held by its task: how many there can be, and when
    /// they have all ended.
    seats: Arc<Semaphore>,
    seat_count: u32,
    stop: watch::Receiver<bool>,
}

/// A running server. Dropping it stops it: the connections are told, and the thread
/// is joined.
pub struct Server {
    addr: SocketAddr,
    token: Option<Token>,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

/// The listener `config` asks for. Fails, without having listened, if the policy does not
/// allow the address (beyond loopback a token is needed), and if the address cannot be
/// listened on.
fn listen(config: &Config) -> anyhow::Result<std::net::TcpListener> {
    Policy::new(config.bind, config.token.clone(), config.extra_origins.clone()).map_err(anyhow::Error::msg)?;
    std::net::TcpListener::bind(config.bind).with_context(|| format!("cannot listen on {}", config.bind))
}

/// Whether `Server::start` can listen on `config` now, without keeping the port: for a program
/// that would rather say so before it has loaded a session, which can take seconds, than after.
/// (The port can be taken in between, and `start` says so then.)
pub fn can_listen(config: &Config) -> anyhow::Result<()> {
    listen(config).map(drop)
}

impl Server {
    /// Listen on `config.bind` and serve. Fails, without having listened, if the policy
    /// does not allow the address (beyond loopback a token is needed), and if the
    /// address cannot be listened on.
    pub fn start(config: Config, parts: Parts) -> anyhow::Result<Server> {
        let listener = listen(&config)?;
        let Config { token, extra_origins, max_clients, auth_timeout, write_timeout, telemetry_period, stop_grace, limits, windows, open_session, .. } = config;
        listener.set_nonblocking(true).context("cannot make the listener non-blocking")?;
        let addr = listener.local_addr().context("cannot tell where the server listens")?;
        // The port may have been left to the system, and the policy has to know it.
        let policy = Arc::new(Policy::new(addr, token.clone(), extra_origins).map_err(anyhow::Error::msg)?);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("cannot start the web server's runtime")?;
        let listener = {
            let _entered = runtime.enter();
            tokio::net::TcpListener::from_std(listener).context("cannot listen with the runtime")?
        };

        let (stop_tx, stop_rx) = watch::channel(false);
        let seat_count = max_clients.clamp(1, u32::MAX as usize) as u32;
        // The pusher is a task of the runtime like the connections, and starts when it runs.
        runtime.spawn(pusher::run(parts.hub.clone(), parts.shared.clone(), parts.telemetry, parts.notes.held(), telemetry_period, stop_rx.clone()));
        let app = Arc::new(App {
            assets: parts.assets,
            policy,
            hub: parts.hub,
            shared: parts.shared,
            owner: parts.owner,
            notes: parts.notes,
            limits,
            windows,
            open_session,
            auth_timeout,
            write_timeout,
            seats: Arc::new(Semaphore::new(seat_count as usize)),
            seat_count,
            stop: stop_rx.clone(),
        });
        let thread = std::thread::Builder::new()
            .name("tang-web".into())
            .spawn(move || run(runtime, listener, app, stop_rx, stop_grace))
            .context("cannot start the web server's thread")?;
        log::info!("Web server listening on http://{addr}/");
        Ok(Server { addr, token, stop: stop_tx, thread: Some(thread) })
    }

    /// Where the server listens (the port is the one the system chose, if it was asked
    /// for 0).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The address to open in a browser: with the token in the fragment, which is
    /// not sent to a server and does not reach logs or `Referer`, when there is one.
    pub fn url(&self) -> String {
        match &self.token {
            Some(token) => format!("http://{}/#token={}", self.addr, token.as_str()),
            None => format!("http://{}/", self.addr),
        }
    }
}

impl Server {
    /// Stop serving: the connections are told, and the thread is joined. Does nothing
    /// the second time.
    pub fn stop(&mut self) {
        self.stop.send_replace(true);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                log::warn!("The web server's thread panicked");
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The server thread: serve until told to stop, then give the connections a moment to
/// say goodbye, and drop the ones that have not (a client that began a request and never
/// finishes it would keep the server from ending for ever).
fn run(runtime: tokio::runtime::Runtime, listener: tokio::net::TcpListener, app: Arc<App>, mut stop: watch::Receiver<bool>, stop_grace: Duration) {
    runtime.block_on(async move {
        let seats = app.seats.clone();
        let seat_count = app.seat_count;
        let router = Router::new()
            .route("/ws", get(upgrade))
            .fallback(get(file))
            .layer(middleware::from_fn_with_state(app.clone(), guard))
            .with_state(app);
        let mut told = stop.clone();
        let shutdown = async move {
            // (A sender that is gone is a stop too.)
            let _ = stop.wait_for(|stopped| *stopped).await;
        };
        let served = async {
            // Ends when the connections of HTTP have.
            if let Err(e) = axum::serve(listener, router).with_graceful_shutdown(shutdown).await {
                log::warn!("Web server: {e}");
            }
            // The WebSockets are tasks of their own, and each ends when the stop reaches it. All
            // the seats back means all of them have.
            let _ = seats.acquire_many(seat_count).await;
        };
        let out_of_time = async move {
            let _ = told.wait_for(|stopped| *stopped).await;
            tokio::time::sleep(stop_grace).await;
        };
        tokio::select! {
            () = served => {}
            // (The connections that are left are dropped with the runtime.)
            () = out_of_time => log::warn!("Web server: a connection did not end within {stop_grace:?} of the stop; dropping it"),
        }
    });
}

// -- HTTP --

/// Applies the `Host` / `Origin` policy to every request, and puts the headers that
/// keep a browser from doing more with a response than it should on every answer.
async fn guard(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let mut response = match check(&app.policy, request.headers()) {
        Ok(()) => next.run(request).await,
        Err(Refusal::Host) => (StatusCode::FORBIDDEN, "Tang does not answer to that Host.\n").into_response(),
        Err(Refusal::Origin) => (StatusCode::FORBIDDEN, "Tang does not talk to pages of that origin.\n").into_response(),
    };
    let headers = response.headers_mut();
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    // (A file that is kept says so itself.)
    headers.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    response
}

/// The policy's verdict on the headers of a request. A header that is there but is not
/// text, or comes twice, is not to be guessed at: it is refused.
fn check(policy: &Policy, headers: &HeaderMap) -> Result<(), Refusal> {
    policy.check(single(headers, header::HOST, Refusal::Host)?, single(headers, header::ORIGIN, Refusal::Origin)?)
}

fn single(headers: &HeaderMap, name: HeaderName, refusal: Refusal) -> Result<Option<&str>, Refusal> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => Ok(None),
        (Some(value), None) => value.to_str().map(Some).map_err(|_| refusal),
        (Some(_), Some(_)) => Err(refusal),
    }
}

/// `GET` of anything but `/ws`: `/` is the page of the interface (or what says how to
/// build it), and the rest a file of the interface or nothing.
async fn file(State(app): State<Arc<App>>, uri: Uri) -> Response {
    assets::serve(&*app.assets, uri.path())
}

/// `GET /ws`: take a seat, if there is one, and upgrade.
async fn upgrade(State(app): State<Arc<App>>, ws: WebSocketUpgrade) -> Response {
    let Ok(seat) = app.seats.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Tang has all the connections it takes: close another tab.\n").into_response();
    };
    // (The transport reads a frame whole, so it takes what the connections that were let in may send: `Connection::on_text` cuts one that was not at `MAX_MESSAGE_BYTES`.)
    ws.max_message_size(MAX_OPEN_MESSAGE_BYTES)
        .max_frame_size(MAX_OPEN_MESSAGE_BYTES)
        .on_upgrade(move |socket| serve_socket(app, socket, seat))
}

// -- WebSocket --

/// How a connection came to its end.
enum End {
    /// The client left or the socket broke: nobody to tell.
    Gone,
    /// The connection has said why (a `Closing` is queued) and wants to be closed.
    Told,
    /// The hub gave up on the client: its queue was full.
    Evicted,
    /// The token did not come in time.
    NoToken,
    /// The server is stopping.
    Stopping,
}

async fn serve_socket(app: Arc<App>, mut socket: WebSocket, _seat: OwnedSemaphorePermit) {
    let (id, mut queue) = app.hub.connect();
    let context = Context {
        hub: app.hub.clone(),
        policy: app.policy.clone(),
        shared: app.shared.clone(),
        owner: app.owner.clone(),
        notes: app.notes.clone(),
        limits: app.limits,
        windows: app.windows,
        open_session: app.open_session,
    };
    // (A connection that is dropped is closed, so the notes it holds end on every path out of here.)
    let mut conn = Connection::new(id, context, Instant::now());
    conn.open();
    log::debug!("Web client {id:?} connected");

    let mut stop = app.stop.clone();
    let auth_timer = tokio::time::sleep(app.auth_timeout);
    tokio::pin!(auth_timer);

    let end = loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if conn.on_text(&text, Instant::now()) == Flow::Close {
                        break End::Told;
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    if conn.on_binary() == Flow::Close {
                        break End::Told;
                    }
                }
                // (Pings are answered by the socket itself.)
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break End::Gone,
            },
            outgoing = queue.recv() => match outgoing {
                Some(frame) => {
                    if !write(&mut socket, frame, app.write_timeout).await {
                        break End::Gone;
                    }
                }
                None => break End::Evicted,
            },
            _ = &mut auth_timer, if !conn.is_open() => break End::NoToken,
            // (The guard `wait_for` returns is not `Send`, so it must not outlive this future.)
            _ = async { let _ = stop.wait_for(|stopped| *stopped).await; } => break End::Stopping,
        }
    };

    conn.close();
    log::debug!("Web client {id:?} done");
    match end {
        End::Gone => {}
        End::Told => {
            // The reason is the last thing the connection queued.
            while let Ok(frame) = queue.try_recv() {
                if !write(&mut socket, frame, app.write_timeout).await {
                    return;
                }
            }
            finish(&mut socket, close_code::POLICY, "closing").await;
        }
        End::Evicted => {
            let error = WireError::new(ErrorCode::Overloaded, "you fell behind and were dropped: reconnect to start over");
            if say(&mut socket, &ServerMessage::Closing { error }, app.write_timeout).await {
                finish(&mut socket, close_code::TRY_AGAIN_LATER, "overloaded").await;
            }
        }
        End::NoToken => {
            let error = WireError::new(ErrorCode::Unauthorized, "the token did not come in time");
            if say(&mut socket, &ServerMessage::Closing { error }, app.write_timeout).await {
                finish(&mut socket, close_code::POLICY, "unauthorized").await;
            }
        }
        End::Stopping => finish(&mut socket, close_code::GOING_AWAY, "the server is stopping").await,
    }
}

/// Write a frame the hub queued. False if the client is gone or too slow.
async fn write(socket: &mut WebSocket, frame: Outgoing, within: Duration) -> bool {
    let message = match frame {
        Outgoing::Text(text) => Message::Text(Utf8Bytes::from(&*text)),
        Outgoing::Binary(bytes) => Message::Binary(Bytes::copy_from_slice(&bytes)),
    };
    send(socket, message, within).await
}

async fn say(socket: &mut WebSocket, message: &ServerMessage, within: Duration) -> bool {
    let json = serde_json::to_string(message).expect("a message is serializable");
    send(socket, Message::Text(json.into()), within).await
}

async fn send(socket: &mut WebSocket, message: Message, within: Duration) -> bool {
    matches!(tokio::time::timeout(within, socket.send(message)).await, Ok(Ok(())))
}

/// Close the WebSocket properly: a close frame, and a moment for the client to answer it.
async fn finish(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = Message::Close(Some(CloseFrame { code, reason: reason.into() }));
    if send(socket, frame, CLOSE_GRACE).await {
        let _ = tokio::time::timeout(CLOSE_GRACE, async { while socket.recv().await.is_some() {} }).await;
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio_tungstenite::MaybeTlsStream;
    use tokio_tungstenite::tungstenite::{self, Message as ClientMessage};

    use super::*;
    use crate::app::model::SessionModel;
    use crate::audio::MidiEvent;
    use crate::web::connection::MAX_MESSAGE_BYTES;
    use crate::held_notes::HeldNotes;
    use crate::piano_filter::{PianoMode, PianoFilter};
    use crate::web::assets::Files;
    use crate::scale::ScaleSetting;
    use crate::web::testclient::{self, Client, header, recv, said, tell, until_closed, wait_until};

    struct Rig {
        server: Server,
        hub: Arc<Hub>,
        owner: crossbeam_channel::Receiver<OwnerRequest>,
        midi: crossbeam_channel::Receiver<MidiEvent>,
        held: Arc<HeldNotes>,
        telemetry: Arc<Telemetry>,
    }

    fn rig() -> Rig {
        rig_with(|_| {})
    }

    fn rig_with(tune: impl FnOnce(&mut Config)) -> Rig {
        rig_serving(tune, Files::interface())
    }

    fn rig_serving(tune: impl FnOnce(&mut Config), files: Files) -> Rig {
        let hub = Arc::new(Hub::new(16));
        let (owner_tx, owner) = crossbeam_channel::bounded(64);
        let (midi_tx, midi) = crossbeam_channel::bounded(256);
        let held = Arc::new(HeldNotes::new());
        let filter = Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight));
        let notes = NoteInput::new(midi_tx, held.clone(), filter);
        let shared = Arc::new(Shared::new(3, SessionModel::default().telemetry_layout()));
        let telemetry = Arc::new(Telemetry::new());
        let mut config = Config::new("127.0.0.1:0".parse().unwrap());
        tune(&mut config);
        let assets = Arc::new(files);
        let server = Server::start(config, Parts { assets, hub: hub.clone(), shared, owner: owner_tx, notes, telemetry: telemetry.clone() }).unwrap();
        Rig { server, hub, owner, midi, held, telemetry }
    }

    fn secret(config: &mut Config) {
        config.token = Some(Token::new("secret"));
    }

    impl Rig {
        fn port(&self) -> u16 {
            self.server.addr().port()
        }

        /// What the owner has been asked, waiting a moment for it.
        fn asked(&self) -> OwnerRequest {
            self.owner.recv_timeout(Duration::from_secs(5)).expect("the owner was not asked anything in five seconds")
        }

        fn played(&self) -> MidiEvent {
            self.midi.recv_timeout(Duration::from_secs(5)).expect("no note arrived in five seconds")
        }
    }

    async fn connect_as(rig: &Rig, headers: &[(&'static str, String)]) -> Result<Client, tungstenite::Error> {
        testclient::connect_as(rig.server.addr(), headers).await
    }

    async fn connect(rig: &Rig) -> Client {
        testclient::connect(rig.server.addr()).await
    }

    fn get(rig: &Rig, host: Option<&str>, method: &str, path: &str, extra: &[(&str, &str)]) -> (u16, Vec<(String, String)>, String) {
        testclient::get(rig.server.addr(), host, method, path, extra)
    }

    fn own_host(rig: &Rig) -> String {
        format!("127.0.0.1:{}", rig.port())
    }

    // -- The page --

    #[test]
    fn the_page_is_served_and_cannot_be_framed_or_sniffed() {
        let rig = rig();
        let (status, headers, body) = get(&rig, Some(&own_host(&rig)), "GET", "/", &[]);
        assert_eq!(status, 200);
        assert!(header(&headers, "content-type").unwrap().starts_with("text/html"));
        assert_eq!(body, "<!doctype html><title>Tang</title>");
        assert_eq!(header(&headers, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&headers, "x-frame-options"), Some("DENY"));
        assert_eq!(header(&headers, "referrer-policy"), Some("no-referrer"));
        assert_eq!(header(&headers, "cache-control"), Some("no-cache"), "the page says how it is kept, and the guard leaves it");
        let policy = header(&headers, "content-security-policy").unwrap();
        assert!(policy.contains("default-src 'self'") && policy.contains("frame-ancestors 'none'"), "{policy}");
    }

    #[test]
    fn the_files_of_the_interface_are_served_with_the_guards_headers_and_keep_their_own_caching() {
        let rig = rig();
        let host = own_host(&rig);
        let (status, headers, body) = get(&rig, Some(&host), "GET", "/assets/index-abc123.js", &[]);
        assert_eq!((status, body.as_str()), (200, "console.log(1)"));
        assert_eq!(header(&headers, "content-type"), Some("text/javascript; charset=utf-8"));
        assert_eq!(header(&headers, "cache-control"), Some("public, max-age=31536000, immutable"));
        assert_eq!(header(&headers, "x-content-type-options"), Some("nosniff"));
        assert_eq!(header(&headers, "x-frame-options"), Some("DENY"));
        // What is not a file gets the guard's own: not kept.
        let (status, headers, _) = get(&rig, Some(&host), "GET", "/assets/missing.js", &[]);
        assert_eq!(status, 404);
        assert_eq!(header(&headers, "cache-control"), Some("no-store"));
    }

    #[test]
    fn a_program_without_the_interface_says_how_to_build_it() {
        let rig = rig_serving(|_| {}, Files::unbuilt());
        let (status, headers, body) = get(&rig, Some(&own_host(&rig)), "GET", "/", &[]);
        assert_eq!(status, 200);
        assert!(body.contains("npm run build"), "{body}");
        assert!(header(&headers, "content-security-policy").unwrap().contains("default-src 'none'"));
        assert_eq!(header(&headers, "x-frame-options"), Some("DENY"));
    }

    #[test]
    fn nothing_else_is_served() {
        let rig = rig();
        let host = own_host(&rig);
        assert_eq!(get(&rig, Some(&host), "GET", "/api/session", &[]).0, 404);
        assert_eq!(get(&rig, Some(&host), "GET", "/missing.js", &[]).0, 404);
        // Not what the folder keeps hidden, nor what is outside it.
        for path in ["/.gitkeep", "/.hidden/secret.txt", "/../Cargo.toml", "/assets/../../Cargo.toml"] {
            assert_eq!(get(&rig, Some(&host), "GET", path, &[]).0, 404, "{path}");
        }
        assert_eq!(get(&rig, Some(&host), "POST", "/", &[]).0, 405);
        assert_eq!(get(&rig, Some(&host), "POST", "/assets/index-abc123.js", &[]).0, 405);
        assert_eq!(get(&rig, Some(&host), "GET", "/ws", &[]).0 / 100, 4, "/ws without an upgrade is not served either");
    }

    #[test]
    fn a_request_that_is_not_for_this_server_is_refused_even_for_the_page() {
        let rig = rig();
        let port = rig.port();
        let refused = |host: Option<&str>, extra: &[(&str, &str)]| get(&rig, host, "GET", "/", extra);
        // DNS rebinding: the page of the attacker's name reaches 127.0.0.1 with its own Host.
        let (status, headers, body) = refused(Some(&format!("evil.example:{port}")), &[]);
        assert_eq!(status, 403);
        assert_eq!(header(&headers, "x-frame-options"), Some("DENY"), "a refusal carries the headers too");
        assert!(!body.contains("evil.example"), "and does not repeat what it was sent");
        assert_eq!(refused(None, &[]).0, 403, "a request without a Host is not one");
        // A page of another origin.
        assert_eq!(refused(Some(&own_host(&rig)), &[("Origin", "http://evil.example")]).0, 403);
        assert_eq!(refused(Some(&own_host(&rig)), &[("Origin", "null")]).0, 403);
        // The same page, by either name of this machine, is fine, and so is a request with no Origin.
        assert_eq!(refused(Some(&own_host(&rig)), &[("Origin", &format!("http://localhost:{port}"))]).0, 200);
        assert_eq!(refused(Some(&format!("localhost:{port}")), &[]).0, 200);
    }

    #[test]
    fn a_header_that_is_not_text_or_comes_twice_is_not_guessed_at() {
        let policy = Policy::new("127.0.0.1:8080".parse().unwrap(), None, vec![]).unwrap();
        let mut headers = HeaderMap::new();
        assert_eq!(check(&policy, &headers), Err(Refusal::Host), "no Host");
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8080"));
        assert_eq!(check(&policy, &headers), Ok(()));
        headers.append(header::HOST, HeaderValue::from_static("127.0.0.1:8080"));
        assert_eq!(check(&policy, &headers), Err(Refusal::Host), "two Hosts");
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8080"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:8080"));
        assert_eq!(check(&policy, &headers), Ok(()));
        headers.append(header::ORIGIN, HeaderValue::from_static("http://127.0.0.1:8080"));
        assert_eq!(check(&policy, &headers), Err(Refusal::Origin), "two Origins, even if they agree");
        headers.insert(header::ORIGIN, HeaderValue::from_bytes(b"http://127.0.0.1:8080\xff").unwrap());
        assert_eq!(check(&policy, &headers), Err(Refusal::Origin), "an Origin that is not text is not 'no Origin'");
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://evil.example"));
        assert_eq!(check(&policy, &headers), Err(Refusal::Origin));
    }

    // -- WebSocket: hello, origins, seats --

    #[tokio::test]
    async fn a_client_is_welcomed_with_hello() {
        let rig = rig();
        let mut client = connect(&rig).await;
        assert_eq!(
            said(&mut client).await,
            json!({"type": "hello", "protocol": 1, "rev": 3, "features": ["notes", "telemetry"], "needsToken": false})
        );
    }

    #[tokio::test]
    async fn a_websocket_from_another_origin_or_host_is_refused() {
        let rig = rig();
        let port = rig.port();
        let refused = |result: Result<Client, tungstenite::Error>| match result {
            Err(tungstenite::Error::Http(response)) => response.status().as_u16(),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("connected"),
        };
        assert_eq!(refused(connect_as(&rig, &[("origin", "http://evil.example".into())]).await), 403);
        assert_eq!(refused(connect_as(&rig, &[("origin", format!("http://evil.example:{port}"))]).await), 403);
        assert_eq!(refused(connect_as(&rig, &[("host", format!("evil.example:{port}"))]).await), 403, "DNS rebinding");
        assert_eq!(rig.hub.client_count(), 0, "nothing was registered for them");

        // The page's own origin (by either name) and a client that is not a page are let in.
        for headers in [
            vec![("origin", format!("http://127.0.0.1:{port}"))],
            vec![("origin", format!("http://localhost:{port}")), ("host", format!("localhost:{port}"))],
            vec![],
        ] {
            let mut client = connect_as(&rig, &headers).await.expect("let in");
            assert_eq!(said(&mut client).await["type"], "hello");
        }
    }

    #[tokio::test]
    async fn an_origin_that_was_allowed_may_connect() {
        let rig = rig_with(|config| config.extra_origins = vec!["http://localhost:5173".into()]);
        let mut client = connect_as(&rig, &[("origin", "http://localhost:5173".into())]).await.expect("let in");
        assert_eq!(said(&mut client).await["type"], "hello");
        assert!(connect_as(&rig, &[("origin", "http://localhost:5174".into())]).await.is_err());
    }

    #[tokio::test]
    async fn only_as_many_connections_as_allowed_and_a_seat_is_free_again_when_one_leaves() {
        let rig = rig_with(|config| config.max_clients = 2);
        let mut first = connect(&rig).await;
        let _second = connect(&rig).await;
        match connect_as(&rig, &[]).await {
            Err(tungstenite::Error::Http(response)) => assert_eq!(response.status().as_u16(), 503),
            other => panic!("{:?}", other.map(|_| ())),
        }
        said(&mut first).await;
        first.close(None).await.unwrap();
        drop(first);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if connect_as(&rig, &[]).await.is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "the seat did not come free");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // -- WebSocket: messages --

    #[tokio::test]
    async fn what_a_client_sends_reaches_the_owner_and_what_the_hub_queues_reaches_the_client() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "subscribe"})).await;
        let OwnerRequest::Subscribe { client: id } = rig.asked() else { panic!("not a subscribe") };

        // The owner answers: it subscribes the client and sends it things, in order.
        assert!(rig.hub.subscribe(id));
        assert!(rig.hub.send(id, Outgoing::Text(r#"{"type":"ack","reqId":0,"rev":0}"#.into())));
        assert!(rig.hub.broadcast(&Outgoing::Text(r#"{"type":"ack","reqId":1,"rev":1}"#.into())).is_empty());
        rig.hub.send_telemetry(crate::app::protocol::TelemetryChannel::Meters, &Outgoing::Binary(vec![9, 9].into()));
        assert_eq!(said(&mut client).await["reqId"], 0);
        assert_eq!(said(&mut client).await["reqId"], 1);

        tell(&mut client, json!({"type": "request", "reqId": 5, "intent": {"type": "setParam", "node": 3, "param": 1, "value": 0.5}})).await;
        match rig.asked() {
            OwnerRequest::Apply { client: from, req_id, intent } => {
                assert_eq!((from, req_id), (id, 5));
                assert_eq!(format!("{intent:?}"), "SetParam { node: NodeId(3), param: 1, value: 0.5 }");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn telemetry_arrives_as_binary_frames_for_as_long_as_the_client_asks() {
        use crate::telemetry::MASTER;
        let rig = rig_with(|config| config.telemetry_period = Duration::from_millis(10));
        let mut client = connect(&rig).await;
        said(&mut client).await;
        rig.telemetry.store(MASTER.peak, 0.5);
        for _ in 0..3 {
            rig.telemetry.bump(MASTER.clips);
        }
        tell(&mut client, json!({"type": "telemetry", "channels": ["meters"]})).await;
        // The master's meter: its peak, then the number of buffers that clipped, and how far its limiter has brought the sound down.
        match recv(&mut client).await {
            Some(ClientMessage::Binary(bytes)) => {
                assert_eq!(&bytes[..4], [0, 0, 3, 0], "the meters, no flags, three values");
                assert_eq!(f32::from_le_bytes(bytes[8..12].try_into().unwrap()), 0.5);
                assert_eq!(f32::from_le_bytes(bytes[12..16].try_into().unwrap()), 3.0);
            }
            other => panic!("{other:?}"),
        }

        // None any more once it asks for none: what is already on its way may still come.
        tell(&mut client, json!({"type": "telemetry", "channels": []})).await;
        wait_until("the hub was not told", || !rig.hub.wanted(crate::app::protocol::TelemetryChannel::Meters)).await;
        while tokio::time::timeout(Duration::from_millis(100), client.next()).await.is_ok() {}
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(tokio::time::timeout(Duration::from_millis(60), client.next()).await.is_err(), "silence");
    }

    #[tokio::test]
    async fn what_is_not_a_message_ends_the_connection_with_the_reason_and_a_close_frame() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        client.send(ClientMessage::text("this is not json")).await.unwrap();
        let (texts, code) = until_closed(&mut client).await;
        assert_eq!(texts.len(), 1);
        assert_eq!((texts[0]["type"].clone(), texts[0]["error"]["code"].clone()), (json!("closing"), json!("badRequest")));
        assert_eq!(code, Some(1008));
        assert!(rig.owner.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_binary_frame_ends_the_connection() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        client.send(ClientMessage::binary(vec![1, 2, 3])).await.unwrap();
        let (texts, code) = until_closed(&mut client).await;
        assert_eq!(texts[0]["error"]["code"], "badRequest");
        assert_eq!(code, Some(1008));
    }

    #[tokio::test]
    async fn a_message_over_the_limit_is_cut_off_by_the_socket_before_the_connection_sees_it() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        let long = json!({"type": "auth", "token": "x".repeat(MAX_OPEN_MESSAGE_BYTES)}).to_string();
        // (The server may reset the connection while this is still being written.)
        let _ = client.send(ClientMessage::text(long)).await;
        let (texts, _code) = until_closed(&mut client).await;
        // The connection would answer a long message with its reason; the socket just ends it.
        assert!(texts.is_empty(), "{texts:?}");
        wait_until("the connection was not closed", || rig.hub.client_count() == 0).await;
    }

    #[tokio::test]
    async fn a_message_over_the_small_limit_reaches_a_connection_that_is_open_and_the_connection_goes_on() {
        // (A session's file goes through as one message: a hundred kilobytes is far over the small limit and far under the other.)
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        let long = json!({"type": "auth", "token": "x".repeat(MAX_MESSAGE_BYTES + 36 * 1024)}).to_string();
        client.send(ClientMessage::text(long)).await.unwrap();
        // The connection is still there: what comes next reaches the owner of the controller.
        tell(&mut client, json!({"type": "subscribe"})).await;
        assert!(matches!(rig.asked(), OwnerRequest::Subscribe { .. }), "the connection went on after the long message");
    }

    #[tokio::test]
    async fn a_message_sent_in_pieces_is_cut_off_at_the_limit_as_a_whole() {
        use tungstenite::protocol::frame::Frame;
        use tungstenite::protocol::frame::coding::{Data, OpCode};
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        // Each piece is within the limit; the two together are not.
        let piece = vec![b' '; MAX_OPEN_MESSAGE_BYTES / 2 + 1];
        let first = Frame::message(piece.clone(), OpCode::Data(Data::Text), false);
        let last = Frame::message(piece, OpCode::Data(Data::Continue), true);
        client.send(ClientMessage::Frame(first)).await.unwrap();
        let _ = client.send(ClientMessage::Frame(last)).await;
        let (texts, _code) = until_closed(&mut client).await;
        assert!(texts.is_empty(), "{texts:?}");
        wait_until("the connection was not closed", || rig.hub.client_count() == 0).await;
    }

    #[test]
    fn a_frame_that_announces_more_than_the_limit_is_refused_before_its_payload_is_read() {
        let rig = rig();
        let mut stream = std::net::TcpStream::connect(rig.server.addr()).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let handshake = format!(
            "GET /ws HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            own_host(&rig)
        );
        stream.write_all(handshake.as_bytes()).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        assert!(head.starts_with(b"HTTP/1.1 101"), "{}", String::from_utf8_lossy(&head));

        // A masked text frame whose header says 10 MiB follows, and then only a few bytes of them.
        let mut frame = vec![0x81, 0x80 | 127];
        frame.extend_from_slice(&(10u64 << 20).to_be_bytes());
        frame.extend_from_slice(&[1, 2, 3, 4]);
        frame.extend_from_slice(&[b'x'; 16]);
        stream.write_all(&frame).unwrap();

        // The server does not wait for the rest: whatever it says, it then closes (Hello and the
        // close frame may come first). Waiting for 10 MiB would be a read that times out.
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    panic!("the server is waiting for the whole frame")
                }
                Err(_) => break, // reset
            }
        }
    }

    // -- WebSocket: the token --

    #[tokio::test]
    async fn with_a_token_the_first_message_must_be_it() {
        let rig = rig_with(secret);
        let mut early = connect(&rig).await;
        assert_eq!(said(&mut early).await["needsToken"], true);
        tell(&mut early, json!({"type": "subscribe"})).await;
        let (texts, code) = until_closed(&mut early).await;
        assert_eq!(texts[0]["error"]["code"], "unauthorized");
        assert_eq!(code, Some(1008));

        let mut wrong = connect(&rig).await;
        said(&mut wrong).await;
        tell(&mut wrong, json!({"type": "auth", "token": "Secret"})).await;
        assert_eq!(until_closed(&mut wrong).await.0[0]["error"]["code"], "unauthorized");
        assert!(rig.owner.try_recv().is_err(), "nothing was processed");

        let mut right = connect(&rig).await;
        said(&mut right).await;
        tell(&mut right, json!({"type": "auth", "token": "secret"})).await;
        tell(&mut right, json!({"type": "subscribe"})).await;
        assert!(matches!(rig.asked(), OwnerRequest::Subscribe { .. }));
    }

    #[tokio::test]
    async fn a_connection_that_does_not_give_the_token_in_time_is_closed() {
        let rig = rig_with(|config| {
            secret(config);
            config.auth_timeout = Duration::from_millis(200);
        });
        let mut client = connect(&rig).await;
        assert_eq!(said(&mut client).await["type"], "hello");
        let started = Instant::now();
        let (texts, code) = until_closed(&mut client).await;
        assert_eq!(texts[0]["error"]["code"], "unauthorized");
        assert_eq!(code, Some(1008));
        assert!(started.elapsed() >= Duration::from_millis(150), "not before it was due");
    }

    #[tokio::test]
    async fn a_connection_that_gave_the_token_is_not_closed_by_the_timer() {
        let rig = rig_with(|config| {
            secret(config);
            config.auth_timeout = Duration::from_millis(100);
        });
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "auth", "token": "secret"})).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        tell(&mut client, json!({"type": "subscribe"})).await;
        assert!(matches!(rig.asked(), OwnerRequest::Subscribe { .. }), "still there after the time was up");
    }

    #[tokio::test]
    async fn a_server_without_a_token_has_no_timer() {
        let rig = rig_with(|config| config.auth_timeout = Duration::from_millis(50));
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        tell(&mut client, json!({"type": "subscribe"})).await;
        assert!(matches!(rig.asked(), OwnerRequest::Subscribe { .. }));
    }

    // -- WebSocket: notes --

    #[tokio::test]
    async fn a_client_that_leaves_with_a_note_held_does_not_leave_it_sounding() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
        assert_eq!(rig.played(), (0, [0x90, 60, 100]));
        assert_eq!(rig.held.held(), [60]);
        // The tab is closed: no note off, no close frame, just gone.
        drop(client);
        assert_eq!(rig.played(), (0, [0x80, 60, 0]));
        assert!(rig.held.held().is_empty());
    }

    #[tokio::test]
    async fn a_connection_that_ends_in_error_ends_its_notes_too() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "noteOn", "note": 64, "velocity": 100})).await;
        assert_eq!(rig.played(), (0, [0x90, 64, 100]));
        client.send(ClientMessage::text("garbage")).await.unwrap();
        // The note ends at once, not after the client has answered the close frame (this client
        // does not read, so it never will, and the server gives up on it after a second).
        let ended = rig.midi.recv_timeout(Duration::from_millis(500)).expect("the note was still sounding");
        assert_eq!(ended, (0, [0x80, 64, 0]));
        until_closed(&mut client).await;
    }

    // -- WebSocket: clients that fall behind, and the end of the server --

    #[tokio::test]
    async fn a_client_the_hub_gave_up_on_is_told_and_closed() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "subscribe"})).await;
        let OwnerRequest::Subscribe { client: id } = rig.asked() else { panic!("not a subscribe") };
        // What the hub does to a client whose queue is full, as far as the connection can tell.
        rig.hub.disconnect(id);
        let (texts, code) = until_closed(&mut client).await;
        assert_eq!(texts[0]["error"]["code"], "overloaded");
        assert_eq!(code, Some(1013));
    }

    #[tokio::test]
    async fn a_client_that_stops_reading_is_given_up_on_and_its_seat_comes_free() {
        let rig = rig_with(|config| {
            config.max_clients = 1;
            config.write_timeout = Duration::from_millis(300);
        });
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "subscribe"})).await;
        let OwnerRequest::Subscribe { client: id } = rig.asked() else { panic!("not a subscribe") };
        rig.hub.subscribe(id);
        // Megabytes more than any socket buffer holds, to a client that reads nothing.
        let big = Outgoing::Binary(vec![0u8; 1 << 20].into());
        for _ in 0..256 {
            if !rig.hub.send(id, big.clone()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let _second = loop {
            match connect_as(&rig, &[]).await {
                Ok(second) => break second,
                Err(_) => {
                    assert!(Instant::now() < deadline, "the stuck connection kept its seat");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        };
        drop(client);
    }

    #[tokio::test]
    async fn stopping_the_server_says_goodbye_and_ends_the_thread() {
        let rig = rig();
        let mut client = connect(&rig).await;
        said(&mut client).await;
        tell(&mut client, json!({"type": "noteOn", "note": 67, "velocity": 100})).await;
        assert_eq!(rig.played(), (0, [0x90, 67, 100]));
        let Rig { server, midi, .. } = rig;
        let stopping = tokio::task::spawn_blocking(move || drop(server));
        let (_texts, code) = until_closed(&mut client).await;
        assert_eq!(code, Some(1001));
        stopping.await.unwrap();
        assert_eq!(midi.recv_timeout(Duration::from_secs(5)).unwrap(), (0, [0x80, 67, 0]), "the note ended with the server");
        assert!(std::net::TcpStream::connect(client_addr_of(&client)).is_err(), "nobody listens any more");
    }

    #[test]
    fn a_connection_that_never_finishes_its_request_does_not_keep_the_server_from_stopping() {
        let rig = rig_with(|config| config.stop_grace = Duration::from_millis(200));
        // The head of a request, without its end: the server waits for the rest for ever.
        let mut stuck = std::net::TcpStream::connect(rig.server.addr()).unwrap();
        stuck.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let Rig { server, .. } = rig;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(server);
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(2)).is_ok(),
            "the server was still stopping two seconds after it was told to, with a grace of 200 ms"
        );
        drop(stuck);
    }

    #[test]
    fn the_grace_of_a_stopping_server_counts_from_the_stop_not_from_the_start() {
        let rig = rig_with(|config| config.stop_grace = Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(400));
        let (status, ..) = get(&rig, Some(&own_host(&rig)), "GET", "/", &[]);
        assert_eq!(status, 200, "a server that has run for longer than its grace is still serving");
    }

    fn client_addr_of(client: &Client) -> SocketAddr {
        match client.get_ref() {
            MaybeTlsStream::Plain(stream) => stream.peer_addr().unwrap(),
            _ => unreachable!("the tests do not use TLS"),
        }
    }

    // -- Starting --

    #[test]
    fn the_server_does_not_start_on_an_address_that_is_taken() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let error = Server::start(Config::new(taken.local_addr().unwrap()), parts()).err().expect("a refusal");
        assert!(error.to_string().contains("cannot listen"), "{error}");
    }

    #[test]
    fn the_server_does_not_listen_beyond_loopback_without_a_token() {
        // On an address that is taken, so that the policy is what speaks first, not the bind.
        let taken = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let error = Server::start(Config::new(taken.local_addr().unwrap()), parts()).err().expect("a refusal");
        assert!(error.to_string().contains("needs a token"), "{error}");
    }

    // -- Asking before starting --

    #[test]
    fn an_address_that_is_free_can_be_listened_on_and_is_still_free_afterwards() {
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        can_listen(&Config::new(free)).expect("a free address");
        std::net::TcpListener::bind(free).expect("asking does not keep the port");
    }

    #[test]
    fn asking_refuses_what_start_refuses_in_the_same_words() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let config = Config::new(taken.local_addr().unwrap());
        let asked = can_listen(&config).expect_err("a refusal");
        let by_start = Server::start(config, parts()).err().expect("a refusal");
        assert_eq!(asked.to_string(), by_start.to_string());
        assert!(asked.to_string().contains("cannot listen"), "{asked}");
        // The policy as well, on an address that is taken, so that it is what speaks first.
        let loose = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let error = can_listen(&Config::new(loose.local_addr().unwrap())).expect_err("a refusal");
        assert!(error.to_string().contains("needs a token"), "{error}");
    }

    #[test]
    fn the_address_to_open_carries_the_token_in_the_fragment() {
        let guarded = rig_with(secret);
        assert_eq!(guarded.server.url(), format!("http://127.0.0.1:{}/#token=secret", guarded.port()));
        let open = rig();
        assert_eq!(open.server.url(), format!("http://127.0.0.1:{}/", open.port()));
    }

    fn parts() -> Parts {
        let (owner, _) = crossbeam_channel::bounded(1);
        let (midi, _) = crossbeam_channel::bounded(1);
        let notes = NoteInput::new(
            midi,
            Arc::new(HeldNotes::new()),
            Arc::new(PianoFilter::new(ScaleSetting::default(), PianoMode::Highlight)),
        );
        Parts {
            assets: Arc::new(Files::interface()),
            hub: Arc::new(Hub::new(4)),
            shared: Arc::new(Shared::new(0, SessionModel::default().telemetry_layout())),
            owner,
            notes,
            telemetry: Arc::new(Telemetry::new()),
        }
    }
}
