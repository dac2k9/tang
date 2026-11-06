//! The audio side of Tang inside a host: what the host's `process` calls (`Audio`), and the thread that keeps the engine taking what the controller sends while the host is not calling it (`spawn_idle`).
//!
//! The renderer is in a `Mutex` that both sides `try_lock`: the host's thread never waits for one that holds it, and the idle thread only works when the host has not rendered a block for a whole period, so
//! the two meet only on the block after a pause (the host spins for the few microseconds that a drain takes: `SPINS`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::audio::{MIDI_CHANNEL_CAPACITY, MidiEvent, Renderer};
use crate::plugin::builtin::HostInput;
use crate::source::Source;

/// The source of what the host sends: the first MIDI device there is (`Source::device(0)`; the controller shows it as the device called `HOST_DEVICE`, and a lane hears it like any other source).
pub(super) const HOST_SOURCE: Source = Source::device(0);

/// How many times the host's thread tries for the renderer before it gives the block up: each try is a few nanoseconds, and what holds it is a drain of the commands that are waiting.
const SPINS: usize = 20_000;

/// The shortest time the idle thread waits between two looks at whether the host is rendering.
const IDLE_LOOK: Duration = Duration::from_millis(20);

/// A MIDI message of a block, as a host gives it: where in the block it is, and its bytes (one to three, padded with zeros).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostMidi {
    pub frame: u32,
    pub bytes: [u8; 3],
}

/// What the host's thread and the idle thread share.
pub(super) struct Shared {
    /// What renders. It is replaced (`replace`) when the session is read again.
    renderer: Mutex<Renderer>,
    /// How many blocks the host has had: the idle thread sees that it is rendering by this going up.
    beats: AtomicU64,
    /// The idle thread's end: set when the engine is to go.
    stop: AtomicBool,
    /// Whether a poisoned renderer (an engine that panicked) has been said.
    said_poisoned: AtomicBool,
    /// What the audio inputs of the session hear: the audio that comes into the host's `process`, put here at the start of every block.
    input: Arc<HostInput>,
}

impl Shared {
    pub(super) fn new(renderer: Renderer, input: Arc<HostInput>) -> Arc<Shared> {
        Arc::new(Shared { renderer: Mutex::new(renderer), beats: AtomicU64::new(0), stop: AtomicBool::new(false), said_poisoned: AtomicBool::new(false), input })
    }

    /// Put another renderer in the place of the one that renders, and give back the old one: the host's thread has the new one from its next block (it renders nothing for the moment that this takes, if it asks then), and the
    /// old one is dropped by whoever is given it (its plugins are dropped where the caller says).
    pub(super) fn replace(&self, renderer: Renderer) -> Renderer {
        let mut guard = self.renderer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::replace(&mut *guard, renderer)
    }

    /// Ask the idle thread to end.
    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// The renderer held, as a block that is being rendered holds it, for a test of what the idle thread does about one that is in use.
    #[cfg(test)]
    pub(super) fn hold_renderer(&self) -> MutexGuard<'_, Renderer> {
        self.renderer.lock().unwrap()
    }

    /// One look of the idle thread: `seen` is how many blocks the host had at the last look. If it has had none since, the renderer takes what the controller has sent (unless it is busy: then the host is rendering, or was a moment ago). Says how many blocks
    /// the host has had now.
    pub(super) fn look(&self, seen: u64) -> u64 {
        let now = self.beats.load(Ordering::Relaxed);
        if now == seen {
            if let Ok(mut renderer) = self.renderer.try_lock() {
                renderer.idle();
            }
        }
        now
    }

    /// The renderer, if it can be had within a few microseconds: `None` for one that a drain or a replacement holds for longer, and for one that panicked.
    fn try_renderer(&self) -> Option<MutexGuard<'_, Renderer>> {
        for _ in 0..SPINS {
            match self.renderer.try_lock() {
                Ok(guard) => return Some(guard),
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
                Err(TryLockError::Poisoned(_)) => {
                    if !self.said_poisoned.swap(true, Ordering::Relaxed) {
                        log::error!("The engine panicked: it renders silence from now on");
                    }
                    return None;
                }
            }
        }
        None
    }
}

/// The thread that takes what the controller sends while the host is not rendering (a track that is switched off, an audio engine that is stopped): the commands go through a channel that holds a few dozen, and the controller waits for room in it when it is
/// full. The thread looks every `period` (at least `IDLE_LOOK`) whether a block has come since it last did, and if none has, has the renderer take what is waiting (`Renderer::idle`). It ends when `Shared::stop` is set.
pub(super) fn spawn_idle(shared: Arc<Shared>, period: Duration) -> std::io::Result<JoinHandle<()>> {
    let period = period.max(IDLE_LOOK);
    std::thread::Builder::new().name("tang-idle".to_string()).spawn(move || {
        let mut seen = shared.beats.load(Ordering::Relaxed);
        while !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(period);
            seen = shared.look(seen);
        }
    })
}

/// The audio thread's handle: what a plugin's `process` is. There is one for each activation, and it is not used from two threads at once.
pub struct Audio {
    shared: Arc<Shared>,
    /// The events of the block, as the engine has them (made once, so that a block asks the allocator for nothing unless a host sends more than the channel of notes holds).
    events: Vec<MidiEvent>,
}

impl Audio {
    pub(super) fn new(shared: Arc<Shared>) -> Audio {
        Audio { shared, events: Vec::with_capacity(MIDI_CHANNEL_CAPACITY) }
    }

    /// Render a block into `output`, a buffer for each channel whose length is the length of the block (the engine has two channels: a host that gives one has the left, one that gives more has the right again), with the MIDI of the block in `midi`, in order, and the
    /// audio that comes into the host's `process` in `input` (a buffer for each channel: none for a plugin that has no audio input, one is heard in both sides), which the audio inputs of the session play. A block that cannot be rendered (the renderer is busy for longer than a few
    /// microseconds, or the engine has panicked) is silence.
    pub fn process(&mut self, midi: &[HostMidi], input: &[&[f32]], output: &mut [&mut [f32]]) {
        // (A block of no frames is not a block: the plugins inside are never given one, what the host sent with it has no frame to be at, and it takes no commands, so it is not counted either: the idle thread goes on taking what the controller sends.)
        if output.first().is_none_or(|channel| channel.is_empty()) {
            return;
        }
        self.shared.beats.fetch_add(1, Ordering::Relaxed);
        match input {
            [left, right, ..] => self.shared.input.put(left, right),
            [mono] => self.shared.input.put(mono, mono),
            [] => self.shared.input.clear(),
        }
        self.events.clear();
        self.events.extend(midi.iter().map(|event| MidiEvent::from_source(HOST_SOURCE, u64::from(event.frame), event.bytes)));
        match self.shared.try_renderer() {
            Some(mut renderer) => renderer.render_planar(&self.events, output),
            None => {
                for channel in output.iter_mut() {
                    channel.fill(0.0);
                }
            }
        }
    }
}
