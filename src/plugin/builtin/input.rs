//! The audio input (`builtin:input`, step 99): the computer's microphone, or another input of the sound card, as an instrument that cables of audio go from.
//!
//! It is an instrument like the oscillator, and what it plays is what the sound card hears: a lane that has it is made with its way out cut (`Plugin::starts_bare`), so that nothing of it is
//! heard until an audio cable is drawn from it to an effect, a group's bus or the mix (a microphone that goes to the mix by itself is heard through the speakers that it listens to). The lane
//! gives it the rest of what every lane has: a volume, a mute and a solo, a meter and a scope, effects in its chain, and modulators on its parameters.
//!
//! *Where the sound comes from.* A thread of its own (`Capture`) opens the input stream (cpal) and puts what it hears in a ring of frames (`InputRing`: lock-free, one writer and one reader),
//! which `process` reads a block at a time on the audio thread (`RingReader`: it waits for a few blocks to be in the ring before it begins, and begins again after a dropout, so that the clocks
//! of the two devices, which are never quite the same, do not make a crackle of every block, and what is too far behind is skipped, so that the delay stays short). The thread is made with
//! the plugin and does nothing until the first block has been rendered (`Capture::start`: a catalog scan or `tang describe` that makes the plugin does not open the microphone, and the
//! system does not ask for a permission that nobody has asked for) and closes the stream when no block has been rendered for a second (a lane that was taken out and is kept by the
//! history, or an engine that is stopped, does not keep the microphone open). A device that runs at another rate than the engine is resampled (linearly), a device of one channel is put in
//! both, and one of more has its first two.
//!
//! *Where the sound comes from in tests and in CI.* Under `--audio-device none` and in the tests there is no device: the plugin plays a quiet sine of 220 Hz (`TONE_HZ`, `TONE_LEVEL`) instead,
//! so that what a cable does to what the input puts out can be heard by the oracle, the tests and a browser test of the real program.
//!
//! *Where the sound comes from in a host.* Tang inside a DAW (step 113) opens no sound card: the plugin plays what the host gives it to hear, the audio that comes into the plugin's own `process` (`HostInput`, which the host's thread fills in
//! just before the engine renders the block, and the plugin reads in it), or silence for a plugin that has no audio input.
//!
//! The parameters are `gain` (0 to 4, linear), `pan` (a balance: the centre leaves both sides as they are) and `device` (a choice: the system's default input, or one of the inputs that the
//! system had when the plugin was made, by its place in that list: a session that is opened on another machine, with other inputs, has what is at that place there).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::plugin::{ParameterInfo, Plugin, Preset};

pub const NAME: &str = "Audio Input";
pub const ID: &str = "builtin:input";
pub const PARAM_COUNT: usize = 3;

const GAIN: u32 = 0;
const PAN: u32 = 1;
const DEVICE: u32 = 2;

/// The largest gain.
const GAIN_MAX: f32 = 4.0;

/// The name of the choice of the system's default input, the first of the `device` parameter's labels.
const DEFAULT_DEVICE: &str = "Default input";

/// The name of the one input that an audio input made for a host has.
const HOST_INPUT: &str = "Host input";

/// How long a change of gain or pan is gone through, in seconds.
const SMOOTH_SEC: f32 = 0.005;

/// The pitch and the level of what the input plays where there is no device (under `--audio-device none` and in the tests).
pub const TONE_HZ: f64 = 220.0;
pub const TONE_LEVEL: f32 = 0.25;

/// Whether the plugins that are made from now on play the test tone and open no device (`--audio-device none`: `use_test_tone`).
static TEST_TONE: AtomicBool = AtomicBool::new(false);

/// Make the audio inputs that are made from now on play a test tone and open no device, as they do in the tests: `--audio-device none` asks for it, for CI and for headless runs, where
/// there is no sound card to listen to.
pub fn use_test_tone(on: bool) {
    TEST_TONE.store(on, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// The ring that the capture thread writes and the audio thread reads
// ---------------------------------------------------------------------------

/// How many frames the ring holds (a power of two): about 0.7 s at 48 kHz, far more than is ever waiting.
const RING_FRAMES: usize = 1 << 15;

/// How many frames may be waiting for the reader before it skips the oldest (about 170 ms at 48 kHz): a device whose clock runs faster than the engine's would be later and later.
const MAX_BACKLOG: usize = 8192;

/// The fewest frames that are waiting when the reader begins (and begins again after a dropout), and it is two blocks if that is more.
const MIN_PRIME: usize = 768;

/// A ring of stereo frames: the capture thread is the one writer (`push`), and one `RingReader` the one reader. Every cell is an atomic of its own, so that nothing is locked, nothing is
/// allocated after it is made, and a number that is read while it is written is a whole number of one write or the other (the reader keeps far enough behind that this does not happen).
pub(crate) struct InputRing {
    cells: Box<[AtomicU32]>,
    /// How many frames have been written, ever.
    written: AtomicU64,
}

impl InputRing {
    pub(crate) fn new() -> Self {
        InputRing { cells: (0..2 * RING_FRAMES).map(|_| AtomicU32::new(0)).collect(), written: AtomicU64::new(0) }
    }

    /// Put a frame in (the writer's: one thread only).
    pub(crate) fn push(&self, left: f32, right: f32) {
        let written = self.written.load(Ordering::Relaxed);
        let at = (written as usize & (RING_FRAMES - 1)) * 2;
        self.cells[at].store(left.to_bits(), Ordering::Relaxed);
        self.cells[at + 1].store(right.to_bits(), Ordering::Relaxed);
        self.written.store(written + 1, Ordering::Release);
    }

    fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    fn frame(&self, number: u64) -> (f32, f32) {
        let at = (number as usize & (RING_FRAMES - 1)) * 2;
        (f32::from_bits(self.cells[at].load(Ordering::Relaxed)), f32::from_bits(self.cells[at + 1].load(Ordering::Relaxed)))
    }
}

/// The reader of an `InputRing`: the audio thread's, which gives the plugin its blocks.
pub(crate) struct RingReader {
    ring: Arc<InputRing>,
    /// The number of the next frame to read.
    read: u64,
    /// Whether there is enough waiting for a block to be read from it: false at the start and after a dropout, until the backlog is the prime again.
    running: bool,
}

impl RingReader {
    pub(crate) fn new(ring: Arc<InputRing>) -> Self {
        let read = ring.written();
        RingReader { ring, read, running: false }
    }

    /// Fill `left` and `right` with the next frames of the ring. What has not arrived is silence: nothing is read until there is a backlog of two blocks (a few more if the blocks are small),
    /// and after a dropout it waits for that again. Frames that are too far behind (more than `MAX_BACKLOG`) are skipped, to the prime. How many frames of the block are silence for want of
    /// a sound that had not come (the start counts: the first blocks wait for the backlog), which a test asks.
    pub(crate) fn pull(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let frames = left.len().min(right.len());
        let prime = (2 * frames).max(MIN_PRIME);
        let written = self.ring.written();
        let mut waiting = written.saturating_sub(self.read);
        if waiting > MAX_BACKLOG as u64 {
            self.read = written - prime as u64;
            waiting = prime as u64;
        }
        if !self.running {
            if waiting < prime as u64 {
                left[..frames].fill(0.0);
                right[..frames].fill(0.0);
                return frames;
            }
            self.running = true;
        }
        let take = (waiting as usize).min(frames);
        for i in 0..take {
            let (l, r) = self.ring.frame(self.read + i as u64);
            left[i] = l;
            right[i] = r;
        }
        left[take..frames].fill(0.0);
        right[take..frames].fill(0.0);
        self.read += take as u64;
        if take < frames {
            self.running = false;
        }
        frames - take
    }
}

// ---------------------------------------------------------------------------
// What a host gives to hear
// ---------------------------------------------------------------------------

/// The audio that comes into a host's `process` for the block that is being rendered (step 113): the host's thread puts it here (`put`) just before it has the engine render the block, and the audio input that was made for the host plays it (`take`) in that
/// block. Both are the thread that renders (the audio input is run by the engine, which the host's thread calls), so there is no waiting: every cell is an atomic only so that the audio input, which is `Send`, can be given the handle that the host's thread has.
pub struct HostInput {
    /// The left side of the block, then the right.
    cells: Box<[AtomicU32]>,
    /// How many frames each side has room for.
    capacity: usize,
    /// How many frames were put for the block that is being rendered: none if nothing was.
    frames: std::sync::atomic::AtomicUsize,
}

impl HostInput {
    /// A feed for blocks of up to `max_frames` frames.
    pub fn new(max_frames: usize) -> Self {
        HostInput { cells: (0..2 * max_frames).map(|_| AtomicU32::new(0)).collect(), capacity: max_frames, frames: std::sync::atomic::AtomicUsize::new(0) }
    }

    /// The audio of the block: its left and right sides (a host that gives one channel has it for both). Frames beyond what the feed has room for are left out.
    pub fn put(&self, left: &[f32], right: &[f32]) {
        let frames = left.len().min(right.len()).min(self.capacity);
        for (cell, sample) in self.cells[..frames].iter().zip(left) {
            cell.store(sample.to_bits(), Ordering::Relaxed);
        }
        for (cell, sample) in self.cells[self.capacity..self.capacity + frames].iter().zip(right) {
            cell.store(sample.to_bits(), Ordering::Relaxed);
        }
        self.frames.store(frames, Ordering::Release);
    }

    /// No audio comes in for the next block: it is silence.
    pub fn clear(&self) {
        self.frames.store(0, Ordering::Release);
    }

    /// Fill `left` and `right` with what was put for this block, and the rest of them (all of them, if nothing was) with silence.
    fn take(&self, left: &mut [f32], right: &mut [f32]) {
        let frames = self.frames.load(Ordering::Acquire).min(left.len()).min(right.len());
        for (sample, cell) in left.iter_mut().zip(&self.cells[..frames]) {
            *sample = f32::from_bits(cell.load(Ordering::Relaxed));
        }
        for (sample, cell) in right.iter_mut().zip(&self.cells[self.capacity..self.capacity + frames]) {
            *sample = f32::from_bits(cell.load(Ordering::Relaxed));
        }
        left[frames..].fill(0.0);
        right[frames..].fill(0.0);
    }
}

// ---------------------------------------------------------------------------
// A device that runs at another rate
// ---------------------------------------------------------------------------

/// A linear resampler of stereo frames, from the rate of a device to the rate of the engine: each frame that comes in makes the frames of the output that lie before it.
pub(crate) struct Resampler {
    /// How many input frames there are for each output frame.
    step: f64,
    /// Where the next output frame lies, from the previous input frame (0) to the one that comes next (1).
    position: f64,
    previous: [f32; 2],
    primed: bool,
}

impl Resampler {
    pub(crate) fn new(from: f64, to: f64) -> Self {
        Resampler { step: from / to, position: 0.0, previous: [0.0; 2], primed: false }
    }

    /// Take the next input frame, and say each output frame that it completes.
    pub(crate) fn push(&mut self, frame: [f32; 2], out: &mut impl FnMut([f32; 2])) {
        if !self.primed {
            self.previous = frame;
            self.primed = true;
            return;
        }
        while self.position < 1.0 {
            let t = self.position as f32;
            out([self.previous[0] + (frame[0] - self.previous[0]) * t, self.previous[1] + (frame[1] - self.previous[1]) * t]);
            self.position += self.step;
        }
        self.position -= 1.0;
        self.previous = frame;
    }
}

/// A frame of a device that has `channels` channels, as two: one channel is both, and more have their first two.
pub(crate) fn stereo_of(frame: &[f32]) -> (f32, f32) {
    match frame {
        [] => (0.0, 0.0),
        [only] => (*only, *only),
        [left, right, ..] => (*left, *right),
    }
}

// ---------------------------------------------------------------------------
// The thread that listens to the device
// ---------------------------------------------------------------------------

/// What the plugin asks of the thread that listens.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// The first block has been rendered: the microphone may be opened.
    Start,
    /// Listen to this input instead (none: the system's default).
    Device(Option<String>),
}

/// How the thread that listens keeps time (fields, so that a test can run it in milliseconds).
#[derive(Clone, Copy)]
struct Timing {
    /// How long it waits for a command before it looks at whether blocks are still being rendered.
    look_every: Duration,
    /// How long without a block before the stream is closed.
    idle_after: Duration,
    /// How long after a stream that could not be opened, or that failed, before it is tried again.
    retry_after: Duration,
}

const TIMING: Timing = Timing { look_every: Duration::from_millis(200), idle_after: Duration::from_secs(1), retry_after: Duration::from_secs(3) };

/// The plugin's end of the thread that listens to the device: it says what to listen to, and that blocks are still being rendered (`beat`). Dropping it ends the thread, which closes the stream
/// (the thread is not waited for: it may be in the middle of opening a device, or of a question that the system asks a person).
pub(crate) struct Capture {
    commands: Option<Sender<Command>>,
    beats: Arc<AtomicU64>,
}

impl Capture {
    /// Make the thread, which does nothing until `start`.
    fn spawn(ring: Arc<InputRing>, engine_rate: u32) -> Capture {
        let (commands, receive) = crossbeam_channel::bounded(8);
        let beats = Arc::new(AtomicU64::new(0));
        let seen = beats.clone();
        let spawned = std::thread::Builder::new().name("tang-audio-input".to_string()).spawn(move || {
            let failed = Arc::new(AtomicBool::new(false));
            listen(&receive, &seen, &failed, TIMING, |name| open(name, engine_rate, &ring, &failed).map(|opened| opened.stream));
        });
        if let Err(why) = spawned {
            log::warn!("Audio input: the thread that listens could not be made ({why})");
        }
        Capture { commands: Some(commands), beats }
    }

    /// A `Capture` that has no thread, and the commands that it was given, for a test.
    #[cfg(test)]
    pub(crate) fn detached() -> (Capture, Receiver<Command>) {
        let (commands, receive) = crossbeam_channel::bounded(8);
        (Capture { commands: Some(commands), beats: Arc::new(AtomicU64::new(0)) }, receive)
    }

    fn start(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.try_send(Command::Start);
        }
    }

    fn listen_to(&self, device: Option<String>) {
        if let Some(commands) = &self.commands {
            let _ = commands.try_send(Command::Device(device));
        }
    }

    /// A block has been rendered.
    fn beat(&self) {
        self.beats.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn beats(&self) -> u64 {
        self.beats.load(Ordering::Relaxed)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // (The thread sees the channel closed and ends.)
        self.commands.take();
    }
}

/// What the thread does: wait for `Start`, open the stream (`open`, which is given the name of the device that is wanted, none for the system's default) while blocks are being rendered, close
/// it when they are not (and when it is asked for another device), and try again, a few seconds later, when it could not be opened or `failed` says it has failed. It ends when the plugin's end
/// of the channel is dropped, and the stream is closed with it.
fn listen<S>(commands: &Receiver<Command>, beats: &AtomicU64, failed: &AtomicBool, timing: Timing, mut open: impl FnMut(Option<&str>) -> Result<S, String>) {
    let mut wanted: Option<String> = None;
    let mut started = false;
    let mut stream: Option<S> = None;
    let mut last_beats = beats.load(Ordering::Relaxed);
    let mut last_block = Instant::now();
    let mut next_try = Instant::now();
    loop {
        match commands.recv_timeout(timing.look_every) {
            Ok(Command::Start) => started = true,
            Ok(Command::Device(device)) => {
                wanted = device;
                stream = None;
                next_try = Instant::now();
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        let seen = beats.load(Ordering::Relaxed);
        if seen != last_beats {
            last_beats = seen;
            last_block = now;
        }
        if failed.swap(false, Ordering::Relaxed) {
            stream = None;
            next_try = now + timing.retry_after;
        }
        if !started || now.duration_since(last_block) >= timing.idle_after {
            stream = None;
            continue;
        }
        if stream.is_none() && now >= next_try {
            match open(wanted.as_deref()) {
                Ok(opened) => stream = Some(opened),
                Err(why) => {
                    log::warn!("Audio input: {why}");
                    next_try = now + timing.retry_after;
                }
            }
        }
    }
}

/// The names of the inputs that the system has, in its order.
fn device_names() -> Vec<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    // (Probing the devices can write to stderr, which is nobody's business.)
    let _quiet = crate::tty::StderrSilencer::new();
    crate::audio::input_host().input_devices().map(|devices| devices.filter_map(|device| device.name().ok()).collect()).unwrap_or_default()
}

/// What to say when an input gives only exact zeros, which a microphone does not (a room has a noise floor). On macOS it is nearly always the permission, which belongs to the terminal that runs
/// Tang, and a permission that was refused is answered with silence and no error, for ever, until it is changed: `__CFBundleIdentifier` names that terminal for the programs it starts, and
/// `TERM_PROGRAM` says what it calls itself. A device that is muted, or is not plugged in, does the same.
fn silence_advice(bundle: Option<&str>, terminal: Option<&str>) -> String {
    let app = match (terminal, bundle) {
        (Some(name), Some(id)) => format!("the terminal that runs Tang ({name}, {id})"),
        (Some(name), None) => format!("the terminal that runs Tang ({name})"),
        (None, Some(id)) => format!("the terminal that runs Tang ({id})"),
        (None, None) => "the terminal that runs Tang".to_string(),
    };
    let again = bundle.map_or(String::new(), |id| format!(" If it is on already, or is not in the list, ask again with `tccutil reset Microphone {id}` and run this again: macOS asks, and keeps the answer."));
    format!("on macOS, {app} may not be allowed to use the microphone, which the system answers with silence and no error: open System Settings > Privacy & Security > Microphone and turn it on.{again} Otherwise the input is muted, or is a device that is not plugged in or has nothing to hear.")
}

/// `silence_advice` for the terminal that this program runs in.
fn silence_advice_here() -> String {
    silence_advice(std::env::var("__CFBundleIdentifier").ok().as_deref(), std::env::var("TERM_PROGRAM").ok().as_deref())
}

/// Notices an input that gives nothing but exact zeros for a while, as one that the system will not let a program hear does (a refused permission is no error): `push` is true the one time that
/// `limit` frames of 0 have come in a row, and the watch is set again by the first sound.
struct SilenceWatch {
    zeros: u64,
    limit: u64,
    said: bool,
}

impl SilenceWatch {
    fn new(limit: u64) -> Self {
        SilenceWatch { zeros: 0, limit, said: false }
    }

    fn push(&mut self, left: f32, right: f32) -> bool {
        if left != 0.0 || right != 0.0 {
            self.zeros = 0;
            self.said = false;
            return false;
        }
        self.zeros += 1;
        if self.zeros >= self.limit && !self.said {
            self.said = true;
            return true;
        }
        false
    }
}

/// How many seconds of exact zeros in a row an open input may give before the log says so.
const SILENCE_SAID_AFTER_SEC: u64 = 3;

/// An input stream that is open, and what it listens to.
struct Opened {
    stream: cpal::Stream,
    device: String,
    channels: usize,
    rate: u32,
}

/// Open an input stream on the device `name` (the system's default for none: a name is looked for as it is, and then as a part of a name, in any case), whose frames go into `ring` at the
/// rate of the engine. `failed` is set when the stream says it has failed.
fn open(name: Option<&str>, engine_rate: u32, ring: &Arc<InputRing>, failed: &Arc<AtomicBool>) -> Result<Opened, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = crate::audio::input_host();
    let device = match name {
        None => host.default_input_device().ok_or_else(|| "there is no input device".to_string())?,
        Some(wanted) => {
            let devices: Vec<cpal::Device> = host.input_devices().map_err(|why| why.to_string())?.collect();
            let called = |device: &cpal::Device| device.name().unwrap_or_default();
            let lower = wanted.to_lowercase();
            let at = devices
                .iter()
                .position(|device| called(device) == wanted)
                .or_else(|| devices.iter().position(|device| called(device).to_lowercase().contains(&lower)))
                .ok_or_else(|| format!("there is no input device called {wanted}"))?;
            devices.into_iter().nth(at).ok_or_else(|| format!("there is no input device called {wanted}"))?
        }
    };
    let supported = device.default_input_config().map_err(|why| why.to_string())?;
    let (channels, rate, format) = (usize::from(supported.channels()), supported.sample_rate().0, supported.sample_format());
    let config = supported.config();
    let mut resampler = (rate != engine_rate).then(|| Resampler::new(f64::from(rate), f64::from(engine_rate)));
    let device_name = device.name().unwrap_or_default();
    let sink = {
        let ring = ring.clone();
        let mut watch = SilenceWatch::new(SILENCE_SAID_AFTER_SEC * u64::from(rate));
        // (The words are made now: the callback is the system's, and makes only what it has to.)
        let silent = format!("Audio input: \"{device_name}\" has given nothing but silence (exact zeros) for {SILENCE_SAID_AFTER_SEC} s: {}", silence_advice_here());
        move |left: f32, right: f32| {
            if watch.push(left, right) {
                log::warn!("{silent}");
            }
            match &mut resampler {
                None => ring.push(left, right),
                Some(resampler) => resampler.push([left, right], &mut |frame| ring.push(frame[0], frame[1])),
            }
        }
    };
    let on_error = {
        let failed = failed.clone();
        move |why: cpal::StreamError| {
            log::warn!("Audio input stream: {why}");
            failed.store(true, Ordering::Relaxed);
        }
    };
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, channels, sink, on_error),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, channels, sink, on_error),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, channels, sink, on_error),
        cpal::SampleFormat::I32 => build::<i32>(&device, &config, channels, sink, on_error),
        other => return Err(format!("the input's sample format ({other:?}) is not one that is read")),
    }
    .map_err(|why| why.to_string())?;
    stream.play().map_err(|why| why.to_string())?;
    log::info!("Audio input: {device_name} ({channels} channel(s), {rate} Hz{})", if rate == engine_rate { "" } else { ", resampled" });
    Ok(Opened { stream, device: device_name, channels, rate })
}

/// `tang listen`: open the input `device` (the system's default for none) for `seconds`, play nothing, and say what it hears, so that a microphone that is not heard by Tang can be told
/// from one that the system does not give it (a permission that was refused, a device that is muted or not plugged in). It is the stream that the plugin opens, read at a place of its own.
pub fn probe(device: Option<&str>, seconds: f32, engine_rate: u32) -> anyhow::Result<()> {
    let ring = Arc::new(InputRing::new());
    let failed = Arc::new(AtomicBool::new(false));
    println!("Listening to {} for {seconds} s (nothing is played) ...", device.map_or("the default input".to_string(), |name| format!("\"{name}\"")));
    let opened = open(device, engine_rate, &ring, &failed).map_err(|why| anyhow::anyhow!("could not open the input: {why}"))?;
    println!("  Opened \"{}\": {} channel(s), {} Hz{}", opened.device, opened.channels, opened.rate, if opened.rate == engine_rate { String::new() } else { format!(", resampled to {engine_rate} Hz") });
    let started = Instant::now();
    let mut seen = ring.written();
    let (mut peak, mut frames, mut moved) = (0.0f32, 0u64, false);
    let (mut window_peak, mut window_frames, mut next_report) = (0.0f32, 0u64, 0.5f32);
    while started.elapsed().as_secs_f32() < seconds {
        std::thread::sleep(Duration::from_millis(25));
        let written = ring.written();
        let waiting = written.saturating_sub(seen).min(RING_FRAMES as u64);
        for number in written - waiting..written {
            let (left, right) = ring.frame(number);
            let loudest = left.abs().max(right.abs());
            moved |= loudest > 0.0;
            window_peak = window_peak.max(loudest);
        }
        frames += waiting;
        window_frames += waiting;
        seen = written;
        if started.elapsed().as_secs_f32() >= next_report {
            println!("  {:>4.1} s  {:>6} frames  {}", next_report, window_frames, level_in_words(window_peak));
            peak = peak.max(window_peak);
            (window_peak, window_frames, next_report) = (0.0, 0, next_report + 0.5);
        }
    }
    peak = peak.max(window_peak);
    drop(opened);
    println!();
    if failed.load(Ordering::Relaxed) {
        println!("The stream failed while it was listening (the line above, `Audio input stream: …`, says why).");
    }
    if frames == 0 {
        println!("Nothing arrived from the input in {seconds} s: the system did not start it. Is the device plugged in and not in use by something that has it for itself?");
        anyhow::bail!("no sound arrived from the input");
    }
    println!("{frames} frames arrived ({:.0} per second); the loudest sample was {}.", frames as f32 / seconds, level_in_words(peak));
    if !moved {
        println!();
        println!("Every sample was exactly 0: the input gave silence, which is not what a microphone gives (a room has a noise floor). Most likely: {}", silence_advice_here());
        anyhow::bail!("the input gave silence");
    }
    println!("The input works: Tang hears it. (Speak, or tap the microphone, while this runs to see the level move.)");
    Ok(())
}

/// A level as a person reads it: decibels below full scale, or "silence" for none.
fn level_in_words(peak: f32) -> String {
    if peak <= 0.0 {
        "silence (exactly 0)".to_string()
    } else {
        format!("peak {:.1} dB", 20.0 * peak.log10())
    }
}

/// The input stream of `device`, whose samples of type `T` go to `sink` as frames of two.
fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut sink: impl FnMut(f32, f32) + Send + 'static,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::Sample;
    use cpal::traits::DeviceTrait;
    let mut frame = vec![0.0f32; channels.max(1)];
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            for samples in data.chunks(channels.max(1)) {
                for (slot, sample) in frame.iter_mut().zip(samples) {
                    *slot = f32::from_sample(*sample);
                }
                let (left, right) = stereo_of(&frame[..samples.len().min(frame.len())]);
                sink(left, right);
            }
        },
        on_error,
        None,
    )
}

// ---------------------------------------------------------------------------
// The plugin
// ---------------------------------------------------------------------------

/// What the plugin plays.
enum Source {
    /// A quiet sine, in place of a device.
    Tone { phase: f64 },
    /// What the sound card hears, from the ring that the capture thread fills (no thread in a test that fills the ring itself).
    Device { reader: RingReader, capture: Option<Capture> },
    /// What the host that Tang is inside of gives it to hear in each block.
    Host(Arc<HostInput>),
}

pub struct AudioInput {
    sample_rate: f32,
    source: Source,
    gain: f32,
    pan: f32,
    /// The place of the input in `devices`, 0 being the system's default.
    device: u32,
    /// The inputs that the system had when the plugin was made.
    devices: Vec<String>,
    /// Gain and pan as they are now, on their way to what is set (in 64 bits: a one-pole in 32 stops short of what it follows by as much as the steps of its number are over its coefficient).
    gain_now: f64,
    pan_now: f64,
    /// Whether the first block has been rendered.
    started: bool,
}

impl AudioInput {
    pub fn new(sample_rate: f32) -> Self {
        if cfg!(test) || TEST_TONE.load(Ordering::Relaxed) {
            return Self::with_source(sample_rate, Source::Tone { phase: 0.0 }, Vec::new());
        }
        let ring = Arc::new(InputRing::new());
        let capture = Capture::spawn(ring.clone(), sample_rate.round() as u32);
        Self::with_source(sample_rate, Source::Device { reader: RingReader::new(ring), capture: Some(capture) }, device_names())
    }

    /// An input that plays what a host gives it to hear: no sound card is opened.
    pub fn with_host(sample_rate: f32, feed: Arc<HostInput>) -> Self {
        Self::with_source(sample_rate, Source::Host(feed), Vec::new())
    }

    fn with_source(sample_rate: f32, source: Source, devices: Vec<String>) -> Self {
        AudioInput { sample_rate, source, gain: 1.0, pan: 0.0, device: 0, devices, gain_now: 1.0, pan_now: 0.0, started: false }
    }

    /// An input that plays what is put in `ring`, with the thread that would fill it in `capture` (none in a test), as an input made for a device is.
    #[cfg(test)]
    pub(crate) fn from_ring(sample_rate: f32, ring: Arc<InputRing>, capture: Option<Capture>, devices: Vec<String>) -> Self {
        Self::with_source(sample_rate, Source::Device { reader: RingReader::new(ring), capture }, devices)
    }

    /// The labels of the `device` parameter: the default input, and the inputs that the system had.
    fn labels(&self) -> Vec<String> {
        let first = if matches!(self.source, Source::Host(_)) { HOST_INPUT } else { DEFAULT_DEVICE };
        std::iter::once(first.to_string()).chain(self.devices.iter().cloned()).collect()
    }

    fn device_called(&self, place: u32) -> Option<String> {
        place.checked_sub(1).and_then(|at| self.devices.get(at as usize)).cloned()
    }
}

impl Plugin for AudioInput {
    fn name(&self) -> &str {
        NAME
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_input_count(&self) -> usize {
        0
    }

    fn audio_output_count(&self) -> usize {
        2
    }

    fn starts_bare(&self) -> bool {
        true
    }

    fn takes_notes(&self) -> bool {
        false
    }

    fn process(&mut self, _midi_events: &[(u64, [u8; 3])], _audio_in: &[&[f32]], audio_out: &mut [&mut [f32]]) -> anyhow::Result<()> {
        let [left, right, ..] = audio_out else {
            return Ok(());
        };
        let frames = left.len().min(right.len());
        match &mut self.source {
            Source::Tone { phase } => {
                let step = TONE_HZ / f64::from(self.sample_rate);
                for i in 0..frames {
                    let sample = TONE_LEVEL * (std::f64::consts::TAU * *phase).sin() as f32;
                    left[i] = sample;
                    right[i] = sample;
                    *phase = (*phase + step).fract();
                }
            }
            Source::Device { reader, capture } => {
                if let Some(capture) = capture {
                    if !self.started {
                        capture.start();
                    }
                    capture.beat();
                }
                reader.pull(left, right);
            }
            Source::Host(feed) => feed.take(left, right),
        }
        self.started = true;
        // Gain and balance, gone to over a few milliseconds so that a modulator that sets them a block at a time does not crackle.
        let coefficient = 1.0 - (-1.0 / (f64::from(SMOOTH_SEC) * f64::from(self.sample_rate))).exp();
        for i in 0..frames {
            self.gain_now += coefficient * (f64::from(self.gain) - self.gain_now);
            self.pan_now += coefficient * (f64::from(self.pan) - self.pan_now);
            let side_l = 1.0 - self.pan_now.max(0.0);
            let side_r = 1.0 + self.pan_now.min(0.0);
            left[i] *= (self.gain_now * side_l) as f32;
            right[i] *= (self.gain_now * side_r) as f32;
        }
        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        let labels = self.labels();
        let places = labels.len() as u32;
        vec![
            ParameterInfo { index: GAIN, name: "gain".into(), min: 0.0, max: GAIN_MAX, default: 1.0, ..Default::default() },
            ParameterInfo { index: PAN, name: "pan".into(), min: -1.0, max: 1.0, default: 0.0, ..Default::default() },
            ParameterInfo { index: DEVICE, name: "device".into(), min: 0.0, max: (places - 1) as f32, default: 0.0, labels: Some(labels), step_count: places - 1, ..Default::default() },
        ]
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            GAIN => Some(self.gain),
            PAN => Some(self.pan),
            DEVICE => Some(self.device as f32),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        if !value.is_finite() {
            return Ok(());
        }
        match index {
            GAIN => {
                self.gain = value.clamp(0.0, GAIN_MAX);
                if !self.started {
                    // (Before the first block there is nothing to glide from: a session that is loaded with a gain of 0.3 does not begin with a few milliseconds of 1.)
                    self.gain_now = f64::from(self.gain);
                }
            }
            PAN => {
                self.pan = value.clamp(-1.0, 1.0);
                if !self.started {
                    self.pan_now = f64::from(self.pan);
                }
            }
            DEVICE => {
                let place = (value.round().max(0.0) as u32).min(self.devices.len() as u32);
                if place != self.device {
                    self.device = place;
                    let name = self.device_called(place);
                    if let Source::Device { capture: Some(capture), .. } = &self.source {
                        capture.listen_to(name);
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        Vec::new()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::bail!("{NAME} has no preset called {id:?}")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// One block of `frames` from `plugin`, as a left and a right side.
    fn block(plugin: &mut AudioInput, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut left, mut right) = (vec![0.0; frames], vec![0.0; frames]);
        plugin.process(&[], &[], &mut [&mut left, &mut right]).unwrap();
        (left, right)
    }

    /// An input that plays what is put in the ring, with no thread, and the commands that it gives the thread.
    fn from_a_ring(devices: &[&str]) -> (AudioInput, Arc<InputRing>, Receiver<Command>) {
        let ring = Arc::new(InputRing::new());
        let (capture, commands) = Capture::detached();
        (AudioInput::from_ring(48000.0, ring.clone(), Some(capture), devices.iter().map(|name| name.to_string()).collect()), ring, commands)
    }

    fn put_in(ring: &InputRing, from: usize, to: usize) {
        for i in from..to {
            ring.push(i as f32, -(i as f32));
        }
    }

    #[test]
    fn the_ring_gives_the_frames_back_in_the_order_they_were_put_in_and_goes_round() {
        let ring = Arc::new(InputRing::new());
        let mut reader = RingReader::new(ring.clone());
        let (mut left, mut right) = (vec![0.0; 256], vec![0.0; 256]);
        let mut next = 0usize;
        let mut written = 0usize;
        // Several times round the ring: a block of 256 in for each block of 256 out, with a backlog of 1,024 to begin with.
        put_in(&ring, 0, 1024);
        written += 1024;
        for _ in 0..(3 * RING_FRAMES / 256) {
            put_in(&ring, written, written + 256);
            written += 256;
            let missing = reader.pull(&mut left, &mut right);
            assert_eq!(missing, 0);
            for i in 0..256 {
                assert_eq!((left[i], right[i]), ((next + i) as f32, -((next + i) as f32)));
            }
            next += 256;
        }
        assert!(next > 2 * RING_FRAMES, "the ring has been gone round more than twice");
    }

    #[test]
    fn a_reader_waits_for_a_backlog_before_it_begins_and_again_after_a_dropout() {
        let ring = Arc::new(InputRing::new());
        let mut reader = RingReader::new(ring.clone());
        let (mut left, mut right) = (vec![9.0; 256], vec![9.0; 256]);
        // Less than the prime (the larger of two blocks and 768): silence, and all of it is missing.
        put_in(&ring, 0, 300);
        assert_eq!(reader.pull(&mut left, &mut right), 256);
        assert!(left.iter().chain(right.iter()).all(|sample| *sample == 0.0));
        // Enough: the frames from the first.
        put_in(&ring, 300, 900);
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        assert_eq!((left[0], left[255], right[255]), (0.0, 255.0, -255.0));
        // It is read until it is not there, and what is not there is silence, and the reader waits again.
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        assert_eq!(reader.pull(&mut left, &mut right), 256 - (900 - 768));
        assert_eq!((left[0], left[131], left[132]), (768.0, 899.0, 0.0));
        put_in(&ring, 900, 1000);
        assert_eq!(reader.pull(&mut left, &mut right), 256, "it waits for the backlog again, and 100 frames are not enough");
        put_in(&ring, 1000, 1700);
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        assert_eq!(left[0], 900.0, "what was put in meanwhile has not been lost");
    }

    #[test]
    fn a_reader_made_on_a_ring_that_has_frames_in_it_plays_what_comes_after_them() {
        let ring = Arc::new(InputRing::new());
        put_in(&ring, 0, 2000);
        let mut reader = RingReader::new(ring.clone());
        put_in(&ring, 2000, 3000);
        let (mut left, mut right) = (vec![9.0; 256], vec![9.0; 256]);
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        assert_eq!((left[0], left[255]), (2000.0, 2255.0), "the frames from before the reader was made are not played");
    }

    #[test]
    fn a_larger_block_waits_for_two_blocks_of_its_own_and_not_for_the_smallest_backlog() {
        let ring = Arc::new(InputRing::new());
        let mut reader = RingReader::new(ring.clone());
        let (mut left, mut right) = (vec![9.0; 1024], vec![9.0; 1024]);
        put_in(&ring, 0, 1500);
        assert_eq!(reader.pull(&mut left, &mut right), 1024, "1,500 frames are less than two blocks of 1,024: silence");
        assert!(left.iter().all(|sample| *sample == 0.0));
        put_in(&ring, 1500, 2100);
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        assert_eq!((left[0], left[1023]), (0.0, 1023.0));
    }

    #[test]
    fn a_backlog_at_the_limit_is_read_from_its_first_frame_and_one_frame_more_is_skipped_to_the_prime() {
        let first_of = |waiting: usize| {
            let ring = Arc::new(InputRing::new());
            let mut reader = RingReader::new(ring.clone());
            put_in(&ring, 0, waiting);
            let (mut left, mut right) = (vec![0.0; 256], vec![0.0; 256]);
            assert_eq!(reader.pull(&mut left, &mut right), 0);
            left[0]
        };
        assert_eq!(first_of(MAX_BACKLOG), 0.0);
        assert_eq!(first_of(MAX_BACKLOG + 1), (MAX_BACKLOG + 1 - 768) as f32);
    }

    #[test]
    fn a_reader_that_is_far_behind_skips_the_oldest_and_goes_on_from_the_prime() {
        let ring = Arc::new(InputRing::new());
        let mut reader = RingReader::new(ring.clone());
        put_in(&ring, 0, MAX_BACKLOG + 5000);
        let (mut left, mut right) = (vec![0.0; 256], vec![0.0; 256]);
        assert_eq!(reader.pull(&mut left, &mut right), 0);
        // (Two blocks of 256 are less than the smallest prime, 768.)
        assert_eq!(left[0], (MAX_BACKLOG + 5000 - 768) as f32);
    }

    #[test]
    fn a_resampler_makes_the_frames_of_the_other_rate_between_the_ones_that_came() {
        let take = |from: f64, to: f64, frames: usize| {
            let mut resampler = Resampler::new(from, to);
            let mut out = Vec::new();
            for i in 0..frames {
                resampler.push([i as f32, -(i as f32)], &mut |frame| out.push(frame));
            }
            out
        };
        // Twice the rate: a frame between each two, on the line.
        let up = take(24000.0, 48000.0, 5);
        assert_eq!(up.iter().map(|frame| frame[0]).collect::<Vec<_>>(), [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
        assert_eq!(up[1], [0.5, -0.5]);
        // Half the rate: every second frame.
        let down = take(48000.0, 24000.0, 7);
        assert_eq!(down.iter().map(|frame| frame[0]).collect::<Vec<_>>(), [0.0, 2.0, 4.0]);
        // A second of 44.1 kHz is a second of 48 kHz, to a frame.
        let second = take(44100.0, 48000.0, 44100);
        assert!((second.len() as i64 - 48000).abs() <= 1, "{}", second.len());
        // A steady level stays as it is.
        let mut resampler = Resampler::new(44100.0, 48000.0);
        let mut steady = Vec::new();
        for _ in 0..100 {
            resampler.push([0.25, 0.25], &mut |frame| steady.push(frame[0]));
        }
        assert!(steady.iter().all(|sample| (*sample - 0.25).abs() < 1e-6));
    }

    #[test]
    fn a_frame_of_a_device_is_two_whatever_its_channels() {
        assert_eq!(stereo_of(&[]), (0.0, 0.0));
        assert_eq!(stereo_of(&[0.5]), (0.5, 0.5));
        assert_eq!(stereo_of(&[0.5, -0.25]), (0.5, -0.25));
        assert_eq!(stereo_of(&[0.5, -0.25, 0.75, 0.1]), (0.5, -0.25));
    }

    #[test]
    fn with_no_device_it_plays_a_quiet_sine_of_220_hz_in_both_sides() {
        let mut input = AudioInput::new(48000.0);
        let (left, right) = block(&mut input, 48000);
        assert_eq!(left, right);
        let peak = left.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!((peak - TONE_LEVEL).abs() < 1e-3, "{peak}");
        let rises = left.windows(2).filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0).count();
        assert!((rises as i64 - 220).abs() <= 1, "{rises}");
        // The phase goes on from block to block: two blocks are one block.
        let mut again = AudioInput::new(48000.0);
        let (first, _) = block(&mut again, 24000);
        let (second, _) = block(&mut again, 24000);
        assert_eq!([first, second].concat(), left);
    }

    #[test]
    fn the_gain_and_the_balance_are_gone_to_and_not_jumped_to() {
        // A gain that is set to nothing is a fall of one time constant in 5 ms (240 frames): the level at frame n is what it would have been, times e^(-(n + 1) / 240).
        let mut reference = AudioInput::new(48000.0);
        block(&mut reference, 4800);
        let (kept, _) = block(&mut reference, 4800);
        let mut input = AudioInput::new(48000.0);
        block(&mut input, 4800);
        input.set_parameter(GAIN, 0.0).unwrap();
        let (fading, _) = block(&mut input, 4800);
        let mut checked = 0;
        for i in (0..4800).step_by(7) {
            if kept[i].abs() > 0.05 {
                let expected = kept[i] * (-((i + 1) as f32) / 240.0).exp();
                assert!((fading[i] - expected).abs() < 1e-4, "frame {i}: {} against {expected}", fading[i]);
                checked += 1;
            }
        }
        assert!(checked > 100);
        let (silent, _) = block(&mut input, 4800);
        assert!(silent.iter().all(|sample| sample.abs() < 1e-6), "gone after 100 ms");
        // A balance that is turned all the way to the right after the first block is gone to as the gain is: the left side falls by one time constant in 5 ms.
        let mut input = AudioInput::new(48000.0);
        block(&mut input, 4800);
        input.set_parameter(PAN, 1.0).unwrap();
        let (turning, right) = block(&mut input, 4800);
        let mut checked = 0;
        for i in (0..4800).step_by(7) {
            if kept[i].abs() > 0.05 {
                assert!((turning[i] - kept[i] * (-((i + 1) as f32) / 240.0).exp()).abs() < 1e-4, "frame {i}: the left side {} of {}", turning[i], kept[i]);
                assert!((right[i] - kept[i]).abs() < 1e-6, "frame {i}: the right side is as it was");
                checked += 1;
            }
        }
        assert!(checked > 100);
        // A balance to the right takes the left side away and leaves the right as it is, and the other way.
        let mut input = AudioInput::new(48000.0);
        input.set_parameter(PAN, 1.0).unwrap();
        block(&mut input, 4800);
        let (left, right) = block(&mut input, 4800);
        assert!(left.iter().all(|sample| sample.abs() < 1e-6));
        assert!(right.iter().any(|sample| sample.abs() > 0.2));
        let mut input = AudioInput::new(48000.0);
        input.set_parameter(PAN, -1.0).unwrap();
        block(&mut input, 4800);
        let (left, right) = block(&mut input, 4800);
        assert!(right.iter().all(|sample| sample.abs() < 1e-6));
        assert!(left.iter().any(|sample| sample.abs() > 0.2));
        // The middle leaves both as they are.
        let mut input = AudioInput::new(48000.0);
        let (left, right) = block(&mut input, 480);
        assert_eq!(left, right);
    }

    #[test]
    fn a_gain_and_a_balance_that_are_set_before_the_first_block_are_those_of_the_first_block() {
        let (kept, _) = block(&mut AudioInput::new(48000.0), 256);
        let mut input = AudioInput::new(48000.0);
        input.set_parameter(GAIN, 0.5).unwrap();
        input.set_parameter(PAN, 1.0).unwrap();
        let (left, right) = block(&mut input, 256);
        // (A glide from a gain of 1 and a balance in the middle would have the left side sounding for the first milliseconds.)
        assert!(left.iter().all(|sample| *sample == 0.0), "a balance all the way to the right has no left side from the first frame");
        for i in 0..256 {
            assert!((right[i] - 0.5 * kept[i]).abs() < 1e-7, "frame {i}: {} against {}", right[i], 0.5 * kept[i]);
        }
    }

    #[test]
    fn a_gain_of_two_is_twice_the_sound_once_it_has_been_gone_to() {
        let mut one = AudioInput::new(48000.0);
        let mut two = AudioInput::new(48000.0);
        two.set_parameter(GAIN, 2.0).unwrap();
        block(&mut one, 4800);
        block(&mut two, 4800);
        let (a, _) = block(&mut one, 480);
        let (b, _) = block(&mut two, 480);
        for (a, b) in a.iter().zip(&b) {
            assert!((b - 2.0 * a).abs() < 1e-4);
        }
    }

    #[test]
    fn the_parameters_are_the_gain_the_balance_and_the_device_with_the_inputs_the_system_had() {
        let (input, _, _) = from_a_ring(&["Built-in Microphone", "USB Mic"]);
        let parameters = input.parameters();
        assert_eq!(parameters.iter().map(|p| (p.index, p.name.as_str())).collect::<Vec<_>>(), [(0, "gain"), (1, "pan"), (2, "device")]);
        assert_eq!((parameters[0].min, parameters[0].max, parameters[0].default), (0.0, 4.0, 1.0));
        assert_eq!((parameters[1].min, parameters[1].max, parameters[1].default), (-1.0, 1.0, 0.0));
        let device = &parameters[2];
        assert_eq!(device.labels.as_deref(), Some(&["Default input".to_string(), "Built-in Microphone".to_string(), "USB Mic".to_string()][..]));
        assert_eq!((device.min, device.max, device.default, device.step_count), (0.0, 2.0, 0.0, 2));
        assert_eq!(parameters.len(), PARAM_COUNT);
        // With no input at all (and in the tests) the choice is the default and nothing else.
        let none = AudioInput::new(48000.0).parameters();
        assert_eq!(none[2].labels.as_deref(), Some(&["Default input".to_string()][..]));
        assert_eq!((none[2].max, none[2].step_count), (0.0, 0));
    }

    #[test]
    fn what_is_set_is_what_is_got_and_what_is_not_a_number_or_not_a_parameter_is_left_alone() {
        let (mut input, _, _) = from_a_ring(&["A", "B"]);
        for (index, value, got) in [(GAIN, 2.5, 2.5), (GAIN, 9.0, 4.0), (GAIN, -1.0, 0.0), (PAN, -0.25, -0.25), (PAN, 3.0, 1.0), (DEVICE, 1.0, 1.0), (DEVICE, 7.0, 2.0), (DEVICE, -3.0, 0.0), (DEVICE, 1.4, 1.0), (DEVICE, 1.6, 2.0)] {
            input.set_parameter(index, value).unwrap();
            assert_eq!(input.get_parameter(index), Some(got), "{index} set to {value}");
        }
        input.set_parameter(GAIN, f32::NAN).unwrap();
        input.set_parameter(9, 1.0).unwrap();
        assert_eq!((input.get_parameter(GAIN), input.get_parameter(9)), (Some(0.0), None));
    }

    #[test]
    fn a_change_of_device_is_told_to_the_thread_once_and_by_name() {
        let (mut input, _, commands) = from_a_ring(&["Built-in Microphone", "USB Mic"]);
        input.set_parameter(DEVICE, 2.0).unwrap();
        assert_eq!(commands.try_recv(), Ok(Command::Device(Some("USB Mic".to_string()))));
        // The same place again, as a modulator that sets it every block does, is no change.
        input.set_parameter(DEVICE, 2.0).unwrap();
        input.set_parameter(DEVICE, 2.4).unwrap();
        assert!(commands.try_recv().is_err());
        input.set_parameter(DEVICE, 0.0).unwrap();
        assert_eq!(commands.try_recv(), Ok(Command::Device(None)));
        // A place past the inputs is the last of them.
        input.set_parameter(DEVICE, 9.0).unwrap();
        assert_eq!(commands.try_recv(), Ok(Command::Device(Some("USB Mic".to_string()))));
    }

    #[test]
    fn the_first_block_starts_the_thread_and_every_block_is_a_beat_for_it() {
        let (mut input, ring, commands) = from_a_ring(&[]);
        assert!(commands.try_recv().is_err(), "nothing is asked of the thread until a block is rendered (a catalog scan makes the plugin and renders nothing)");
        for blocks in 1..=3u64 {
            block(&mut input, 256);
            if let Source::Device { capture: Some(capture), .. } = &input.source {
                assert_eq!(capture.beats(), blocks);
            }
        }
        assert_eq!(commands.try_recv(), Ok(Command::Start));
        assert!(commands.try_recv().is_err(), "once");
        drop(ring);
    }

    #[test]
    fn it_plays_what_the_ring_has_in_both_sides_and_is_silent_until_there_is_enough() {
        let (mut input, ring, _commands) = from_a_ring(&[]);
        for i in 0..300 {
            ring.push(0.5, -0.25 + i as f32 * 0.0);
        }
        let (left, right) = block(&mut input, 256);
        assert!(left.iter().chain(right.iter()).all(|sample| *sample == 0.0), "300 frames are not the backlog that it waits for");
        for _ in 0..900 {
            ring.push(0.5, -0.25);
        }
        let (left, right) = block(&mut input, 256);
        assert!(left.iter().all(|sample| (*sample - 0.5).abs() < 1e-6) && right.iter().all(|sample| (*sample + 0.25).abs() < 1e-6));
    }

    #[test]
    fn it_starts_bare_has_two_outputs_and_none_in_and_no_presets() {
        let mut input = AudioInput::new(48000.0);
        assert!(input.starts_bare() && input.is_instrument());
        assert_eq!((input.audio_input_count(), input.audio_output_count(), input.latency()), (0, 2, 0));
        assert_eq!(input.name(), NAME);
        assert!(input.presets().is_empty());
        assert!(input.load_preset("anything").is_err());
        // The others are heard as they are.
        let oscillator = super::super::load("builtin:osc", 48000.0, 256).unwrap();
        assert!(!oscillator.starts_bare());
        let loaded = super::super::load(ID, 48000.0, 256).unwrap();
        assert!(loaded.starts_bare());
    }

    #[test]
    fn an_audio_input_takes_no_notes_and_every_other_plugin_does() {
        assert!(!AudioInput::new(48000.0).takes_notes());
        assert!(!super::super::load(ID, 48000.0, 256).unwrap().takes_notes(), "as it is loaded from the registry");
        for id in ["builtin:osc", "builtin:filter", "builtin:reverb", "builtin:delay", "builtin:drive", "builtin:math"] {
            assert!(super::super::load(id, 48000.0, 256).unwrap().takes_notes(), "{id} takes notes");
        }
    }

    #[test]
    fn the_advice_for_a_silent_input_names_the_terminal_and_the_command_that_asks_again() {
        let advice = silence_advice(Some("dev.warp.Warp-Stable"), Some("WarpTerminal"));
        assert!(advice.contains("the terminal that runs Tang (WarpTerminal, dev.warp.Warp-Stable)"), "{advice}");
        assert!(advice.contains("System Settings > Privacy & Security > Microphone"), "{advice}");
        assert!(advice.contains("`tccutil reset Microphone dev.warp.Warp-Stable`"), "{advice}");
        assert!(advice.contains("muted"), "{advice}");
        // Without a name for the terminal, it is said as it is, and there is no command to ask again with.
        let plain = silence_advice(None, None);
        assert!(plain.contains("the terminal that runs Tang may not be allowed") && !plain.contains("tccutil"), "{plain}");
        assert!(silence_advice(None, Some("iTerm.app")).contains("(iTerm.app)"));
        assert!(silence_advice(Some("com.apple.Terminal"), None).contains("(com.apple.Terminal)"));
    }

    #[test]
    fn an_input_that_gives_only_zeros_for_the_limit_is_said_so_once_and_the_first_sound_sets_the_watch_again() {
        let mut watch = SilenceWatch::new(4);
        let said: Vec<bool> = (0..6).map(|_| watch.push(0.0, 0.0)).collect();
        assert_eq!(said, [false, false, false, true, false, false], "once, at the fourth frame of nothing");
        // A frame with anything in it (one side is enough) breaks the run, however small it is.
        assert!(!watch.push(0.0, 1e-9));
        assert_eq!((0..4).map(|_| watch.push(0.0, 0.0)).collect::<Vec<_>>(), [false, false, false, true], "and it says so again after another run");
        // A run that is one frame short does not say it.
        let mut short = SilenceWatch::new(4);
        assert_eq!((0..3).map(|_| short.push(0.0, 0.0)).collect::<Vec<_>>(), [false, false, false]);
        assert!(!short.push(0.5, 0.0) && !short.push(0.0, 0.0));
    }

    #[test]
    fn a_level_is_said_in_decibels_below_full_scale_and_none_is_silence() {
        assert_eq!(level_in_words(0.0), "silence (exactly 0)");
        assert_eq!(level_in_words(1.0), "peak 0.0 dB");
        assert_eq!(level_in_words(0.1), "peak -20.0 dB");
        assert_eq!(level_in_words(0.5), "peak -6.0 dB");
    }

    /// The real input, for a person who has one: `cargo test --lib the_real_input -- --ignored --nocapture` says what the system gives (the same as `tang listen`, from the test's own program).
    #[test]
    #[ignore = "needs a microphone, and the permission to use it: cargo test --lib the_real_input -- --ignored --nocapture"]
    fn the_real_input_is_heard() {
        probe(None, 2.0, 48000).unwrap();
    }

    // The thread that listens, run with a fake in place of the sound card and a clock in milliseconds.

    /// A stream that counts itself while it is open: dropping it is closing it.
    struct FakeStream(Arc<AtomicUsize>);

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Each time the fake card was asked to open a stream: the device that was wanted, and when.
    type Asked = Arc<Mutex<Vec<(Option<String>, Instant)>>>;

    const FAST: Timing = Timing { look_every: Duration::from_millis(5), idle_after: Duration::from_millis(80), retry_after: Duration::from_millis(60) };

    /// `listen` on a thread of its own, with what a test needs to drive and to look at it.
    struct Listening {
        commands: Option<Sender<Command>>,
        beats: Arc<AtomicU64>,
        failed: Arc<AtomicBool>,
        /// How many streams are open now.
        open: Arc<AtomicUsize>,
        /// Each time it was asked to open one: the device and when.
        asked: Asked,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Listening {
        /// The thread on the clock of `FAST`, with a card that cannot be opened the first `failures` times it is asked.
        fn new(failures: usize) -> Listening {
            Listening::with(FAST, failures)
        }

        /// The thread on the clock `timing`, with a card that cannot be opened the first `failures` times it is asked.
        fn with(timing: Timing, failures: usize) -> Listening {
            let (commands, receive) = crossbeam_channel::bounded(8);
            let (beats, failed, open, asked) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(Vec::new())));
            let thread = {
                let (beats, failed, open, asked) = (beats.clone(), failed.clone(), open.clone(), asked.clone());
                let mut failures = failures;
                std::thread::spawn(move || {
                    listen(&receive, &beats, &failed, timing, |name| {
                        asked.lock().unwrap().push((name.map(str::to_string), Instant::now()));
                        if failures > 0 {
                            failures -= 1;
                            return Err("no such input".to_string());
                        }
                        open.fetch_add(1, Ordering::SeqCst);
                        Ok(FakeStream(open.clone()))
                    });
                })
            };
            Listening { commands: Some(commands), beats, failed, open, asked, thread: Some(thread) }
        }

        fn send(&self, command: Command) {
            self.commands.as_ref().unwrap().send(command).unwrap();
        }

        fn asked(&self) -> Vec<Option<String>> {
            self.asked.lock().unwrap().iter().map(|(name, _)| name.clone()).collect()
        }

        /// Wait (for up to three seconds) until `done`, rendering a block every few milliseconds while `rendering`.
        fn until(&self, what: &str, rendering: bool, done: impl Fn(&Listening) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !done(self) {
                assert!(Instant::now() < deadline, "{what}: not within 3 s (asked {:?}, {} open)", self.asked(), self.open.load(Ordering::SeqCst));
                if rendering {
                    self.beats.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(3));
            }
        }

        /// Render a block every few milliseconds for `ms`.
        fn render_for(&self, ms: u64) {
            let until = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < until {
                self.beats.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
            }
        }
    }

    impl Drop for Listening {
        fn drop(&mut self) {
            self.commands.take();
            if let Some(thread) = self.thread.take() {
                // (A thread that does not end when its channel is closed is a failure, and not a test that waits for it for ever.)
                let deadline = Instant::now() + Duration::from_millis(1500);
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(3));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                } else if !std::thread::panicking() {
                    panic!("the thread that listens did not end when the plugin let go of it");
                }
            }
        }
    }

    #[test]
    fn nothing_is_opened_until_the_first_block_has_been_rendered_and_then_one_stream_is_open_while_blocks_go_on() {
        let listening = Listening::new(0);
        // Time passes with no block and no word of a start: nothing is opened (a catalog scan makes the plugin and renders nothing). A start with no block behind it is not enough either.
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(listening.asked(), Vec::<Option<String>>::new());
        listening.send(Command::Start);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(listening.asked(), Vec::<Option<String>>::new(), "the start came and no block has been rendered for longer than the idle time");
        // Blocks go on: the system's default input is opened, once, and kept open while they do.
        listening.until("the stream is opened", true, |l| l.open.load(Ordering::SeqCst) == 1);
        listening.render_for(250);
        assert_eq!((listening.asked(), listening.open.load(Ordering::SeqCst)), (vec![None], 1));
    }

    #[test]
    fn the_stream_is_closed_when_no_block_is_rendered_for_the_idle_time_and_opened_again_when_blocks_come_back() {
        let listening = Listening::new(0);
        listening.send(Command::Start);
        listening.until("the stream is opened", true, |l| l.open.load(Ordering::SeqCst) == 1);
        listening.until("the stream is closed with no block", false, |l| l.open.load(Ordering::SeqCst) == 0);
        assert_eq!(listening.asked().len(), 1);
        listening.until("the stream is opened again", true, |l| l.open.load(Ordering::SeqCst) == 1);
        assert_eq!(listening.asked(), vec![None, None]);
    }

    #[test]
    fn another_device_closes_the_stream_and_the_one_that_is_asked_for_is_opened_by_name() {
        let listening = Listening::new(0);
        listening.send(Command::Start);
        listening.until("the stream is opened", true, |l| l.open.load(Ordering::SeqCst) == 1);
        listening.send(Command::Device(Some("USB Mic".to_string())));
        listening.until("the new device is opened", true, |l| l.asked().len() == 2);
        listening.render_for(100);
        assert_eq!((listening.asked(), listening.open.load(Ordering::SeqCst)), (vec![None, Some("USB Mic".to_string())], 1), "the first stream is closed, and one is open");
        listening.send(Command::Device(None));
        listening.until("the default is opened again", true, |l| l.asked().len() == 3);
        listening.render_for(100);
        assert_eq!((listening.asked().last().cloned(), listening.open.load(Ordering::SeqCst)), (Some(None), 1));
    }

    #[test]
    fn another_device_is_opened_at_once_and_does_not_wait_for_the_retry_time() {
        let listening = Listening::with(Timing { retry_after: Duration::from_millis(600), ..FAST }, 0);
        listening.send(Command::Start);
        listening.until("the stream is opened", true, |l| l.open.load(Ordering::SeqCst) == 1);
        let sent = Instant::now();
        listening.send(Command::Device(Some("USB Mic".to_string())));
        listening.until("the new device is opened", true, |l| l.asked().len() == 2);
        let (_, opened) = listening.asked.lock().unwrap()[1].clone();
        assert!(opened.duration_since(sent) < Duration::from_millis(400), "{:?} after the change of device, with a retry time of 600 ms", opened.duration_since(sent));
    }

    #[test]
    fn a_stream_that_says_it_has_failed_is_closed_and_the_device_is_tried_again_but_not_before_the_retry_time() {
        let listening = Listening::new(0);
        listening.send(Command::Start);
        listening.until("the stream is opened", true, |l| l.open.load(Ordering::SeqCst) == 1);
        let failed_at = Instant::now();
        listening.failed.store(true, Ordering::SeqCst);
        listening.until("the stream is closed", true, |l| l.asked().len() == 2);
        let (_, again) = listening.asked.lock().unwrap()[1].clone();
        assert!(again.duration_since(failed_at) >= FAST.retry_after, "{:?} after the failure", again.duration_since(failed_at));
        listening.render_for(100);
        assert_eq!(listening.open.load(Ordering::SeqCst), 1, "one stream, the new one");
    }

    #[test]
    fn an_input_that_cannot_be_opened_is_tried_again_after_the_retry_time_and_not_at_once() {
        let listening = Listening::new(2);
        listening.send(Command::Start);
        listening.until("the third try opens it", true, |l| l.open.load(Ordering::SeqCst) == 1);
        let asked = listening.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 3);
        for pair in asked.windows(2) {
            assert!(pair[1].1.duration_since(pair[0].1) >= FAST.retry_after, "{:?} between two tries", pair[1].1.duration_since(pair[0].1));
        }
    }

    #[test]
    fn the_thread_ends_and_the_stream_is_closed_when_the_plugin_lets_go_of_it() {
        let mut listening = Listening::new(0);
        listening.send(Command::Start);
        listening.until("the stream is opened", true, |l| l.open.load(Ordering::SeqCst) == 1);
        listening.commands.take();
        let thread = listening.thread.take().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !thread.is_finished() {
            assert!(Instant::now() < deadline, "the thread did not end");
            std::thread::sleep(Duration::from_millis(3));
        }
        thread.join().unwrap();
        assert_eq!(listening.open.load(Ordering::SeqCst), 0, "the stream went with the thread");
    }
}
