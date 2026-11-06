//! The plugin run the way a DAW runs it (`common`: a host in the same process): the two plugins that the file has, their ports, notes at their frames, audio through, the state kept and given back, and a session that
//! the host gives while the plugin is playing.

mod common;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::note_ports::{NoteDialect, NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_host::prelude::*;
use common::*;

const NOTE_ON: [u8; 3] = [0x90, 69, 100];
const NOTE_OFF: [u8; 3] = [0x80, 69, 0];

#[test]
fn the_file_has_two_plugins_that_a_host_lists_by_these_ids_names_and_features() {
    let bundle = bundle();
    let factory = factory(&bundle);
    assert_eq!(factory.plugin_count(), 2);
    let instrument = factory.plugin_descriptor(0).unwrap();
    assert_eq!((instrument.id().unwrap(), instrument.name().unwrap()), (instrument_id(), c"Tang"));
    assert_eq!(instrument.features().map(|feature| feature.to_bytes()).collect::<Vec<_>>(), [&b"instrument"[..], b"synthesizer", b"stereo"]);
    let effect = factory.plugin_descriptor(1).unwrap();
    assert_eq!((effect.id().unwrap(), effect.name().unwrap()), (effect_id(), c"Tang FX"));
    assert_eq!(effect.features().map(|feature| feature.to_bytes()).collect::<Vec<_>>(), [&b"audio-effect"[..], b"stereo"]);
    assert!(factory.plugin_descriptor(2).is_none());
    assert!(instrument.version().is_some() && instrument.vendor().is_some() && instrument.description().is_some());
}

#[test]
fn a_plugin_that_is_not_in_the_file_is_not_made() {
    let bundle = bundle();
    let info = HostInfo::new("a test host", "", "", "").unwrap();
    assert!(PluginInstance::<Handlers>::new(|_| TestShared::default(), |_| TestMainThread, &bundle, c"no.such.plugin", &info).is_err());
}

#[test]
fn the_instrument_has_notes_in_and_a_stereo_output_and_the_effect_has_a_stereo_input_as_well() {
    let bundle = bundle();
    for (id, audio_in) in [(instrument_id(), 0), (effect_id(), 1)] {
        let mut instance = instance(&bundle, id);
        let mut handle = instance.plugin_handle();
        let audio = handle.get_extension::<PluginAudioPorts>().unwrap();
        assert_eq!((audio.count(&mut handle, true), audio.count(&mut handle, false)), (audio_in, 1), "{id:?}");
        let mut buffer = AudioPortInfoBuffer::new();
        let out = audio.get(&mut handle, 0, false, &mut buffer).unwrap();
        assert_eq!((out.channel_count, out.flags.contains(AudioPortFlags::IS_MAIN)), (2, true));
        if audio_in == 1 {
            let mut buffer = AudioPortInfoBuffer::new();
            let input = audio.get(&mut handle, 0, true, &mut buffer).unwrap();
            assert_eq!(input.channel_count, 2);
            assert_ne!(input.id, out.id, "an input and an output are not the same port");
        }
        let notes = handle.get_extension::<PluginNotePorts>().unwrap();
        assert_eq!((notes.count(&mut handle, true), notes.count(&mut handle, false)), (1, 0), "{id:?}");
        let mut buffer = NotePortInfoBuffer::new();
        let port = notes.get(&mut handle, 0, true, &mut buffer).unwrap();
        assert_eq!(port.preferred_dialect, Some(NoteDialect::Midi), "the notes that a host sends are MIDI");
        assert!(port.supported_dialects.contains(NoteDialects::MIDI | NoteDialects::CLAP));
    }
}

#[test]
fn the_one_parameter_is_the_switch_that_opens_the_page_and_it_starts_closed() {
    let bundle = bundle();
    for id in [instrument_id(), effect_id()] {
        let mut instance = instance(&bundle, id);
        let mut handle = instance.plugin_handle();
        let params = handle.get_extension::<PluginParams>().expect("the plugin has parameters");
        assert_eq!(params.count(&mut handle), 1, "{id:?}");
        let mut buffer = ParamInfoBuffer::new();
        let info = params.get_info(&mut handle, 0, &mut buffer).expect("its description");
        assert_eq!((info.id, info.name, info.min_value, info.max_value, info.default_value), (ClapId::new(0), &b"Open the page"[..], 0.0, 1.0, 0.0));
        assert!(info.flags.contains(ParamInfoFlags::IS_STEPPED), "a switch has two places");
        assert!(params.get_info(&mut handle, 1, &mut ParamInfoBuffer::new()).is_none(), "and there is no other");
        assert_eq!(params.get_value(&mut handle, ClapId::new(0)), Some(0.0), "closed to begin with");
        assert_eq!(params.get_value(&mut handle, ClapId::new(7)), None, "no other parameter has a value");
        let mut text = [std::mem::MaybeUninit::<u8>::uninit(); 32];
        assert_eq!(params.value_to_text(&mut handle, ClapId::new(0), 0.0, &mut text).unwrap(), b"Closed");
        assert_eq!(params.value_to_text(&mut handle, ClapId::new(0), 1.0, &mut text).unwrap(), b"Open");
        assert!(params.value_to_text(&mut handle, ClapId::new(7), 1.0, &mut text).is_err());
        for (written, value) in [(c"open", 1.0), (c"On", 1.0), (c"1", 1.0), (c"closed", 0.0), (c"OFF", 0.0), (c"0", 0.0)] {
            assert_eq!(params.text_to_value(&mut handle, ClapId::new(0), written), Some(value), "{written:?}");
        }
        assert_eq!(params.text_to_value(&mut handle, ClapId::new(0), c"maybe"), None);
        assert_eq!(params.text_to_value(&mut handle, ClapId::new(7), c"open"), None);
    }
}

#[test]
fn the_instrument_plays_the_midi_that_a_host_sends_from_the_frame_it_says_and_is_silent_when_it_ends() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert_eq!(rig.loudest(&[], 4), 0.0, "nothing is playing");
    let (left, right) = rig.play(&[(100, NOTE_ON)]);
    assert!(left[..100].iter().chain(&right[..100]).all(|&sample| sample == 0.0), "silence up to the note's frame, over what the host had in its buffers");
    assert!(left[100..].iter().any(|&sample| sample.abs() > 0.01), "and the note from it");
    assert!(rig.loudest(&[], 8) > 0.05, "a held note goes on");
    assert!(rig.settled(&[(0, NOTE_OFF)], 60) < 1e-4, "silent when it has ended");
    rig.stop();
}

#[test]
fn a_host_that_gives_one_channel_has_the_left_side_played_into_it() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    let mut loudest = 0.0f32;
    for block in 0..8 {
        let mut input_events = EventBuffer::with_capacity(1);
        if block == 0 {
            input_events.push(&clack_host::events::event_types::MidiEvent::new(0, 0, NOTE_ON));
        }
        let mut mono = vec![9.0f32; BLOCK];
        let mut ports = AudioPorts::with_capacity(1, 1);
        let mut outputs = ports.with_output_buffers([AudioPortBuffer { channels: AudioPortBufferType::f32_output_only([mono.as_mut_slice()].into_iter()), latency: 0 }]);
        let mut output_events = EventBuffer::with_capacity(1);
        rig.processor.as_mut().unwrap().process(&InputAudioBuffers::empty(), &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None).unwrap();
        loudest = mono.iter().fold(loudest, |loudest, sample| loudest.max(sample.abs()));
    }
    // (The buffer was full of 9s: a plugin that leaves it alone is as loud as that, which is not what a note is.)
    assert!(loudest > 0.05 && loudest < 1.0, "a host with one channel is played into, over what it had in its buffer: {loudest}");
    rig.stop();
}

#[test]
fn a_block_of_no_frames_is_processed_without_trouble_and_the_next_block_plays() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    let (left, right) = rig.play_frames(&[(0, NOTE_ON)], 0);
    assert!((left.len(), right.len()) == (0, 0), "a block of nothing has nothing in it");
    assert_eq!(rig.loudest(&[], 4), 0.0, "and the note that came with it, which has no frame to be at, is not played");
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05, "the next block that has frames plays what it is sent");
    rig.stop();
}

#[test]
fn the_notes_of_claps_own_dialect_are_played_too() {
    use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    // (Through the same block function with its own events: a note on key 69, channel 0.)
    let mut input_events = EventBuffer::with_capacity(2);
    input_events.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 69u16, 1u32), 0.8));
    let (mut left, mut right) = (vec![9.0f32; BLOCK], vec![9.0f32; BLOCK]);
    let mut ports = AudioPorts::with_capacity(2, 1);
    let mut outputs = ports.with_output_buffers([AudioPortBuffer {
        channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
        latency: 0,
    }]);
    let mut output_events = EventBuffer::with_capacity(1);
    rig.processor.as_mut().unwrap().process(&InputAudioBuffers::empty(), &mut outputs, &input_events.as_input(), &mut output_events.as_output(), None, None).unwrap();
    assert!(left.iter().any(|&sample| sample.abs() > 0.01), "a note in the CLAP dialect plays");
    let mut off = EventBuffer::with_capacity(1);
    off.push(&NoteOffEvent::new(0, Pckn::new(0u16, 0u16, 69u16, 1u32), 0.0));
    let mut ports = AudioPorts::with_capacity(2, 1);
    let (mut left, mut right) = (vec![9.0f32; BLOCK], vec![9.0f32; BLOCK]);
    let mut outputs = ports.with_output_buffers([AudioPortBuffer {
        channels: AudioPortBufferType::f32_output_only([left.as_mut_slice(), right.as_mut_slice()].into_iter()),
        latency: 0,
    }]);
    rig.processor.as_mut().unwrap().process(&InputAudioBuffers::empty(), &mut outputs, &off.as_input(), &mut output_events.as_output(), None, None).unwrap();
    assert!(rig.settled(&[], 60) < 1e-4, "and ends");
    rig.stop();
}

#[test]
fn a_reset_silences_what_is_sounding() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05);
    rig.processor.as_mut().unwrap().reset();
    assert!(rig.settled(&[], 60) < 1e-4, "the note that was held has been let go of");
    rig.stop();
}

/// A sine of 0.25 for a block, on both sides.
fn sine() -> [Vec<f32>; 2] {
    let side: Vec<f32> = (0..BLOCK).map(|i| 0.25 * (std::f32::consts::TAU * 480.0 * i as f32 / RATE as f32).sin()).collect();
    [side.clone(), side]
}

#[test]
fn the_effect_plays_what_comes_in_and_a_silent_input_is_a_silent_output() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, effect_id());
    let silence = [vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]];
    let mut loudest = 0.0f32;
    for _ in 0..8 {
        let (left, right) = rig.filter(&silence);
        loudest = left.iter().chain(&right).fold(loudest, |loudest, sample| loudest.max(sample.abs()));
    }
    assert_eq!(loudest, 0.0, "nothing in, nothing out");
    for _ in 0..40 {
        let (left, right) = rig.filter(&sine());
        loudest = left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
    }
    assert!((0.2..=0.3).contains(&loudest), "what comes in is what goes out: {loudest}");
    rig.stop();
}

#[test]
fn the_effect_is_played_what_comes_in_when_the_host_gives_the_input_and_the_output_as_the_same_memory() {
    use clack_host::process::audio_buffers::{InputAudioBuffers, OutputAudioBuffers};
    use clap_sys::audio_buffer::clap_audio_buffer;
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, effect_id());
    let mut loudest = 0.0f32;
    for _ in 0..40 {
        let [mut left, mut right] = sine();
        let mut pointers = [left.as_mut_ptr(), right.as_mut_ptr()];
        let buffer = clap_audio_buffer { data32: pointers.as_mut_ptr(), data64: std::ptr::null_mut(), channel_count: 2, latency: 0, constant_mask: 0 };
        let in_buffers = [buffer];
        let mut out_buffers = [buffer];
        // SAFETY: the pointers are those of two buffers of `BLOCK` samples that are alive for the call, and the input is the output, as a host that processes in place gives it.
        let (inputs, mut outputs) = unsafe { (InputAudioBuffers::from_raw_buffers(&in_buffers, BLOCK as u32), OutputAudioBuffers::from_raw_buffers(&mut out_buffers, BLOCK as u32)) };
        let (events, mut out_events) = (EventBuffer::with_capacity(1), EventBuffer::with_capacity(1));
        rig.processor.as_mut().unwrap().process(&inputs, &mut outputs, &events.as_input(), &mut out_events.as_output(), None, None).unwrap();
        loudest = left.iter().chain(&right).fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
    }
    assert!((0.2..=0.3).contains(&loudest), "in place: what comes in is what goes out: {loudest}");
    rig.stop();
}

#[test]
fn the_state_is_the_session_and_another_instance_that_is_given_it_plays_it() {
    let bundle = bundle();
    // A host that opens a project gives the state before it activates the plugin.
    let mut quiet = instance(&bundle, instrument_id());
    {
        let mut handle = quiet.plugin_handle();
        let state = handle.get_extension::<clack_extensions::state::PluginState>().unwrap();
        state.load(&mut handle, &mut std::io::Cursor::new(state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.0\n"))).unwrap();
    }
    let processor = quiet
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: RATE, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap();
    let mut rig = Rig { instance: quiet, processor: Some(processor) };
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) < 1e-4, "the oscillator of the state has no volume");
    let saved = rig.save();
    assert_eq!(&saved[..8], b"TANGSESS", "what is kept says what it is");
    let text = String::from_utf8(saved[9..].to_vec()).unwrap();
    assert!(text.contains("builtin:osc") && text.contains("volume = 0"), "the state is the session:\n{text}");
    rig.stop();

    // A new instance given the bytes that were saved plays the same.
    let mut again = Rig::start(&bundle, instrument_id());
    assert!(again.loudest(&[(0, NOTE_ON)], 8) > 0.05, "a new instance is a new session: the oscillator sounds");
    again.load(&saved).expect("the state is taken");
    assert!(again.loudest(&[(0, NOTE_OFF), (0, NOTE_ON)], 8) < 1e-4, "given the state while it plays, it plays that");
    again.stop();
}

#[test]
fn a_state_that_is_not_a_session_is_refused_and_the_plugin_goes_on_with_what_it_had() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    assert!(rig.load(&[0xFF, 0xFE, 0x00, 0x01]).is_err(), "bytes that are not text");
    let mut later = state_of("x");
    later[8] = 9;
    assert!(rig.load(&later).is_err(), "a version that this does not know");
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05, "it still plays");
    rig.stop();
}

#[test]
fn a_state_that_cannot_be_loaded_gives_a_new_session_and_is_what_is_saved_until_something_is_changed() {
    let bundle = bundle();
    let broken = "[[instrument]]\nplugin = \"builtin:no-such-thing\"\n";
    let mut rig = Rig::start(&bundle, instrument_id());
    rig.load(&state_of(broken)).expect("the state is taken: a plugin that is not installed is not the host's business");
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.05, "a new session plays in its place");
    let saved = rig.save();
    assert_eq!(&saved[9..], broken.as_bytes(), "a project that is opened and saved does not lose the session that this machine could not play");
    rig.stop();
}

#[test]
fn every_instance_is_a_session_of_its_own() {
    let bundle = bundle();
    let mut a = Rig::start(&bundle, instrument_id());
    let mut b = Rig::start(&bundle, instrument_id());
    b.load(&state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.0\n")).unwrap();
    assert!(a.loudest(&[(0, NOTE_ON)], 8) > 0.05);
    assert!(b.loudest(&[(0, NOTE_ON)], 8) < 1e-4, "the other has its own");
    a.stop();
    b.stop();
}

#[test]
fn a_plugin_is_activated_again_at_another_rate_and_plays_the_same_session() {
    let bundle = bundle();
    let mut rig = Rig::start(&bundle, instrument_id());
    rig.load(&state_of("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.5\n")).unwrap();
    let before = rig.save();
    let Rig { mut instance, processor } = rig;
    instance.deactivate(processor.unwrap().stop_processing());
    let processor = instance
        .activate(|_, _| TestAudioProcessor, PluginAudioConfiguration { sample_rate: 44_100.0, min_frames_count: 1, max_frames_count: BLOCK as u32 })
        .unwrap()
        .start_processing()
        .unwrap();
    let mut rig = Rig { instance, processor: Some(processor) };
    assert!(rig.loudest(&[(0, NOTE_ON)], 8) > 0.02, "it plays at the new rate");
    let after = rig.save();
    assert_eq!(before, after, "the session came through the change of rate as it was");
    rig.stop();
}
