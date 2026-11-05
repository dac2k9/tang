mod delay;
mod drive;
mod filter;
mod input;
mod math;
mod oscillator;
mod reverb;

use super::{Plugin, PluginInfo};

use delay::Delay;
use drive::Drive;
use filter::Filter;
use input::AudioInput;
use math::Math;
use oscillator::{Oscillator, Waveform};
use reverb::Reverb;

/// Soft-clip a signal that feeds back on itself (a reverb's tail, a delay's echoes) so that it can't leave the [-1, 1] range the audio
/// device expects. A feedback loop with a gain near 1 builds up energy under sustained input (steady-state gain ~= 1/(1 - feedback));
/// left unbounded that clips harshly at the DAC and crackles. This is the identity below ±KNEE and a C1-smooth saturation above,
/// asymptoting to ±1 — transparent at normal levels, gentle compression when hot.
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

/// `load_with`, for a program that has a sound card: what the tests load the built-in plugins with.
#[cfg(test)]
pub fn load(
    source: &str,
    sample_rate: f32,
    max_block_size: usize,
) -> anyhow::Result<Box<dyn Plugin>> {
    load_with(source, sample_rate, max_block_size, None)
}

/// Load a built-in plugin by source string (e.g. `"builtin:osc"`).
///
/// `osc` is the unified oscillator (pick the waveform via its `waveform`
/// parameter). `sine`/`triangle`/`square` are retained as load-only aliases
/// that start the oscillator on that waveform, so older sessions keep working.
///
/// Inside a host that Tang is a plugin of (step 113) the audio input that is made plays what the host gives it to hear (`host_input`) and opens no sound card; with none it opens the system's input.
pub fn load_with(
    source: &str,
    sample_rate: f32,
    _max_block_size: usize,
    host_input: Option<&std::sync::Arc<HostInput>>,
) -> anyhow::Result<Box<dyn Plugin>> {
    let name = source.strip_prefix("builtin:").unwrap_or(source);
    match name {
        "osc" | "oscillator" | "sine" => {
            Ok(Box::new(Oscillator::new(sample_rate, Waveform::Sine)))
        }
        "triangle" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Triangle))),
        "square" => Ok(Box::new(Oscillator::new(sample_rate, Waveform::Square))),
        "reverb" => Ok(Box::new(Reverb::new(sample_rate))),
        "filter" => Ok(Box::new(Filter::new(sample_rate))),
        "delay" => Ok(Box::new(Delay::new(sample_rate))),
        "drive" => Ok(Box::new(Drive::new(sample_rate))),
        "math" => Ok(Box::new(Math::new(sample_rate))),
        "input" => Ok(Box::new(match host_input {
            Some(feed) => AudioInput::with_host(sample_rate, feed.clone()),
            None => AudioInput::new(sample_rate),
        })),
        // Only in the tests: an effect with one audio output, which the chains (stereo) have no place for.
        #[cfg(test)]
        "mono-effect" => Ok(Box::new(crate::app::testutil::MonoEffect)),
        _ => anyhow::bail!(
            "Unknown built-in plugin: {name:?}\n\
             Available built-ins: osc, input, reverb, filter, delay, drive, math\n\
             Usage: builtin:osc"
        ),
    }
}

pub use input::{HostInput, probe as listen_to_the_input, use_test_tone};

/// Return enumeration info for all built-in plugins.
pub fn enumerate_plugins() -> Vec<PluginInfo> {
    vec![
        PluginInfo {
            name: "Oscillator".into(),
            id: "builtin:osc".into(),
            is_instrument: true,
            param_count: oscillator::PARAM_COUNT,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: input::NAME.into(),
            id: input::ID.into(),
            is_instrument: true,
            param_count: input::PARAM_COUNT,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: reverb::NAME.into(),
            id: reverb::ID.into(),
            is_instrument: false,
            param_count: reverb::PARAM_COUNT,
            preset_count: reverb::PRESETS.len(),
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: filter::NAME.into(),
            id: filter::ID.into(),
            is_instrument: false,
            param_count: filter::PARAM_COUNT,
            preset_count: 0,
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: delay::NAME.into(),
            id: delay::ID.into(),
            is_instrument: false,
            param_count: delay::PARAM_COUNT,
            preset_count: delay::PRESETS.len(),
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: drive::NAME.into(),
            id: drive::ID.into(),
            is_instrument: false,
            param_count: drive::PARAM_COUNT,
            preset_count: drive::PRESETS.len(),
            path: "(built-in)".into(),
            scan_ms: 0,
        },
        PluginInfo {
            name: math::NAME.into(),
            id: math::ID.into(),
            is_instrument: false,
            param_count: math::PARAM_COUNT,
            preset_count: math::PRESETS.len(),
            path: "(built-in)".into(),
            scan_ms: 0,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unified osc and the legacy waveform aliases all load, and the
    /// aliases start the oscillator on the matching waveform (the `waveform`
    /// parameter, index 0: sine=0, triangle=1, square=2).
    #[test]
    fn osc_and_aliases_load() {
        for (id, expected_waveform) in [
            ("builtin:osc", 0.0),
            ("builtin:sine", 0.0),
            ("builtin:triangle", 1.0),
            ("builtin:square", 2.0),
        ] {
            let mut p = load(id, 48000.0, 512).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(p.is_instrument(), "{id} should be an instrument");
            assert_eq!(
                p.get_parameter(0),
                Some(expected_waveform),
                "{id} should start on waveform {expected_waveform}"
            );
        }
    }

    /// What each parameter of a built-in says about itself besides its range:
    /// `(name, unit, group, step count)`.
    fn metadata(id: &str) -> Vec<(String, Option<String>, Vec<String>, u32)> {
        let plugin = load(id, 48000.0, 512).unwrap();
        plugin.parameters().into_iter().map(|p| (p.name, p.unit, p.group, p.step_count)).collect()
    }

    /// The expected `(name, unit, group, step count)` of a parameter.
    fn row(name: &str, unit: Option<&str>, group: &[&str], step_count: u32) -> (String, Option<String>, Vec<String>, u32) {
        (name.to_string(), unit.map(String::from), group.iter().map(|g| g.to_string()).collect(), step_count)
    }

    #[test]
    fn the_oscillators_parameters_say_their_unit_their_section_and_their_steps() {
        assert_eq!(
            metadata("builtin:osc"),
            [
                row("waveform", None, &[], 4),
                row("detune", Some("st"), &[], 0),
                row("volume", None, &[], 0),
                row("attack", Some("s"), &["Envelope"], 0),
                row("decay", Some("s"), &["Envelope"], 0),
                row("sustain", None, &["Envelope"], 0),
                row("release", Some("s"), &["Envelope"], 0),
                row("pan", None, &[], 0),
                row("unison", None, &[], 6),
                row("spread", Some("st"), &[], 0),
                row("width", None, &[], 0),
                row("pulse", None, &[], 0),
                row("fm", None, &["FM"], 0),
                row("fm_ratio", None, &["FM"], 15),
                row("sub", None, &["Voice"], 0),
                row("sync", None, &["Voice"], 0),
                row("drift", None, &["Voice"], 0),
                row("velocity", None, &["Performance"], 0),
                row("bend", None, &["Performance"], 8),
                row("quantize", None, &["Voice"], 15),
            ]
        );
    }

    #[test]
    fn the_drives_parameters_say_their_steps() {
        // The type is a choice of four shapes; the rest are plain numbers, and none has a unit (the drive is a knob that is linear in decibels, which is not a number of them).
        assert_eq!(
            metadata("builtin:drive"),
            [row("type", None, &[], 3), row("drive", None, &[], 0), row("tone", None, &[], 0), row("level", None, &[], 0)]
        );
    }

    #[test]
    fn the_maths_parameters_say_their_unit_their_section_and_their_steps() {
        // Three choices (the mode, the bits of the logic and the wave) and five numbers; the pitch is a note number, which has no unit.
        assert_eq!(
            metadata("builtin:math"),
            [
                row("mode", None, &[], 11),
                row("amount", None, &[], 0),
                row("bits", None, &["Logic"], 11),
                row("pitch", None, &["Carrier"], 0),
                row("wave", None, &["Carrier"], 3),
                row("attack", Some("s"), &["Follower"], 0),
                row("release", Some("s"), &["Follower"], 0),
                row("level", None, &[], 0),
            ]
        );
    }

    #[test]
    fn the_math_is_the_one_built_in_effect_with_a_second_input() {
        for info in enumerate_plugins() {
            let plugin = load(&info.id, 48000.0, 512).unwrap();
            let side = plugin.side_input();
            assert_eq!(side.is_some(), info.id == "builtin:math", "{}", info.id);
        }
        assert_eq!(load("builtin:math", 48000.0, 512).unwrap().side_input(), Some(crate::plugin::SideInput { main: "A", side: "B" }));
    }

    #[test]
    fn the_filters_parameters_say_their_steps() {
        // The cutoff is a position on an exponential scale, not a frequency: no unit.
        assert_eq!(
            metadata("builtin:filter"),
            [row("cutoff", None, &[], 0), row("resonance", None, &[], 0), row("type", None, &[], 6), row("drive", None, &[], 0)]
        );
    }

    #[test]
    fn the_delays_parameters_say_their_unit_and_their_section() {
        assert_eq!(
            metadata("builtin:delay"),
            [
                row("time", Some("s"), &[], 0),
                row("feedback", None, &[], 0),
                row("tone", None, &[], 0),
                row("pingpong", None, &[], 0),
                row("mod_rate", Some("Hz"), &["Modulation"], 0),
                row("mod_depth", Some("ms"), &["Modulation"], 0),
            ]
        );
    }

    #[test]
    fn the_reverbs_parameters_say_their_unit_and_their_section() {
        assert_eq!(
            metadata("builtin:reverb"),
            [
                row("decay", None, &[], 0),
                row("size", None, &[], 0),
                row("damping", None, &[], 0),
                row("predelay", Some("s"), &[], 0),
                row("diffusion", None, &[], 0),
                row("mod_rate", Some("Hz"), &["Modulation"], 0),
                row("mod_depth", None, &["Modulation"], 0),
                row("width", None, &[], 0),
                row("lowcut", Some("Hz"), &[], 0),
                row("level", None, &[], 0),
            ]
        );
    }

    #[test]
    fn a_parameter_with_value_labels_has_a_step_for_each_gap_between_them() {
        for info in enumerate_plugins() {
            let plugin = load(&info.id, 48000.0, 512).unwrap();
            for p in plugin.parameters() {
                let what = format!("{}: {}", info.id, p.name);
                match &p.labels {
                    Some(labels) => {
                        assert_eq!(p.step_count as usize, labels.len() - 1, "{what}");
                        assert_eq!((p.min, p.max), (0.0, p.step_count as f32), "{what}");
                    }
                    None => assert_eq!(p.step_count, 0, "{what}: continuous"),
                }
                assert_ne!(p.unit.as_deref(), Some(""), "{what}: no unit is None, not an empty string");
                assert!(p.group.iter().all(|g| !g.is_empty()), "{what}: a section has a name");
            }
        }
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
    fn enumerate_lists_one_oscillator_and_the_audio_input_as_the_instruments() {
        let plugins = enumerate_plugins();
        let instruments: Vec<_> = plugins.iter().filter(|p| p.is_instrument).map(|p| p.id.as_str()).collect();
        assert_eq!(instruments, ["builtin:osc", "builtin:input"], "a single oscillator (the old ids are aliases that load, not plugins), and the audio input (step 99)");
    }
}
