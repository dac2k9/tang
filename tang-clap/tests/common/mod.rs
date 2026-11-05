//! A host in the same process, the way a DAW runs a plugin: the file's factory, the plugin made from its id, its extensions asked for, activated, and blocks of audio and events given to it. What a test of the plugin has in common.

#![allow(dead_code)]

use clack_extensions::state::PluginState;
use clack_host::events::event_types::MidiEvent;
use clack_host::factory::plugin::PluginFactory;
use clack_host::prelude::*;
use clack_host::process::StartedPluginAudioProcessor;
use tang_clap::TangEntry;

pub const RATE: f64 = 48_000.0;
pub const BLOCK: usize = 256;

pub struct Handlers;
pub struct TestShared;
pub struct TestMainThread;
pub struct TestAudioProcessor;

impl SharedHandler<'_> for TestShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

impl AudioProcessorHandler<'_> for TestAudioProcessor {}
impl MainThreadHandler<'_> for TestMainThread {}

impl HostHandlers for Handlers {
    type Shared<'a> = TestShared;
    type MainThread<'a> = TestMainThread;
    type AudioProcessor<'a> = TestAudioProcessor;
}

/// The file, loaded from its entry as a host loads a file that it has found.
pub fn bundle() -> PluginBundle {
    PluginBundle::load_from_clack::<TangEntry>(c"").expect("the entry loads")
}

/// An instance of the plugin with this id, which is not active yet.
pub fn instance(bundle: &PluginBundle, id: &std::ffi::CStr) -> PluginInstance<Handlers> {
    let info = HostInfo::new("a test host", "", "", "").unwrap();
    PluginInstance::<Handlers>::new(|_| TestShared, |_| TestMainThread, bundle, id, &info).expect("the plugin is made")
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
        let mut input_events = EventBuffer::with_capacity(midi.len().max(1));
        for &(time, bytes) in midi {
            input_events.push(&MidiEvent::new(time, 0, bytes));
        }
        let mut output_events = EventBuffer::with_capacity(4);
        let (mut left, mut right) = (vec![9.0f32; BLOCK], vec![9.0f32; BLOCK]);
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
