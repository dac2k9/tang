//! What the events of a CLAP block are to Tang: MIDI, as bytes with the frame each is at. A host that prefers the MIDI dialect (Tang says it does) sends MIDI messages; one that sends notes in CLAP's own dialect (a note has a
//! key, a channel and a velocity from 0 to 1) is understood too, and the note-ons and note-offs of it become the MIDI that they are. What a CLAP note says beyond that (the expressions of a note: its own volume, pan and tuning) is
//! not carried over yet.

use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::events::{Match, UnknownEvent};
use tang::embed::HostMidi;

/// The MIDI for a note that starts: a velocity of 0 to 1 is 1 to 127 (a velocity of 0 would be a note-off in MIDI). None for a note that has a key that MIDI does not have.
pub fn note_on(channel: u16, key: u16, velocity: f64) -> Option<[u8; 3]> {
    let velocity = if velocity.is_finite() { (velocity * 127.0).round().clamp(1.0, 127.0) as u8 } else { 64 };
    Some([0x90 | channel_of(channel), key_of(key)?, velocity])
}

/// The MIDI for a note that ends, with the velocity that it is released at (0 to 1).
pub fn note_off(channel: u16, key: u16, velocity: f64) -> Option<[u8; 3]> {
    let velocity = if velocity.is_finite() { (velocity * 127.0).round().clamp(0.0, 127.0) as u8 } else { 0 };
    Some([0x80 | channel_of(channel), key_of(key)?, velocity])
}

/// The MIDI channel, 0 to 15, of a CLAP channel (a host that has more than 16 gets the last).
fn channel_of(channel: u16) -> u8 {
    channel.min(15) as u8
}

/// The MIDI key, if there is one: 0 to 127.
fn key_of(key: u16) -> Option<u8> {
    u8::try_from(key).ok().filter(|&key| key < 128)
}

/// A channel or a key that an event names: nothing for an event that is for all of them (a note is for one key).
fn specific(matched: Match<u16>) -> Option<u16> {
    match matched {
        Match::Specific(value) => Some(value),
        Match::All => None,
    }
}

/// Put the MIDI that `event` is, if it is, in `out`, at its frame.
pub fn convert(event: &UnknownEvent, out: &mut Vec<HostMidi>) {
    let frame = event.header().time();
    let bytes = match event.as_core_event() {
        Some(CoreEventSpace::Midi(midi)) => Some(midi.data()),
        Some(CoreEventSpace::NoteOn(note)) => specific(note.key()).and_then(|key| note_on(specific(note.channel()).unwrap_or(0), key, note.velocity())),
        Some(CoreEventSpace::NoteOff(note)) => specific(note.key()).and_then(|key| note_off(specific(note.channel()).unwrap_or(0), key, note.velocity())),
        // (A choke is a note that is silenced at once: the nearest MIDI has to it is a note-off.)
        Some(CoreEventSpace::NoteChoke(note)) => specific(note.key()).and_then(|key| note_off(specific(note.channel()).unwrap_or(0), key, 0.0)),
        _ => None,
    };
    if let Some(bytes) = bytes {
        out.push(HostMidi { frame, bytes });
    }
}

/// A note-off of every note on every channel: what a host's `reset` is to the engine (a transport that jumps, a plugin that is told to forget what it was playing), as the MIDI that an "all notes off" controller is.
pub fn all_notes_off() -> impl Iterator<Item = HostMidi> {
    (0u8..16).map(|channel| HostMidi { frame: 0, bytes: [0xB0 | channel, 123, 0] })
}

#[cfg(test)]
mod tests {
    use clack_plugin::events::event_types::{MidiEvent, NoteChokeEvent, NoteOffEvent, NoteOnEvent};
    use clack_plugin::events::{Event, Pckn};

    use super::*;

    fn converted(event: &UnknownEvent) -> Vec<HostMidi> {
        let mut out = Vec::new();
        convert(event, &mut out);
        out
    }

    #[test]
    fn a_velocity_of_0_to_1_is_1_to_127_and_never_0_which_would_end_the_note() {
        assert_eq!(note_on(0, 60, 1.0), Some([0x90, 60, 127]));
        assert_eq!(note_on(0, 60, 0.5), Some([0x90, 60, 64]));
        assert_eq!(note_on(0, 60, 0.7), Some([0x90, 60, 89]), "the scale is 127, not 128 (0.7 of 128 would be 90)");
        assert_eq!(note_on(0, 60, 0.9), Some([0x90, 60, 114]));
        assert_eq!(note_on(0, 60, 0.0), Some([0x90, 60, 1]), "a note-on that is silent is still a note-on");
        assert_eq!(note_on(0, 60, 7.0), Some([0x90, 60, 127]), "beyond the top");
        assert_eq!(note_on(0, 60, -1.0), Some([0x90, 60, 1]));
        assert_eq!(note_on(0, 60, f64::NAN), Some([0x90, 60, 64]), "a velocity that is not a number is a middle one");
        assert_eq!(note_off(0, 60, 0.0), Some([0x80, 60, 0]));
        assert_eq!(note_off(0, 60, 1.0), Some([0x80, 60, 127]));
        assert_eq!(note_off(0, 60, 0.7), Some([0x80, 60, 89]));
        assert_eq!(note_off(0, 60, f64::NAN), Some([0x80, 60, 0]));
    }

    #[test]
    fn the_channel_and_the_key_are_midis_or_the_note_is_none() {
        assert_eq!(note_on(5, 61, 1.0), Some([0x95, 61, 127]));
        assert_eq!(note_off(15, 61, 0.0), Some([0x8F, 61, 0]));
        assert_eq!(note_on(99, 61, 1.0), Some([0x9F, 61, 127]), "more than 16 channels: the last");
        assert_eq!(note_on(0, 127, 1.0), Some([0x90, 127, 127]));
        assert_eq!(note_on(0, 128, 1.0), None, "MIDI has no key 128");
        assert_eq!(note_off(0, 5000, 1.0), None);
    }

    #[test]
    fn midi_that_a_host_sends_is_passed_on_at_its_frame() {
        let event = MidiEvent::new(37, 0, [0xB0, 1, 99]);
        assert_eq!(converted(event.as_unknown()), [HostMidi { frame: 37, bytes: [0xB0, 1, 99] }]);
    }

    #[test]
    fn the_notes_of_claps_own_dialect_become_midi_at_their_frames() {
        let on = NoteOnEvent::new(10, Pckn::new(0u16, 2u16, 64u16, 7u32), 0.8);
        assert_eq!(converted(on.as_unknown()), [HostMidi { frame: 10, bytes: [0x92, 64, 102] }]);
        let off = NoteOffEvent::new(200, Pckn::new(0u16, 2u16, 64u16, 7u32), 0.6);
        assert_eq!(converted(off.as_unknown()), [HostMidi { frame: 200, bytes: [0x82, 64, 76] }], "the velocity that a note is released at goes with it");
        let choke = NoteChokeEvent::new(5, Pckn::new(0u16, 3u16, 40u16, 1u32));
        assert_eq!(converted(choke.as_unknown()), [HostMidi { frame: 5, bytes: [0x83, 40, 0] }], "a choke ends the note");
    }

    #[test]
    fn a_note_that_is_for_every_key_is_no_note() {
        let on = NoteOnEvent::new(0, Pckn::match_all(), 1.0);
        assert!(converted(on.as_unknown()).is_empty());
    }

    #[test]
    fn a_reset_is_an_all_notes_off_on_each_of_the_sixteen_channels() {
        let all: Vec<HostMidi> = all_notes_off().collect();
        assert_eq!(all.len(), 16);
        assert!(all.iter().all(|event| event.frame == 0));
        assert_eq!(all[0].bytes, [0xB0, 123, 0]);
        assert_eq!(all[15].bytes, [0xBF, 123, 0]);
    }
}
