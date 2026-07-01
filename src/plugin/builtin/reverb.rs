use std::f32::consts::TAU;

use crate::plugin::{ParameterInfo, Plugin, Preset};

pub const NAME: &str = "Reverb";
pub const ID: &str = "builtin:reverb";
pub const PARAM_COUNT: usize = 10;

/// Number of FDN delay lines.
const N: usize = 16;

/// Base FDN delay-line lengths in milliseconds, spread roughly logarithmically
/// from ~24ms to ~120ms for a hall-sized response. Coprime values keep the
/// modal density irregular and avoid flutter echoes.
const BASE_LENGTHS_MS: [f32; N] = [
    23.7, 27.3, 31.1, 35.7, 41.0, 47.1, 53.9, 59.3, 65.7, 73.1, 79.3, 87.7, 93.1, 101.3, 109.7,
    119.3,
];

/// Schroeder allpass diffuser lengths in samples. Short enough to be fine as
/// fixed sample counts at any reasonable sample rate.
const DIFFUSER_LENGTHS: [usize; 4] = [142, 379, 107, 277];

const MAX_PREDELAY_SEC: f32 = 0.25;

/// Maximum modulation excursion as fraction of base delay length.
const MAX_MOD_FRACTION: f32 = 0.012;

struct ParamSpec {
    name: &'static str,
    min: f32,
    max: f32,
    default: f32,
}

const PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "decay",     min: 0.1,  max: 0.99,             default: 0.88 },
    ParamSpec { name: "size",      min: 0.3,  max: 1.0,              default: 0.85 },
    ParamSpec { name: "damping",   min: 0.0,  max: 1.0,              default: 0.35 },
    ParamSpec { name: "predelay",  min: 0.0,  max: MAX_PREDELAY_SEC, default: 0.04 },
    ParamSpec { name: "diffusion", min: 0.0,  max: 0.85,             default: 0.70 },
    ParamSpec { name: "mod_rate",  min: 0.05, max: 2.0,              default: 0.30 },
    ParamSpec { name: "mod_depth", min: 0.0,  max: 1.0,              default: 0.50 },
    ParamSpec { name: "width",     min: 0.0,  max: 1.0,              default: 1.00 },
    ParamSpec { name: "lowcut",    min: 20.0, max: 500.0,            default: 80.0 },
    ParamSpec { name: "level",     min: 0.0,  max: 2.0,              default: 1.00 },
];

pub const PRESETS: &[(&str, &str, [f32; 10])] = &[
    // (id, display_name, [decay, size, damping, predelay, diffusion, mod_rate, mod_depth, width, lowcut, level])
    ("arcadia",   "Arcadia Dream Hall", [0.94, 0.95, 0.30, 0.06, 0.78, 0.25, 0.70, 1.00,  80.0, 0.85]),
    ("cathedral", "Cathedral",          [0.96, 1.00, 0.55, 0.10, 0.72, 0.15, 0.55, 1.00,  60.0, 0.80]),
    ("plate",     "Vintage Plate",      [0.80, 0.55, 0.25, 0.02, 0.78, 0.18, 0.30, 0.85, 120.0, 1.00]),
    ("room",      "Small Room",         [0.55, 0.40, 0.40, 0.01, 0.65, 0.40, 0.20, 0.90, 100.0, 1.00]),
    ("ambient",   "Ambient Wash",       [0.96, 0.95, 0.20, 0.12, 0.78, 0.45, 0.85, 1.00, 100.0, 0.80]),
];

struct DelayLine {
    buf: Vec<f32>,
    write_idx: usize,
}

impl DelayLine {
    fn new(max_samples: usize) -> Self {
        Self {
            buf: vec![0.0; max_samples.max(2)],
            write_idx: 0,
        }
    }

    fn write(&mut self, x: f32) {
        self.buf[self.write_idx] = x;
        self.write_idx = (self.write_idx + 1) % self.buf.len();
    }

    /// Fractional read of a sample written `delay` samples ago.
    fn read_lerp(&self, delay: f32) -> f32 {
        let len = self.buf.len();
        let max_d = (len - 2) as f32;
        let d = delay.clamp(1.0, max_d);
        let d_int = d as usize;
        let frac = d - d_int as f32;
        let near = self.buf[(self.write_idx + len - d_int) % len];
        let far = self.buf[(self.write_idx + len - d_int - 1) % len];
        near * (1.0 - frac) + far * frac
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

struct Allpass {
    buf: Vec<f32>,
    idx: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            idx: 0,
        }
    }

    /// Schroeder allpass, lattice form: one delay line, unit magnitude response.
    fn process(&mut self, x: f32, g: f32) -> f32 {
        let v_delayed = self.buf[self.idx];
        let v = x - g * v_delayed;
        self.buf[self.idx] = v;
        let y = g * v + v_delayed;
        self.idx = (self.idx + 1) % self.buf.len();
        y
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Soft-clip the wet output so a long, energetic reverb tail can't leave the
/// [-1, 1] range the audio device expects. An FDN with a near-lossless feedback
/// matrix and long `decay` builds up modal energy under sustained input
/// (steady-state gain ~= 1/(1 - decay)); left unbounded that clips harshly at
/// the DAC and crackles. This is the identity below ±KNEE and a C1-smooth
/// saturation above, asymptoting to ±1 — transparent at normal levels, gentle
/// compression when hot.
#[inline]
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

pub struct Reverb {
    sample_rate: f32,

    delay_lines: [DelayLine; N],
    damp_states: [f32; N],
    lfo_phases: [f32; N],

    diffusers: [Allpass; 4],

    predelay_buf: Vec<f32>,
    predelay_idx: usize,

    hpf_state_l: f32,
    hpf_state_r: f32,

    decay: f32,
    size: f32,
    damping: f32,
    predelay_sec: f32,
    diffusion: f32,
    mod_rate: f32,
    mod_depth: f32,
    width: f32,
    lowcut_hz: f32,
    level: f32,
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let max_size = 1.0 + MAX_MOD_FRACTION;
        let delay_lines = std::array::from_fn(|i| {
            let max_samples = (BASE_LENGTHS_MS[i] * 1e-3 * sample_rate * max_size).ceil() as usize
                + 4;
            DelayLine::new(max_samples)
        });
        let predelay_samples = (MAX_PREDELAY_SEC * sample_rate).ceil() as usize + 1;
        let lfo_phases = std::array::from_fn(|i| (i as f32 * 0.0617).rem_euclid(1.0));
        let diffusers = std::array::from_fn(|i| Allpass::new(DIFFUSER_LENGTHS[i]));

        Self {
            sample_rate,
            delay_lines,
            damp_states: [0.0; N],
            lfo_phases,
            diffusers,
            predelay_buf: vec![0.0; predelay_samples],
            predelay_idx: 0,
            hpf_state_l: 0.0,
            hpf_state_r: 0.0,
            decay: PARAMS[0].default,
            size: PARAMS[1].default,
            damping: PARAMS[2].default,
            predelay_sec: PARAMS[3].default,
            diffusion: PARAMS[4].default,
            mod_rate: PARAMS[5].default,
            mod_depth: PARAMS[6].default,
            width: PARAMS[7].default,
            lowcut_hz: PARAMS[8].default,
            level: PARAMS[9].default,
        }
    }

    fn lowcut_coef(&self) -> f32 {
        let fc = self.lowcut_hz.clamp(20.0, self.sample_rate * 0.45);
        1.0 - (-TAU * fc / self.sample_rate).exp()
    }
}

impl Plugin for Reverb {
    fn name(&self) -> &str {
        NAME
    }

    fn is_instrument(&self) -> bool {
        false
    }

    fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn audio_input_count(&self) -> usize {
        2
    }

    fn audio_output_count(&self) -> usize {
        2
    }

    fn process(
        &mut self,
        _midi_events: &[(u64, [u8; 3])],
        audio_in: &[&[f32]],
        audio_out: &mut [&mut [f32]],
    ) -> anyhow::Result<()> {
        let block_size = audio_out[0].len();

        let in_l = audio_in.first().copied();
        let in_r = audio_in.get(1).copied().or(in_l);

        let lowcut_coef = self.lowcut_coef();
        let damp_coef = (self.damping * 0.85).clamp(0.0, 0.95);
        let predelay_len = self.predelay_buf.len();
        let predelay_samples = ((self.predelay_sec * self.sample_rate) as usize)
            .min(predelay_len.saturating_sub(1));
        let inv_sr = 1.0 / self.sample_rate;
        let mod_rate = self.mod_rate;
        let mod_depth = self.mod_depth;
        let size = self.size;
        let decay = self.decay;
        let diffusion = self.diffusion;
        let width = self.width;
        let level = self.level;
        let factor = 2.0 / N as f32;
        let inj_gain = 1.0 / (N as f32).sqrt();
        let tap_scale = (2.0 / N as f32).sqrt();

        for frame in 0..block_size {
            let l_in = in_l.map(|s| s[frame]).unwrap_or(0.0);
            let r_in = in_r.map(|s| s[frame]).unwrap_or(0.0);

            // Lowcut HPF per channel: HPF = input - LPF(input).
            let lpf_l = self.hpf_state_l + lowcut_coef * (l_in - self.hpf_state_l);
            self.hpf_state_l = lpf_l;
            let l_hp = l_in - lpf_l;
            let lpf_r = self.hpf_state_r + lowcut_coef * (r_in - self.hpf_state_r);
            self.hpf_state_r = lpf_r;
            let r_hp = r_in - lpf_r;

            let mono_in = (l_hp + r_hp) * 0.5;

            // Pre-delay.
            self.predelay_buf[self.predelay_idx] = mono_in;
            let read_idx = (self.predelay_idx + predelay_len - predelay_samples) % predelay_len;
            let predelayed = self.predelay_buf[read_idx];
            self.predelay_idx = (self.predelay_idx + 1) % predelay_len;

            // Input diffusion: 4 cascaded Schroeder allpasses.
            let mut diffused = predelayed;
            for diffuser in self.diffusers.iter_mut() {
                diffused = diffuser.process(diffused, diffusion);
            }

            // Read FDN delay lines with per-line modulation.
            let mut tap = [0.0f32; N];
            for i in 0..N {
                let base = BASE_LENGTHS_MS[i] * 1e-3 * self.sample_rate * size;
                let lfo = (TAU * self.lfo_phases[i]).sin();
                let mod_amount = mod_depth * base * MAX_MOD_FRACTION;
                tap[i] = self.delay_lines[i].read_lerp(base + lfo * mod_amount);

                // Slightly different rate per line for richer modulation.
                let rate = mod_rate * (1.0 + 0.043 * i as f32);
                self.lfo_phases[i] += rate * inv_sr;
                if self.lfo_phases[i] >= 1.0 {
                    self.lfo_phases[i] -= 1.0;
                }
            }

            // HF damping in feedback path: one-pole LPF per line.
            let mut damped = [0.0f32; N];
            for i in 0..N {
                let lpf = (1.0 - damp_coef) * tap[i] + damp_coef * self.damp_states[i];
                self.damp_states[i] = lpf;
                damped[i] = lpf;
            }

            // Householder feedback matrix M = I - (2/N) * J.
            let sum: f32 = damped.iter().sum();
            let mut fb = [0.0f32; N];
            for (fb_i, &d_i) in fb.iter_mut().zip(damped.iter()) {
                *fb_i = (d_i - factor * sum) * decay;
            }

            // Inject input + write feedback into delay lines.
            for (i, (line, &fb_i)) in self.delay_lines.iter_mut().zip(fb.iter()).enumerate() {
                let sign = if i & 1 == 0 { 1.0 } else { -1.0 };
                line.write(diffused * inj_gain * sign + fb_i);
            }

            // Stereo tap: even indices to L, odd to R, with alternating
            // pair-signs for decorrelation.
            let mut out_l = 0.0;
            let mut out_r = 0.0;
            for (i, &t) in tap.iter().enumerate() {
                let s = if (i / 2) & 1 == 0 { t } else { -t };
                if i & 1 == 0 {
                    out_l += s;
                } else {
                    out_r += s;
                }
            }
            out_l *= tap_scale;
            out_r *= tap_scale;

            // Width via mid/side.
            let mid = (out_l + out_r) * 0.5;
            let side = (out_l - out_r) * 0.5 * width;
            let l_out = (mid + side) * level;
            let r_out = (mid - side) * level;

            // Bound the wet output so a hot tail can't clip the DAC and crackle.
            audio_out[0][frame] = soft_clip(l_out);
            if audio_out.len() > 1 {
                audio_out[1][frame] = soft_clip(r_out);
            }
        }

        Ok(())
    }

    fn parameters(&self) -> Vec<ParameterInfo> {
        PARAMS
            .iter()
            .enumerate()
            .map(|(i, p)| ParameterInfo {
                index: i as u32,
                name: p.name.into(),
                min: p.min,
                max: p.max,
                default: p.default,
                ..Default::default()
            })
            .collect()
    }

    fn get_parameter(&mut self, index: u32) -> Option<f32> {
        match index {
            0 => Some(self.decay),
            1 => Some(self.size),
            2 => Some(self.damping),
            3 => Some(self.predelay_sec),
            4 => Some(self.diffusion),
            5 => Some(self.mod_rate),
            6 => Some(self.mod_depth),
            7 => Some(self.width),
            8 => Some(self.lowcut_hz),
            9 => Some(self.level),
            _ => None,
        }
    }

    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        let i = index as usize;
        let spec = PARAMS
            .get(i)
            .ok_or_else(|| anyhow::anyhow!("no parameter with index {index}"))?;
        let v = value.clamp(spec.min, spec.max);
        let old_size = self.size;
        match i {
            0 => self.decay = v,
            1 => self.size = v,
            2 => self.damping = v,
            3 => self.predelay_sec = v,
            4 => self.diffusion = v,
            5 => self.mod_rate = v,
            6 => self.mod_depth = v,
            7 => self.width = v,
            8 => self.lowcut_hz = v,
            9 => self.level = v,
            _ => unreachable!(),
        }
        // Big size changes can leave damping state with stale energy; gently
        // clear taps when size jumps a lot, to avoid a brief click. Threshold
        // chosen so the user dragging a slider continuously is smooth.
        if i == 1 && (old_size - self.size).abs() > 0.4 {
            self.damp_states = [0.0; N];
        }
        Ok(())
    }

    fn presets(&self) -> Vec<Preset> {
        PRESETS
            .iter()
            .map(|(id, name, _)| Preset {
                name: (*name).into(),
                id: (*id).into(),
            })
            .collect()
    }

    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        let (_, _, values) = PRESETS
            .iter()
            .find(|(pid, _, _)| *pid == id)
            .ok_or_else(|| anyhow::anyhow!("no preset with id {id:?}"))?;
        self.decay = values[0];
        self.size = values[1];
        self.damping = values[2];
        self.predelay_sec = values[3];
        self.diffusion = values[4];
        self.mod_rate = values[5];
        self.mod_depth = values[6];
        self.width = values[7];
        self.lowcut_hz = values[8];
        self.level = values[9];
        // Fresh decay state for the new sound.
        for d in self.delay_lines.iter_mut() {
            d.clear();
        }
        for a in self.diffusers.iter_mut() {
            a.clear();
        }
        self.damp_states = [0.0; N];
        self.predelay_buf.fill(0.0);
        self.predelay_idx = 0;
        self.hpf_state_l = 0.0;
        self.hpf_state_r = 0.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(reverb: &mut Reverb, blocks: usize, block_size: usize, impulse: bool) -> (f32, f32) {
        let mut total_energy = 0.0f32;
        let mut max_abs = 0.0f32;
        for b in 0..blocks {
            let mut in_l = vec![0.0f32; block_size];
            let mut in_r = vec![0.0f32; block_size];
            if impulse && b == 0 {
                in_l[0] = 1.0;
                in_r[0] = 1.0;
            }
            let mut out_l = vec![0.0f32; block_size];
            let mut out_r = vec![0.0f32; block_size];
            let in_slices: Vec<&[f32]> = vec![&in_l, &in_r];
            let mut out_refs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
            reverb
                .process(&[], &in_slices, &mut out_refs)
                .expect("process");
            for &s in out_l.iter().chain(out_r.iter()) {
                assert!(s.is_finite(), "non-finite sample: {s}");
                total_energy += s * s;
                max_abs = max_abs.max(s.abs());
            }
        }
        (total_energy, max_abs)
    }

    #[test]
    fn impulse_produces_decaying_tail() {
        let mut reverb = Reverb::new(48_000.0);
        reverb.set_parameter(3, 0.0).unwrap(); // predelay = 0 so we hear it immediately
        let (energy, max_abs) = run(&mut reverb, 200, 256, true);
        assert!(energy > 0.001, "negligible output energy: {energy}");
        assert!(max_abs < 5.0, "output too hot: {max_abs}");
    }

    #[test]
    fn silence_in_eventually_silence_out() {
        let mut reverb = Reverb::new(48_000.0);
        reverb.set_parameter(0, 0.7).unwrap(); // shorter decay
        let (e1, _) = run(&mut reverb, 50, 256, true);
        // Now let it decay with no input for plenty of blocks.
        let (e2, _) = run(&mut reverb, 1000, 256, false);
        // Energy in the long tail should be much less than the initial response.
        assert!(e2 < e1, "tail energy did not decrease: e1={e1} e2={e2}");
    }

    #[test]
    fn presets_load_without_error() {
        let mut reverb = Reverb::new(48_000.0);
        for (id, _, _) in PRESETS {
            reverb.load_preset(id).expect(id);
            let (energy, max_abs) = run(&mut reverb, 50, 256, true);
            assert!(energy > 0.0, "preset {id} produced no output");
            assert!(max_abs < 10.0, "preset {id} produced hot output: {max_abs}");
        }
    }

    #[test]
    fn sustained_input_output_stays_bounded() {
        // A long reverb driven by sustained full-scale input builds up modal
        // energy; the soft-clipped wet output must stay within [-1, 1] so it
        // can't clip the DAC and crackle.
        let mut reverb = Reverb::new(48_000.0);
        reverb.set_parameter(0, 0.99).unwrap(); // long decay
        reverb.set_parameter(9, 2.0).unwrap(); // max level
        let block_size = 256;
        let sr = 48_000.0f32;
        let freq = 220.0f32;
        let mut phase = 0.0f32;
        let mut max_abs = 0.0f32;
        for _ in 0..400 {
            // Sustained full-scale tone (DC would be removed by the lowcut).
            let mut in_l = vec![0.0f32; block_size];
            let mut in_r = vec![0.0f32; block_size];
            for k in 0..block_size {
                let s = (phase * TAU).sin();
                in_l[k] = s;
                in_r[k] = s;
                phase = (phase + freq / sr).fract();
            }
            let mut out_l = vec![0.0f32; block_size];
            let mut out_r = vec![0.0f32; block_size];
            let in_slices: Vec<&[f32]> = vec![&in_l, &in_r];
            let mut out_refs: Vec<&mut [f32]> = vec![&mut out_l, &mut out_r];
            reverb.process(&[], &in_slices, &mut out_refs).unwrap();
            for &s in out_l.iter().chain(out_r.iter()) {
                assert!(s.is_finite(), "non-finite sample: {s}");
                max_abs = max_abs.max(s.abs());
            }
        }
        assert!(max_abs <= 1.0 + 1e-4, "output left [-1,1]: {max_abs}");
        // Confirm it was actually driven into the limiter (so the test is
        // meaningful — without soft-clipping this tone builds up past 1.0).
        assert!(max_abs > 0.9, "expected a hot tail near the ceiling: {max_abs}");
    }

    #[test]
    fn soft_clip_transparent_then_bounded() {
        // Transparent below the knee.
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.5), -0.5);
        // C0-continuous at the knee.
        assert!((soft_clip(0.8) - 0.8).abs() < 1e-6);
        // Bounded to (-1, 1) no matter how hot.
        assert!((0.99..=1.0).contains(&soft_clip(100.0)));
        assert!((-1.0..=-0.99).contains(&soft_clip(-100.0)));
    }

    #[test]
    fn parameters_clamp_and_roundtrip() {
        let mut reverb = Reverb::new(48_000.0);
        for (i, spec) in PARAMS.iter().enumerate() {
            reverb.set_parameter(i as u32, spec.max + 1000.0).unwrap();
            let v = reverb.get_parameter(i as u32).unwrap();
            assert!(v <= spec.max + 1e-6, "param {} not clamped to max", spec.name);
            reverb.set_parameter(i as u32, spec.min - 1000.0).unwrap();
            let v = reverb.get_parameter(i as u32).unwrap();
            assert!(v >= spec.min - 1e-6, "param {} not clamped to min", spec.name);
        }
    }
}
