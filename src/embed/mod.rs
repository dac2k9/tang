//! Tang inside a host (step 113; `docs/reference/plugin-host.md`): the engine and the controller for a host that has a sound card and a clock of its own, a DAW that runs Tang as a plugin, in place of the program's own.
//!
//! The pieces are the ones of `tang serve`, split between the host's threads and ours. *The host's audio thread* calls `Audio::process` for every block, with the MIDI of the block and the audio that comes in, and has the block rendered by the same `Renderer` that a
//! sound card's callback has. *The controller's thread* (`control`) owns the controller and serves the session to browsers on a port of its own, as `tang serve` does on the main thread; the host's threads never touch it. A thread that only keeps the engine taking
//! what the controller sends while the host is not calling `process` (`engine::spawn_idle`) is what lets a page go on editing a plugin that is switched off. *The host's main thread* has a `Host`: it starts all of this when the plugin is activated at a sample rate, asks
//! for the session as text when the host saves its project (`session_text`), and gives it back when the host opens one (`set_session_text`).
//!
//! The session is the plugin's saved state: the same text as a session file, which has what the plugins inside keep in them (step 112, part C).

mod control;
mod engine;
mod log_file;

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::audio::{Engine, MidiEvent, Renderer};
use crate::plugin::builtin::HostInput;
use crate::plugin::chain::CHAIN_CHANNELS;
use crate::source::Events;

use control::{Control, Setup};
use engine::Shared;

pub use control::HOST_DEVICE;
pub use engine::{Audio, HostMidi};
pub use log_file::path as log_path;

/// How long the controller's thread may take to say that it has loaded the session: plugins that are loaded can take seconds, and a host waits for a plugin that is being activated.
const LOAD_WAIT: Duration = Duration::from_secs(300);

/// How long the host waits for the controller's thread to give the session as text.
const ASK_WAIT: Duration = Duration::from_secs(30);

/// What a plugin is, which is what its session starts as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Notes come in, and what the session plays goes out.
    Instrument,
    /// Audio comes in, and what the session makes of it goes out: the session starts as an audio input (which plays what comes in) that goes to the mix.
    Effect,
}

impl Kind {
    /// The session that a new instance starts as, as the text of a session file. The master's limiter is out of the sound: a host works in floating point and has headroom, and the limiter holds the sound back by 2 ms, which a host that is not told
    /// of it cannot make up for (an effect would be late against its own dry sound); a person who wants it puts it in, in the page.
    pub fn default_session(self) -> &'static str {
        match self {
            Kind::Instrument => "[[instrument]]\nplugin = \"builtin:osc\"\n\n[master]\nlimiter = false\n",
            Kind::Effect => "[[instrument]]\nplugin = \"builtin:input\"\n\n[master]\nlimiter = false\n",
        }
    }
}

/// What renders until a session is loaded: silence.
struct Silent;

impl Engine for Silent {
    fn num_channels(&self) -> usize {
        CHAIN_CHANNELS
    }

    fn process(&mut self, _: Events<'_>, _: &mut [Vec<f32>]) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A renderer that renders silence, for a block of up to `frames` frames.
fn silent_renderer(frames: usize) -> Renderer {
    let (_, notes) = crossbeam_channel::bounded::<MidiEvent>(1);
    Renderer::new(Silent, notes, frames)
}

/// The time that a block of `frames` frames lasts at `rate`.
fn block_period(rate: f32, frames: u32) -> Duration {
    Duration::from_secs_f64(f64::from(frames) / f64::from(rate.max(1.0)))
}

/// A session that is loaded and running: the controller's thread, which owns the controller and serves the page.
struct Session {
    control: Sender<Control>,
    thread: Option<JoinHandle<()>>,
    /// Where the page is, if it is served.
    url: Option<String>,
    /// The text of a session that could not be loaded, which this one stands in for: it is what a save gives for as long as nothing in this one has been changed (a project with a plugin that is not installed on this machine is not
    /// lost by being opened and saved).
    unreadable: Option<String>,
}

impl Session {
    /// Start a controller's thread on `text`, give the engine that it makes to `shared` (so that the commands of the load are taken out as they are sent), and wait until the session is loaded. Whatever the engine had is dropped, here.
    fn start(shared: &Arc<Shared>, text: &str, rate: f32, frames: u32, serve: bool, input: &Arc<HostInput>) -> Result<Session, String> {
        let (audio_tx, audio_rx) = crossbeam_channel::bounded(1);
        let (started_tx, started_rx) = crossbeam_channel::bounded(1);
        let (control_tx, control_rx) = crossbeam_channel::unbounded();
        let setup = Setup { text: text.to_string(), sample_rate: rate, max_frames: frames as usize, serve, input: input.clone() };
        let thread = std::thread::Builder::new()
            .name("tang-controller".to_string())
            .spawn(move || control::run(setup, audio_tx, started_tx, control_rx))
            .map_err(|error| format!("the controller's thread could not be made: {error}"))?;
        let mut session = Session { control: control_tx, thread: Some(thread), url: None, unreadable: None };

        let parts: Result<(crate::plugin::chain::AudioGraph, Receiver<MidiEvent>), _> = audio_rx.recv_timeout(LOAD_WAIT);
        match parts {
            Ok((graph, notes)) => drop(shared.replace(Renderer::new(graph, notes, frames as usize))),
            // (The thread ended before it had an engine: it says why.)
            Err(_) => {
                let why = started_rx.try_recv().ok().and_then(Result::err).unwrap_or_else(|| "the controller's thread ended".to_string());
                session.stop();
                return Err(why);
            }
        }
        match started_rx.recv_timeout(LOAD_WAIT) {
            Ok(Ok(started)) => {
                session.url = started.url;
                Ok(session)
            }
            Ok(Err(why)) => {
                session.stop();
                Err(why)
            }
            Err(_) => {
                session.stop();
                Err("the session was not loaded in five minutes".to_string())
            }
        }
    }

    /// Stop the thread, and wait for it: the page is closed and the controller shut down. The engine's plugins are to be gone by now (`Host::stop` drops them first, as the program stops its engine before it shuts the controller down).
    fn stop(&mut self) {
        let _ = self.control.send(Control::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a `Host` has while it is activated.
struct Running {
    rate: f32,
    frames: u32,
    shared: Arc<Shared>,
    input: Arc<HostInput>,
    idle: Option<JoinHandle<()>>,
    session: Option<Session>,
}

/// Tang inside a host, as the host's main thread has it: one for each instance of the plugin.
pub struct Host {
    kind: Kind,
    serve: bool,
    /// The session as it was last given or asked for: what starts the engine, and what a save gives while there is none.
    text: Option<String>,
    running: Option<Running>,
}

impl Host {
    /// A plugin of this kind that is not running yet (`activate` starts it), which serves its session to browsers if `serve` (a build that has no web interface serves none).
    pub fn new(kind: Kind, serve: bool) -> Host {
        log_file::init();
        Host { kind, serve, text: None, running: None }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Start the engine for blocks of up to `max_frames` frames at `sample_rate`, and give the host's audio thread its end of it. A plugin that is activated again at the rate and the block size that it has is not made again (the sound that the page has been
    /// editing goes on); at another, the session is read out and loaded again, since the plugins inside are loaded for the rate and the size, and the audio thread's handle of the old engine is no more. A session that cannot be loaded (a plugin that is not
    /// installed, a file that is not one) is stood in for by a new one, with a note in the log, and what was given is kept for as long as nothing in the new one is changed.
    pub fn activate(&mut self, sample_rate: f64, max_frames: u32) -> Result<Audio, String> {
        let rate = sample_rate as f32;
        if !(rate.is_finite() && rate > 0.0) || max_frames == 0 {
            return Err(format!("a sample rate of {sample_rate} and blocks of {max_frames} frames cannot be played"));
        }
        if let Some(running) = &self.running {
            if running.rate == rate && running.frames == max_frames {
                return Ok(Audio::new(running.shared.clone()));
            }
            // (What the page has changed is read out before the engine goes; a controller that cannot say is no reason not to play.)
            if let Err(why) = self.session_text() {
                log::warn!("The session could not be read out before the engine was made again: {why}");
            }
            self.stop();
        }
        let input = Arc::new(HostInput::new(max_frames as usize));
        let shared = Shared::new(silent_renderer(max_frames as usize), input.clone());
        let idle = engine::spawn_idle(shared.clone(), block_period(rate, max_frames) * 2).map_err(|error| format!("the idle thread could not be made: {error}"))?;
        let mut running = Running { rate, frames: max_frames, shared: shared.clone(), input, idle: Some(idle), session: None };
        let text = self.text.clone().unwrap_or_else(|| self.kind.default_session().to_string());
        let started = self.load(&running, &text);
        match started {
            Ok(session) => running.session = Some(session),
            Err(why) => {
                // (The idle thread and the engine go with `running`, which `stop` is given.)
                self.running = Some(running);
                self.stop();
                return Err(why);
            }
        }
        self.running = Some(running);
        Ok(Audio::new(shared))
    }

    /// The session of `text` loaded onto `running`'s engine, or, if it cannot be, a new one with `text` kept (`Session::unreadable`).
    fn load(&self, running: &Running, text: &str) -> Result<Session, String> {
        let attempt = Session::start(&running.shared, text, running.rate, running.frames, self.serve, &running.input);
        match attempt {
            Ok(session) => Ok(session),
            Err(why) => {
                log::error!("The session of this plugin could not be loaded ({why}); a new session stands in for it, and what was given is kept until the new one is changed");
                let default = self.kind.default_session();
                let mut session = Session::start(&running.shared, default, running.rate, running.frames, self.serve, &running.input)?;
                session.unreadable = Some(text.to_string());
                Ok(session)
            }
        }
    }

    /// The session as the text of a session file. While the engine runs it is asked of the controller's thread, which reads out what is in the plugins inside; before it runs, it is what was given, or a new session's.
    pub fn session_text(&mut self) -> Result<String, String> {
        let Some(session) = self.running.as_ref().and_then(|running| running.session.as_ref()) else {
            return Ok(self.text.clone().unwrap_or_else(|| self.kind.default_session().to_string()));
        };
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        session.control.send(Control::Session(reply_tx)).map_err(|_| "the controller's thread has stopped".to_string())?;
        let (text, changed) = reply_rx.recv_timeout(ASK_WAIT).map_err(|_| "the controller's thread did not answer".to_string())??;
        // A session that stands in for one that could not be loaded is not what is saved until something in it is changed.
        let text = match (&session.unreadable, changed) {
            (Some(original), false) => original.clone(),
            _ => text,
        };
        self.text = Some(text.clone());
        Ok(text)
    }

    /// Have the session be this text (a host opened a project, or a preset): what starts the engine if it is not running, and while it runs the engine is made again under the handle that the audio thread has, with the page that was served going to another address.
    pub fn set_session_text(&mut self, text: String) -> Result<(), String> {
        self.text = Some(text.clone());
        let Some(running) = self.running.take() else { return Ok(()) };
        let mut running = running;
        let old = running.session.take();
        let loaded = self.load(&running, &text);
        // (The old engine was dropped when the new one took its place; its controller is stopped now, after the plugins it loaded.)
        drop(old);
        let result = loaded.map(|session| running.session = Some(session));
        self.running = Some(running);
        result
    }

    /// Where the page of this instance is, if the engine runs and serves one.
    pub fn url(&self) -> Option<String> {
        self.running.as_ref()?.session.as_ref()?.url.clone()
    }

    /// Stop the engine: the idle thread, the plugins inside, the controller and the page, in that order (the plugins go before the controller that loaded them, as the program stops its engine before it shuts the controller down).
    fn stop(&mut self) {
        let Some(mut running) = self.running.take() else { return };
        running.shared.stop();
        if let Some(idle) = running.idle.take() {
            let _ = idle.join();
        }
        drop(running.shared.replace(silent_renderer(running.frames as usize)));
        drop(running.session.take());
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 256;
    const RATE: f64 = 48_000.0;
    const NOTE_ON: [u8; 3] = [0x90, 69, 100];
    const NOTE_OFF: [u8; 3] = [0x80, 69, 0];

    /// What a host does with `blocks` blocks: the first has the events `midi` (each at its own frame) and the audio `input` comes in at all of them (none for an instrument). The left side of the last block, and the loudest sample of all of them on either side.
    fn play(audio: &mut Audio, midi: &[HostMidi], input: Option<&[f32]>, blocks: usize) -> (Vec<f32>, f32) {
        let (mut last, mut loudest) = (Vec::new(), 0.0f32);
        for block in 0..blocks {
            let (mut left, mut right) = (vec![1.0f32; BLOCK], vec![1.0f32; BLOCK]);
            let events: &[HostMidi] = if block == 0 { midi } else { &[] };
            match input {
                Some(samples) => audio.process(events, &[samples, samples], &mut [&mut left, &mut right]),
                None => audio.process(events, &[], &mut [&mut left, &mut right]),
            }
            loudest = left.iter().chain(&right).fold(loudest, |loudest, sample| loudest.max(sample.abs()));
            last = left;
        }
        (last, loudest)
    }

    fn at(frame: u32, bytes: [u8; 3]) -> HostMidi {
        HostMidi { frame, bytes }
    }

    /// A host with the instrument's default session, activated.
    fn instrument() -> (Host, Audio) {
        let mut host = Host::new(Kind::Instrument, false);
        let audio = host.activate(RATE, BLOCK as u32).expect("the engine starts");
        (host, audio)
    }

    #[test]
    fn a_note_from_the_host_is_heard_from_its_frame_and_is_silent_again_when_it_ends() {
        let (_host, mut audio) = instrument();
        let (_, quiet) = play(&mut audio, &[], None, 4);
        assert_eq!(quiet, 0.0, "nothing is playing");

        // A note that starts in the middle of a block is silent before its frame, and the host's buffers are written over (they were full of 1s).
        let (mut left, mut right) = (vec![1.0f32; BLOCK], vec![1.0f32; BLOCK]);
        audio.process(&[at(128, NOTE_ON)], &[], &mut [&mut left, &mut right]);
        assert!(left[..128].iter().chain(&right[..128]).all(|&sample| sample == 0.0), "silence until its frame");
        assert!(left[128..].iter().any(|&sample| sample.abs() > 0.01), "the note from its frame");
        let (_, held) = play(&mut audio, &[], None, 8);
        assert!(held > 0.05, "a held note goes on sounding: {held}");

        // Released, it dies away (the oscillator's release is 10 ms).
        play(&mut audio, &[at(0, NOTE_OFF)], None, 1);
        let (tail, _) = play(&mut audio, &[], None, 40);
        assert!(tail.iter().all(|&sample| sample.abs() < 1e-4), "silent again");
    }

    #[test]
    fn the_session_that_is_given_is_what_plays_and_what_is_given_back() {
        let mut host = Host::new(Kind::Instrument, false);
        assert_eq!(host.session_text().unwrap(), Kind::Instrument.default_session(), "a plugin that was given nothing is a new session");
        host.set_session_text("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.0\n".to_string()).unwrap();
        let mut audio = host.activate(RATE, BLOCK as u32).unwrap();
        let (_, loudest) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
        assert!(loudest < 1e-4, "an oscillator whose volume is 0 is silent: {loudest}");
        let text = host.session_text().unwrap();
        assert!(text.contains("builtin:osc") && text.contains("volume = 0"), "what is saved is what was loaded:\n{text}");
        // The text can be read again by a program: it is a session file.
        crate::session::parse(&text).expect("a session file");
    }

    #[test]
    fn a_session_that_is_given_while_the_engine_runs_plays_under_the_handle_the_host_has() {
        let (mut host, mut audio) = instrument();
        let (_, before) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
        assert!(before > 0.05, "the oscillator sounds: {before}");
        play(&mut audio, &[at(0, NOTE_OFF)], None, 40);

        // The host loads a state (a preset): the same handle plays the new session, and a note that came before is not carried over.
        host.set_session_text("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.0\n".to_string()).unwrap();
        let (_, after) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
        assert!(after < 1e-4, "the new session is the one that plays: {after}");
        assert!(host.session_text().unwrap().contains("volume = 0"));

        // And once more, back: the handle goes on being the host's.
        host.set_session_text(Kind::Instrument.default_session().to_string()).unwrap();
        let (_, again) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
        assert!(again > 0.05, "{again}");
    }

    #[test]
    fn a_session_that_cannot_be_loaded_is_stood_in_for_and_is_what_a_save_gives_while_nothing_is_changed() {
        for broken in ["this is not { a session", "[[instrument]]\nplugin = \"builtin:no-such-thing\"\n"] {
            let mut host = Host::new(Kind::Instrument, false);
            host.set_session_text(broken.to_string()).unwrap();
            let mut audio = host.activate(RATE, BLOCK as u32).expect("a plugin that cannot read its session still starts");
            let (_, loudest) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
            assert!(loudest > 0.05, "a new session plays in its place: {loudest}");
            assert_eq!(host.session_text().unwrap(), broken, "what the host gave is what it gets back, so that a project is not lost by being opened and saved");
        }
    }

    #[test]
    fn a_session_of_more_lanes_than_the_command_channel_holds_loads_while_the_host_is_not_rendering() {
        // The host calls nothing but `activate`: it is the idle thread that takes the commands out of the channel (64 of them), or the load waits for room in it for ever.
        let lanes = "[[instrument]]\nplugin = \"builtin:osc\"\n\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n\n".repeat(40);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut host = Host::new(Kind::Instrument, false);
            host.set_session_text(lanes).unwrap();
            let audio = host.activate(RATE, BLOCK as u32).map(|_| ());
            let _ = done_tx.send(audio);
            host
        });
        done_rx.recv_timeout(Duration::from_secs(60)).expect("the session loaded in a minute").expect("and loaded");
        let mut host = thread.join().unwrap();
        let text = host.session_text().unwrap();
        assert_eq!(text.matches("[[instrument]]").count(), 40, "every lane is there");
    }

    #[test]
    fn what_the_host_sends_to_hear_is_what_an_audio_input_plays_and_no_sound_card_is_opened() {
        let mut host = Host::new(Kind::Effect, false);
        let mut audio = host.activate(RATE, BLOCK as u32).unwrap();
        // Silence in, silence out: the audio input has nothing to play of its own (the tone of a test is not there: this is a host).
        let (_, quiet) = play(&mut audio, &[], Some(&[0.0; BLOCK]), 8);
        assert_eq!(quiet, 0.0);
        // A sine of 0.25 comes through at about that level (the master's limiter is above it).
        let sine: Vec<f32> = (0..BLOCK).map(|i| 0.25 * (std::f32::consts::TAU * 480.0 * i as f32 / RATE as f32).sin()).collect();
        let (_, loudest) = play(&mut audio, &[], Some(&sine), 40);
        assert!((0.2..=0.3).contains(&loudest), "what comes in is what goes out: {loudest}");
        // An instrument that has an audio input in its session has nothing to hear, since the host gives it no audio.
        let mut host = Host::new(Kind::Instrument, false);
        host.set_session_text(Kind::Effect.default_session().to_string()).unwrap();
        let mut audio = host.activate(RATE, BLOCK as u32).unwrap();
        assert_eq!(play(&mut audio, &[], None, 8).1, 0.0);
    }

    #[test]
    fn a_new_effect_passes_what_comes_in_unchanged_and_not_late() {
        // No limiter holds the sound back, and the audio input's gain is 1: every sample that comes out is the sample that went in, in the same frame (a host cannot make up for a delay that it is not told of).
        let mut host = Host::new(Kind::Effect, false);
        let mut audio = host.activate(RATE, BLOCK as u32).unwrap();
        let noise: Vec<f32> = (0..BLOCK).map(|i| ((i * 7919 % 1000) as f32 / 500.0 - 1.0) * 0.5).collect();
        for _ in 0..4 {
            let (mut left, mut right) = (vec![9.0f32; BLOCK], vec![9.0f32; BLOCK]);
            audio.process(&[], &[&noise, &noise], &mut [&mut left, &mut right]);
            assert_eq!(left, noise, "the same samples in the same frames");
            assert_eq!(right, noise);
        }
        for kind in [Kind::Instrument, Kind::Effect] {
            assert!(kind.default_session().contains("limiter = false"), "{kind:?}");
        }
    }

    #[test]
    fn the_engine_is_made_again_for_another_sample_rate_and_not_for_the_same_one() {
        let (mut host, mut audio) = instrument();
        let (_, first) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
        assert!(first > 0.05);
        // The same rate and size: the engine is the one that was playing (the note that is held is still there).
        drop(audio);
        let mut same = host.activate(RATE, BLOCK as u32).unwrap();
        let (_, held) = play(&mut same, &[], None, 4);
        assert!(held > 0.05, "the same engine: the note goes on: {held}");
        // Another rate: a new engine, which has no note held, and the session is the one that was.
        drop(same);
        let mut other = host.activate(44_100.0, BLOCK as u32).unwrap();
        let (_, nothing) = play(&mut other, &[], None, 4);
        assert_eq!(nothing, 0.0, "a new engine");
        let (_, again) = play(&mut other, &[at(0, NOTE_ON)], None, 8);
        assert!(again > 0.05);
    }

    #[test]
    fn a_rate_or_a_size_that_cannot_be_played_is_refused() {
        let mut host = Host::new(Kind::Instrument, false);
        for (rate, frames) in [(0.0, 256), (-44_100.0, 256), (f64::NAN, 256), (48_000.0, 0)] {
            assert!(host.activate(rate, frames).is_err(), "{rate} Hz in {frames} frames");
        }
    }

    #[test]
    fn the_kinds_start_as_sessions_that_a_program_can_read() {
        for kind in [Kind::Instrument, Kind::Effect] {
            crate::session::parse(kind.default_session()).unwrap_or_else(|error| panic!("{kind:?}: {error}"));
        }
        assert!(Kind::Instrument.default_session().contains("builtin:osc"));
        assert!(Kind::Effect.default_session().contains("builtin:input"));
    }

    /// An engine that counts how often it is told that it is idle, and how many blocks it has rendered.
    struct Counting(Arc<std::sync::atomic::AtomicUsize>, Arc<std::sync::atomic::AtomicUsize>);

    impl Engine for Counting {
        fn num_channels(&self) -> usize {
            2
        }
        fn process(&mut self, _: Events<'_>, _: &mut [Vec<f32>]) -> anyhow::Result<()> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        fn idle(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn counting() -> (Arc<Shared>, Arc<std::sync::atomic::AtomicUsize>, Arc<std::sync::atomic::AtomicUsize>) {
        let (idles, blocks) = (Arc::new(std::sync::atomic::AtomicUsize::new(0)), Arc::new(std::sync::atomic::AtomicUsize::new(0)));
        let (_, notes) = crossbeam_channel::bounded::<MidiEvent>(1);
        let renderer = Renderer::new(Counting(idles.clone(), blocks.clone()), notes, BLOCK);
        (Shared::new(renderer, Arc::new(HostInput::new(BLOCK))), idles, blocks)
    }

    #[test]
    fn a_look_of_the_idle_thread_takes_what_is_sent_when_no_block_has_come_since_the_last_look_and_not_when_one_has() {
        use std::sync::atomic::Ordering;
        let (shared, idles, blocks) = counting();
        let mut audio = Audio::new(shared.clone());
        let mut seen = shared.look(0);
        assert_eq!(idles.load(Ordering::Relaxed), 1, "a first look with no block ever: the host is not rendering");
        // A block between two looks: the host is rendering, and the engine is left to it.
        let (mut left, mut right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        audio.process(&[], &[], &mut [&mut left, &mut right]);
        seen = shared.look(seen);
        assert_eq!((idles.load(Ordering::Relaxed), blocks.load(Ordering::Relaxed)), (1, 1), "no idling after a block");
        // None since: it is idle again, and goes on being while no block comes.
        seen = shared.look(seen);
        seen = shared.look(seen);
        assert_eq!(idles.load(Ordering::Relaxed), 3);
        // A look that finds the renderer in use does nothing, and does not wait for it.
        let held = shared.hold_renderer();
        let before = idles.load(Ordering::Relaxed);
        assert_eq!(shared.look(seen), seen, "it is told that no block has come");
        drop(held);
        assert_eq!(idles.load(Ordering::Relaxed), before, "nothing was done to an engine in use");
    }

    #[test]
    fn the_idle_thread_takes_what_the_controller_sends_while_the_host_renders_nothing_and_ends_when_it_is_told_to() {
        use std::sync::atomic::Ordering;
        let (shared, idles, _) = counting();
        let idle = engine::spawn_idle(shared.clone(), Duration::from_millis(1)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while idles.load(Ordering::Relaxed) < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(idles.load(Ordering::Relaxed) >= 3, "it looked while the host did not render");
        shared.stop();
        idle.join().unwrap();
        let ended = idles.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(idles.load(Ordering::Relaxed), ended, "and it is gone");
    }

    #[test]
    fn a_host_that_gives_one_channel_hears_the_left_and_one_that_gives_more_gets_the_right_again() {
        let (_host, mut audio) = instrument();
        play(&mut audio, &[at(0, NOTE_ON)], None, 4);
        let mut mono = vec![0.0f32; BLOCK];
        audio.process(&[], &[], &mut [&mut mono]);
        assert!(mono.iter().any(|&sample| sample.abs() > 0.01), "a mono host has the left side");
        let (mut a, mut b, mut c) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        audio.process(&[], &[], &mut [&mut a, &mut b, &mut c]);
        assert_eq!(b, c, "a third channel is the second again");
    }

    #[cfg(feature = "web")]
    mod page {
        use std::net::SocketAddr;

        use serde_json::{Value, json};

        use super::*;
        use crate::web::testclient::{connect, said, tell, wait_until};

        /// Where the page of `host` is.
        fn address(host: &Host) -> SocketAddr {
            let url = host.url().expect("the page is served");
            url.trim_start_matches("http://").trim_end_matches('/').parse().unwrap_or_else(|error| panic!("{url}: {error}"))
        }

        /// The answer to the request `req_id`: the next `ack` or `rejected` for it.
        async fn answer(client: &mut crate::web::testclient::Client, req_id: u64) -> Value {
            loop {
                let message = said(client).await;
                if message["reqId"] == req_id {
                    return message;
                }
            }
        }

        #[tokio::test]
        async fn the_page_of_a_plugin_changes_what_the_host_hears_and_what_a_save_gives_and_its_keys_are_heard_and_the_host_is_an_input() {
            let mut host = Host::new(Kind::Instrument, true);
            let mut audio = host.activate(RATE, BLOCK as u32).unwrap();
            let mut client = connect(address(&host)).await;
            assert_eq!(said(&mut client).await["type"], "hello");
            // (What a client that subscribes is told: the snapshot, the catalog, what can be undone, and the MIDI inputs that are open, of which the host is one.)
            tell(&mut client, json!({"type": "subscribe"})).await;
            let snapshot = said(&mut client).await;
            assert_eq!(snapshot["type"], "snapshot");
            assert_eq!(said(&mut client).await["type"], "catalog");
            assert_eq!(said(&mut client).await["type"], "history");
            let inputs = said(&mut client).await;
            assert_eq!(inputs["type"], "inputs");
            assert!(inputs.to_string().contains(&format!("\"{HOST_DEVICE}\"")), "the host is a device that notes come from: {inputs}");
            let nodes = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap();
            let osc = nodes.iter().find(|node| node["name"] == "Oscillator").expect("the oscillator");
            let volume = osc["plugin"]["params"].as_array().unwrap().iter().find(|param| param["name"] == "volume").unwrap();
            let (node, param) = (osc["id"].as_u64().unwrap(), volume["index"].as_u64().unwrap());

            // What the host plays is heard; the volume turned to nothing in the page, the same note is silent; and the text that a save gives has it.
            let (_, heard) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
            assert!(heard > 0.05, "{heard}");
            play(&mut audio, &[at(0, NOTE_OFF)], None, 40);
            tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": node, "param": param, "value": 0.0}})).await;
            assert_eq!(answer(&mut client, 1).await["type"], "ack");
            // (The oscillator eases its volume over a few milliseconds.)
            tokio::time::sleep(Duration::from_millis(100)).await;
            play(&mut audio, &[], None, 20);
            let (_, silent) = play(&mut audio, &[at(0, NOTE_ON)], None, 8);
            assert!(silent < 1e-3, "the page turned the volume to 0: {silent}");
            let text = host.session_text().unwrap();
            assert!(text.contains("volume = 0"), "a save has what the page changed:\n{text}");

            // A key pressed in the page is heard in the host's blocks too: it is the keys' source, and the host is another.
            tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "setParam", "node": node, "param": param, "value": 1.0}})).await;
            assert_eq!(answer(&mut client, 2).await["type"], "ack");
            tokio::time::sleep(Duration::from_millis(100)).await;
            play(&mut audio, &[at(0, NOTE_OFF)], None, 40);
            tell(&mut client, json!({"type": "noteOn", "note": 60, "velocity": 100})).await;
            wait_until("a key of the page is heard in the host's blocks", || play(&mut audio, &[], None, 2).1 > 0.05).await;
        }

        #[tokio::test]
        async fn a_page_is_served_for_each_instance_on_a_port_of_its_own_and_none_when_none_is_asked_for() {
            let mut first = Host::new(Kind::Instrument, true);
            let mut second = Host::new(Kind::Effect, true);
            let mut plain = Host::new(Kind::Instrument, false);
            let _a = first.activate(RATE, BLOCK as u32).unwrap();
            let _b = second.activate(RATE, BLOCK as u32).unwrap();
            let _c = plain.activate(RATE, BLOCK as u32).unwrap();
            assert_ne!(address(&first), address(&second), "each instance has its own port");
            assert!(plain.url().is_none());
            assert!(Host::new(Kind::Instrument, true).url().is_none(), "none before it is activated");
        }
    }
}
