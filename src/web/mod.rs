//! The web runtime (feature `web`): the part of Tang that a browser talks to; see
//! `docs/web-ui.md` §3.2, §7 and §8.
//!
//! The controller, the audio thread and the MIDI threads stay synchronous and
//! never see a socket. The runtime lives on threads of its own (an async server),
//! and the two sides meet in a few places: requests go to the owner of the
//! controller over a channel, what the controller applies and the telemetry come
//! back through the hub to every client's bounded queue, and notes go straight to
//! the audio thread through `NoteInput`.

use std::net::SocketAddr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;

use crate::app::controller::Controller;
use crate::app::layout::TelemetryLayout;
use crate::cli::WebArgs;
use crate::web::runtime::Runtime;
use crate::web::security::Token;
use crate::web::server::Config;

pub mod assets;
pub mod connection;
pub mod hub;
pub mod owner;
pub mod pusher;
pub mod runtime;
pub mod security;
pub mod server;
#[cfg(test)]
pub(crate) mod testclient;

/// What the owner of the controller shares with every connection and the telemetry
/// pusher: things they only read.
pub struct Shared {
    /// The session's revision, for `Hello`.
    pub rev: AtomicU64,
    /// What the telemetry slots mean now, for the frames.
    layout: Mutex<Arc<TelemetryLayout>>,
}

impl Shared {
    pub fn new(rev: u64, layout: TelemetryLayout) -> Shared {
        Shared { rev: AtomicU64::new(rev), layout: Mutex::new(Arc::new(layout)) }
    }

    pub fn layout(&self) -> Arc<TelemetryLayout> {
        self.layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    pub fn set_layout(&self, layout: TelemetryLayout) {
        *self.layout.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(layout);
    }
}

/// The server's configuration for an address to listen on. Beyond this machine anyone who
/// can reach the port could drive the session, and loading a plugin loads native code, so
/// there it comes with a token that was made for this run.
pub(crate) fn config_for(bind: SocketAddr, origins: &[String]) -> Config {
    let mut config = Config::new(bind);
    config.extra_origins = origins.to_vec();
    // (A page shows the button that opens a plugin's own window only on a machine that can show one.)
    config.windows = crate::window::available().is_ok();
    if !bind.ip().is_loopback() {
        config.token = Some(Token::generate());
    }
    config
}

/// Whether the web interface could listen as `args` ask, found out before the session is
/// loaded (which can take seconds) rather than after: a port that is taken, or an address
/// the policy does not allow. The port is not kept.
pub fn preflight(args: &WebArgs) -> anyhow::Result<()> {
    server::can_listen(&config_for(args.web_bind, &args.web_origins))
}

/// `tang serve`: serve the session of `ctl` to browsers until the process is told to
/// stop (Ctrl+C, SIGTERM).
pub fn serve(ctl: &mut Controller, args: &WebArgs) -> anyhow::Result<()> {
    let (quit_tx, quit) = crossbeam_channel::bounded(1);
    ctrlc::set_handler(move || {
        let _ = quit_tx.try_send(());
    })
    .context("cannot listen for Ctrl+C")?;

    // The browsers pick plugins from the catalog, and it is not scanned until someone asks (the TUI
    // asks when it starts; this has no TUI): in the background, so that serving does not wait for it.
    ctl.catalog.ensure_scan();
    let mut runtime = Runtime::start(ctl, config_for(args.web_bind, &args.web_origins))?;
    // On stdout, where a script can find it; the log is on stderr.
    println!("Web interface: {}", runtime.url());
    if !args.web_bind.ip().is_loopback() {
        log::warn!("Listening beyond this machine: whoever has the token in that address can control this session");
        if args.web_bind.ip().is_unspecified() {
            log::warn!("Open it by an address of this machine rather than {}", args.web_bind.ip());
        }
    }
    log::info!("Serving. Ctrl+C to stop.");
    runtime.run_headless(ctl, &quit);
    log::info!("Stopping the web interface");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_on_this_machine_needs_no_token() {
        for bind in ["127.0.0.1:8765", "127.0.0.2:1", "[::1]:8765"] {
            assert!(config_for(bind.parse().unwrap(), &[]).token.is_none(), "{bind}");
        }
    }

    #[test]
    fn a_server_beyond_this_machine_gets_a_token_of_its_own_every_time() {
        for bind in ["0.0.0.0:8765", "192.168.1.5:8765", "[::]:8765", "[fe80::1]:8765"] {
            let config = config_for(bind.parse().unwrap(), &[]);
            let token = config.token.expect(bind);
            assert_eq!(token.as_str().len(), 32, "{bind}: 128 bits as hex");
            let again = config_for(bind.parse().unwrap(), &[]).token.unwrap();
            assert_ne!(token.as_str(), again.as_str(), "a new one for each run");
            // (And the server's own policy takes it.)
            crate::web::security::Policy::new(config.bind, Some(token), vec![]).expect(bind);
        }
    }

    #[test]
    fn the_extra_origins_are_passed_on() {
        let config = config_for("127.0.0.1:8765".parse().unwrap(), &["http://localhost:5173".to_string()]);
        assert_eq!(config.extra_origins, ["http://localhost:5173"]);
        assert_eq!(config.bind, "127.0.0.1:8765".parse().unwrap());
    }
}
