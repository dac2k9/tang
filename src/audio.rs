use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Receiver;

use crate::plugin::chain::AudioGraph;

/// A MIDI event on its way to the audio thread: who sent it (`Source`), its frame in the next block and its raw bytes.
/// Standard MIDI messages are 1–3 bytes; we use a fixed array to avoid heap allocation.
pub use crate::source::MidiEvent;
use crate::source::{Events, Source};

/// What the audio callback renders with: everything between the notes that arrive and the samples that go out. `AudioGraph` is the one there is. It is a trait so that there can be
/// another one (the compiled graph of `docs/patch-graph.md` §11) next to it, chosen at start, and so that the oracle of `crate::oracle` can run two of them on the same input and
/// say whether they agree.
pub trait Engine: Send {
    /// How many channels `process` renders.
    fn num_channels(&self) -> usize;

    /// Render a block: `out` has a buffer for each channel, and its length is the length of the block. The events are the notes and controllers that arrived since the last block, each
    /// with its frame in this one and the source it came from. What the controller has sent since the last block (commands, plugins coming and going) is taken first.
    fn process(&mut self, midi_events: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()>;

    /// What the engine does while nothing asks it for sound (a host that has stopped calling it: the plugin that Tang is in a DAW is switched off, or its audio is stopped): take what the controller has sent (the commands,
    /// the plugins that come and go), so that the channel they are sent through does not fill up and the controller wait for room in it, and render nothing. The default is an engine that has nothing to take.
    fn idle(&mut self) {}

    /// What the lanes, the groups and the audio cables are, for the oracle to hold to the model (the ids that the engine has for them are the model's): `None` for an engine that has no such
    /// things to say.
    #[cfg(test)]
    fn facts(&self) -> Option<crate::plugin::chain::Facts> {
        None
    }
}

impl Engine for AudioGraph {
    fn num_channels(&self) -> usize {
        AudioGraph::num_channels(self)
    }

    fn process(&mut self, midi_events: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()> {
        AudioGraph::process_events(self, midi_events, out)
    }

    fn idle(&mut self) {
        self.drain_commands();
    }

    #[cfg(test)]
    fn facts(&self) -> Option<crate::plugin::chain::Facts> {
        Some(AudioGraph::facts(self))
    }
}

impl<E: Engine + ?Sized> Engine for Box<E> {
    fn num_channels(&self) -> usize {
        (**self).num_channels()
    }

    #[cfg(test)]
    fn facts(&self) -> Option<crate::plugin::chain::Facts> {
        (**self).facts()
    }

    fn process(&mut self, midi_events: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()> {
        (**self).process(midi_events, out)
    }

    fn idle(&mut self) {
        (**self).idle()
    }
}

/// Select the audio host for the current platform.
/// On Linux the default is ALSA, which PipeWire serves as an ordinary stream
/// with its own buffer: if tang misses a deadline only tang glitches, and
/// WirePlumber moves it to a new default device like any other app. `--jack`
/// instead runs tang inside PipeWire's graph cycle through the JACK bridge,
/// which avoids that extra buffer but lets a late tang cycle glitch every app.
#[cfg(target_os = "linux")]
fn select_audio_host(jack: bool, buffer_size: u32, sample_rate: u32) -> anyhow::Result<cpal::Host> {
    // Request our latency from PipeWire as a cooperative hint. PIPEWIRE_LATENCY
    // is per-client: PipeWire negotiates the global quantum as the min across
    // active clients, and other apps recover when ours is paused or xruns.
    // PIPEWIRE_QUANTUM, by contrast, would force the global quantum. Without
    // the hint, PipeWire's ALSA plugin requests cpal's period size, which is a
    // quarter of the buffer, and drags the global quantum down with it.
    let latency = format!("{buffer_size}/{sample_rate}");
    // SAFETY: called once at startup before other threads read env vars.
    unsafe { std::env::set_var("PIPEWIRE_LATENCY", &latency) };
    log::info!("Set PIPEWIRE_LATENCY={latency}");

    if !jack {
        log::info!("Using ALSA audio host");
        return Ok(cpal::default_host());
    }

    let host = cpal::host_from_id(cpal::HostId::Jack)
        .map_err(|e| anyhow::anyhow!("JACK host unavailable: {e}"))?;
    // Without PipeWire's JACK bridge (pw-jack or ld.so.conf), the host object
    // is created but cannot connect to any server.
    if host.default_output_device().is_none() {
        anyhow::bail!("JACK host has no output devices (run with pw-jack, or install PipeWire's JACK ld.so.conf)");
    }
    log::info!("Using JACK audio host (PipeWire)");
    Ok(host)
}

#[cfg(not(target_os = "linux"))]
fn select_audio_host(_jack: bool, _buffer_size: u32, _sample_rate: u32) -> anyhow::Result<cpal::Host> {
    log::info!("Using default audio host");
    Ok(cpal::default_host())
}

/// Whether `--jack` was given (set once, at startup, before any plugin is made: the audio input is a plugin, which is made far from the command line).
static JACK_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Say whether `--jack` was given, so that the audio input opens its stream on the host that the output does.
pub fn use_jack(jack: bool) {
    JACK_REQUESTED.store(jack, std::sync::atomic::Ordering::Relaxed);
}

/// The audio host an input stream is opened on (the audio input, `builtin:input`): the output's host, which is the system's default host (ALSA on Linux) unless `--jack` was given,
/// and then JACK if it has an input device (so that the input goes where the output goes: through PipeWire's JACK bridge and not through the ALSA layer). It sets nothing in the
/// environment (`select_audio_host` does that once, at startup, before other threads read it), and JACK is asked for by name, so that it is the same code on every platform.
pub fn input_host() -> cpal::Host {
    if JACK_REQUESTED.load(std::sync::atomic::Ordering::Relaxed) {
        let jack = cpal::available_hosts().into_iter().find(|id| id.name() == "JACK");
        if let Some(host) = jack.and_then(|id| cpal::host_from_id(id).ok()) {
            if host.default_input_device().is_some() {
                return host;
            }
        }
    }
    cpal::default_host()
}

/// Attempt to promote the current thread to SCHED_FIFO real-time scheduling.
/// Called once from the audio callback thread on its first invocation.
/// Fails silently (with a log warning) if the process lacks `rtprio` privileges.
#[cfg(target_os = "linux")]
fn promote_to_realtime() {
    unsafe {
        // Check if the thread already has RT scheduling (e.g. PipeWire's JACK
        // backend promotes callback threads via rtkit).
        let mut current_param: libc::sched_param = std::mem::zeroed();
        let mut policy: i32 = 0;
        if libc::pthread_getschedparam(libc::pthread_self(), &mut policy, &mut current_param) == 0 {
            // Mask off flag bits — PipeWire may set extra bits on the policy value.
            let base_policy = policy & 0xF;
            if base_policy == libc::SCHED_FIFO || base_policy == libc::SCHED_RR {
                log::info!(
                    "Audio thread already has RT scheduling (policy={}, priority={})",
                    if base_policy == libc::SCHED_FIFO { "FIFO" } else { "RR" },
                    current_param.sched_priority
                );
                return;
            }
        }

        let param = libc::sched_param { sched_priority: 50 };
        let ret = libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param);
        if ret == 0 {
            log::info!("Audio thread promoted to SCHED_FIFO priority 50");
        } else {
            log::warn!(
                "Could not set real-time scheduling (err {ret}). \
                 For RT audio, add your user to the 'audio' group \
                 and ensure rtprio is set in /etc/security/limits.d/"
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn promote_to_realtime() {
    // macOS cpal (CoreAudio) already runs the audio callback on a real-time thread.
    log::info!("macOS: CoreAudio handles real-time scheduling");
}

#[cfg(target_os = "windows")]
fn promote_to_realtime() {
    // Windows cpal (WASAPI) already uses MMCSS for real-time audio.
    log::info!("Windows: WASAPI handles real-time scheduling");
}

/// While one of these is alive, the thread's floating point unit takes the
/// numbers nearer to zero than a normal 32-bit float can be (denormals, under
/// 1.2e-38) as zero, in what it computes and in what it computes with, and
/// the state it had is given back when this goes.
///
/// The tails of filters, reverbs and envelopes run down through the denormals,
/// and arithmetic on them takes many times as long on some processors (a
/// hundred times on x86): the tail of a filter that was shut made more than half
/// the samples of its two seconds denormal. Inaudible and slow is what to flush,
/// for the plugins the block runs as much as for the built-in ones. A guard is for the
/// time of a block and not set once for the thread, since a host's callback is
/// not always on a thread of its own and a plugin may change the unit.
///
/// What it does not do is end a tail. A state that moves by a product of its own
/// distance to something (`s += g * (x - s)`) stops where that product is flushed,
/// and stays there: the built-in filter's output was a constant of 2e-38 to 5e-35 (by its
/// type and cutoff) for as long as it was watched, and it lets go of its states under
/// `TAIL_FLOOR` now.
struct NoDenormals {
    saved: fpu::State,
    /// The state is the thread's: this is put back on the thread it was taken from.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl NoDenormals {
    fn new() -> Self {
        NoDenormals { saved: fpu::flush_denormals(), _not_send: std::marker::PhantomData }
    }
}

impl Drop for NoDenormals {
    fn drop(&mut self) {
        fpu::restore(self.saved);
    }
}

/// The floating point control register of the thread, where the processor has one that this knows.
#[cfg(target_arch = "x86_64")]
mod fpu {
    use std::arch::asm;

    /// MXCSR: the unit's state.
    pub type State = u32;

    /// Flush to zero (bit 15: a result that is a denormal is zero) and denormals are zero (bit 6: an operand that is one is).
    const FLUSH: State = (1 << 15) | (1 << 6);

    /// Turn the flushing on, and say what the state was.
    pub fn flush_denormals() -> State {
        let mut saved: State = 0;
        // SAFETY: `stmxcsr` stores this thread's MXCSR through the pointer, to a variable that is here.
        unsafe { asm!("stmxcsr [{}]", in(reg) &mut saved, options(nostack, preserves_flags)) };
        restore(saved | FLUSH);
        saved
    }

    pub fn restore(state: State) {
        // SAFETY: `ldmxcsr` loads this thread's MXCSR from the pointer, to a variable that is here. The bits are the ones
        // `stmxcsr` gave with the flushing ones set, so none that is reserved is.
        unsafe { asm!("ldmxcsr [{}]", in(reg) &state, options(nostack, readonly, preserves_flags)) };
    }
}

#[cfg(target_arch = "aarch64")]
mod fpu {
    use std::arch::asm;

    /// FPCR: the unit's control register.
    pub type State = u64;

    /// Flush to zero (bit 24: an operand that is a denormal is zero, and so is a result that is one).
    const FLUSH: State = 1 << 24;

    /// Turn the flushing on, and say what the state was.
    pub fn flush_denormals() -> State {
        let saved: State;
        // SAFETY: reads this thread's FPCR.
        unsafe { asm!("mrs {}, fpcr", out(reg) saved, options(nostack, preserves_flags)) };
        restore(saved | FLUSH);
        saved
    }

    pub fn restore(state: State) {
        // SAFETY: writes this thread's FPCR with the value it had, and the flushing bit.
        unsafe { asm!("msr fpcr, {}", in(reg) state, options(nostack, preserves_flags)) };
    }
}

/// Where the unit's state is not known, nothing is flushed.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod fpu {
    pub type State = ();

    pub fn flush_denormals() -> State {}

    pub fn restore(_: State) {}
}

/// How many messages the channel to the audio thread holds (`main` makes it so): what a block can take without the allocator.
pub const MIDI_CHANNEL_CAPACITY: usize = 1024;

/// What an audio callback does with the audio thread's end of the session, apart
/// from where the samples go: take the MIDI that has arrived, render the graph into
/// one buffer per channel, and interleave them into the output. The buffers are
/// made once and reused, so a block is rendered without allocating.
pub(crate) struct Renderer {
    graph: Box<dyn Engine>,
    midi_rx: Receiver<MidiEvent>,
    /// The MIDI that arrived since the last block: the frame and bytes of each, as the engine and the plugins have them, and beside it the source of each.
    midi_events: Vec<(u64, [u8; 3])>,
    midi_sources: Vec<Source>,
    /// One buffer per channel, a block long.
    channel_bufs: Vec<Vec<f32>>,
    num_channels: usize,
    /// How many blocks a host has had of `render_planar`, for the log.
    planar_blocks: u64,
}

impl Renderer {
    pub(crate) fn new(graph: impl Engine + 'static, midi_rx: Receiver<MidiEvent>, buffer_size: usize) -> Self {
        let num_channels = graph.num_channels();
        Renderer {
            graph: Box::new(graph),
            midi_rx,
            // Room for what the channel holds, so that a block that takes a burst of events (a panic is 48 and the notes held) asks the allocator for nothing.
            midi_events: Vec::with_capacity(MIDI_CHANNEL_CAPACITY),
            midi_sources: Vec::with_capacity(MIDI_CHANNEL_CAPACITY),
            channel_bufs: (0..num_channels).map(|_| vec![0.0f32; buffer_size]).collect(),
            num_channels,
            planar_blocks: 0,
        }
    }

    /// The engine that renders, for a test to ask what it has.
    #[cfg(test)]
    pub(crate) fn engine(&self) -> &dyn Engine {
        &*self.graph
    }

    /// Fill `data`, interleaved (`frames * channels` samples), with the next block;
    /// `cb_num` is the number of the block, for the log. Silence if the graph fails.
    pub(crate) fn render(&mut self, data: &mut [f32], cb_num: u64) {
        let _no_denormals = NoDenormals::new();
        self.render_block(data, cb_num);
    }

    /// `render`, with the floating point unit as it is.
    fn render_block(&mut self, data: &mut [f32], cb_num: u64) {
        let num_channels = self.num_channels;
        let frames = data.len() / num_channels;

        if !self.render_to_buffers(frames, &[], cb_num) {
            data.fill(0.0);
            return;
        }

        // Interleave back into the output buffer
        for frame in 0..frames {
            for ch in 0..num_channels {
                data[frame * num_channels + ch] = self.channel_bufs[ch][frame];
            }
        }

        // Log peak level when there were MIDI events (and somebody is listening: the scan
        // is for the log alone, and this is the audio thread).
        if !self.midi_events.is_empty() && log::log_enabled!(log::Level::Debug) {
            let peak = data.iter().fold(0.0f32, |max, &s| max.max(s.abs()));
            log::debug!("Audio cb #{cb_num}: output peak = {peak:.6}");
        }
    }

    /// The first half of a block: take the MIDI that has arrived (what the keys and the devices have sent since the last block, which is at the start of this one) and add the events that a host sends with the block (`host`, each at its own frame), and have the engine
    /// render `frames` frames into the buffers of the channels. False if the engine failed, which has been logged: the buffers are not to be used then.
    fn render_to_buffers(&mut self, frames: usize, host: &[MidiEvent], cb_num: u64) -> bool {
        // Drain all pending MIDI events (reuse the pre-allocated vecs)
        self.midi_events.clear();
        self.midi_sources.clear();
        while let Ok(event) = self.midi_rx.try_recv() {
            self.midi_events.push((event.frame, event.bytes));
            self.midi_sources.push(event.source);
        }
        // (A host can send more in one block than the channel holds: none is dropped, since a note that ends is one of them.)
        for event in host {
            self.midi_events.push((event.frame, event.bytes));
            self.midi_sources.push(event.source);
        }

        if !self.midi_events.is_empty() {
            log::debug!("Audio cb #{cb_num}: processing {} MIDI event(s) into {frames} frames", self.midi_events.len());
        }

        // Resize and zero pre-allocated per-channel buffers
        for buf in self.channel_bufs.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        if let Err(e) = self.graph.process(Events::new(&self.midi_events, &self.midi_sources), &mut self.channel_bufs) {
            log::error!("Audio graph process error: {e}");
            return false;
        }
        true
    }

    /// A block for a host that gives a buffer for each channel (a plugin's `process`) and its own events: `out` has the buffers, whose length is the length of the block, and `host` the events of the block, each at its own frame, in order. The channels of the
    /// engine are copied into the buffers (a host that gives more channels than the engine has gets the last one again, one that gives fewer gets the first ones); silence if the engine fails.
    pub(crate) fn render_planar(&mut self, host: &[MidiEvent], out: &mut [&mut [f32]]) {
        let _no_denormals = NoDenormals::new();
        let frames = out.first().map_or(0, |channel| channel.len());
        let cb_num = self.planar_blocks;
        self.planar_blocks += 1;
        if !self.render_to_buffers(frames, host, cb_num) {
            for channel in out.iter_mut() {
                channel.fill(0.0);
            }
            return;
        }
        let last = self.num_channels.saturating_sub(1);
        for (c, channel) in out.iter_mut().enumerate() {
            let rendered = &self.channel_bufs[c.min(last)];
            let len = channel.len().min(rendered.len());
            channel[..len].copy_from_slice(&rendered[..len]);
            channel[len..].fill(0.0);
        }
    }

    /// What the renderer does while nothing asks it for a block (`Engine::idle`): the notes that arrive are not kept for the block that comes later, and the engine takes what the controller has sent.
    pub(crate) fn idle(&mut self) {
        while self.midi_rx.try_recv().is_ok() {}
        self.graph.idle();
    }
}

/// The device name that stands for none: `--audio-device none` renders the engine
/// on a timer with no sound card, for CI, headless runs (`tang serve`) and tests.
pub const NULL_DEVICE: &str = "none";

/// The time one block of `buffer_size` frames lasts at `sample_rate`.
fn block_period(buffer_size: u32, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(buffer_size as f64 / sample_rate as f64)
}

/// When the next block of the null backend is due, once the one due at `deadline`
/// has been rendered and the time is `now`. Blocks keep the cadence (a period after
/// the one before, so that waking up late does not make it drift), unless the
/// engine has fallen more than a period behind (a stall): then the blocks it
/// missed are not rendered in a burst, the cadence starts again from `now`.
fn next_deadline(deadline: Instant, period: Duration, now: Instant) -> Instant {
    let next = deadline + period;
    if now.saturating_duration_since(next) > period { now } else { next }
}

/// The null backend: a thread that renders a block every `period`, and throws
/// the samples away.
struct NullBackend {
    /// What renders, until `play` hands it to the thread.
    renderer: Option<Renderer>,
    frames: usize,
    period: Duration,
    running: Option<Running>,
}

struct Running {
    stop: Arc<AtomicBool>,
    /// Gives the renderer back when it ends, so that the plugins are dropped by
    /// whoever stops the engine, not by the rendering thread.
    thread: JoinHandle<Renderer>,
}

impl NullBackend {
    fn play(&mut self) -> anyhow::Result<()> {
        let Some(renderer) = self.renderer.take() else {
            return Ok(()); // already playing
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (frames, period) = (self.frames, self.period);
        let thread = std::thread::Builder::new().name("tang-null-audio".to_string()).spawn({
            let stop = stop.clone();
            move || render_on_a_timer(renderer, frames, period, &stop)
        })?;
        self.running = Some(Running { stop, thread });
        log::info!("Audio stream started");
        Ok(())
    }

    fn stop(mut self) {
        if let Some(running) = self.running.take() {
            running.stop.store(true, Ordering::Relaxed);
            match running.thread.join() {
                Ok(renderer) => drop(renderer),
                Err(_) => log::error!("The null audio thread panicked"),
            }
        }
        log::info!("Audio stream stopped");
    }
}

/// Render a block every `period` until `stop`, into a buffer nobody listens to.
fn render_on_a_timer(mut renderer: Renderer, frames: usize, period: Duration, stop: &AtomicBool) -> Renderer {
    let mut data = vec![0.0f32; frames * renderer.num_channels];
    let mut cb_num: u64 = 0;
    let mut deadline = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if cb_num == 0 {
            log::info!("Audio callback running (null backend, {frames} frames every {period:?})");
        }
        renderer.render(&mut data, cb_num);
        cb_num += 1;
        let now = Instant::now();
        deadline = next_deadline(deadline, period, now);
        if let Some(wait) = deadline.checked_duration_since(now) {
            std::thread::sleep(wait);
        }
    }
    renderer
}

/// Keep our JACK output ports linked to the system default sink as it changes.
/// cpal links them to `system:playback_*` once; PipeWire's JACK bridge resolves
/// that alias to the current default sink, but the links themselves are fixed
/// and WirePlumber does not move JACK clients the way it moves regular streams.
/// Dropping the returned sender stops the watcher.
#[cfg(target_os = "linux")]
fn follow_default_sink(client_name: String, num_channels: usize) -> Option<crossbeam_channel::Sender<()>> {
    use jack::PortSpec;

    let client = match jack::Client::new("tang_follow_default", jack::ClientOptions::NO_START_SERVER) {
        Ok((client, _)) => client,
        Err(e) => {
            log::warn!("Cannot follow the default audio sink: {e}");
            return None;
        }
    };
    let audio_type = jack::AudioIn.jack_port_type().to_owned();
    let default_sink = move |client: &jack::Client| {
        client.ports(Some("system:playback_.*"), Some(&audio_type), jack::PortFlags::IS_INPUT)
    };
    let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(0);
    let mut linked = default_sink(&client);
    std::thread::Builder::new()
        .name("follow-default-sink".into())
        .spawn(move || {
            while stop_rx.recv_timeout(std::time::Duration::from_secs(1))
                == Err(crossbeam_channel::RecvTimeoutError::Timeout)
            {
                let sinks = default_sink(&client);
                if sinks.is_empty() || sinks == linked {
                    continue;
                }
                log::info!("Default audio sink changed, relinking to {sinks:?}");
                for ch in 0..num_channels {
                    let port = format!("{client_name}:out_{ch}");
                    if !linked.is_empty() {
                        client.disconnect_ports_by_name(&port, &linked[ch % linked.len()]).ok();
                    }
                    if let Err(e) = client.connect_ports_by_name(&port, &sinks[ch % sinks.len()]) {
                        log::warn!("Failed to link {port}: {e}");
                    }
                }
                linked = sinks;
            }
        })
        .ok()?;
    Some(stop_tx)
}

pub struct AudioEngine {
    backend: Backend,
    #[cfg(target_os = "linux")]
    _follow_default_sink: Option<crossbeam_channel::Sender<()>>,
}

enum Backend {
    Cpal(cpal::Stream),
    /// Boxed: the renderer it holds is much bigger than a stream.
    Null(Box<NullBackend>),
}

impl AudioEngine {
    /// Stop the audio stream. Call this before dropping the plugin.
    pub fn stop(self) {
        let stream = match self.backend {
            Backend::Cpal(stream) => stream,
            Backend::Null(null) => return null.stop(),
        };
        // Pause the stream first so the callback stops being invoked
        if let Err(e) = stream.pause() {
            log::warn!("Failed to pause audio stream: {e}");
        }
        // Give the audio callback time to finish if it's mid-flight
        std::thread::sleep(std::time::Duration::from_millis(50));
        // Now drop the stream
        drop(stream);
        log::info!("Audio stream stopped");
    }

    /// Start playing the audio stream. Call after initial commands are queued.
    pub fn play(&mut self) -> anyhow::Result<()> {
        match &mut self.backend {
            Backend::Cpal(stream) => {
                stream.play()?;
                log::info!("Audio stream started");
                Ok(())
            }
            Backend::Null(null) => null.play(),
        }
    }

    /// Build the audio engine but don't start playing yet.
    /// Call `play()` after queuing initial graph commands.
    pub fn build(
        graph: impl Engine + 'static,
        midi_rx: Receiver<MidiEvent>,
        device_name: Option<&str>,
        jack: bool,
        sample_rate: u32,
        buffer_size: u32,
    ) -> anyhow::Result<Self> {
        if device_name == Some(NULL_DEVICE) {
            log::info!("Using audio device: {NULL_DEVICE} (no audio output; the engine runs on a timer)");
            return Ok(AudioEngine {
                backend: Backend::Null(Box::new(NullBackend {
                    renderer: Some(Renderer::new(graph, midi_rx, buffer_size as usize)),
                    frames: buffer_size as usize,
                    period: block_period(buffer_size, sample_rate),
                    running: None,
                })),
                #[cfg(target_os = "linux")]
                _follow_default_sink: None,
            });
        }
        let host = select_audio_host(jack, buffer_size, sample_rate)?;

        let device = if let Some(name) = device_name {
            host.output_devices()?
                .find(|d| d.name().map(|n| n.contains(name)).unwrap_or(false))
                .ok_or_else(|| anyhow::anyhow!("Audio device not found: {name}"))?
        } else {
            host.default_output_device()
                .ok_or_else(|| anyhow::anyhow!("No default audio output device"))?
        };

        let dev_name = device.name().unwrap_or_else(|_| "Unknown".into());
        log::info!("Using audio device: {dev_name}");

        let num_channels = graph.num_channels();

        let config = cpal::StreamConfig {
            channels: num_channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Fixed(buffer_size),
        };

        log::info!(
            "Audio config: {}ch, {}Hz, buffer={}",
            num_channels,
            sample_rate,
            buffer_size
        );

        // The renderer lives in the closure; its buffers are reused every callback
        let mut renderer = Renderer::new(graph, midi_rx, buffer_size as usize);

        let mut callback_count: u64 = 0;

        let stream = device.build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let cb_num = callback_count;
                callback_count += 1;

                // Log first callback to confirm audio is running
                if cb_num == 0 {
                    log::info!("Audio callback running (first call, {} frames)", data.len() / num_channels);
                    promote_to_realtime();
                }

                renderer.render(data, cb_num);
            },
            move |err| {
                let msg = err.to_string();
                if msg.contains("buffer size changed") {
                    log::info!("Audio: {msg}");
                } else {
                    log::error!("Audio stream error: {msg}");
                }
            },
            None,
        )?;

        Ok(AudioEngine {
            backend: Backend::Cpal(stream),
            #[cfg(target_os = "linux")]
            _follow_default_sink: if host.id() == cpal::HostId::Jack && device_name.is_none() {
                follow_default_sink(dev_name, num_channels)
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::load_onto;

    const NOTE_ON_A4: MidiEvent = MidiEvent::keys(0, [0x90, 69, 100]);

    /// A renderer for `session`, with the channels it listens on and the audio
    /// thread's end of the plugin hand-back channel kept alive.
    struct Rig {
        renderer: Renderer,
        midi_tx: crossbeam_channel::Sender<MidiEvent>,
        _return_rx: Receiver<Box<dyn crate::plugin::Plugin>>,
    }

    fn rig(session: &str, buffer_size: usize) -> Rig {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(1024);
        let graph = AudioGraph::new(2, cmd_rx, return_tx);
        let renderer = Renderer::new(graph, midi_rx, buffer_size);
        load_onto(session, &cmd_tx);
        Rig { renderer, midi_tx, _return_rx: return_rx }
    }

    /// An engine that is not the graph: channel `c` is the constant `c + 1`, and it counts the blocks it has rendered.
    struct Constants {
        channels: usize,
        blocks: usize,
    }

    impl Engine for Constants {
        fn num_channels(&self) -> usize {
            self.channels
        }
        fn process(&mut self, _: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()> {
            self.blocks += 1;
            for (c, channel) in out.iter_mut().enumerate() {
                channel.fill(c as f32 + 1.0);
            }
            Ok(())
        }
    }

    #[test]
    fn the_renderer_drives_any_engine_and_interleaves_what_it_renders() {
        let (_midi_tx, midi_rx) = crossbeam_channel::bounded(8);
        let mut renderer = Renderer::new(Constants { channels: 2, blocks: 0 }, midi_rx, 4);
        let mut data = vec![0.0f32; 4 * 2];
        renderer.render(&mut data, 0);
        assert_eq!(data, [1.0, 2.0, 1.0, 2.0, 1.0, 2.0, 1.0, 2.0], "the channels of the engine, interleaved");
        let (_midi_tx, midi_rx) = crossbeam_channel::bounded(8);
        let renderer = Renderer::new(Constants { channels: 3, blocks: 0 }, midi_rx, 4);
        assert_eq!(renderer.num_channels, 3, "as many channels as the engine says it has");
    }

    #[test]
    fn a_boxed_engine_is_an_engine_and_renders_what_the_graph_renders_by_itself() {
        // The graph driven directly, and the same graph behind a `Box<dyn Engine>` in a renderer, on the same chord: the same bits. (The effect has a mix of a half: a reverb puts out
        // its tail only, and a file's effect is wet unless it says another mix, so that it would be silent for the few milliseconds that this renders.)
        let session = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nunison = 2.0\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.5\n";
        let chord = [(0, [0x90, 48, 100]), (0, [0x90, 55, 90]), (0, [0x90, 60, 80])];
        let make = || {
            let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
            let (return_tx, _return_rx) = crossbeam_channel::bounded(16);
            let graph = AudioGraph::new(2, cmd_rx, return_tx);
            load_onto(session, &cmd_tx);
            (graph, _return_rx)
        };
        let (mut direct, _keep_direct) = make();
        let mut bufs = vec![vec![0.0f32; 64]; 2];
        let mut by_hand = Vec::new();
        for block in 0..8 {
            let events: &[(u64, [u8; 3])] = if block == 0 { &chord } else { &[] };
            for buf in bufs.iter_mut() {
                buf.fill(0.0);
            }
            direct.process(events, &mut bufs).unwrap();
            for (left, right) in bufs[0].iter().zip(&bufs[1]) {
                by_hand.extend([*left, *right]);
            }
        }
        let (boxed, _keep_boxed) = make();
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(1024);
        let boxed: Box<dyn Engine> = Box::new(boxed);
        let mut renderer = Renderer::new(boxed, midi_rx, 64);
        let mut through_the_renderer = Vec::new();
        let mut data = vec![0.0f32; 64 * 2];
        for block in 0..8 {
            if block == 0 {
                for event in chord {
                    midi_tx.send(event.into()).unwrap();
                }
            }
            renderer.render(&mut data, block);
            through_the_renderer.extend_from_slice(&data);
        }
        assert!(by_hand.iter().any(|&s| s != 0.0), "the chord is heard");
        assert!(by_hand == through_the_renderer, "the same bits");
    }

    #[test]
    fn a_renderer_with_nothing_in_the_graph_renders_silence() {
        let mut rig = rig("", 64);
        let mut data = vec![0.5f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert!(data.iter().all(|&s| s == 0.0), "the block is overwritten, not mixed into");
    }

    #[test]
    fn a_note_sent_before_the_block_sounds_in_it_and_the_channels_are_interleaved() {
        // Hard left: the right channel is silent, so the interleaving shows.
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\npan = -1.0\n", 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 256 * 2];
        // The note is on from the first block; its envelope has risen a few blocks in.
        for cb in 0..4 {
            rig.renderer.render(&mut data, cb);
        }
        let left: Vec<f32> = data.iter().step_by(2).copied().collect();
        let right: Vec<f32> = data.iter().skip(1).step_by(2).copied().collect();
        let peak = |channel: &[f32]| channel.iter().fold(0.0f32, |max, &s| max.max(s.abs()));
        assert!(peak(&left) > 0.1, "the left channel carries the note: {left:?}");
        // The pan is smoothed over a few milliseconds, so the right channel has not
        // quite settled to silence; it is far below the left.
        assert!(peak(&right) < peak(&left) / 10.0, "and the right is nearly silent: {right:?}");
    }

    #[test]
    fn a_block_is_as_long_as_the_buffer_it_is_given() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        // Shorter than the size the renderer was made for, and then longer: both are fine.
        for frames in [100, 300] {
            let mut data = vec![0.0f32; frames * 2];
            rig.renderer.render(&mut data, 0);
            assert_eq!(data.len(), frames * 2);
        }
    }

    #[test]
    fn the_midi_of_one_block_is_not_rendered_again_in_the_next() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 64);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert_eq!(rig.renderer.midi_events.len(), 1);
        rig.renderer.render(&mut data, 1);
        assert!(rig.renderer.midi_events.is_empty(), "nothing arrived, so nothing is in the second block");
    }

    #[test]
    fn a_block_is_rendered_from_zero_not_on_top_of_the_one_before() {
        // A note that has been released and has died away leaves nothing behind.
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nrelease = 0.002\n", 64);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert!(data.iter().any(|&s| s != 0.0));
        rig.midi_tx.send(MidiEvent::keys(0, [0x80, 69, 0])).unwrap();
        for cb in 1..20 {
            rig.renderer.render(&mut data, cb);
        }
        assert!(data.iter().all(|&s| s == 0.0), "the tail is over, and the block is silent: {:?}", &data[..8]);
    }

    /// An instrument whose `process` always fails.
    struct Failing;

    impl crate::plugin::Plugin for Failing {
        fn name(&self) -> &str {
            "Failing"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn sample_rate(&self) -> f32 {
            48_000.0
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            0
        }
        fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
            anyhow::bail!("this plugin cannot render")
        }
        fn parameters(&self) -> Vec<crate::plugin::ParameterInfo> {
            Vec::new()
        }
        fn get_parameter(&mut self, _: u32) -> Option<f32> {
            None
        }
        fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
            Ok(())
        }
        fn presets(&self) -> Vec<crate::plugin::Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_graph_that_fails_leaves_silence_in_the_block() {
        use crate::plugin::chain::GraphCommand;
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(8);
        let (return_tx, _return_rx) = crossbeam_channel::bounded(8);
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(8);
        let mut renderer = Renderer::new(AudioGraph::new(2, cmd_rx, return_tx), midi_rx, 64);
        cmd_tx.send(GraphCommand::AddInstrument { range: None, node: crate::plugin::chain::test_node() }).unwrap();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: Box::new(Failing),
                inst_buf: vec![Vec::new(), Vec::new()],
                remapper: None,
                keep: None,
            })
            .unwrap();
        midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.7f32; 64 * 2];
        renderer.render(&mut data, 0);
        assert!(data.iter().all(|&s| s == 0.0), "what the callback was handed is not played");
    }

    /// What an engine was given in a block: the source, the frame and the bytes of each event.
    type Block = Vec<(Source, u64, [u8; 3])>;

    /// An engine that is not the graph either: it writes down the events of every block that it is given, each with its source.
    struct Listener {
        blocks: std::sync::Arc<std::sync::Mutex<Vec<Block>>>,
    }

    impl Engine for Listener {
        fn num_channels(&self) -> usize {
            2
        }
        fn process(&mut self, midi_events: Events<'_>, _: &mut [Vec<f32>]) -> anyhow::Result<()> {
            self.blocks.lock().unwrap().push(midi_events.iter().collect());
            Ok(())
        }
    }

    #[test]
    fn the_renderer_gives_the_engine_the_events_of_the_block_in_the_order_they_came_and_the_source_of_each() {
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(16);
        let blocks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut renderer = Renderer::new(Listener { blocks: blocks.clone() }, midi_rx, 4);
        let mut data = vec![0.0f32; 4 * 2];
        midi_tx.send(MidiEvent::from_source(Source::device(1), 0, [0x90, 64, 90])).unwrap();
        midi_tx.send(MidiEvent::keys(0, [0x90, 60, 100])).unwrap();
        midi_tx.send(MidiEvent::from_source(Source::BROADCAST, 2, [0xB0, 123, 0])).unwrap();
        renderer.render(&mut data, 0);
        renderer.render(&mut data, 1);
        midi_tx.send(MidiEvent::from_source(Source::device(0), 0, [0x80, 64, 0])).unwrap();
        renderer.render(&mut data, 2);
        assert_eq!(
            *blocks.lock().unwrap(),
            [
                vec![(Source::device(1), 0, [0x90, 64, 90]), (Source::KEYS, 0, [0x90, 60, 100]), (Source::BROADCAST, 2, [0xB0, 123, 0])],
                vec![],
                vec![(Source::device(0), 0, [0x80, 64, 0])],
            ]
        );
    }

    /// An engine that only counts what it is given: it keeps no list, so that nothing but the renderer can ask for memory while a block is rendered.
    struct Counter {
        events: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        from_devices: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Engine for Counter {
        fn num_channels(&self) -> usize {
            2
        }
        fn process(&mut self, midi_events: Events<'_>, _: &mut [Vec<f32>]) -> anyhow::Result<()> {
            use std::sync::atomic::Ordering::Relaxed;
            self.events.fetch_add(midi_events.len(), Relaxed);
            self.from_devices.fetch_add(midi_events.iter().filter(|(source, ..)| *source != Source::KEYS).count(), Relaxed);
            Ok(())
        }
    }

    #[test]
    fn a_block_that_takes_as_many_events_as_the_channel_holds_asks_the_allocator_for_nothing() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(MIDI_CHANNEL_CAPACITY);
        let (events, from_devices) = (std::sync::Arc::new(AtomicUsize::new(0)), std::sync::Arc::new(AtomicUsize::new(0)));
        let mut renderer = Renderer::new(Counter { events: events.clone(), from_devices: from_devices.clone() }, midi_rx, 4);
        let mut data = vec![0.0f32; 4 * 2];
        for n in 0..MIDI_CHANNEL_CAPACITY {
            let source = if n % 2 == 0 { Source::KEYS } else { Source::device(n % 5) };
            midi_tx.send(MidiEvent::from_source(source, 0, [0x90, (n % 128) as u8, 100])).unwrap();
        }
        let ((), allocated) = crate::alloc_guard::allocations_in(|| renderer.render(&mut data, 0));
        assert_eq!(allocated, 0, "the room for the events and their sources was made with the renderer");
        assert_eq!((events.load(Relaxed), from_devices.load(Relaxed)), (MIDI_CHANNEL_CAPACITY, MIDI_CHANNEL_CAPACITY / 2), "the whole burst is in one block, with its sources");
        assert!(midi_tx.is_empty());
    }

    #[test]
    fn midi_that_arrived_is_taken_off_the_channel() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 64);
        for _ in 0..3 {
            rig.midi_tx.send(NOTE_ON_A4).unwrap();
        }
        let mut data = vec![0.0f32; 64 * 2];
        rig.renderer.render(&mut data, 0);
        assert!(rig.midi_tx.is_empty(), "all of it, in one block");
    }

    // -- Denormals --

    /// A note through a delay of a millisecond with the most feedback it has, let go at once: the repeats run down, as the tail of a note does,
    /// through every number a 32-bit float can hold.
    const FAST_ECHO: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nrelease = 0.002\n\
                             [[instrument.effect]]\nplugin = \"builtin:delay\"\nmix = 1.0\n[instrument.effect.params]\ntime = 0.001\nfeedback = 0.95\n";

    /// Render the note of `FAST_ECHO` and the two seconds after it (the key is let go in the
    /// fifth block) with `render`, and count the samples that are denormal: not zero, and nearer
    /// to it than the smallest normal number.
    fn denormals_in_the_tail(render: impl Fn(&mut Renderer, &mut [f32], u64)) -> usize {
        let mut rig = rig(FAST_ECHO, 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 256 * 2];
        let mut denormals = 0;
        for cb in 0..400 {
            if cb == 4 {
                rig.midi_tx.send(MidiEvent::keys(0, [0x80, 69, 0])).unwrap();
            }
            render(&mut rig.renderer, &mut data, cb);
            denormals += data.iter().filter(|s| s.is_subnormal()).count();
        }
        denormals
    }

    #[test]
    fn the_tail_of_a_fast_echo_goes_through_denormals_when_the_unit_is_left_as_it_is() {
        // The witness of the test below: without it, that test would pass for a tail that never had one.
        let denormals = denormals_in_the_tail(|renderer, data, cb| renderer.render_block(data, cb));
        assert!(denormals > 0, "the tail has denormal samples, which are slow to compute with on some processors");
    }

    #[test]
    fn a_rendered_block_has_no_denormals_in_it_even_in_the_tail_of_a_fast_echo() {
        let denormals = denormals_in_the_tail(|renderer, data, cb| renderer.render(data, cb));
        assert_eq!(denormals, 0, "denormals are flushed to zero while a block is rendered");
    }

    // -- Tails that end --

    /// The loudest sample of each block of a note of `session` that is let go in block `hold`, from the release on, for `seconds`.
    fn tail_peaks(session: &str, hold: usize, seconds: f32) -> Vec<f32> {
        let mut rig = rig(session, 256);
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        let mut data = vec![0.0f32; 256 * 2];
        let mut peaks = Vec::new();
        for cb in 0..hold + (seconds * 48_000.0 / 256.0) as usize {
            if cb == hold {
                rig.midi_tx.send(MidiEvent::keys(0, [0x80, 69, 0])).unwrap();
            }
            rig.renderer.render(&mut data, cb as u64);
            if cb >= hold {
                peaks.push(data.iter().fold(0.0f32, |peak, s| peak.max(s.abs())));
            }
        }
        peaks
    }

    /// A saw through the built-in filter of `kind` (its `type` parameter) at `cutoff`, with the resonance the tracer has.
    fn saw_through_a_filter(kind: usize, cutoff: f32) -> String {
        format!(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nwaveform = 3.0\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = {cutoff}\nresonance = 2.0\ntype = {kind}.0\n"
        )
    }

    #[test]
    fn a_note_through_a_filter_of_each_type_and_cutoff_ends_in_silence_after_it_is_let_go() {
        let mut never = Vec::new();
        for kind in 0..7 {
            for cutoff in [0.0, 0.1, 0.5, 0.9] {
                if tail_peaks(&saw_through_a_filter(kind, cutoff), 100, 8.0).last() != Some(&0.0) {
                    never.push(format!("type {kind} at cutoff {cutoff}"));
                }
            }
        }
        assert!(never.is_empty(), "still not silent 8 seconds after the note: {never:?}");
    }

    /// The tracer's filter: a low-pass at 0.5 that an LFO of 0.5 Hz sweeps by 0.4 of its range, so that the cutoff it has when the key goes up is anywhere from 0.1 to 0.9.
    const SWEPT_FILTER: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nwaveform = 3.0\n\
        [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.5\nresonance = 2.0\n\
        [[instrument.modulator]]\ntype = \"lfo\"\nrate = 0.5\n[[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.4\n";

    #[test]
    fn a_note_through_a_filter_that_an_lfo_sweeps_ends_in_silence_wherever_the_sweep_is_when_it_is_let_go() {
        // The LFO's turn takes two seconds, which is 375 blocks: the releases are spread over it.
        let never: Vec<usize> = (60..60 + 375).step_by(25).filter(|&hold| tail_peaks(SWEPT_FILTER, hold, 8.0).last() != Some(&0.0)).collect();
        assert!(never.is_empty(), "still not silent 8 seconds after the note, let go in block: {never:?}");
    }

    #[test]
    fn the_last_sound_in_the_tail_of_a_note_through_a_shut_filter_is_under_1e_15() {
        // The tail that runs down the slowest: what is cut off when the filter lets go of it is not heard.
        let peaks = tail_peaks(&saw_through_a_filter(0, 0.0), 100, 8.0);
        let last_sound = peaks.iter().rev().find(|&&peak| peak != 0.0).copied().expect("a note has a sound");
        assert!(last_sound < 1e-15, "{last_sound:e}");
        assert_eq!(peaks.last(), Some(&0.0), "and then it is silent");
    }

    #[test]
    fn a_guard_makes_a_denormal_a_zero_for_as_long_as_it_lives_and_gives_the_unit_back() {
        // `black_box` at each product, so that the compiler does not make it itself or move it out of the guard.
        let times = |a: f32, b: f32| std::hint::black_box(a) * std::hint::black_box(b);
        let (smallest_normal, smallest_denormal) = (f32::MIN_POSITIVE, f32::from_bits(1));
        assert!(times(smallest_normal, 0.5).is_subnormal(), "as the unit is: half of the smallest normal number is a denormal");
        assert!(times(smallest_denormal, 1.0) > 0.0, "and a denormal is a number");
        {
            let _guard = NoDenormals::new();
            assert_eq!(times(smallest_normal, 0.5), 0.0, "a result that is a denormal is zero");
            assert_eq!(times(smallest_denormal, 1.0), 0.0, "and so is an operand");
        }
        assert!(times(smallest_normal, 0.5).is_subnormal(), "when the guard has gone, the unit is as it was");
        assert!(times(smallest_denormal, 1.0) > 0.0);
    }

    // -- The null backend: `--audio-device none` --

    use std::sync::atomic::AtomicUsize;

    use crate::telemetry::{self, Telemetry};

    /// An engine on the null backend, with the channels it listens on, the cells it
    /// publishes into and a sender for commands, built and not yet playing.
    struct NullRig {
        engine: AudioEngine,
        midi_tx: crossbeam_channel::Sender<MidiEvent>,
        cmd_tx: crossbeam_channel::Sender<crate::plugin::chain::GraphCommand>,
        telemetry: Arc<Telemetry>,
        _return_rx: Receiver<Box<dyn crate::plugin::Plugin>>,
    }

    fn null_rig(buffer_size: u32) -> NullRig {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(1024);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        let telemetry = Arc::new(Telemetry::new());
        graph.set_telemetry(telemetry.clone());
        let engine = AudioEngine::build(graph, midi_rx, Some(NULL_DEVICE), false, 48_000, buffer_size).expect("the null backend needs no device");
        NullRig { engine, midi_tx, cmd_tx, telemetry, _return_rx: return_rx }
    }

    /// Wait up to five seconds for `done`.
    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "gave up waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn without_a_device_a_note_is_rendered_on_a_timer_and_measured() {
        let mut rig = null_rig(256);
        load_onto("[[instrument]]\nplugin = \"builtin:osc\"\n", &rig.cmd_tx);
        rig.engine.play().unwrap();
        assert_eq!(rig.telemetry.load(telemetry::MASTER.peak), 0.0, "silence before a note");
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        wait_for("the master's peak to rise", || rig.telemetry.load(telemetry::MASTER.peak) > 0.1);
        assert_eq!(rig.telemetry.count(telemetry::MASTER.clips), 0, "and it does not clip");
        rig.engine.stop();
    }

    #[test]
    fn a_lane_that_is_too_loud_clips_and_the_clips_are_counted() {
        // Three oscillators at four times the volume: far past full scale. Safe to
        // run: nothing is played.
        let mut rig = null_rig(256);
        let lane = "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 4.0\n";
        // (The limiter on the output is in the sound unless a session says it is not, and keeps a mix under its ceiling: this session says it is not, to have a mix that clips.)
        load_onto(&format!("[master]\nlimiter = false\n{}", lane.repeat(3)), &rig.cmd_tx);
        rig.engine.play().unwrap();
        rig.midi_tx.send(NOTE_ON_A4).unwrap();
        wait_for("a clip to be counted", || rig.telemetry.count(telemetry::MASTER.clips) > 0);
        assert!(rig.telemetry.load(telemetry::MASTER.peak) > 1.0, "the peak is past full scale");
        let counted = rig.telemetry.count(telemetry::MASTER.clips);
        wait_for("another buffer to clip", || rig.telemetry.count(telemetry::MASTER.clips) > counted);
        rig.engine.stop();
    }

    /// An instrument that renders nothing, counts the blocks it is asked for, and
    /// says which thread dropped it.
    struct Probe {
        blocks: Arc<AtomicUsize>,
        /// How many frames the last block had.
        frames: Arc<AtomicUsize>,
        dropped_on: crossbeam_channel::Sender<std::thread::ThreadId>,
    }

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.dropped_on.send(std::thread::current().id());
        }
    }

    impl crate::plugin::Plugin for Probe {
        fn name(&self) -> &str {
            "Probe"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn sample_rate(&self) -> f32 {
            48_000.0
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            0
        }
        fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], out: &mut [&mut [f32]]) -> anyhow::Result<()> {
            self.frames.store(out[0].len(), Ordering::Relaxed);
            self.blocks.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn parameters(&self) -> Vec<crate::plugin::ParameterInfo> {
            Vec::new()
        }
        fn get_parameter(&mut self, _: u32) -> Option<f32> {
            None
        }
        fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
            Ok(())
        }
        fn presets(&self) -> Vec<crate::plugin::Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A rig with a probe in the first lane, the blocks it has been asked for, the
    /// frames of the last one, and where it reports being dropped.
    fn probed(buffer_size: u32) -> (NullRig, Arc<AtomicUsize>, Arc<AtomicUsize>, Receiver<std::thread::ThreadId>) {
        use crate::plugin::chain::GraphCommand;
        let rig = null_rig(buffer_size);
        let blocks = Arc::new(AtomicUsize::new(0));
        let frames = Arc::new(AtomicUsize::new(0));
        let (dropped_on, dropped) = crossbeam_channel::unbounded();
        rig.cmd_tx.send(GraphCommand::AddInstrument { range: None, node: crate::plugin::chain::test_node() }).unwrap();
        rig.cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: Box::new(Probe { blocks: blocks.clone(), frames: frames.clone(), dropped_on }),
                inst_buf: vec![Vec::new(), Vec::new()],
                remapper: None,
                keep: None,
            })
            .unwrap();
        (rig, blocks, frames, dropped)
    }

    #[test]
    fn nothing_is_rendered_before_play_and_blocks_come_at_the_pace_of_the_buffer() {
        // 480 frames at 48 kHz: one block every 10 ms.
        let (mut rig, blocks, frames, _dropped) = probed(480);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(blocks.load(Ordering::Relaxed), 0, "not before play");
        rig.engine.play().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let rendered = blocks.load(Ordering::Relaxed);
        // About 50, with room for a busy machine: not a free-running loop (thousands),
        // and not stalled.
        assert!((15..=60).contains(&rendered), "{rendered} blocks in half a second of 10 ms blocks");
        assert_eq!(frames.load(Ordering::Relaxed), 480, "each block is the buffer's size");
        rig.engine.stop();
    }

    #[test]
    fn stopping_ends_the_rendering_and_drops_the_plugins_on_the_thread_that_stops() {
        let (mut rig, blocks, _frames, dropped) = probed(128);
        rig.engine.play().unwrap();
        wait_for("blocks to be rendered", || blocks.load(Ordering::Relaxed) > 3);
        rig.engine.stop();
        let after_stop = blocks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(blocks.load(Ordering::Relaxed), after_stop, "nothing is rendered after stop");
        let thread = dropped.recv_timeout(Duration::from_secs(5)).expect("the plugin was dropped");
        assert_eq!(thread, std::thread::current().id(), "here, not on the rendering thread");
    }

    #[test]
    fn an_engine_that_never_played_stops_cleanly() {
        let (rig, blocks, _frames, _dropped) = probed(128);
        rig.engine.stop();
        assert_eq!(blocks.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn play_twice_does_not_start_two_threads() {
        let (mut rig, blocks, _frames, _dropped) = probed(480);
        rig.engine.play().unwrap();
        rig.engine.play().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let rendered = blocks.load(Ordering::Relaxed);
        assert!(rendered <= 40, "one thread's worth of 10 ms blocks in 300 ms: {rendered}");
        rig.engine.stop();
    }

    #[test]
    fn a_block_is_as_long_as_the_buffer_in_time() {
        assert_eq!(block_period(480, 48_000), Duration::from_millis(10));
        assert_eq!(block_period(1024, 48_000), Duration::from_secs_f64(1024.0 / 48_000.0));
        assert_eq!(block_period(256, 44_100), Duration::from_secs_f64(256.0 / 44_100.0));
    }

    #[test]
    fn the_next_block_is_due_a_period_after_the_last_unless_the_engine_fell_far_behind() {
        let t0 = Instant::now();
        let period = Duration::from_millis(10);
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // On time, early or a little late: the cadence holds, so that sleeping does not make it drift.
        assert_eq!(next_deadline(at(0), period, at(2)), at(10));
        assert_eq!(next_deadline(at(0), period, at(0)), at(10));
        assert_eq!(next_deadline(at(0), period, at(15)), at(10), "5 ms behind the next one is still caught up by running early");
        assert_eq!(next_deadline(at(0), period, at(20)), at(10), "exactly one period behind: the next block is due now-ish");
        // More than a period behind (a stall): do not burst through the missed blocks.
        assert_eq!(next_deadline(at(0), period, at(35)), at(35));
    }

    // -- The example sessions --

    /// The example sessions of this repository that show what the built-in plugins do (the others name a plugin that has to be installed, or are somebody's saved sound).
    const SOUND_EXAMPLES: [&str; 18] = [
        "supersaw.toml",
        "wind.toml",
        "bell.toml",
        "arp.toml",
        "pwm.toml",
        "epiano.toml",
        "sync-lead.toml",
        "sub-bass.toml",
        "fold.toml",
        "wobble.toml",
        "gate.toml",
        "melody.toml",
        "shared-reverb.toml",
        "shared-lfo.toml",
        "ring-bell.toml",
        "growl.toml",
        "pump.toml",
        "microphone.toml",
    ];

    fn example(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The left side of `session` as it sounds for `seconds` when the keys `keys` go down after `from` seconds and up `held` seconds after that (at the velocity that the keys of
    /// the page and of the terminal have, 100).
    fn play(session: &str, keys: &[u8], from: f32, held: f32, seconds: f32) -> Vec<f32> {
        play_hit(session, keys, 100, from, held, seconds)
    }

    /// The same, with the keys hit at `velocity`.
    fn play_hit(session: &str, keys: &[u8], velocity: u8, from: f32, held: f32, seconds: f32) -> Vec<f32> {
        let mut rig = rig(session, 256);
        let blocks = (seconds * 48_000.0 / 256.0) as usize;
        let press_at = (from * 48_000.0 / 256.0) as usize;
        let release_at = ((from + held) * 48_000.0 / 256.0) as usize;
        let mut data = vec![0.0f32; 256 * 2];
        let mut left = Vec::with_capacity(blocks * 256);
        for cb in 0..blocks {
            if cb == press_at {
                for &key in keys {
                    rig.midi_tx.send(MidiEvent::keys(0, [0x90, key, velocity])).unwrap();
                }
            }
            if cb == release_at {
                for &key in keys {
                    rig.midi_tx.send(MidiEvent::keys(0, [0x80, key, 0])).unwrap();
                }
            }
            rig.renderer.render(&mut data, cb as u64);
            left.extend(data.chunks(2).map(|frame| frame[0]));
        }
        left
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn the_example_sounds_load_and_play_a_chord_that_is_heard_and_is_not_loud() {
        for name in SOUND_EXAMPLES {
            let out = play(&example(name), &[48, 52, 55], 0.0, 5.0, 8.0);
            let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
            assert!(out.iter().all(|x| x.is_finite()), "{name}");
            assert!(peak > 0.05, "{name} is heard: its loudest sample is {peak}");
            assert!(peak < 0.6, "{name} is not loud: its loudest sample is {peak}");
        }
    }

    #[test]
    fn the_microphone_example_is_heard_through_its_cables_alone_and_the_tone_of_the_null_backend_stands_in_for_the_microphone() {
        // (In a test, as under `--audio-device none`, the audio input plays a sine of 220 Hz at 0.25 where there would be a microphone, and no key is held: the input does not hear them.)
        let session = example("microphone.toml");
        let out = play(&session, &[], 0.0, 0.0, 3.0);
        assert!(rms(&out[48_000..]) > 0.03, "the tone is heard through the cables: {}", rms(&out[48_000..]));
        // The lane has no way out of its own: with the two cables taken out the session is silent, to the bit (what is left is a lane that is cut and a lane of effects that nothing feeds).
        let without = session.split("\n# The microphone into the echo").next().expect("the cables come last");
        assert!(!without.contains("[[cable]]"), "{without}");
        let silent = play(without, &[], 0.0, 0.0, 3.0);
        assert!(silent.iter().all(|sample| *sample == 0.0), "no cable, no sound: {}", silent.iter().fold(0.0f32, |m, x| m.max(x.abs())));
        // The dry cable alone is the tone at half of the lane's 0.8 volume, a sine of 0.25: its peak is 0.1, and the echo and the reverb are more.
        let dry_only = session.replace("to = \"instrument[1].effect[0]\"\n", "to = \"instrument[1].effect[0]\"\ngain = 0.0\n");
        let dry = play(&dry_only, &[], 0.0, 0.0, 3.0);
        let peak = dry[48_000..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.1).abs() < 0.005, "the tone at half of 0.8 of 0.25: {peak}");
    }

    /// The frequency of the periodic signal in `window`, from its autocorrelation: the first of the lags between 40 and 1000 Hz that is nearly as good as the best (the multiples of a
    /// period are as good as the period itself), brought between two samples. It is biased for a wave that is not a sine: a triangle at F3 reads 5.6 cents low (measured), so a test that asks
    /// for less than that of one wants `pitch_by_crossings`, which reads a wave that is centred to 0.01 cent.
    fn pitch_of(window: &[f32], sample_rate: f32) -> f32 {
        let (shortest, longest) = ((sample_rate / 1000.0) as usize, (sample_rate / 40.0) as usize);
        let correlation: Vec<f32> = (shortest..=longest).map(|lag| window[..window.len() - longest].iter().zip(&window[lag..]).map(|(a, b)| a * b).sum()).collect();
        let best = correlation.iter().cloned().fold(f32::MIN, f32::max);
        let first = (1..correlation.len() - 1)
            .find(|&i| correlation[i] > 0.9 * best && correlation[i] >= correlation[i - 1] && correlation[i] >= correlation[i + 1])
            .expect("a period");
        let (a, b, c) = (correlation[first - 1], correlation[first], correlation[first + 1]);
        let offset = 0.5 * (a - c) / (a - 2.0 * b + c);
        sample_rate / ((shortest + first) as f32 + offset)
    }

    #[test]
    fn the_arp_example_goes_through_the_notes_of_a_minor_seventh_in_tune_when_a_key_is_held() {
        // C3 held, after a pause that is not a whole number of steps: the steps are the semitones 0 3 7 10 12 10 7 3 of a chord, six a second, and the key starts them from the first.
        let out = play(&example("arp.toml"), &[48], 0.4, 3.0, 3.6);
        let c3 = 440.0 * 2.0f32.powf(-21.0 / 12.0);
        for (step, semitones) in [0, 3, 7, 10, 12, 10, 7, 3, 0, 3, 7, 10].into_iter().enumerate() {
            let start = ((0.4 + step as f32 / 6.0 + 0.03) * 48_000.0) as usize;
            let heard = pitch_of(&out[start..start + 4800], 48_000.0);
            let wanted = c3 * 2.0f32.powf(semitones as f32 / 12.0);
            let cents = 1200.0 * (heard / wanted).log2();
            assert!(cents.abs() < 5.0, "step {step}: {heard} Hz for {wanted} Hz, {cents} cents off");
        }
    }

    /// `session` with its tempo said to be `bpm` beats a minute: its own `bpm` line, if it has one, is replaced (a key of the top level goes before every table).
    fn at_tempo(session: &str, bpm: f32) -> String {
        let rest: Vec<&str> = session.lines().filter(|line| !line.trim_start().starts_with("bpm")).collect();
        format!("bpm = {bpm:?}\n{}", rest.join("\n"))
    }

    #[test]
    fn the_arp_example_goes_by_the_tempo_and_not_by_its_own_rate() {
        // Its steps are an eighth-note triplet each: six a second at 120 beats a minute (which is the rate it keeps), three at 60 and nine at 180.
        let c3 = 440.0 * 2.0f32.powf(-21.0 / 12.0);
        for (bpm, per_second) in [(60.0f32, 3.0f32), (180.0, 9.0)] {
            let out = play(&at_tempo(&example("arp.toml"), bpm), &[48], 0.4, 3.0, 3.4);
            for (step, semitones) in [0, 3, 7, 10, 12, 10, 7, 3].into_iter().enumerate() {
                // (A window of 75 ms, a tenth of the way into the step: it is over before the step is, at nine a second too.)
                let start = ((0.4 + (step as f32 + 0.1) / per_second) * 48_000.0) as usize;
                let heard = pitch_of(&out[start..start + 3600], 48_000.0);
                let wanted = c3 * 2.0f32.powf(semitones as f32 / 12.0);
                let cents = 1200.0 * (heard / wanted).log2();
                assert!(cents.abs() < 5.0, "at {bpm} beats a minute, step {step}: {heard} Hz for {wanted} Hz, {cents} cents off");
            }
        }
    }

    /// The period of the signal in `series`, in its samples: from its autocorrelation with the mean taken out, the first peak between the lags `shortest` and `longest` that is nearly
    /// as good as the best (the multiples of a period are as good as the period), brought between two samples.
    fn period_in(series: &[f32], shortest: usize, longest: usize) -> f32 {
        let mean = series.iter().sum::<f32>() / series.len() as f32;
        let x: Vec<f32> = series.iter().map(|v| v - mean).collect();
        let correlation: Vec<f32> = (shortest..=longest).map(|lag| x[..x.len() - longest].iter().zip(&x[lag..]).map(|(a, b)| a * b).sum()).collect();
        let best = correlation.iter().cloned().fold(f32::MIN, f32::max);
        let first = (1..correlation.len() - 1)
            .find(|&i| correlation[i] > 0.8 * best && correlation[i] >= correlation[i - 1] && correlation[i] >= correlation[i + 1])
            .expect("a period");
        let (a, b, c) = (correlation[first - 1], correlation[first], correlation[first + 1]);
        (shortest + first) as f32 + 0.5 * (a - c) / (a - 2.0 * b + c)
    }

    #[test]
    fn the_wobble_example_goes_round_in_an_eighth_note_of_its_tempo_and_in_twice_as_long_at_half_of_it() {
        // How long the filter takes to open and shut again, from the brightness of the sound (how much of it is in its changes) in a window of 40 ms every 5 ms, from a second in.
        let seconds_a_cycle = |bpm: f32| {
            let out = play(&at_tempo(&example("wobble.toml"), bpm), &[48], 0.0, 5.0, 4.5);
            let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
            let series: Vec<f32> = (0..).map(|k| 48_000 + k * 240).take_while(|from| from + 1920 <= out.len()).map(|from| brightness(&out[from..from + 1920])).collect();
            period_in(&series, 20, 250) * 240.0 / 48_000.0
        };
        let (fast, slow) = (seconds_a_cycle(140.0), seconds_a_cycle(70.0));
        // An eighth note is half a beat: 0.2143 s at 140 beats a minute and 0.4286 s at 70.
        assert!((fast - 0.5 * 60.0 / 140.0).abs() < 0.03 * fast, "a cycle takes {fast} s at 140 beats a minute");
        assert!((slow / fast - 2.0).abs() < 0.04, "and {slow} s at 70: {} times as long", slow / fast);
    }

    #[test]
    fn the_gate_example_opens_and_shuts_on_the_sixteenths_of_its_tempo_and_is_twice_as_slow_at_half_of_it() {
        // Its sixteen steps, as the file says them: a step that is heard, and one that is not.
        let heard = [true, false, true, true, false, true, false, true, true, false, false, true, false, true, true, false];
        for (bpm, step_seconds) in [(120.0f32, 0.125f32), (60.0, 0.25)] {
            let out = play(&at_tempo(&example("gate.toml"), bpm), &[48], 0.4, 5.0, 4.4);
            // (The middle of each step: from three tenths of it to seven, which a block's lateness and the 5 ms that the volume is gone to over do not reach.)
            let level = |step: usize| {
                let from = 0.4 + step as f32 * step_seconds;
                rms(&out[((from + 0.3 * step_seconds) * 48_000.0) as usize..((from + 0.7 * step_seconds) * 48_000.0) as usize])
            };
            let loudest = (0..16).map(level).fold(0.0f32, f32::max);
            assert!(loudest > 0.02, "it is heard at {bpm} beats a minute: {loudest}");
            for (step, open) in heard.into_iter().enumerate() {
                // (The three oscillators of a note beat against each other, so the steps that are heard are not all as loud: from a half to the loudest, measured.)
                if open {
                    assert!(level(step) > 0.3 * loudest, "step {step} is heard at {bpm} beats a minute: {} of {loudest}", level(step));
                } else {
                    assert!(level(step) < 0.05 * loudest, "step {step} is not heard at {bpm} beats a minute: {} of {loudest}", level(step));
                }
            }
        }
    }

    /// The frequency of the signal in `window` from the places where it goes up through zero (each brought between two samples): for a wave that is centred, as a triangle is, which `pitch_of`
    /// reads up to 5.6 cents wrong at some pitches (measured: F3 as 174.6 Hz less 5.6 cents) where this reads 0.01 cent.
    fn pitch_by_crossings(window: &[f32], sample_rate: f32) -> f32 {
        let crossings: Vec<f32> = window.windows(2).enumerate().filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0).map(|(i, w)| i as f32 + (-w[0]) / (w[1] - w[0])).collect();
        (crossings.len() - 1) as f32 * sample_rate / (crossings[crossings.len() - 1] - crossings[0])
    }

    #[test]
    fn the_melody_example_plays_only_the_notes_of_the_minor_pentatonic_counted_from_the_key_and_changes_note_on_the_eighths_of_its_tempo() {
        // C4 held: a new number every eighth note (0.25 s at 120 beats a minute), held by `quantize` to the tones 0 3 5 7 10 of a minor pentatonic from the key, in any octave.
        let out = play(&example("melody.toml"), &[60], 0.0, 4.5, 4.4);
        let c4 = 440.0 * 2.0f32.powf(-9.0 / 12.0);
        // The tone, in semitones from the key, of a window of 75 ms that begins `fraction` of the way into a step.
        let tone_at = |step: usize, fraction: f32| {
            let start = ((step as f32 + fraction) * 0.25 * 48_000.0) as usize;
            let semitones = 12.0 * (pitch_by_crossings(&out[start..start + 3600], 48_000.0) / c4).log2();
            let nearest = semitones.round();
            assert!((semitones - nearest).abs() < 0.05, "step {step}: {semitones} semitones from the key is not a note");
            assert!([0, 3, 5, 7, 10].contains(&(nearest as i32).rem_euclid(12)), "step {step}: {nearest} semitones from the key is not in the minor pentatonic");
            nearest as i32
        };
        let steps: Vec<i32> = (0..16).map(|step| tone_at(step, 0.1)).collect();
        // A step is one note all through (a number that goes faster than the eighths would change it halfway), and most steps are another note than the one before (a number goes slower than the
        // eighths and a note is held for two: at most half of them; a random number is held to the same tone as the last about one time in eight).
        for (step, &tone) in steps.iter().enumerate() {
            assert_eq!(tone_at(step, 0.6), tone, "step {step} is one note");
        }
        let changes = steps.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(changes >= 10, "the note changes at {changes} of 15 steps: {steps:?}");
        assert!(steps.iter().collect::<std::collections::BTreeSet<_>>().len() >= 5, "it is a melody and not one note: {steps:?}");
    }

    #[test]
    fn the_bell_example_starts_bright_and_dies_away_to_a_plain_tone() {
        let out = play(&example("bell.toml"), &[72], 0.0, 3.0, 4.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        // How much of a signal is in its changes: more for a signal with high partials than for one with a low tone.
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (start, later) = (brightness(window(0.02)), brightness(window(1.8)));
        assert!(start > 4.0 * later, "the first moments are brighter than the later ones: {start} and {later}");
        assert!(rms(window(1.8)) < 0.6 * rms(window(0.02)), "and louder");
    }

    #[test]
    fn the_wind_example_swells_in_over_the_attack_and_is_still_there_when_the_key_goes_up() {
        let out = play(&example("wind.toml"), &[48], 0.0, 5.0, 8.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.2) * 48_000.0) as usize];
        // An attack of two and a half seconds: the first moments are far quieter than the held sound, and the release (four seconds) is far from over a moment after the key goes up.
        assert!(rms(window(0.1)) < 0.2 * rms(window(4.5)), "{} and {}", rms(window(0.1)), rms(window(4.5)));
        assert!(rms(window(5.2)) > 0.5 * rms(window(4.5)), "{} and {}", rms(window(5.2)), rms(window(4.5)));
    }

    #[test]
    fn the_shared_reverb_example_has_no_group_its_room_is_a_reverb_in_a_lane_that_has_no_instrument_and_both_oscillators_are_cabled_to_it() {
        // (The page has no way to make a group, step 103: the room that the example had on the bus of a group is a reverb that stands by itself, and it sounds the same to the bit.)
        use crate::plugin::chain::{CableFrom, CableTo};
        let run = crate::app::testutil::load(&example("shared-reverb.toml"));
        assert!(run.model.groups.is_empty(), "the example has no group");
        assert_eq!(run.model.instruments.len(), 3);
        let room = &run.model.instruments[2];
        assert!(room.instrument.is_none(), "the room's lane has no instrument");
        assert_eq!(room.effects.iter().map(|effect| effect.name.as_str()).collect::<Vec<_>>(), ["Reverb"]);
        let reverb = room.effects[0].node;
        let sends: Vec<_> = run.model.cables.iter().map(|cable| (cable.from, cable.to, cable.gain)).collect();
        let (saw, square) = (run.model.instruments[0].node, run.model.instruments[1].node);
        assert_eq!(sends, [(CableFrom::Instrument(saw), CableTo::Effect(reverb), 1.0), (CableFrom::Instrument(square), CableTo::Effect(reverb), 0.6)]);
    }

    #[test]
    fn the_shared_reverb_example_rings_on_in_a_room_that_both_oscillators_are_sent_to_by_their_cables_and_not_otherwise() {
        // The keys go up after a second, and the oscillators are done a quarter of a second after that (their release): what is heard from 1.6 s on is the reverb's tail, which only
        // the cables bring into the reverb (it stands by itself, in a lane that has no instrument), so that the same session with its cables taken out has none, and with one of them has the room of one oscillator.
        let session = example("shared-reverb.toml");
        // (The tables, at the start of a line: the header of the file says `[[cable]]` too.)
        let first = session.find("\n[[cable]]\n").expect("the example has its cables") + 1;
        let second = first + 1 + session[first + 1..].find("\n[[cable]]\n").expect("and two of them") + 1;
        let tail = |session: &str| {
            let out = play(session, &[48, 52, 55], 0.0, 1.0, 3.0);
            out[(1.6 * 48_000.0) as usize..(2.6 * 48_000.0) as usize].to_vec()
        };
        let both = tail(&session);
        let neither = tail(&session[..first]);
        let saw_only = tail(&session[..second]);
        let square_only = tail(&format!("{}{}", &session[..first], &session[second..]));
        assert!(rms(&both) > 0.002, "the room rings on after the keys: {}", rms(&both));
        assert!(rms(&neither) < 1e-6, "and does not without the cables: {}", rms(&neither));
        assert!(rms(&saw_only) > 0.001 && rms(&square_only) > 0.001, "each cable brings its oscillator into the room: {} and {}", rms(&saw_only), rms(&square_only));
        // One room that both are in: the reverb is linear at this level, so what the two cables bring is what each brings, added (the two do not add up in size: the saw's partials and the square's
        // that are the same pitch are in step or against each other, and the tail with both is a little smaller than the saw's alone).
        let worst = both.iter().zip(saw_only.iter().zip(&square_only)).map(|(b, (s, q))| (b - s - q).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-4 * rms(&both), "the room of both is the room of each, added: the largest difference is {worst}, against an rms of {}", rms(&both));
    }

    #[test]
    fn the_shared_reverb_example_with_the_way_out_of_both_oscillators_cut_is_the_room_alone() {
        // With their way out cut the oscillators are heard through the room and not beside it. The reverb is linear at this level, so that the room is what the whole session is less the
        // oscillators as they are heard without it (the session with its cables taken out), and the session with their way out cut is that, from the first moment of a chord that is held.
        let session = example("shared-reverb.toml");
        let first = session.find("\n[[cable]]\n").expect("the example has its cables") + 1;
        let heard = |session: &str| play(session, &[48, 52, 55], 0.0, 1.0, 3.0);
        let whole = heard(&session);
        let oscillators = heard(&session[..first]);
        // (The oscillators' tables are the ones that have a plugin: the room's lane has none, and keeps its way out.)
        let cut = heard(&session.replace("[[instrument]]\nplugin = \"builtin:osc\"\n", "[[instrument]]\nroute = \"none\"\nplugin = \"builtin:osc\"\n"));
        let room: Vec<f32> = whole.iter().zip(&oscillators).map(|(w, o)| w - o).collect();
        assert!(rms(&room) > 0.002, "the room is heard: {}", rms(&room));
        let worst = cut.iter().zip(&room).map(|(c, r)| (c - r).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5 * rms(&room), "the session with their way out cut is the room alone: the largest difference is {worst}, against an rms of {}", rms(&room));
        // (The oscillators beside the room are a good deal more than the room: the whole is not the cut session.)
        assert!(rms(&whole) > 2.0 * rms(&cut), "{} and {}", rms(&whole), rms(&cut));
    }

    #[test]
    fn the_shared_reverb_example_with_the_saw_soloed_is_the_saw_and_the_room_that_it_sends_to_and_the_session_with_the_square_muted() {
        // The saw is soloed: it is heard as it is, and the room is heard for its cable, though the room itself is not soloed; the square is not soloed, so that it is not heard and sends
        // nothing, as when it is muted, and the two sessions are one sound to the bit.
        let session = example("shared-reverb.toml");
        let heard = |session: &str| play(session, &[48, 52, 55], 0.0, 1.0, 3.0);
        let soloed = heard(&session.replacen("[[instrument]]\n", "[[instrument]]\nsolo = true\n", 1));
        let square = session.find("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.12\n").expect("the square's table") + "[[instrument]]\n".len();
        let muted = heard(&format!("{}mute = true\n{}", &session[..square], &session[square..]));
        assert_eq!(soloed, muted, "soloing the saw is muting the square");
        // It is not nothing, and not the whole session: the room rings on after the keys (the saw's cable alone brings it, and it is the room that the file with that cable alone has), and the square is gone.
        let first = session.find("\n[[cable]]\n").expect("the example has its cables") + 1;
        let second = first + 1 + session[first + 1..].find("\n[[cable]]\n").expect("and two of them") + 1;
        let tail = |out: &[f32]| out[(1.6 * 48_000.0) as usize..(2.6 * 48_000.0) as usize].to_vec();
        let (room, saw_only) = (tail(&soloed), tail(&heard(&session[..second])));
        assert!(rms(&room) > 0.001, "the room is heard for the saw's cable: {}", rms(&room));
        let worst = room.iter().zip(&saw_only).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        // (Measured: an rms of 0.00265, and no difference at all, the same bits; the test asks for a millionth of the rms, so that it does not hold a platform whose rounding is another's to the bit.)
        assert!(worst < 1e-6 * rms(&room), "the room is the room of the saw's cable alone: the largest difference is {worst}, against an rms of {}", rms(&room));
        assert_ne!(soloed, heard(&session), "the square is not in what is heard");
    }

    #[test]
    fn the_shared_lfo_example_has_one_lfo_that_is_in_no_lane_with_a_cable_to_the_filter_of_each_lane() {
        // The file's `to` names a plugin by its place in the session: the first effect of the first lane and of the second. They are the two filters, and the cables are the two depths.
        let run = crate::app::testutil::load(&example("shared-lfo.toml"));
        assert_eq!(run.model.free_modulators.len(), 1, "one modulator that is nobody's");
        let lfo = &run.model.free_modulators[0];
        let filters: Vec<_> = run.model.instruments.iter().map(|lane| (lane.effects[0].name.as_str(), lane.effects[0].node)).collect();
        assert_eq!(filters.iter().map(|(name, _)| *name).collect::<Vec<_>>(), ["Filter", "Filter"]);
        let aims: Vec<_> = lfo.targets.iter().map(|t| (t.aim, t.param_name.as_str(), t.depth)).collect();
        let (first, second) = (filters[0].1, filters[1].1);
        assert_eq!(aims.len(), 2);
        assert!(matches!(aims[0], (crate::app::model::Driven::Param { plugin, .. }, "cutoff", depth) if plugin == first && depth == 0.2), "{aims:?}");
        assert!(matches!(aims[1], (crate::app::model::Driven::Param { plugin, .. }, "cutoff", depth) if plugin == second && depth == -0.2), "{aims:?}");
    }

    #[test]
    fn the_shared_lfo_example_opens_the_filter_of_one_lane_as_it_shuts_the_filter_of_the_other_and_is_another_sound_without_its_lfo() {
        // The LFO is a sine of 0.5 Hz that begins at its middle, so that it is at its top at 0.5 s and at its bottom at 1.5 s. The saw's filter has +0.2 of it and the square's has -0.2, so
        // each lane alone (the other one muted: a mute is heard, and nothing else is changed) is brightest at the top for the saw and at the bottom for the square.
        let session = example("shared-lfo.toml");
        let lane_one = session.find("[[instrument]]\n").expect("the first lane") + "[[instrument]]\n".len();
        let lane_two = session.rfind("[[instrument]]\n").expect("the second lane") + "[[instrument]]\n".len();
        let saw_alone = format!("{}mute = true\n{}", &session[..lane_two], &session[lane_two..]);
        let square_alone = format!("{}mute = true\n{}", &session[..lane_one], &session[lane_one..]);
        let heard = |session: &str| play(session, &[48, 52, 55], 0.0, 3.0, 3.0);
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let window = |out: &[f32], from: f32| out[(from * 48_000.0) as usize..((from + 0.2) * 48_000.0) as usize].to_vec();
        let (saw, square) = (heard(&saw_alone), heard(&square_alone));
        let (saw_top, saw_bottom) = (brightness(&window(&saw, 0.4)), brightness(&window(&saw, 1.4)));
        let (square_top, square_bottom) = (brightness(&window(&square, 0.4)), brightness(&window(&square, 1.4)));
        // (Measured: the saw 0.142 at the top and 0.025 at the bottom, 5.8 times, and the square 0.037 and 0.166, 4.4 times; the test asks for 2.)
        assert!(saw_top > 2.0 * saw_bottom, "the saw is brightest when the LFO is at its top: {saw_top} at the top and {saw_bottom} at the bottom");
        assert!(square_bottom > 2.0 * square_top, "and the square when it is at its bottom: {square_top} at the top and {square_bottom} at the bottom");
        // Without the LFO the two filters stay where they are set, and the session is another sound.
        let without = &session[..session.find("\n[[modulator]]\n").expect("the example's LFO") + 1];
        let (with, still) = (heard(&session), heard(without));
        let worst = with.iter().zip(&still).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        // (Measured: 0.43, against a loudest sample of 0.47; the test asks for 0.05.)
        assert!(worst > 0.05, "the LFO is heard: the largest difference is {worst}");
    }

    /// The size of the single bin of the Hann-windowed Fourier transform of `samples` at `hz`, in relative units (the window's scale cancels in a ratio of two of them).
    fn level_at(samples: &[f32], hz: f32) -> f32 {
        let omega = std::f32::consts::TAU * hz / 48_000.0;
        let coefficient = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        let n = samples.len() as f32;
        for (i, &x) in samples.iter().enumerate() {
            let window = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (n - 1.0)).cos();
            let s = x * window + coefficient * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        let (real, imaginary) = (s1 - s2 * omega.cos(), s2 * omega.sin());
        (real * real + imaginary * imaginary).sqrt()
    }

    #[test]
    fn the_ring_bell_example_has_a_cable_into_each_input_of_its_math_effect() {
        let run = crate::app::testutil::load(&example("ring-bell.toml"));
        // The third lane holds the effects and no instrument: the math effect, and a reverb after it.
        let lane = &run.model.instruments[2];
        assert!(lane.instrument.is_none(), "the lane that holds the effects has no instrument");
        assert_eq!(lane.effects.iter().map(|fx| fx.name.as_str()).collect::<Vec<_>>(), ["Math", "Reverb"]);
        let math = lane.effects[0].node;
        assert!(lane.effects[0].side.is_some(), "the math effect has a second input");
        let ends: Vec<_> = run.model.cables.iter().map(|cable| (cable.from, cable.to)).collect();
        assert_eq!(
            ends,
            [
                (crate::plugin::chain::CableFrom::Instrument(run.model.instruments[0].node), crate::plugin::chain::CableTo::Effect(math)),
                (crate::plugin::chain::CableFrom::Instrument(run.model.instruments[1].node), crate::plugin::chain::CableTo::EffectSide(math)),
            ]
        );
    }

    #[test]
    fn the_ring_bell_example_has_the_sum_and_the_difference_of_the_two_pitches_and_neither_of_them_and_the_effects_own_carrier_when_the_cable_to_b_is_out() {
        // C3 on the first oscillator and F#3 (a tritone above) on the second: the product of two sines is a sine at the sum of their pitches and one at the difference, and none at their own.
        let (c3, f_sharp3) = (440.0 * 2.0f32.powf(-21.0 / 12.0), 440.0 * 2.0f32.powf(-15.0 / 12.0));
        let session = example("ring-bell.toml");
        let window = |out: &[f32]| out[(0.05 * 48_000.0) as usize..(0.35 * 48_000.0) as usize].to_vec();
        let both = window(&play(&session, &[48], 0.0, 1.0, 1.0));
        let (sum, difference) = (level_at(&both, c3 + f_sharp3), level_at(&both, f_sharp3 - c3));
        let (own, other) = (level_at(&both, c3), level_at(&both, f_sharp3));
        // (Measured: the sum is 119 and the difference 114, against 0.15 and 0.03 at the two pitches themselves, 770 times as much; the test asks for 100.)
        assert!(sum > 100.0 * own.max(other) && difference > 100.0 * own.max(other), "sum {sum} and difference {difference}, against {own} and {other} at the two pitches themselves");
        // With the second cable taken out, nothing is patched to B, and B is the effect's own carrier: a sine of the Ring Bell's pitch, E4. What is heard is the first oscillator times that.
        let e4 = 440.0 * 2.0f32.powf(-5.0 / 12.0);
        let carrier = window(&play(&session[..session.rfind("\n[[cable]]\n").expect("the second cable") + 1], &[48], 0.0, 1.0, 1.0));
        let (up, down) = (level_at(&carrier, e4 + c3), level_at(&carrier, e4 - c3));
        // (Measured: 327 and 342, against 0.09 and 0.05 where the two oscillators' sum and difference were, and 0.41 and 0.20 at the pitches themselves: 790 times the loudest of those.)
        assert!(up > 100.0 * level_at(&carrier, c3 + f_sharp3) && down > 100.0 * level_at(&carrier, f_sharp3 - c3), "{up} and {down} at the carrier's sum and difference");
        assert!(up > 100.0 * level_at(&carrier, c3) && down > 100.0 * level_at(&carrier, e4), "{up} and {down}, against the pitches themselves");
    }

    #[test]
    fn the_growl_example_has_a_math_effect_in_its_chain_with_nothing_at_its_second_input_and_an_lfo_on_its_carrier() {
        let run = crate::app::testutil::load(&example("growl.toml"));
        let lane = &run.model.instruments[0];
        assert_eq!(lane.effects.iter().map(|fx| fx.name.as_str()).collect::<Vec<_>>(), ["Math"]);
        assert!(lane.effects[0].side.is_some());
        assert!(run.model.cables.is_empty(), "nothing is patched to B: the effect's own carrier is");
        let lfo = &run.model.free_modulators[0];
        let aims: Vec<_> = lfo.targets.iter().map(|t| (t.aim, t.param_name.as_str(), t.depth)).collect();
        assert!(matches!(aims[..], [(crate::app::model::Driven::Param { plugin, .. }, "pitch", depth)] if plugin == lane.effects[0].node && depth == 0.04), "{aims:?}");
        // The preset is the Xor Growl's, with its bits turned down to four.
        let value = |name: &str| lane.effects[0].params.iter().find(|p| p.name == name).unwrap().value;
        assert_eq!((value("mode"), value("bits"), value("pitch")), (7.0, 3.0, 40.0));
    }

    #[test]
    fn the_growl_example_is_another_sound_than_the_saw_it_is_made_of_and_the_lfo_on_its_carrier_is_heard() {
        let session = example("growl.toml");
        let heard = |session: &str| play(session, &[48], 0.0, 4.0, 4.0);
        let growl = heard(&session);
        // Without the Math effect: the saw alone (the effect's table and what is under it up to the LFO are taken out).
        let from = session.find("[[instrument.effect]]").expect("the Math effect");
        let to = session.find("[[modulator]]").expect("the LFO");
        let saw = heard(&format!("{}{}", &session[..from], &session[to..session.find("[[modulator.target]]").unwrap()]));
        let worst = growl.iter().zip(&saw).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        // (Measured: 0.52, against a loudest sample of 0.43 for the growl; the test asks for 0.05.)
        assert!(worst > 0.05, "the Math is heard: the largest difference from the saw alone is {worst}");
        // The LFO moves the carrier's pitch: with its table taken out the carrier stays at E2, and the sound is another.
        let without_lfo = heard(&session[..to]);
        let worst = growl.iter().zip(&without_lfo).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        // (Measured: 0.69; the test asks for 0.05.)
        assert!(worst > 0.05, "the LFO is heard: the largest difference is {worst}");
    }

    #[test]
    fn the_pump_example_has_the_kick_at_the_second_input_of_the_pads_math_effect() {
        let run = crate::app::testutil::load(&example("pump.toml"));
        let math = run.model.instruments[0].effects[0].node;
        assert_eq!(run.model.instruments[0].effects[0].name, "Math");
        assert_eq!(run.model.cables.len(), 1);
        assert_eq!((run.model.cables[0].from, run.model.cables[0].to), (crate::plugin::chain::CableFrom::Instrument(run.model.instruments[1].node), crate::plugin::chain::CableTo::EffectSide(math)));
    }

    #[test]
    fn the_pump_example_turns_the_pad_down_at_each_kick_and_lets_it_come_back_before_the_next_and_is_another_sound_with_the_cable_out() {
        // Only the pad is heard: the kick has no way out of its own (`route = "none"`), and what it does is turn the pad down through its cable. A kick is on each beat, 0.5 s at 120 beats a minute,
        // from the key: the third is at 1.0 s. What the Math does to the pad is told by the pad without it (the chord's voices beat against each other, so that its own level is never the same
        // twice: the same pad is the yardstick), as the part of it that is left in a window of 50 ms.
        let session = example("pump.toml");
        assert!(session.contains("transpose = -24\nroute = \"none\"\n"), "the kick has no way out");
        let chord = |session: &str| play(session, &[48, 52, 55], 0.0, 3.0, 2.0);
        let without_cable = &session[..session.find("# B (`.side`)").expect("the cable's comment")];
        let plain = chord(&without_cable.replace("[[instrument.effect]]\nplugin = \"builtin:math\"\npreset = \"Pump\"\nmix = 1.0\n", ""));
        assert!(!without_cable.is_empty() && plain.len() == 96_000, "the pad alone is two seconds");
        let left = |out: &[f32], from: f32| rms(&out[(from * 48_000.0) as usize..((from + 0.05) * 48_000.0) as usize]) / rms(&plain[(from * 48_000.0) as usize..((from + 0.05) * 48_000.0) as usize]);
        let pumping = chord(&session);
        let (ducked, back) = (left(&pumping, 1.03), left(&pumping, 1.40));
        // (Measured: 0.09 of the pad is left just after a kick and 0.56 before the next, 6.2 times as much; the test asks for 3.)
        assert!(back > 3.0 * ducked, "{ducked} of the pad is left just after a kick and {back} before the next one");
        // With the cable out B is the effect's own carrier, a sine of the Pump preset's pitch (-36: a little over 1 Hz), which the follower follows too: the pad goes on pumping at the carrier's pace,
        // and the sound is another than the kick's.
        let steady = chord(without_cable);
        let difference: Vec<f32> = pumping.iter().zip(&steady).map(|(a, b)| a - b).collect();
        // (Measured: the difference is 0.66 of the pumping pad's own level; the test asks for 0.2.)
        assert!(rms(&difference) > 0.2 * rms(&pumping), "the difference is {} against {}", rms(&difference), rms(&pumping));
        assert!(rms(&steady) > 0.0 && steady.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn the_examples_have_the_delay_presets_that_they_name() {
        // (A preset that the plugin does not have is only logged by the loader, so a misspelled name would load and sound like no preset at all.)
        for (name, effect, preset, time) in [("pwm.toml", 1, "Slapback", 0.09), ("epiano.toml", 0, "Chorus", 0.02)] {
            let run = crate::app::testutil::load(&example(name));
            let delay = &run.model.instruments[0].effects[effect];
            let heard = delay.params.iter().find(|p| p.name == "time").unwrap().value;
            assert_eq!((delay.name.as_str(), heard), ("Delay", time), "{name}: the time of {preset}, and not the delay's own");
        }
    }

    #[test]
    fn the_sync_lead_example_opens_its_ratio_over_the_start_of_a_note_and_has_the_pitch_of_the_key_when_it_settles() {
        let out = play(&example("sync-lead.toml"), &[60], 0.0, 3.0, 4.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        // The ratio goes from 1 (a plain saw) to 6 over the 0.4 s of the envelope's attack and settles at 2.75 (1.2 s later): the sound is brightest at the top of the sweep.
        let (start, top, settled) = (brightness(window(0.02)), brightness(window(0.4)), brightness(window(2.4)));
        assert!(top > 1.4 * start && top > 1.25 * settled, "{start} at the start, {top} at the top and {settled} when it has settled");
        // A saw that goes round at 2.75 times the pitch of the key would be heard at 719 Hz: it is the sync that keeps it at the key's, C4. (The pitch is not measured while the ratio moves: a wave that changes
        // from one cycle to the next has an autocorrelation whose peak is moved, by 26 cents at the start of the note, and at the top of the sweep is another one, at 314 Hz, 6/5 of the key's.)
        let c4 = 440.0 * 2.0f32.powf(-9.0 / 12.0);
        let cents = 1200.0 * (pitch_of(window(2.4), 48_000.0) / c4).log2();
        assert!(cents.abs() < 5.0, "{cents} cents from C4");
    }

    #[test]
    fn the_sub_bass_example_sounds_an_octave_under_the_key_and_plucks() {
        let out = play(&example("sub-bass.toml"), &[48], 0.0, 2.0, 3.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        // C3 is pressed. The sine under it is so much of the sound that it goes round once for two cycles of the saw: what is heard is C2.
        let c2 = 440.0 * 2.0f32.powf(-33.0 / 12.0);
        for from in [0.5f32, 1.0, 1.8] {
            let cents = 1200.0 * (pitch_of(window(from), 48_000.0) / c2).log2();
            assert!(cents.abs() < 5.0, "{cents} cents from C2 at {from} s");
        }
        // The filter is opened by an envelope at the start of the note: brighter then than later.
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (start, later) = (brightness(window(0.02)), brightness(window(1.8)));
        assert!(start > 1.3 * later, "{start} and {later}");
    }

    #[test]
    fn the_fold_example_sweeps_from_a_soft_sound_to_a_bright_one_with_the_pitch_of_the_key() {
        // The LFO goes round in 5 s (0.2 Hz), so that it is at its top at 1.25 s and at its bottom at 3.75 s: the drive is at 0.8 and at 0.2 (a gain of 27 and of 2.3 at the fold).
        let out = play(&example("fold.toml"), &[48], 0.0, 9.0, 5.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (folded, soft) = (brightness(window(1.25)), brightness(window(3.75)));
        assert!(folded > 4.0 * soft, "{folded} at the top of the sweep and {soft} at the bottom");
        // A fold changes the colour of a wave and not its period. (Not measured at the very top: there the first peak of the autocorrelation that is nearly the best is at a lag of
        // the seventh harmonic, and it says 915 Hz for a key of 131.)
        let c3 = 440.0 * 2.0f32.powf(-21.0 / 12.0);
        for from in [1.0f32, 1.5, 2.0] {
            let cents = 1200.0 * (pitch_of(window(from), 48_000.0) / c3).log2();
            assert!(cents.abs() < 5.0, "{cents} cents from C3 at {from} s");
        }
    }

    #[test]
    fn the_epiano_example_is_softer_when_a_key_is_hit_softer_and_is_the_sound_it_was_at_the_velocity_of_the_computer_keys() {
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let hit = |velocity: u8| play_hit(&example("epiano.toml"), &[60], velocity, 0.0, 3.0, 4.0);
        let window = |out: &[f32], from: f32| out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize].to_vec();
        let (soft, usual, hard) = (hit(40), hit(100), hit(120));
        // Half of the level is the velocity: a key hit at 40 is 0.68 of one at 120 (the measure is 0.675), and it is no different in its brightness (the velocity is the level's only).
        let ratio = rms(&window(&soft, 0.02)) / rms(&window(&hard, 0.02));
        assert!(ratio > 0.6 && ratio < 0.75, "{ratio}");
        assert!((brightness(&window(&soft, 0.02)) / brightness(&window(&hard, 0.02)) - 1.0).abs() < 0.03);
        // At 100, which the keys of the computer and of the page hit at, it is the sound that the file made before it had a velocity (these are its numbers then, in four places of a note of 4 s).
        for (from, rms_then, brightness_then) in [(0.02f32, 0.0396f32, 0.080f32), (0.3, 0.0321, 0.069), (1.0, 0.0127, 0.059), (2.0, 0.0059, 0.061)] {
            let (r, b) = (rms(&window(&usual, from)), brightness(&window(&usual, from)));
            assert!((r / rms_then - 1.0).abs() < 0.03, "at {from} s: {r} for {rms_then}");
            assert!((b / brightness_then - 1.0).abs() < 0.03, "at {from} s: {b} for {brightness_then}");
        }
    }

    #[test]
    fn the_epiano_example_is_brightest_at_the_start_of_a_note_and_mellows_as_it_rings() {
        let out = play(&example("epiano.toml"), &[60], 0.0, 3.0, 4.0);
        let window = |from: f32| &out[(from * 48_000.0) as usize..((from + 0.1) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let (start, later) = (brightness(window(0.02)), brightness(window(1.0)));
        assert!(start > 1.2 * later, "{start} and {later}");
    }

    #[test]
    fn the_fart_example_starts_firm_and_bright_sputters_out_darker_and_is_over_by_itself_with_the_key_still_down() {
        // C3 down at 0.1 s and held for four. The windows are counted from the key.
        let out = play(&example("fart.toml"), &[48], 0.1, 4.0, 4.5);
        let after = |from: f32, length: f32| &out[((0.1 + from) * 48_000.0) as usize..((0.1 + from + length) * 48_000.0) as usize];
        let brightness = |x: &[f32]| (x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()).sqrt();
        // The pitch and the cutoff go down with the pressure: the puffs begin as a buzz and end as single flaps, which is a sound with far less in its changes.
        let (start, late) = (brightness(after(0.02, 0.1)), brightness(after(0.8, 0.1)));
        assert!(start > 2.0 * late, "the first moments are brighter than the last: {start} and {late}");
        // It is firm at the start (no 10 ms of the first half second is far under the body of the sound: the quietest is 0.57 of it) and it sputters at the end (windows of 10 ms between 0.65 s and 0.95 s that
        // are 20 dB under it: lips that did not quite close; five of them, with partial dips between, since the filter rings on after the volume has gone to nothing).
        let body = rms(after(0.1, 0.4));
        assert!(body > 0.02, "it is heard: {body}");
        let under = |from: f32, length: f32, window: f32, fraction: f32| (0..(length / window) as usize).filter(|i| rms(after(from + *i as f32 * window, window)) < fraction * body).count();
        assert_eq!(under(0.02, 0.48, 0.01, 0.3), 0, "firm at the start");
        let gaps = under(0.65, 0.3, 0.01, 0.1);
        assert!(gaps >= 3, "gaps at the end: {gaps}");
        // It is over by itself, whatever the key does: nothing at all is left from 1.5 s, with the key down until 4.1 s.
        assert!(after(1.5, 2.5).iter().all(|x| x.abs() < 1e-6), "{}", after(1.5, 2.5).iter().fold(0.0f32, |m, x| m.max(x.abs())));
        // A chord of it is a choir, heard and not loud (as the chord of every sound example is).
        let choir = play(&example("fart.toml"), &[48, 52, 55], 0.0, 5.0, 8.0);
        let peak = choir.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(choir.iter().all(|x| x.is_finite()) && peak > 0.05 && peak < 0.6, "a chord is heard and is not loud: its loudest sample is {peak}");
    }

    #[test]
    fn no_two_farts_of_the_fart_example_are_alike_since_the_random_lfos_go_on_between_the_keys() {
        let session = example("fart.toml");
        // The same key, the same way, two seconds after the first fart is over (the LFOs have gone round in the meantime: a key starts their fades again and not their waves): another fart.
        let first = play(&session, &[48], 0.0, 1.5, 1.2);
        let second = play(&session, &[48], 2.0, 1.5, 3.2);
        let (a, b) = (&first[..57_600], &second[96_000..96_000 + 57_600]);
        assert!(rms(a) > 0.02 && rms(b) > 0.02, "both are heard: {} and {}", rms(a), rms(b));
        let correlation = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() / (a.iter().map(|x| x * x).sum::<f32>() * b.iter().map(|x| x * x).sum::<f32>()).sqrt();
        // (They share the key, the pressure and the start of the first puff, so that they are not unlike: 0.40 as it is measured.)
        assert!(correlation.abs() < 0.6, "the second is not the first again: {correlation}");
    }

    /// An engine that writes down the events of each block as it is given them, and how often it has been told that it is idle; channel `c` is the constant `c + 1`. `fail` makes it an engine that fails.
    struct Noting {
        seen: Seen,
        idles: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        fail: bool,
    }

    impl Engine for Noting {
        fn num_channels(&self) -> usize {
            2
        }
        fn process(&mut self, events: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()> {
            self.seen.lock().unwrap().push(events.iter().collect());
            if self.fail {
                anyhow::bail!("the engine failed");
            }
            for (c, channel) in out.iter_mut().enumerate() {
                channel.fill(c as f32 + 1.0);
            }
            Ok(())
        }
        fn idle(&mut self) {
            self.idles.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    type Seen = std::sync::Arc<std::sync::Mutex<Vec<Vec<(crate::source::Source, u64, [u8; 3])>>>>;

    fn noting(fail: bool) -> (Renderer, crossbeam_channel::Sender<MidiEvent>, Seen, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let (seen, idles) = (Seen::default(), std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)));
        let (midi_tx, midi_rx) = crossbeam_channel::bounded(16);
        (Renderer::new(Noting { seen: seen.clone(), idles: idles.clone(), fail }, midi_rx, 4), midi_tx, seen, idles)
    }

    #[test]
    fn a_host_that_gives_a_buffer_for_each_channel_has_the_engines_channels_and_the_last_again_when_it_gives_more() {
        let (mut renderer, _midi_tx, _seen, _idles) = noting(false);
        let (mut a, mut b, mut c) = (vec![9.0f32; 4], vec![9.0f32; 4], vec![9.0f32; 4]);
        renderer.render_planar(&[], &mut [&mut a, &mut b, &mut c]);
        assert_eq!((a, b, c), (vec![1.0; 4], vec![2.0; 4], vec![2.0; 4]), "the first two channels, and the last of the engine again for the third");
        let mut only = vec![9.0f32; 4];
        renderer.render_planar(&[], &mut [&mut only]);
        assert_eq!(only, vec![1.0; 4], "a host that gives one channel has the first");
        // The block is as long as the first buffer says, whatever the others are: what is beyond it is silence.
        let (mut short, mut long) = (vec![9.0f32; 4], vec![9.0f32; 6]);
        renderer.render_planar(&[], &mut [&mut short, &mut long]);
        assert_eq!(long, [2.0, 2.0, 2.0, 2.0, 0.0, 0.0]);
        // And a buffer that is shorter than the first has as much as it can hold.
        let (mut long, mut short) = (vec![9.0f32; 6], vec![9.0f32; 4]);
        renderer.render_planar(&[], &mut [&mut long, &mut short]);
        assert_eq!((long, short), (vec![1.0; 6], vec![2.0; 4]));
    }

    /// An engine that adds 1 to what is in the buffers it is given (as an engine that mixes into them does).
    struct Adds;

    impl Engine for Adds {
        fn num_channels(&self) -> usize {
            2
        }
        fn process(&mut self, _: Events<'_>, out: &mut [Vec<f32>]) -> anyhow::Result<()> {
            for channel in out.iter_mut() {
                for sample in channel.iter_mut() {
                    *sample += 1.0;
                }
            }
            Ok(())
        }
    }

    #[test]
    fn the_buffers_that_an_engine_is_given_are_silent_for_every_block_whatever_it_left_in_them() {
        let (_midi_tx, midi_rx) = crossbeam_channel::bounded(8);
        let mut renderer = Renderer::new(Adds, midi_rx, 4);
        let mut data = vec![0.0f32; 8];
        for block in 0..3 {
            renderer.render(&mut data, block);
            assert_eq!(data, vec![1.0; 8], "block {block}: what the engine added to silence, and not to the block before");
        }
        let (mut left, mut right) = (vec![9.0f32; 4], vec![9.0f32; 4]);
        for _ in 0..3 {
            renderer.render_planar(&[], &mut [&mut left, &mut right]);
            assert_eq!((left.clone(), right.clone()), (vec![1.0; 4], vec![1.0; 4]));
        }
    }

    #[test]
    fn the_events_of_a_host_come_after_the_notes_that_arrived_each_at_its_own_frame_and_from_its_own_source() {
        use crate::source::Source;
        let (mut renderer, midi_tx, seen, _idles) = noting(false);
        midi_tx.send(MidiEvent::keys(0, [0x90, 60, 100])).unwrap();
        let host = [MidiEvent::from_source(Source::device(0), 7, [0x90, 64, 90]), MidiEvent::from_source(Source::device(0), 9, [0x80, 64, 0])];
        let mut out = vec![0.0f32; 4];
        renderer.render_planar(&host, &mut [&mut out]);
        assert_eq!(
            seen.lock().unwrap()[0],
            [(Source::KEYS, 0, [0x90, 60, 100]), (Source::device(0), 7, [0x90, 64, 90]), (Source::device(0), 9, [0x80, 64, 0])],
            "what arrived is at the start of the block and first; the host's are where it says"
        );
        // Taken: the next block has none of them again.
        renderer.render_planar(&[], &mut [&mut out]);
        assert!(seen.lock().unwrap()[1].is_empty());
    }

    #[test]
    fn an_engine_that_fails_is_silence_to_a_host() {
        let (mut renderer, _midi_tx, seen, _idles) = noting(true);
        let (mut left, mut right) = (vec![9.0f32; 4], vec![9.0f32; 4]);
        renderer.render_planar(&[], &mut [&mut left, &mut right]);
        assert_eq!(seen.lock().unwrap().len(), 1, "it was asked");
        assert_eq!((left, right), (vec![0.0; 4], vec![0.0; 4]));
    }

    #[test]
    fn an_engine_that_fails_is_silence_to_a_sound_card_too() {
        let (mut renderer, _midi_tx, seen, _idles) = noting(true);
        let mut data = vec![9.0f32; 8];
        renderer.render(&mut data, 0);
        assert_eq!(seen.lock().unwrap().len(), 1, "it was asked");
        assert_eq!(data, vec![0.0; 8]);
    }

    #[test]
    fn a_renderer_that_is_idle_drops_the_notes_that_arrive_and_tells_the_engine_and_renders_nothing() {
        use std::sync::atomic::Ordering;
        let (mut renderer, midi_tx, seen, idles) = noting(false);
        midi_tx.send(MidiEvent::keys(0, [0x90, 60, 100])).unwrap();
        midi_tx.send(MidiEvent::keys(0, [0x90, 62, 100])).unwrap();
        renderer.idle();
        renderer.idle();
        assert_eq!(idles.load(Ordering::Relaxed), 2, "the engine was told each time");
        assert!(seen.lock().unwrap().is_empty(), "no block was rendered");
        assert!(midi_tx.is_empty(), "the notes were taken out of the channel");
        let mut out = vec![0.0f32; 4];
        renderer.render_planar(&[], &mut [&mut out]);
        assert!(seen.lock().unwrap()[0].is_empty(), "and are not for the block that comes later");
    }

    #[test]
    fn the_graph_that_is_idle_takes_the_commands_that_are_waiting_and_renders_nothing() {
        let mut rig = rig("[[instrument]]\nplugin = \"builtin:osc\"\n", 4);
        // The commands that `rig` sent are waiting in the channel, and no block has been rendered: the graph has no lane.
        assert_eq!(rig.renderer.engine().facts().unwrap().lanes.len(), 0);
        rig.renderer.idle();
        assert_eq!(rig.renderer.engine().facts().unwrap().lanes.len(), 1, "an idle graph has taken them");
    }
}
