//! A host in the same process, the way a DAW runs a plugin: the file's factory, the plugin made from its id, its extensions asked for, activated, and blocks of audio and events given to it. What a test of the plugin has in common.

#![allow(dead_code)]

use clack_extensions::state::{HostState, HostStateImpl, PluginState};
use clack_host::events::event_types::MidiEvent;
use clack_host::factory::plugin::PluginFactory;
use clack_host::prelude::*;
use clack_host::process::StartedPluginAudioProcessor;
use tang_clap::TangEntry;

pub const RATE: f64 = 48_000.0;
pub const BLOCK: usize = 256;

pub struct Handlers;
/// What the plugin may ask of the host from any thread: it counts the callbacks it was asked for (a host calls the plugin's main thread back when it can).
#[derive(Default)]
pub struct TestShared {
    pub callbacks: std::sync::atomic::AtomicUsize,
    /// How many times the plugin has told the host that the project has changed (`state.mark_dirty`, on the main thread).
    pub dirty: std::sync::atomic::AtomicUsize,
}
pub struct TestMainThread<'a> {
    pub shared: &'a TestShared,
}
pub struct TestAudioProcessor;

impl SharedHandler<'_> for TestShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {
        self.callbacks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl AudioProcessorHandler<'_> for TestAudioProcessor {}
impl<'a> MainThreadHandler<'a> for TestMainThread<'a> {}

impl HostStateImpl for TestMainThread<'_> {
    fn mark_dirty(&mut self) {
        self.shared.dirty.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl HostHandlers for Handlers {
    type Shared<'a> = TestShared;
    type MainThread<'a> = TestMainThread<'a>;
    type AudioProcessor<'a> = TestAudioProcessor;

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostState>();
    }
}

/// The file, loaded from its entry as a host loads a file that it has found.
pub fn bundle() -> PluginBundle {
    PluginBundle::load_from_clack::<TangEntry>(c"").expect("the entry loads")
}

/// An instance of the plugin with this id, which is not active yet.
pub fn instance(bundle: &PluginBundle, id: &std::ffi::CStr) -> PluginInstance<Handlers> {
    let info = HostInfo::new("a test host", "", "", "").unwrap();
    PluginInstance::<Handlers>::new(|_| TestShared::default(), |shared| TestMainThread { shared }, bundle, id, &info).expect("the plugin is made")
}

/// An instance that has been activated and started, with what plays it.
pub struct Rig {
    pub instance: PluginInstance<Handlers>,
    pub processor: Option<StartedPluginAudioProcessor<Handlers>>,
}

impl Rig {
    /// The plugin with this id, activated at `RATE` for blocks of up to `BLOCK` frames, and started.
    pub fn start(bundle: &PluginBundle, id: &std::ffi::CStr) -> Rig {
        let mut instance = instance(bundle, id);
        let processor = instance
            .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: RATE, min_frames_count: 1, max_frames_count: BLOCK as u32 })
            .expect("the plugin activates")
            .start_processing()
            .expect("the plugin starts");
        Rig { instance, processor: Some(processor) }
    }

    /// One block of an instrument (it has no audio in): the events `midi` (each at its frame), and what it played, a side each. The buffers are full of 9s before, so that a plugin that leaves one alone is heard.
    pub fn play(&mut self, midi: &[(u32, [u8; 3])]) -> (Vec<f32>, Vec<f32>) {
        self.play_frames(midi, BLOCK)
    }

    /// The same, for a block of `frames` frames (a host may send one that has none).
    pub fn play_frames(&mut self, midi: &[(u32, [u8; 3])], frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut input_events = EventBuffer::with_capacity(midi.len().max(1));
        for &(time, bytes) in midi {
            input_events.push(&MidiEvent::new(time, 0, bytes));
        }
        let mut output_events = EventBuffer::with_capacity(4);
        let (mut left, mut right) = (vec![9.0f32; frames], vec![9.0f32; frames]);
        let mut ports = AudioPorts::with_capacity(2, 1);
        let mut outputs = ports.with_output_buffers([AudioPortBuffer {
            channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
            latency: 0,
        }]);
        self.processor
            .as_mut()
            .expect("started")
            .process(&InputAudioBuffers::empty(), &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None)
            .expect("the block is processed");
        (left, right)
    }

    /// One block of an instrument in which the host sets the switch that opens the page to `value` (if it does), and the values that the plugin says the switch is set to in that block (what it puts back).
    pub fn block_with(&mut self, value: Option<f64>) -> Vec<f64> {
        let mut input_events = EventBuffer::with_capacity(1);
        if let Some(value) = value {
            input_events.push(&clack_host::events::event_types::ParamValueEvent::new(0, ClapId::new(0), Pckn::match_all(), value, clack_host::utils::Cookie::empty()));
        }
        let mut output_events = EventBuffer::with_capacity(4);
        let (mut left, mut right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let mut ports = AudioPorts::with_capacity(2, 1);
        let mut outputs = ports.with_output_buffers([AudioPortBuffer {
            channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
            latency: 0,
        }]);
        self.processor
            .as_mut()
            .expect("started")
            .process(&InputAudioBuffers::empty(), &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None)
            .expect("the block is processed");
        switch_values(&output_events)
    }

    /// The host sends the plugin the changes of its parameters without a block, while it is active and not processing (`flush`): the switch is set to `value` (if it is), and what the plugin says it is set to.
    pub fn flush_with(&mut self, value: Option<f64>) -> Vec<f64> {
        let params = self.instance.plugin_handle().get_extension::<clack_extensions::params::PluginParams>().expect("the plugin has parameters");
        let mut input_events = EventBuffer::with_capacity(1);
        if let Some(value) = value {
            input_events.push(&clack_host::events::event_types::ParamValueEvent::new(0, ClapId::new(0), Pckn::match_all(), value, clack_host::utils::Cookie::empty()));
        }
        let mut output_events = EventBuffer::with_capacity(2);
        let mut handle = self.processor.as_mut().expect("started").plugin_handle();
        params.flush_active(&mut handle, &input_events.as_input(), &mut output_events.as_output());
        switch_values(&output_events)
    }

    /// How many times the plugin has asked the host to call its main thread back.
    pub fn callbacks(&self) -> usize {
        self.instance.access_shared_handler(|shared| shared.callbacks.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// How many times the plugin has told the host that the project has changed.
    pub fn dirty_marks(&self) -> usize {
        self.instance.access_shared_handler(|shared| shared.dirty.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// The host calls the plugin's main thread back, as it does when it is asked to.
    pub fn call_back(&mut self) {
        self.instance.call_on_main_thread_callback();
    }

    /// `blocks` blocks of an instrument, the first with `midi`: the loudest sample of any of them.
    pub fn loudest(&mut self, midi: &[(u32, [u8; 3])], blocks: usize) -> f32 {
        let mut loudest = 0.0f32;
        for block in 0..blocks {
            let (left, right) = self.play(if block == 0 { midi } else { &[] });
            loudest = left.iter().chain(&right).fold(loudest, |loudest, sample| loudest.max(sample.abs()));
        }
        loudest
    }

    /// `blocks` blocks of an instrument, the first with `midi`: the loudest sample of the last one (what has settled, with the tail of a note that has ended gone).
    pub fn settled(&mut self, midi: &[(u32, [u8; 3])], blocks: usize) -> f32 {
        let mut last = 0.0f32;
        for block in 0..blocks {
            let (left, right) = self.play(if block == 0 { midi } else { &[] });
            last = left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
        }
        last
    }

    /// One block of an effect that has the audio in `input` (a side each) in buffers of their own, and what it made of it.
    pub fn filter(&mut self, input: &[Vec<f32>; 2]) -> (Vec<f32>, Vec<f32>) {
        let (mut in_left, mut in_right) = (input[0].clone(), input[1].clone());
        let (mut left, mut right) = (vec![9.0f32; input[0].len()], vec![9.0f32; input[0].len()]);
        let (mut in_ports, mut out_ports) = (AudioPorts::with_capacity(2, 1), AudioPorts::with_capacity(2, 1));
        let inputs = in_ports.with_input_buffers([AudioPortBuffer {
            channels: AudioPortBufferType::f32_input_only([InputChannel::variable(&mut in_left), InputChannel::variable(&mut in_right)].into_iter()),
            latency: 0,
        }]);
        let mut outputs = out_ports.with_output_buffers([AudioPortBuffer {
            channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
            latency: 0,
        }]);
        let (input_events, mut output_events) = (EventBuffer::with_capacity(1), EventBuffer::with_capacity(1));
        self.processor
            .as_mut()
            .expect("started")
            .process(&inputs, &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None)
            .expect("the block is processed");
        (left, right)
    }

    /// The state that the host would keep, as bytes.
    pub fn save(&mut self) -> Vec<u8> {
        let mut handle = self.instance.plugin_handle();
        let state = handle.get_extension::<PluginState>().expect("the plugin keeps a state");
        let mut bytes = Vec::new();
        state.save(&mut handle, &mut bytes).expect("the state is saved");
        bytes
    }

    /// Give the plugin a state, as a host that opens a project does.
    pub fn load(&mut self, bytes: &[u8]) -> Result<(), clack_extensions::state::StateError> {
        let mut handle = self.instance.plugin_handle();
        let state = handle.get_extension::<PluginState>().expect("the plugin keeps a state");
        state.load(&mut handle, &mut std::io::Cursor::new(bytes.to_vec()))
    }

    /// Stop the plugin and deactivate it, as a host does before it lets it go.
    pub fn stop(mut self) {
        if let Some(processor) = self.processor.take() {
            self.instance.deactivate(processor.stop_processing());
        }
    }
}

/// The values that the plugin says the switch that opens the page is set to, in the events that it sent.
pub fn switch_values(events: &EventBuffer) -> Vec<f64> {
    events
        .as_input()
        .iter()
        .filter_map(|event| match event.as_core_event() {
            Some(clack_host::events::spaces::CoreEventSpace::ParamValue(change)) if change.param_id() == Some(ClapId::new(0)) => Some(change.value()),
            _ => None,
        })
        .collect()
}

/// The state that a host keeps for a plugin whose session is `text`.
pub fn state_of(text: &str) -> Vec<u8> {
    let mut bytes = b"TANGSESS".to_vec();
    bytes.push(1);
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

pub fn instrument_id() -> &'static std::ffi::CStr {
    c"io.github.apanloco.tang.instrument"
}

pub fn effect_id() -> &'static std::ffi::CStr {
    c"io.github.apanloco.tang.effect"
}

/// The plugin factory of a bundle.
pub fn factory(bundle: &PluginBundle) -> PluginFactory<'_> {
    bundle.get_factory::<PluginFactory>().expect("the file has a plugin factory")
}
