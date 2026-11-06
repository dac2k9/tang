//! The desk that lists and fetches sounds for the connections (step 116).
//!
//! A library of sounds (`app::sounds::Library`) is asked on a thread of its own, not on the connection's, which is a task of the server's single thread that every browser shares, and not on the owner's, which
//! is the controller's: a folder is quick, and a library that is far away (a server, one day) is not, and neither of them may wait for it. The desk is that thread and the queue in front of it: a job goes in with
//! `try_send` (a queue that is full is "busy", as the owner's is), and what comes out is told to the client that asked, through the hub; a sound that is opened is read here and then goes to the owner as the
//! intent that a file the page read goes as (`Intent::OpenSession`), under the request's own `req_id`, so that the owner's answer (`Ack` or `Rejected`) is the page's.

use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError};

use crate::app::intent::Intent;
use crate::app::protocol::{ErrorCode, ServerMessage, WireError};
use crate::app::sounds::{SoundLibrary, Sounds};
use crate::web::connection::OwnerRequest;
use crate::web::hub::{ClientId, Hub, Outgoing};

/// How many jobs may wait for the desk at once: a page asks for each library once when its dialog opens, and for a sound when a person picks one.
pub const DESK_QUEUE: usize = 32;

enum Job {
    List { client: ClientId, req_id: u32, library: String },
    Open { client: ClientId, req_id: u32, library: String, id: String },
}

/// The way to the desk, which every connection holds a copy of. When the last copy is dropped the thread ends.
#[derive(Clone)]
pub struct Desk {
    jobs: Sender<Job>,
    libraries: Arc<[SoundLibrary]>,
}

/// The thread of the desk, for the server to wait for when it stops (a thread of a plugin that is still running when a host unloads it is a crash).
pub struct Worker {
    thread: Option<JoinHandle<()>>,
}

impl Desk {
    /// Start the desk over `sounds`: answers go to the clients through `hub`, and a sound that is to be opened goes to `owner`.
    pub fn start(sounds: Arc<Sounds>, hub: Arc<Hub>, owner: Sender<OwnerRequest>) -> std::io::Result<(Desk, Worker)> {
        let (jobs, queue) = crossbeam_channel::bounded(DESK_QUEUE);
        let libraries: Arc<[SoundLibrary]> = sounds.infos().into();
        let thread = std::thread::Builder::new().name("tang-sounds".into()).spawn(move || serve(&sounds, &hub, &owner, &queue))?;
        Ok((Desk { jobs, libraries }, Worker { thread: Some(thread) }))
    }

    /// The libraries, as the hello says them.
    pub fn libraries(&self) -> &[SoundLibrary] {
        &self.libraries
    }

    /// Ask for the sounds of `library` for `client`, whose answer is `Sounds` (or `Rejected`) under `req_id`. False if the desk is too busy to take it.
    pub fn list(&self, client: ClientId, req_id: u32, library: String) -> bool {
        self.put(Job::List { client, req_id, library })
    }

    /// Ask for the sound `id` of `library` to be opened for `client`: its answer is the owner's, under `req_id`. False if the desk is too busy to take it.
    pub fn open(&self, client: ClientId, req_id: u32, library: String, id: String) -> bool {
        self.put(Job::Open { client, req_id, library, id })
    }

    fn put(&self, job: Job) -> bool {
        !matches!(self.jobs.try_send(job), Err(TrySendError::Full(_) | TrySendError::Disconnected(_)))
    }
}

#[cfg(test)]
impl Desk {
    /// A desk that takes nothing, as one whose queue is full is for a moment: for a test of what a connection says then.
    pub(crate) fn stalled() -> Desk {
        let (jobs, _queue) = crossbeam_channel::bounded(0);
        Desk { jobs, libraries: Arc::from(Vec::new()) }
    }
}

impl Worker {
    /// Wait for the thread to end, which it does when every `Desk` has been dropped.
    pub fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                log::warn!("The thread that lists sounds panicked");
            }
        }
    }
}

fn serve(sounds: &Sounds, hub: &Hub, owner: &Sender<OwnerRequest>, queue: &Receiver<Job>) {
    while let Ok(job) = queue.recv() {
        match job {
            Job::List { client, req_id, library } => {
                let message = match sounds.list(&library) {
                    Ok(listing) => ServerMessage::Sounds { req_id, library, entries: listing.entries, truncated: listing.truncated },
                    Err(error) => ServerMessage::Rejected { req_id, error },
                };
                tell(hub, client, &message);
            }
            Job::Open { client, req_id, library, id } => match sounds.read(&library, &id) {
                Ok(text) => {
                    if owner.try_send(OwnerRequest::Apply { client, req_id, intent: Intent::OpenSession { text } }).is_err() {
                        tell(hub, client, &ServerMessage::Rejected { req_id, error: WireError::new(ErrorCode::RateLimited, "the server is busy: try again") });
                    }
                }
                Err(error) => tell(hub, client, &ServerMessage::Rejected { req_id, error }),
            },
        }
    }
}

fn tell(hub: &Hub, client: ClientId, message: &ServerMessage) {
    let json = serde_json::to_string(message).expect("a message is serializable");
    // (A client that has gone is not told.)
    hub.send(client, Outgoing::Text(json.into()));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::mpsc::Receiver as ClientQueue;

    use super::*;
    use crate::app::sounds::{Folder, Library, Listing, SoundEntry};

    const OSC: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    /// A library that answers when it is let, to see a desk go on while a library is slow.
    #[derive(Debug)]
    struct Gated {
        gate: Mutex<Receiver<()>>,
        texts: HashMap<String, String>,
    }

    impl Library for Gated {
        fn info(&self) -> SoundLibrary {
            SoundLibrary { id: "far".to_string(), label: "Far".to_string(), place: "https://example.test/".to_string() }
        }
        fn list(&self) -> Result<Listing, WireError> {
            self.gate.lock().unwrap().recv().ok();
            Ok(Listing { entries: vec![SoundEntry { id: "a.toml".to_string(), name: "a".to_string(), detail: String::new(), openable: true, modified: None }], truncated: false })
        }
        fn read(&self, id: &str) -> Result<String, WireError> {
            self.gate.lock().unwrap().recv().ok();
            self.texts.get(id).cloned().ok_or_else(|| WireError::new(ErrorCode::NotFound, "no such sound"))
        }
    }

    struct Rig {
        desk: Desk,
        worker: Worker,
        client: ClientId,
        queue: ClientQueue<Outgoing>,
        owner: Receiver<OwnerRequest>,
        _dir: Option<tempfile::TempDir>,
    }

    fn rig_over(sounds: Sounds, dir: Option<tempfile::TempDir>, owner_capacity: usize) -> Rig {
        let hub = Arc::new(Hub::new(16));
        let (client, queue) = hub.connect();
        let (owner_tx, owner) = crossbeam_channel::bounded(owner_capacity);
        let (desk, worker) = Desk::start(Arc::new(sounds), hub, owner_tx).unwrap();
        Rig { desk, worker, client, queue, owner, _dir: dir }
    }

    fn folder_rig(owner_capacity: usize) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pad.toml"), OSC).unwrap();
        let sounds = Sounds::new(vec![Box::new(Folder::new("here", "Here", dir.path().to_path_buf()))]);
        rig_over(sounds, Some(dir), owner_capacity)
    }

    /// The next message that the client was told, waiting for it a while (the desk is a thread).
    fn told(rig: &mut Rig) -> Value {
        for _ in 0..500 {
            if let Ok(Outgoing::Text(text)) = rig.queue.try_recv() {
                return serde_json::from_str(&text).unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the client was told nothing");
    }

    fn ended(mut rig: Rig) {
        drop(rig.desk);
        rig.worker.join();
    }

    #[test]
    fn the_desk_says_the_libraries_it_has() {
        let rig = folder_rig(4);
        assert_eq!(rig.desk.libraries().iter().map(|library| library.id.as_str()).collect::<Vec<_>>(), ["here"]);
        ended(rig);
    }

    #[test]
    fn a_listing_is_told_to_the_client_that_asked_under_its_request_id() {
        let mut rig = folder_rig(4);
        assert!(rig.desk.list(rig.client, 7, "here".to_string()));
        let said = told(&mut rig);
        assert_eq!(said["type"], "sounds");
        assert_eq!((said["reqId"].as_u64(), said["library"].as_str(), said["truncated"].as_bool()), (Some(7), Some("here"), Some(false)));
        assert_eq!(said["entries"][0]["id"], json!("pad.toml"));
        assert_eq!(said["entries"][0]["detail"], json!("osc"));
        ended(rig);
    }

    #[test]
    fn a_listing_that_cannot_be_made_is_rejected_with_why() {
        let mut rig = folder_rig(4);
        assert!(rig.desk.list(rig.client, 8, "elsewhere".to_string()));
        let said = told(&mut rig);
        assert_eq!((said["type"].as_str(), said["reqId"].as_u64(), said["error"]["code"].as_str()), (Some("rejected"), Some(8), Some("notFound")));
        assert!(said["error"]["message"].as_str().unwrap().contains("elsewhere"), "{said}");
        ended(rig);
    }

    #[test]
    fn a_sound_that_is_opened_is_read_and_goes_to_the_owner_as_the_session_to_open_under_the_requests_id() {
        let mut rig = folder_rig(4);
        assert!(rig.desk.open(rig.client, 9, "here".to_string(), "pad.toml".to_string()));
        let request = rig.owner.recv_timeout(Duration::from_secs(5)).expect("the owner is asked");
        assert_eq!(request, OwnerRequest::Apply { client: rig.client, req_id: 9, intent: Intent::OpenSession { text: OSC.to_string() } });
        assert!(rig.queue.try_recv().is_err(), "and the client is told nothing by the desk: the owner answers");
        ended(rig);
    }

    #[test]
    fn a_sound_that_cannot_be_read_is_rejected_and_the_owner_is_not_asked() {
        let mut rig = folder_rig(4);
        assert!(rig.desk.open(rig.client, 10, "here".to_string(), "../gone.toml".to_string()));
        let said = told(&mut rig);
        assert_eq!((said["type"].as_str(), said["reqId"].as_u64(), said["error"]["code"].as_str()), (Some("rejected"), Some(10), Some("invalid")));
        assert!(rig.owner.try_recv().is_err());
        ended(rig);
    }

    #[test]
    fn an_owner_that_is_too_busy_or_gone_is_told_to_the_client_as_busy() {
        // (An owner that takes none: its queue is as long as it can be made short, which is one, and it is full.)
        let mut rig = folder_rig(1);
        rig.desk.open(rig.client, 1, "here".to_string(), "pad.toml".to_string());
        rig.desk.open(rig.client, 2, "here".to_string(), "pad.toml".to_string());
        let said = told(&mut rig);
        assert_eq!((said["type"].as_str(), said["reqId"].as_u64(), said["error"]["code"].as_str()), (Some("rejected"), Some(2), Some("rateLimited")), "{said}");
        assert_eq!(said["error"]["message"], json!("the server is busy: try again"));
        assert!(matches!(rig.owner.try_recv(), Ok(OwnerRequest::Apply { req_id: 1, .. })), "the first was taken");
        // (And one that is gone.)
        let Rig { desk, mut worker, client, mut queue, owner, _dir } = rig;
        drop(owner);
        desk.open(client, 3, "here".to_string(), "pad.toml".to_string());
        let mut said = None;
        for _ in 0..500 {
            if let Ok(Outgoing::Text(text)) = queue.try_recv() {
                said = Some(serde_json::from_str::<Value>(&text).unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(said.expect("told")["reqId"], json!(3));
        drop(desk);
        worker.join();
    }

    #[test]
    fn a_library_that_is_slow_holds_up_nobody_who_asks_and_a_queue_that_is_full_is_busy() {
        let (open, gate) = crossbeam_channel::unbounded();
        let far = Gated { gate: Mutex::new(gate), texts: HashMap::new() };
        let mut rig = rig_over(Sounds::new(vec![Box::new(far)]), None, 4);
        // The first is taken by the desk, which waits in the library; then the queue takes as many as it holds, and no more.
        assert!(rig.desk.list(rig.client, 1, "far".to_string()), "the first, which the desk takes and waits in");
        let mut taken = 0;
        for at in 0..DESK_QUEUE + 4 {
            if rig.desk.list(rig.client, 100 + at as u32, "far".to_string()) {
                taken += 1;
            }
        }
        assert!((DESK_QUEUE - 1..=DESK_QUEUE).contains(&taken), "the queue holds {DESK_QUEUE}, one of which is the first if the desk had not yet taken it: {taken}");
        assert!(!rig.desk.list(rig.client, 999, "far".to_string()), "and the next is turned away at once, as busy");
        // Let them all go: each is told.
        for _ in 0..taken + 1 {
            open.send(()).unwrap();
        }
        assert_eq!(told(&mut rig)["reqId"], json!(1));
        drop(open);
        ended(rig);
    }

    #[test]
    fn a_client_that_has_gone_is_not_a_reason_to_stop() {
        let hub = Arc::new(Hub::new(16));
        let (gone, queue) = hub.connect();
        hub.disconnect(gone);
        drop(queue);
        let (owner_tx, _owner) = crossbeam_channel::bounded(4);
        let dir = tempfile::tempdir().unwrap();
        let sounds = Sounds::new(vec![Box::new(Folder::new("here", "Here", dir.path().to_path_buf()))]);
        let (desk, mut worker) = Desk::start(Arc::new(sounds), hub.clone(), owner_tx).unwrap();
        let (client, mut live) = hub.connect();
        assert!(desk.list(gone, 1, "here".to_string()));
        assert!(desk.list(client, 2, "here".to_string()));
        let mut said = None;
        for _ in 0..500 {
            if let Ok(Outgoing::Text(text)) = live.try_recv() {
                said = Some(serde_json::from_str::<Value>(&text).unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(said.expect("the desk went on")["reqId"], json!(2));
        drop(desk);
        worker.join();
    }

    #[test]
    fn the_thread_of_the_desk_ends_when_the_last_desk_is_dropped() {
        let rig = folder_rig(4);
        let Rig { desk, mut worker, .. } = rig;
        let more = desk.clone();
        drop(desk);
        std::thread::sleep(Duration::from_millis(50));
        assert!(worker.thread.as_ref().is_some_and(|thread| !thread.is_finished()), "a copy is still held");
        drop(more);
        worker.join();
        assert!(worker.thread.is_none());
    }
}
