use std::collections::HashMap;
use std::f32::consts::PI;

use crate::plugin::{ParameterInfo, Plugin, Preset};

/// Selectable waveform names for the `waveform` enum parameter. Saw is
/// appended (index 3) so the existing sine/triangle/square indices — and the
/// numeric `waveform` values saved in sessions — stay stable.
pub const WAVEFORMS: [&str; 4] = ["sine", "triangle", "square", "saw"];

#[derive(Copy, Clone)]
pub enum Waveform {
    Sine,
    Triangle,
    Square,
    Saw,
}

impl Waveform {
    /// Map the `waveform` parameter value (0–3, rounded) to a waveform.
    fn from_param(v: f32) -> Self {
        match v.round() as i32 {
            1 => Waveform::Triangle,
            2 => Waveform::Square,
            3 => Waveform::Saw,
            _ => Waveform::Sine,
        }
    }

    /// The `waveform` parameter value for this waveform.
    fn to_param(self) -> f32 {
        match self {
            Waveform::Sine => 0.0,
            Waveform::Triangle => 1.0,
            Waveform::Square => 2.0,
            Waveform::Saw => 3.0,
        }
    }

    /// Render one sample for a phase in [0.0, 1.0).
    fn sample(self, phase: f32) -> f32 {
        match self {
            Waveform::Sine => (2.0 * PI * phase).sin(),
            // Triangle in [-1, 1]: rises 0→1 over [0, 0.25), falls 1→-1 over
            // [0.25, 0.75), rises -1→0 over [0.75, 1.0).
            Waveform::Triangle => 4.0 * (phase - (phase + 0.5).floor()).abs() - 1.0,
            // Naïve square (no anti-aliasing): +1 for first half, -1 for second.
            Waveform::Square => if phase < 0.5 { 1.0 } else { -1.0 },
            // Naïve rising sawtooth (no anti-aliasing): -1 → +1 across the cycle.
            Waveform::Saw => 2.0 * phase - 1.0,
        }
    }
}

/// A simple polyphonic oscillator, useful for testing audio/MIDI without
/// external plugins. Each voice has its own ADSR amplitude envelope, so
/// notes attack and release independently.
pub struct Oscillator {
    sample_rate: f32,
    voices: HashMap<u8, Voice>,
    /// Detune in semitones, applied to all voices. Modulator-friendly.
    detune: f32,
    /// Output volume target (0.0–1.0). Modulator-friendly.
    volume: f32,
    /// Smoothed volume actually applied, ramping toward `volume` to avoid
    /// zipper noise when the parameter is tweaked or modulated at block rate.
    volume_smoothed: f32,
    /// Stereo pan target, -1.0 = hard left, 0.0 = center, +1.0 = hard right.
    /// Modulator-friendly.
    pan: f32,
    /// Smoothed pan actually applied (same anti-zipper ramp as `volume`).
    pan_smoothed: f32,
    waveform: Waveform,
    /// Per-voice envelope: attack/decay/release in seconds, sustain 0.0–1.0.
    attack: f32,
    decay: f32,
    sustain: f32,
    release: f32,
}

#[derive(Copy, Clone)]
enum VoiceState {
    Attack,
    Decay,
    Sustain,
    Release,
}

struct Voice {
    phase: f32,
    amp: f32,
    state: VoiceState,
}

pub const PARAM_COUNT: usize = 8; // waveform, detune, volume, attack, decay, sustain, release, pan

const PAN_MIN: f32 = -1.0;
const PAN_MAX: f32 = 1.0;

// ±2 octaves. Wide enough that a modulator (e.g. a learned pitch bend) can
// produce a dramatic pitch sweep; use the target's depth to dial the amount
// down for subtle detuning.
const DETUNE_MIN: f32 = -24.0;
const DETUNE_MAX: f32 = 24.0;

// Envelope defaults preserve the oscillator's original behavior: ramps just
// long enough to avoid clicks on note start/stop, full sustain.
const DEFAULT_ATTACK_SEC: f32 = 0.004;
const DEFAULT_DECAY_SEC: f32 = 0.1;
const DEFAULT_SUSTAIN: f32 = 1.0;
const DEFAULT_RELEASE_SEC: f32 = 0.012;
/// Envelope times are floored to this when applied, so even a 0 setting
/// keeps the click-guard ramps.
const MIN_RAMP_SEC: f32 = 0.002;
/// Maximum envelope time in seconds (matches the modulator envelope UI).
const ADSR_TIME_MAX: f32 = 10.0;

// One-pole smoothing time constant for the volume parameter.
const VOLUME_SMOOTH_SEC: f32 = 0.005;

impl Oscillator {
    pub fn new(sample_rate: f32, waveform: Waveform) -> Self {
        Self {
            sample_rate,
            voices: HashMap::new(),
            detune: 0.0,
            volume: 1.0,
            volume_smoothed: 1.0,
            pan: 0.0,
            pan_smoothed: 0.0,
            waveform,
            attack: DEFAULT_ATTACK_SEC,
            decay: DEFAULT_DECAY_SEC,
            sustain: DEFAULT_SUSTAIN,
            release: DEFAULT_RELEASE_SEC,
        }
    }

    fn note_to_freq(note: u8, detune_semitones: f32) -> f32 {
        440.0 * 2.0_f32.powf((note as f32 - 69.0 + detune_semitones) / 12.0)
    }
}

impl Plugin for Oscillator {
    fn name(&self) -> &str {
        "Oscillator"
    }

    fn is_instrument(&self) -> bool {
        true
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_output_count(&self) -> usize {
        2
    }

    fn audio_input_count(&self) -> usize {
        0
    }

    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        _audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let block_size = audio_out[0].len();

        // Clear output buffers
        for ch in audio_out.iter_mut() {
            for s in ch.iter_mut() {
                *s = 0.0;
            }
        }

        let mut event_idx = 0;
        // Per-frame envelope increments. Attack ramps 0→1 over `attack` sec,
        // decay 1→sustain over `decay` sec, release falls at a 1→0-over-
        // `release`-sec rate. Times are floored to keep ramps click-free.
        let attack_inc = 1.0 / (self.attack.max(MIN_RAMP_SEC) * self.sample_rate);
        let decay_inc =
            (1.0 - self.sustain) / (self.decay.max(MIN_RAMP_SEC) * self.sample_rate);
        let release_inc = 1.0 / (self.release.max(MIN_RAMP_SEC) * self.sample_rate);
        let volume_alpha = 1.0 - (-1.0 / (VOLUME_SMOOTH_SEC * self.sample_rate)).exp();

        for frame in 0..block_size {
            // Process MIDI events at this frame
            while event_idx < midi_events.len() && midi_events[event_idx].0 as usize <= frame {
                let [status, note, velocity] = midi_events[event_idx].1;
                let msg_type = status & 0xF0;
                match msg_type {
                    0x90 if velocity > 0 => {
                        // Note-on: attack from current amp (0 if new voice, or
                        // the partially-released level on retrigger — avoids a
                        // click when re-hitting a still-decaying note).
                        self.voices
                            .entry(note)
                            .and_modify(|v| v.state = VoiceState::Attack)
                            .or_insert(Voice {
                                phase: 0.0,
                                amp: 0.0,
                                state: VoiceState::Attack,
                            });
                    }
                    0x80 | 0x90 => {
                        if let Some(v) = self.voices.get_mut(&note) {
                            v.state = VoiceState::Release;
                        }
                    }
                    _ => {}
                }
                event_idx += 1;
            }

            // Render all active voices with fixed per-voice gain.
            // Using a fixed gain avoids amplitude discontinuities when
            // voices are added/removed (dividing by voice count causes
            // existing voices to suddenly change volume).
            const VOICE_GAIN: f32 = 0.25;
            let mut sample = 0.0_f32;
            for (&note, voice) in self.voices.iter_mut() {
                match voice.state {
                    VoiceState::Attack => {
                        voice.amp += attack_inc;
                        if voice.amp >= 1.0 {
                            voice.amp = 1.0;
                            voice.state = VoiceState::Decay;
                        }
                    }
                    VoiceState::Decay => {
                        voice.amp -= decay_inc;
                        if voice.amp <= self.sustain {
                            voice.amp = self.sustain;
                            voice.state = VoiceState::Sustain;
                        }
                    }
                    VoiceState::Sustain => {
                        // Track the sustain parameter (it may be modulated).
                        voice.amp = self.sustain;
                    }
                    VoiceState::Release => {
                        voice.amp -= release_inc;
                        if voice.amp < 0.0 {
                            voice.amp = 0.0;
                        }
                    }
                }

                let freq = Self::note_to_freq(note, self.detune);
                sample += self.waveform.sample(voice.phase) * voice.amp * VOICE_GAIN;
                voice.phase += freq / self.sample_rate;
                if voice.phase >= 1.0 {
                    voice.phase -= 1.0;
                }
            }

            // Drop fully-released voices.
            self.voices
                .retain(|_, v| !(matches!(v.state, VoiceState::Release) && v.amp <= 0.0));

            self.volume_smoothed += (self.volume - self.volume_smoothed) * volume_alpha;
            self.pan_smoothed += (self.pan - self.pan_smoothed) * volume_alpha;
            let mono = sample * self.volume_smoothed;

            // Balance-style pan: center leaves both channels at full (matching
            // the old mono-to-both behavior), panning attenuates the far channel.
            let p = self.pan_smoothed;
            let (left_gain, right_gain) = if p >= 0.0 {
                (1.0 - p, 1.0)
            } else {
                (1.0, 1.0 + p)
            };
            if audio_out.len() > 1 {
                audio_out[0][frame] = mono * left_gain;
                audio_out[1][frame] = mono * right_gain;
            } else {
                // Mono output: pan collapses to full level.
                audio_out[0][frame] = mono;
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        vec![
            ParameterInfo {
                index: 0,
                name: "waveform".into(),
                min: 0.0,
                max: (WAVEFORMS.len() - 1) as f32,
                default: 0.0,
                labels: Some(WAVEFORMS.iter().map(|s| s.to_string()).collect()),
            },
            ParameterInfo {
                index: 1,
                name: "detune".into(),
                min: DETUNE_MIN,
                max: DETUNE_MAX,
                default: 0.0,
                ..Default::default()
            },
            ParameterInfo {
                index: 2,
                name: "volume".into(),
                min: 0.0,
                max: 1.0,
                default: 1.0,
                ..Default::default()
            },
            ParameterInfo {
                index: 3,
                name: "attack".into(),
                min: 0.0,
                max: ADSR_TIME_MAX,
                default: DEFAULT_ATTACK_SEC,
                ..Default::default()
            },
            ParameterInfo {
                index: 4,
                name: "decay".into(),
                min: 0.0,
                max: ADSR_TIME_MAX,
                default: DEFAULT_DECAY_SEC,
                ..Default::default()
            },
            ParameterInfo {
                index: 5,
                name: "sustain".into(),
                min: 0.0,
                max: 1.0,
                default: DEFAULT_SUSTAIN,
                ..Default::default()
            },
            ParameterInfo {
                index: 6,
                name: "release".into(),
                min: 0.0,
                max: ADSR_TIME_MAX,
                default: DEFAULT_RELEASE_SEC,
                ..Default::default()
            },
            ParameterInfo {
                index: 7,
                name: "pan".into(),
                min: PAN_MIN,
                max: PAN_MAX,
                default: 0.0,
                ..Default::default()
            },
        ]
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.waveform.to_param()),
            1 => Some(self.detune),
            2 => Some(self.volume),
            3 => Some(self.attack),
            4 => Some(self.decay),
            5 => Some(self.sustain),
            6 => Some(self.release),
            7 => Some(self.pan),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        match index {
            0 => {
                self.waveform = Waveform::from_param(value);
                Ok(())
            }
            1 => {
                self.detune = value.clamp(DETUNE_MIN, DETUNE_MAX);
                Ok(())
            }
            2 => {
                self.volume = value.clamp(0.0, 1.0);
                Ok(())
            }
            3 => {
                self.attack = value.clamp(0.0, ADSR_TIME_MAX);
                Ok(())
            }
            4 => {
                self.decay = value.clamp(0.0, ADSR_TIME_MAX);
                Ok(())
            }
            5 => {
                self.sustain = value.clamp(0.0, 1.0);
                Ok(())
            }
            6 => {
                self.release = value.clamp(0.0, ADSR_TIME_MAX);
                Ok(())
            }
            7 => {
                self.pan = value.clamp(PAN_MIN, PAN_MAX);
                Ok(())
            }
            _ => anyhow::bail!("no parameter with index {index}"),
        }
    }

    fn presets(&self) -> Vec<Preset> {
        Vec::new()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        anyhow::bail!("no preset with id {id:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48000.0;

    /// Render `frames` of audio with the given MIDI events, returning the
    /// left channel.
    fn render(osc: &mut Oscillator, events: &[(u64, [u8; 3])], frames: usize) -> Vec<f32> {
        let mut left = vec![0.0f32; frames];
        let mut right = vec![0.0f32; frames];
        let mut out: Vec<&mut [f32]> = vec![&mut left, &mut right];
        osc.process(events, &[], &mut out).unwrap();
        left
    }

    /// Peak amplitude of the last quarter of the buffer — past the attack
    /// ramp and volume smoothing, so the level has settled.
    fn settled_peak(samples: &[f32]) -> f32 {
        samples[samples.len() * 3 / 4..]
            .iter()
            .fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    #[test]
    fn parameters_match_param_count() {
        let osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.parameters().len(), PARAM_COUNT);
    }

    #[test]
    fn parameters_in_named_order() {
        // The save/load path resolves params by name, but assert the order
        // so the index-based get/set arms stay aligned with parameters().
        let osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let params = osc.parameters();
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan"]
        );
        // The waveform parameter is an enum.
        assert!(params[0].labels.is_some());
    }

    #[test]
    fn waveform_param_switches_output() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let rms = |s: &[f32]| {
            let tail = &s[s.len() * 3 / 4..];
            (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
        };
        let mut sine = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let sine_rms = rms(&render(&mut sine, &NOTE_ON, frames));

        // Same oscillator, switched to square via the parameter.
        let mut sq = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        sq.set_parameter(0, 2.0).unwrap();
        let sq_rms = rms(&render(&mut sq, &NOTE_ON, frames));

        // A square's RMS is ~√2× a sine of the same peak — clearly different.
        assert!(
            sq_rms > sine_rms * 1.2,
            "square should have more energy than sine: square {sq_rms}, sine {sine_rms}"
        );
        assert_eq!(sq.get_parameter(0), Some(2.0));
    }

    #[test]
    fn saw_waveform_selectable() {
        // Saw is index 3 (appended after square) and round-trips.
        assert_eq!(WAVEFORMS[3], "saw");
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(0, 3.0).unwrap();
        assert_eq!(osc.get_parameter(0), Some(3.0));

        // It renders a real signal that swings both positive and negative
        // (a rising ramp), scaled by the per-voice gain (~0.25 peak).
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let out = render(&mut osc, &NOTE_ON, frames);
        let tail = &out[out.len() * 3 / 4..];
        let max = tail.iter().cloned().fold(f32::MIN, f32::max);
        let min = tail.iter().cloned().fold(f32::MAX, f32::min);
        assert!(max > 0.15 && min < -0.15, "saw should swing across the range: [{min}, {max}]");
    }

    #[test]
    fn volume_round_trips_and_clamps() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(2), Some(1.0));
        osc.set_parameter(2, 0.7).unwrap();
        assert_eq!(osc.get_parameter(2), Some(0.7));
        osc.set_parameter(2, 2.5).unwrap();
        assert_eq!(osc.get_parameter(2), Some(1.0));
        osc.set_parameter(2, -1.0).unwrap();
        assert_eq!(osc.get_parameter(2), Some(0.0));
    }

    #[test]
    fn volume_scales_output() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let note_on = [(0u64, [0x90, 60, 100])];

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let full = settled_peak(&render(&mut osc, &note_on, frames));

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(2, 0.5).unwrap();
        let half = settled_peak(&render(&mut osc, &note_on, frames));

        assert!(full > 0.2, "expected audible output, got peak {full}");
        let ratio = half / full;
        assert!((ratio - 0.5).abs() < 0.01, "expected ~0.5 ratio, got {ratio}");
    }

    #[test]
    fn volume_zero_silences_output() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(2, 0.0).unwrap();
        let peak = settled_peak(&render(&mut osc, &[(0, [0x90, 60, 100])], frames));
        assert!(peak < 1e-3, "expected silence, got peak {peak}");
    }

    #[test]
    fn pan_round_trips_and_clamps() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(7), Some(0.0)); // default center
        osc.set_parameter(7, -0.5).unwrap();
        assert_eq!(osc.get_parameter(7), Some(-0.5));
        osc.set_parameter(7, 5.0).unwrap();
        assert_eq!(osc.get_parameter(7), Some(1.0)); // clamped to hard right
        osc.set_parameter(7, -5.0).unwrap();
        assert_eq!(osc.get_parameter(7), Some(-1.0)); // clamped to hard left
    }

    #[test]
    fn detune_round_trips_and_clamps_to_two_octaves() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        assert_eq!(osc.get_parameter(1), Some(0.0)); // default: no detune
        osc.set_parameter(1, 12.0).unwrap();
        assert_eq!(osc.get_parameter(1), Some(12.0)); // an octave up
        osc.set_parameter(1, 100.0).unwrap();
        assert_eq!(osc.get_parameter(1), Some(24.0)); // clamped to +2 octaves
        osc.set_parameter(1, -100.0).unwrap();
        assert_eq!(osc.get_parameter(1), Some(-24.0)); // clamped to −2 octaves
    }

    #[test]
    fn pan_balances_channels() {
        let frames = (SAMPLE_RATE * 0.1) as usize;
        // Settled peak of each channel (past the attack + pan-smoothing ramps).
        let render_lr = |osc: &mut Oscillator| -> (f32, f32) {
            let mut l = vec![0.0f32; frames];
            let mut r = vec![0.0f32; frames];
            let mut out: Vec<&mut [f32]> = vec![&mut l, &mut r];
            osc.process(&NOTE_ON, &[], &mut out).unwrap();
            (settled_peak(&l), settled_peak(&r))
        };

        // Center: both channels equal (matches the old mono-to-both output).
        let mut center = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let (cl, cr) = render_lr(&mut center);
        assert!(cl > 0.2 && (cl - cr).abs() < 1e-3, "center should be equal: L {cl}, R {cr}");

        // Hard right: left silent, right stays at full (center level).
        let mut right = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        right.set_parameter(7, 1.0).unwrap();
        let (rl, rr) = render_lr(&mut right);
        assert!(rl < 1e-2, "hard right should silence the left channel: {rl}");
        assert!((rr - cr).abs() < 0.02, "hard right keeps the right at full: {rr} vs {cr}");

        // Hard left: mirror image.
        let mut left = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        left.set_parameter(7, -1.0).unwrap();
        let (ll, lr) = render_lr(&mut left);
        assert!(lr < 1e-2, "hard left should silence the right channel: {lr}");
        assert!((ll - cl).abs() < 0.02, "hard left keeps the left at full: {ll} vs {cl}");
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    const NOTE_ON: [(u64, [u8; 3]); 1] = [(0, [0x90, 60, 100])];
    const NOTE_OFF: [(u64, [u8; 3]); 1] = [(0, [0x80, 60, 0])];

    #[test]
    fn adsr_defaults_match_legacy_ramps() {
        // Default envelope = the original click-guard ramps: full level
        // within ~10ms of note-on, silent within ~25ms of note-off.
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let on = render(&mut osc, &NOTE_ON, (SAMPLE_RATE * 0.05) as usize);
        assert!(settled_peak(&on) > 0.2, "expected full level after note-on");

        let off = render(&mut osc, &NOTE_OFF, (SAMPLE_RATE * 0.05) as usize);
        assert!(
            settled_peak(&off) < 1e-3,
            "expected silence shortly after note-off, got {}",
            settled_peak(&off)
        );
    }

    #[test]
    fn adsr_attack_ramps_slowly() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(3, 0.2).unwrap(); // 200ms attack
        let early = render(&mut osc, &NOTE_ON, (SAMPLE_RATE * 0.02) as usize);
        let later = render(&mut osc, &[], (SAMPLE_RATE * 0.3) as usize);
        let early_peak = peak(&early);
        let full_peak = settled_peak(&later);
        assert!(full_peak > 0.2, "expected full level eventually, got {full_peak}");
        assert!(
            early_peak < 0.15 * full_peak,
            "20ms into a 200ms attack should still be quiet: early {early_peak}, full {full_peak}"
        );
    }

    #[test]
    fn adsr_release_rings_after_note_off() {
        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(6, 0.5).unwrap(); // 500ms release
        let on = render(&mut osc, &NOTE_ON, (SAMPLE_RATE * 0.05) as usize);
        let full_peak = settled_peak(&on);

        // 200ms after note-off the voice must still be sounding (linear
        // release from 1.0 puts it at ~0.6 of full level).
        let tail = render(&mut osc, &NOTE_OFF, (SAMPLE_RATE * 0.2) as usize);
        let tail_end = settled_peak(&tail);
        assert!(
            tail_end > 0.3 * full_peak,
            "release tail should still ring at 200ms: tail {tail_end}, full {full_peak}"
        );

        // Well past the 500ms release it must be silent.
        let done = render(&mut osc, &[], (SAMPLE_RATE * 0.5) as usize);
        assert!(
            settled_peak(&done) < 1e-3,
            "expected silence after release completes, got {}",
            settled_peak(&done)
        );
    }

    #[test]
    fn adsr_sustain_sets_held_level() {
        let frames = (SAMPLE_RATE * 0.2) as usize;

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        let full = settled_peak(&render(&mut osc, &NOTE_ON, frames));

        let mut osc = Oscillator::new(SAMPLE_RATE, Waveform::Sine);
        osc.set_parameter(4, 0.02).unwrap(); // quick decay
        osc.set_parameter(5, 0.5).unwrap(); // sustain at half
        let half = settled_peak(&render(&mut osc, &NOTE_ON, frames));

        let ratio = half / full;
        assert!(
            (ratio - 0.5).abs() < 0.02,
            "expected sustain at ~0.5 of full level, got ratio {ratio}"
        );
    }
}
