#![allow(clippy::collapsible_if)]

mod audio;
mod chord;
mod cli;
mod config;
mod enumerate;
mod held_notes;
mod midi;
mod piano;
mod piano_filter;
mod plugin;
mod scale;
mod session;
mod tty;
mod tui;

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use clap::Parser;
use cli::{Cli, Command, EnumerateTarget};

/// Convert a MIDI note number to a human-readable name (e.g. 60 → "C4").
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = (note / 12) as i8 - 1;
    let name = NAMES[(note % 12) as usize];
    format!("{name}{octave}")
}
use crossterm::event::{
    self, Event, KeyCode, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load application config (extra plugin paths, etc.)
    if let Ok(config_dir) = dirs_config() {
        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(text) => match toml::from_str::<config::Config>(&text) {
                    Ok(cfg) => config::init(cfg),
                    Err(e) => eprintln!("Warning: failed to parse {}: {e}", config_path.display()),
                },
                Err(e) => eprintln!("Warning: failed to read {}: {e}", config_path.display()),
            }
        }
    }

    // Set LV2_PATH for extra LV2 search directories (before any LV2 world is created)
    let extra_lv2 = config::extra_lv2_paths();
    if !extra_lv2.is_empty() {
        let current = std::env::var("LV2_PATH").unwrap_or_default();
        let extra: String = extra_lv2
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        let new = if current.is_empty() {
            extra
        } else {
            format!("{extra}:{current}")
        };
        // Safety: called before any threads are spawned.
        unsafe {
            std::env::set_var("LV2_PATH", new);
        }
    }

    let Cli {
        session,
        audio_device,
        midi_device,
        buffer_size,
        sample_rate,
        command,
    } = cli;

    match command {
        None => run_session(session, audio_device, midi_device, buffer_size, sample_rate, true),
        Some(Command::Enumerate(target)) => {
            env_logger::init();
            match target {
                EnumerateTarget::Midi => enumerate::midi(),
                EnumerateTarget::Audio => enumerate::audio(),
                EnumerateTarget::Plugins => enumerate::plugins(),
                EnumerateTarget::Builtins => enumerate::builtins(),
            }
        }
        Some(Command::Describe { plugin: source }) => {
            env_logger::init();
            let p = plugin::load(&source, 48000.0, 512, &plugin::Runtime::default())?;
            println!("{}", p.name());
            println!(
                "  Type:          {}",
                if p.is_instrument() {
                    "instrument"
                } else {
                    "effect"
                }
            );
            println!("  Audio outputs: {}", p.audio_output_count());
            let params = p.parameters();
            println!("  Parameters:    {}", params.len());
            for param in &params {
                if let Some(labels) = &param.labels {
                    println!(
                        "    [{}] {} ({}, default={})",
                        param.index,
                        param.name,
                        labels.join("|"),
                        labels
                            .get(param.default.round() as usize)
                            .map(|s| s.as_str())
                            .unwrap_or("?"),
                    );
                } else {
                    println!(
                        "    [{}] {} (min={}, max={}, default={})",
                        param.index, param.name, param.min, param.max, param.default
                    );
                }
            }
            let presets = p.presets();
            if presets.is_empty() {
                println!("  Presets:       (none)");
            } else {
                println!("  Presets:       {}", presets.len());
                for preset in &presets {
                    println!("    {} ({})", preset.name, preset.id);
                }
            }
            Ok(())
        }
        Some(Command::Play { session: play_session }) => {
            run_session(play_session, audio_device, midi_device, buffer_size, sample_rate, false)
        }
    }
}

/// Custom logger that writes to stderr with \r\n line endings for raw mode.
struct RawModeLogger;

impl log::Log for RawModeLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            let secs = now.as_secs() % 86400; // time of day
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            let ms = now.subsec_millis();
            let _ = write!(
                std::io::stderr(),
                "[{h:02}:{m:02}:{s:02}.{ms:03} {}] {}\r\n",
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

static RAW_MODE_LOGGER: RawModeLogger = RawModeLogger;

/// Create a default in-memory session config and a path for saving later.
/// The file is NOT written to disk — only created on Ctrl+S.
fn default_session() -> anyhow::Result<(session::SessionConfig, std::path::PathBuf)> {
    let dir = dirs_config_sessions()?;

    // Generate a short unique ID from the current timestamp.
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let id = format!("{:x}", ts.as_millis());
    let path = dir.join(format!("session-{id}.toml"));

    let config = session::SessionConfig {
        instruments: vec![session::InstrumentSlotConfig {
            range: None,
            transpose: 0,
            instrument: Some(session::PluginConfig {
                plugin: "builtin:osc".into(),
                preset: None,
                volume: 1.0,
                pitch_bend_range: 2.0,
                remap: Default::default(),
                params: Default::default(),
                modulators: vec![],
            }),
            effects: vec![],
            pattern: None,
            group: None,
        }],
        groups: vec![],
        piano: None,
    };

    log::info!("New session (will save to {} on Ctrl+S)", path.display());
    Ok((config, path))
}

fn dirs_config_sessions() -> anyhow::Result<std::path::PathBuf> {
    let config = dirs_config()?;
    Ok(config.join("sessions"))
}

fn dirs_config() -> anyhow::Result<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Path::new(&home).join(".config").join("tang"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
            return Ok(Path::new(&config).join("tang"));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Path::new(&home).join(".config").join("tang"));
        }
    }
    anyhow::bail!("could not determine config directory")
}

/// Load modulators from a plugin's config and send the commands to the audio thread.
/// Build the lane's modulators (lane-scoped) and send them to the audio
/// thread. Targets address plugins by chain slot (0 = instrument, 1..N =
/// effect). `chain_params[slot]` holds that plugin's parameters for name→index
/// resolution.
///
/// Migration: modulators come from the instrument's config (slot defaults to
/// the target's `slot`, i.e. instrument unless set) and — for older sessions —
/// from each effect's config (those target their own effect, so their slot is
/// forced to that effect, and their cross-mod sibling indices are offset by
/// where the effect's group lands in the merged lane list).
fn load_lane_modulators(
    inst_config: &session::InstrumentSlotConfig,
    chain_params: &[Vec<plugin::ParameterInfo>],
    inst_idx: usize,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
) -> anyhow::Result<Vec<tui::LoadedModulator>> {
    // Gather modulator groups with their default slot (instrument = 0, each
    // effect = its slot) and their base index in the merged lane list.
    let mut groups: Vec<(&[session::ModulatorConfig], usize)> = Vec::new();
    if let Some(ref inst) = inst_config.instrument {
        groups.push((&inst.modulators, 0));
    }
    for (i, fx) in inst_config.effects.iter().enumerate() {
        groups.push((&fx.modulators, i + 1));
    }
    let mut bases = Vec::with_capacity(groups.len());
    let mut acc = 0usize;
    for (configs, _) in &groups {
        bases.push(acc);
        acc += configs.len();
    }

    let mut loaded = Vec::new();
    let mut lane_index = 0usize;
    for (gi, (mod_configs, default_slot)) in groups.iter().enumerate() {
        let group_base = bases[gi];
        let is_instrument_group = *default_slot == 0;
        for mod_config in mod_configs.iter() {
            let mod_idx = lane_index;
        let (source, loaded_source, desc) = build_mod_source(mod_config);

        cmd_tx
            .send(plugin::chain::GraphCommand::InsertModulator {
                inst: inst_idx,
                index: mod_idx,
                source,
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        let mut loaded_targets: Vec<tui::LoadedModTarget> = Vec::new();
        for target_config in &mod_config.targets {
            // Determine the target kind and associated metadata.
            let (kind, slot, label, param_min, param_max, base_value) =
                if let Some(ref param_name) = target_config.param {
                    // Plugin parameter target. Instrument-group modulators
                    // honor the target's `slot` (instrument unless set);
                    // migrated effect modulators target their own effect.
                    let slot = if is_instrument_group { target_config.slot } else { *default_slot };
                    let param_info = chain_params
                        .get(slot)
                        .and_then(|ps| ps.iter().find(|p| p.name == *param_name));
                    let param_info = match param_info {
                        Some(p) => p,
                        None => {
                            log::warn!(
                                "Modulator target param '{param_name}' not found in slot {slot}",
                            );
                            continue;
                        }
                    };
                    (
                        plugin::chain::ModTargetKind::PluginParam { slot, param_index: param_info.index },
                        slot,
                        param_info.name.clone(),
                        param_info.min,
                        param_info.max,
                        param_info.default,
                    )
                } else if let Some(mi) = target_config.mod_rate {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     0, format!("Mod {mi} rate"), 0.01, 50.0, 1.0)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let mi = pair.first().copied().unwrap_or(0) + group_base;
                    let ti = pair.get(1).copied().unwrap_or(0);
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     0, format!("Mod {mi} depth {ti}"), 0.0, 1.0, 0.5)
                } else if let Some(mi) = target_config.mod_attack {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     0, format!("Mod {mi} attack"), 0.001, 10.0, 0.01)
                } else if let Some(mi) = target_config.mod_decay {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     0, format!("Mod {mi} decay"), 0.001, 10.0, 0.3)
                } else if let Some(mi) = target_config.mod_sustain {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     0, format!("Mod {mi} sustain"), 0.0, 1.0, 0.7)
                } else if let Some(mi) = target_config.mod_release {
                    let mi = mi + group_base;
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     0, format!("Mod {mi} release"), 0.001, 10.0, 0.5)
                } else {
                    log::warn!("Modulator target has no param or mod_* field, skipping");
                    continue;
                };

            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
            };

            cmd_tx
                .send(plugin::chain::GraphCommand::AddModTarget {
                    inst: inst_idx,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(tui::LoadedModTarget {
                slot,
                param_name: label.clone(),
                kind,
                depth: target_config.depth as f32,
                param_min,
                param_max,
            });

            log::info!("Modulator {mod_idx} target: '{label}' (slot {slot}) depth={}", target_config.depth);
        }

        loaded.push(tui::LoadedModulator {
            source: loaded_source,
            targets: loaded_targets,
        });

        log::info!("Loaded lane modulator {mod_idx} for inst={inst_idx}: {desc}");
        lane_index += 1;
        }
    }
    Ok(loaded)
}

/// Build the audio-thread + TUI source representations and a description for a
/// modulator config. Shared by the lane and group modulator loaders.
fn build_mod_source(
    mod_config: &session::ModulatorConfig,
) -> (plugin::chain::ModSource, tui::LoadedModSource, String) {
    match mod_config.mod_type.as_str() {
        "envelope" => {
            let source = plugin::chain::ModSource::Envelope {
                attack: mod_config.attack as f32,
                decay: mod_config.decay as f32,
                sustain: mod_config.sustain as f32,
                release: mod_config.release as f32,
                state: plugin::chain::EnvState::Idle,
                level: 0.0,
                notes_held: 0,
            };
            let loaded_source = tui::LoadedModSource::Envelope {
                attack: mod_config.attack as f32,
                decay: mod_config.decay as f32,
                sustain: mod_config.sustain as f32,
                release: mod_config.release as f32,
            };
            (source, loaded_source, "ADSR envelope".to_string())
        }
        _ => {
            // Default: LFO.
            let waveform = plugin::chain::LfoWaveform::from_str(&mod_config.waveform)
                .unwrap_or_else(|| {
                    log::warn!(
                        "Unknown waveform '{}', defaulting to sine",
                        mod_config.waveform
                    );
                    plugin::chain::LfoWaveform::Sine
                });
            let source = plugin::chain::ModSource::Lfo {
                waveform,
                rate: mod_config.rate as f32,
                phase: 0.0,
            };
            let loaded_source = tui::LoadedModSource::Lfo {
                waveform,
                rate: mod_config.rate as f32,
            };
            let desc = format!("{} {:.1}Hz", waveform.name(), mod_config.rate);
            (source, loaded_source, desc)
        }
    }
}

/// Build a group's modulators and send them to the audio thread. Targets
/// address a member instrument's chain (`member` + `slot`), one of the group's
/// own bus effects (`bus`), or a sibling group modulator (`mod_*`).
/// `member_chains[ordinal][slot]` holds each member's plugin params (slot 0 =
/// instrument); `bus_params[i]` holds bus effect `i`'s params.
/// (base_value, min, max) for a member modulator's field, used to seed a
/// group→member-modulator cross-mod target on load. Falls back to per-field
/// defaults if the member modulator's type doesn't match the field.
fn member_mod_field_info(mm: &tui::LoadedModulator, field: plugin::chain::CrossModField) -> (f32, f32, f32) {
    use plugin::chain::CrossModField;
    use tui::LoadedModSource;
    match field {
        CrossModField::Rate => {
            let v = if let LoadedModSource::Lfo { rate, .. } = &mm.source { *rate } else { 1.0 };
            (v, 0.01, 50.0)
        }
        CrossModField::Attack => {
            let v = if let LoadedModSource::Envelope { attack, .. } = &mm.source { *attack } else { 0.01 };
            (v, 0.001, 10.0)
        }
        CrossModField::Decay => {
            let v = if let LoadedModSource::Envelope { decay, .. } = &mm.source { *decay } else { 0.3 };
            (v, 0.001, 10.0)
        }
        CrossModField::Sustain => {
            let v = if let LoadedModSource::Envelope { sustain, .. } = &mm.source { *sustain } else { 0.7 };
            (v, 0.0, 1.0)
        }
        CrossModField::Release => {
            let v = if let LoadedModSource::Envelope { release, .. } = &mm.source { *release } else { 0.5 };
            (v, 0.001, 10.0)
        }
        CrossModField::Depth(ti) => {
            let v = mm.targets.get(ti).map(|t| t.depth).unwrap_or(0.5);
            (v, 0.0, 1.0)
        }
    }
}

fn load_group_modulators(
    group_config: &session::GroupConfig,
    g_idx: usize,
    member_chains: &[Vec<Vec<plugin::ParameterInfo>>],
    member_mods: &[&[tui::LoadedModulator]],
    bus_params: &[Vec<plugin::ParameterInfo>],
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
) -> anyhow::Result<Vec<tui::LoadedModulator>> {
    let mut loaded = Vec::new();
    for (mod_idx, mod_config) in group_config.modulators.iter().enumerate() {
        let (source, loaded_source, desc) = build_mod_source(mod_config);
        cmd_tx
            .send(plugin::chain::GraphCommand::InsertGroupModulator {
                group: g_idx,
                index: mod_idx,
                source,
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        let mut loaded_targets: Vec<tui::LoadedModTarget> = Vec::new();
        for target_config in &mod_config.targets {
            let (kind, slot, label, param_min, param_max, base_value) =
                if let Some(member) = target_config.member {
                    if let Some(ref param_name) = target_config.param {
                        // Member instrument plugin parameter.
                        let slot = target_config.slot;
                        let param_info = member_chains
                            .get(member)
                            .and_then(|chain| chain.get(slot))
                            .and_then(|ps| ps.iter().find(|p| p.name == *param_name));
                        let Some(param_info) = param_info else {
                            log::warn!(
                                "Group modulator target param '{param_name}' not found (member {member}, slot {slot})"
                            );
                            continue;
                        };
                        (
                            plugin::chain::ModTargetKind::GroupMember {
                                member,
                                slot,
                                param_index: param_info.index,
                            },
                            slot,
                            param_info.name.clone(),
                            param_info.min,
                            param_info.max,
                            param_info.default,
                        )
                    } else {
                        // Cross-mod a member instrument's modulator (member + mod_*).
                        use plugin::chain::CrossModField;
                        let (field, mi) = if let Some(mi) = target_config.mod_rate {
                            (CrossModField::Rate, mi)
                        } else if let Some(mi) = target_config.mod_attack {
                            (CrossModField::Attack, mi)
                        } else if let Some(mi) = target_config.mod_decay {
                            (CrossModField::Decay, mi)
                        } else if let Some(mi) = target_config.mod_sustain {
                            (CrossModField::Sustain, mi)
                        } else if let Some(mi) = target_config.mod_release {
                            (CrossModField::Release, mi)
                        } else if let Some(ref pair) = target_config.mod_depth {
                            let mi = pair.first().copied().unwrap_or(0);
                            let ti = pair.get(1).copied().unwrap_or(0);
                            (CrossModField::Depth(ti), mi)
                        } else {
                            log::warn!("Group modulator member target has no param or mod_* field, skipping");
                            continue;
                        };
                        let mm = member_mods.get(member).and_then(|mods| mods.get(mi));
                        let Some(mm) = mm else {
                            log::warn!("Group member-mod target: member {member} has no modulator {mi}");
                            continue;
                        };
                        let (base, min, max) = member_mod_field_info(mm, field);
                        let field_name = match field {
                            CrossModField::Rate => "rate".to_string(),
                            CrossModField::Attack => "attack".to_string(),
                            CrossModField::Decay => "decay".to_string(),
                            CrossModField::Sustain => "sustain".to_string(),
                            CrossModField::Release => "release".to_string(),
                            CrossModField::Depth(ti) => format!("depth {ti}"),
                        };
                        (
                            plugin::chain::ModTargetKind::GroupMemberMod { member, mod_index: mi, field },
                            0,
                            format!("M{member} Mod{mi} {field_name}"),
                            min,
                            max,
                            base,
                        )
                    }
                } else if let Some(bus) = target_config.bus {
                    let Some(ref param_name) = target_config.param else {
                        log::warn!("Group modulator bus target without param, skipping");
                        continue;
                    };
                    let param_info = bus_params
                        .get(bus)
                        .and_then(|ps| ps.iter().find(|p| p.name == *param_name));
                    let Some(param_info) = param_info else {
                        log::warn!(
                            "Group modulator target param '{param_name}' not found (bus {bus})"
                        );
                        continue;
                    };
                    (
                        plugin::chain::ModTargetKind::GroupBus {
                            effect_index: bus,
                            param_index: param_info.index,
                        },
                        0,
                        param_info.name.clone(),
                        param_info.min,
                        param_info.max,
                        param_info.default,
                    )
                } else if let Some(mi) = target_config.mod_rate {
                    (plugin::chain::ModTargetKind::ModulatorRate { mod_index: mi },
                     0, format!("Mod {mi} rate"), 0.01, 50.0, 1.0)
                } else if let Some(ref pair) = target_config.mod_depth {
                    let mi = pair.first().copied().unwrap_or(0);
                    let ti = pair.get(1).copied().unwrap_or(0);
                    (plugin::chain::ModTargetKind::ModulatorDepth { mod_index: mi, target_index: ti },
                     0, format!("Mod {mi} depth {ti}"), 0.0, 1.0, 0.5)
                } else if let Some(mi) = target_config.mod_attack {
                    (plugin::chain::ModTargetKind::ModulatorAttack { mod_index: mi },
                     0, format!("Mod {mi} attack"), 0.001, 10.0, 0.01)
                } else if let Some(mi) = target_config.mod_decay {
                    (plugin::chain::ModTargetKind::ModulatorDecay { mod_index: mi },
                     0, format!("Mod {mi} decay"), 0.001, 10.0, 0.3)
                } else if let Some(mi) = target_config.mod_sustain {
                    (plugin::chain::ModTargetKind::ModulatorSustain { mod_index: mi },
                     0, format!("Mod {mi} sustain"), 0.0, 1.0, 0.7)
                } else if let Some(mi) = target_config.mod_release {
                    (plugin::chain::ModTargetKind::ModulatorRelease { mod_index: mi },
                     0, format!("Mod {mi} release"), 0.001, 10.0, 0.5)
                } else {
                    log::warn!("Group modulator target has no param/member/bus/mod_* field, skipping");
                    continue;
                };

            let target = plugin::chain::ModTarget {
                kind: kind.clone(),
                depth: target_config.depth as f32,
                base_value,
                param_min,
                param_max,
            };
            cmd_tx
                .send(plugin::chain::GraphCommand::AddGroupModTarget {
                    group: g_idx,
                    mod_index: mod_idx,
                    target,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            loaded_targets.push(tui::LoadedModTarget {
                slot,
                param_name: label,
                kind,
                depth: target_config.depth as f32,
                param_min,
                param_max,
            });
        }

        loaded.push(tui::LoadedModulator {
            source: loaded_source,
            targets: loaded_targets,
        });
        log::info!("Loaded group modulator {mod_idx} for group={g_idx}: {desc}");
    }
    Ok(loaded)
}

fn run_session(
    session: Option<String>,
    audio_device: Option<String>,
    midi_device: Option<String>,
    buffer_size: u32,
    sample_rate: u32,
    use_tui: bool,
) -> anyhow::Result<()> {
    // Set up raw mode logger early so plugin loading messages are visible
    log::set_logger(&RAW_MODE_LOGGER).ok();
    log::set_max_level(
        std::env::var("RUST_LOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(log::LevelFilter::Info),
    );

    let sample_rate_f = sample_rate as f32;
    let max_block_size = buffer_size as usize;

    // Load or create session config.
    let (config, source) = match session {
        Some(s) => {
            let config = session::load(&s)?;
            (config, s)
        }
        None => {
            let (config, path) = default_session()?;
            (config, path.to_string_lossy().to_string())
        }
    };

    let session_dir = Path::new(&source).parent().unwrap_or_else(|| Path::new("."));

    // Splash screen while plugins load (TUI mode only). Plugin loads can take
    // seconds for heavyweight instruments; the splash thread owns the terminal
    // and animates while this thread loads. Dropped (= terminal restored)
    // before tui::run sets up its own terminal — also on the error path.
    let total_slots: usize = config
        .instruments
        .iter()
        .map(|i| i.instrument.is_some() as usize + i.effects.len())
        .sum();
    let splash = if use_tui {
        match tui::splash::Splash::start(total_slots) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("Splash screen unavailable: {e}");
                None
            }
        }
    } else {
        None
    };
    // While the splash owns the screen, suppress logging if stderr is the
    // terminal (it would corrupt the alternate screen). Logging to a
    // redirected stderr (`tang 2> debug.log`) is unaffected.
    let prev_log_level = log::max_level();
    if splash.is_some() && std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        log::set_max_level(log::LevelFilter::Off);
    }
    let mut loaded_slots = 0usize;

    // Create shared LV2 world (scans system plugins once, reused for all LV2 loads)
    #[cfg(feature = "lv2")]
    let runtime = plugin::Runtime::with_lv2(max_block_size);
    #[cfg(not(feature = "lv2"))]
    let runtime = plugin::Runtime::default();

    // Create channels
    let (midi_tx, midi_rx) = crossbeam_channel::bounded::<audio::MidiEvent>(1024);
    let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<plugin::chain::GraphCommand>(64);
    let (return_tx, return_rx) = crossbeam_channel::bounded::<Box<dyn plugin::Plugin>>(16);

    // Shared "currently held notes" state — populated by both the hardware MIDI
    // thread and the virtual piano, read by the TUI Piano tab.
    let held = std::sync::Arc::new(held_notes::HeldNotes::new());

    // Shared piano-tab filter: current scale + Highlight/Locked mode. Read by
    // both senders to drop off-scale notes when locked; written by the TUI.
    let piano_initial_scale = config
        .piano
        .as_ref()
        .and_then(|p| p.scale.as_deref().and_then(scale::ScaleSetting::parse))
        .unwrap_or_default();
    let piano_initial_mode = if config.piano.as_ref().is_some_and(|p| p.locked) {
        piano_filter::PianoMode::Locked
    } else {
        piano_filter::PianoMode::Highlight
    };
    let piano_filter = std::sync::Arc::new(piano_filter::PianoFilter::new(
        piano_initial_scale,
        piano_initial_mode,
    ));

    // Create empty audio graph (outputs silence until instruments are added)
    let num_channels = 2; // stereo — see CLAUDE.md design decision
    let mut graph = plugin::chain::AudioGraph::new(num_channels, cmd_rx, return_tx);

    // Pattern recording completion channel
    let (pattern_tx, pattern_rx) = crossbeam_channel::bounded::<plugin::chain::PatternNotification>(64);
    graph.set_pattern_tx(pattern_tx.clone());
    // Give the audio thread the live piano scale for in-key pattern transposition.
    graph.set_piano_filter(piano_filter.clone());

    // Start MIDI input
    if let Some(sp) = &splash {
        sp.status("Opening MIDI inputs…");
    }
    let mut midi_mgr = midi::MidiManager::new(midi_tx.clone(), midi_device, held.clone(), piano_filter.clone());
    midi_mgr.open_ports()?;
    log::info!("MIDI inputs connected: {}", midi_mgr.connection_count());

    // Build audio engine (not playing yet — will start after initial commands are queued)
    if let Some(sp) = &splash {
        sp.status("Starting audio engine…");
    }
    let engine = audio::AudioEngine::build(
        graph,
        midi_rx,
        audio_device.as_deref(),
        sample_rate,
        buffer_size,
    )?;

    // Build TUI metadata while loading plugins into the graph.
    let mut loaded_instruments: Vec<tui::LoadedInstrument> = Vec::new();

    // Set up submix groups first, so instruments can reference them by index.
    let mut loaded_groups: Vec<tui::LoadedGroup> = Vec::new();
    for (g_idx, group_config) in config.groups.iter().enumerate() {
        cmd_tx
            .send(plugin::chain::GraphCommand::AddGroup)
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;
        if (group_config.volume - 1.0).abs() > f32::EPSILON {
            cmd_tx
                .send(plugin::chain::GraphCommand::SetGroupVolume {
                    group: g_idx,
                    value: group_config.volume,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;
        }
        if group_config.pan.abs() > f32::EPSILON {
            cmd_tx
                .send(plugin::chain::GraphCommand::SetGroupPan {
                    group: g_idx,
                    value: group_config.pan,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;
        }
        let mut group_effects: Vec<tui::LoadedPlugin> = Vec::new();
        for (fx_idx, effect_config) in group_config.effects.iter().enumerate() {
            let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
            let mut effect = plugin::load(&effect_source, sample_rate_f, max_block_size, &runtime)?;
            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let effect_presets = effect.presets();
            let effect_params = effect.parameters();
            let effect_baselines: Vec<f32> = effect_params
                .iter()
                .map(|p| effect.get_parameter(p.index).unwrap_or(p.default))
                .collect();
            let effect_name = effect.name().to_string();
            cmd_tx
                .send(plugin::chain::GraphCommand::InsertGroupEffect {
                    group: g_idx,
                    index: fx_idx,
                    effect,
                    mix: effect_config.mix,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;
            let mut fx_values = effect_baselines.clone();
            for (name, &value) in &effect_config.params {
                if let Some(info) = effect_params.iter().find(|p| p.name == *name) {
                    let _ = cmd_tx.send(plugin::chain::GraphCommand::SetGroupParameter {
                        group: g_idx,
                        index: fx_idx,
                        param_index: info.index,
                        value: value as f32,
                    });
                    if let Some(v) = fx_values.get_mut(info.index as usize) {
                        *v = value as f32;
                    }
                }
            }
            group_effects.push(tui::LoadedPlugin {
                name: effect_name,
                id: effect_source,
                is_instrument: false,
                params: effect_params,
                param_defaults: effect_baselines,
                param_values: fx_values,
                presets: effect_presets,
                current_preset: effect_config.preset.clone(),
                mix: effect_config.mix as f32,
            });
        }
        loaded_groups.push(tui::LoadedGroup {
            name: group_config.name.clone(),
            volume: group_config.volume,
            pan: group_config.pan,
            effects: group_effects,
            // Group modulators are loaded after the instruments, once member
            // chains are known (so member-target param names resolve).
            modulators: Vec::new(),
        });
    }

    // Set up the graph structure: add instrument lanes.
    for (inst_idx, inst_config) in config.instruments.iter().enumerate() {
        cmd_tx
            .send(plugin::chain::GraphCommand::AddInstrument {
                range: inst_config.range,
            })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;

        // Assign group membership (groups were created above).
        if inst_config.group.is_some() {
            cmd_tx
                .send(plugin::chain::GraphCommand::SetLaneGroup {
                    inst: inst_idx,
                    group: inst_config.group,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;
        }

        // Load instrument (if present)
        let loaded_instrument = if let Some(plug_config) = &inst_config.instrument {
            let instrument_source =
                session::resolve_plugin_path(&plug_config.plugin, session_dir);
            if let Some(sp) = &splash {
                sp.status(format!(
                    "Loading {}…",
                    tui::splash::display_name(&plug_config.plugin)
                ));
            }
            let mut instrument =
                plugin::load(&instrument_source, sample_rate_f, max_block_size, &runtime)?;
            loaded_slots += 1;
            if let Some(sp) = &splash {
                sp.progress(loaded_slots);
            }
            log::info!(
                "Loaded instrument for inst={}: {}",
                inst_idx,
                instrument.name()
            );

            if let Some(ref preset_name) = plug_config.preset {
                session::apply_preset(&mut instrument, preset_name);
            }
            let inst_presets = instrument.presets();
            let inst_current_preset = plug_config.preset.clone();

            // Snapshot post-preset parameter values; these become the baseline
            // the TUI compares against to decide which params are user-edited
            // and need to be saved.
            let inst_params = instrument.parameters();
            let inst_baselines: Vec<f32> = inst_params
                .iter()
                .map(|p| instrument.get_parameter(p.index).unwrap_or(p.default))
                .collect();

            // Build note remapper if configured
            let remapper = if plug_config.remap.is_empty() {
                None
            } else {
                let r = plugin::chain::NoteRemapper::from_config(
                    &plug_config.remap,
                    plug_config.pitch_bend_range,
                )?;
                log::info!(
                    "Note remapper: {} entries, pitch_bend_range=±{}",
                    plug_config.remap.len(),
                    plug_config.pitch_bend_range,
                );
                Some(r)
            };

            let inst_name = instrument.name().to_string();
            let inst_buf = (0..instrument.audio_output_count())
                .map(|_| Vec::new())
                .collect();
            cmd_tx
                .send(plugin::chain::GraphCommand::SwapInstrument {
                    inst: inst_idx,
                    instrument,
                    inst_buf,
                    remapper,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            // Set volume if not default
            if (plug_config.volume - 1.0).abs() > f64::EPSILON {
                cmd_tx
                    .send(plugin::chain::GraphCommand::SetVolume {
                        inst: inst_idx,
                        value: plug_config.volume as f32,
                    })
                    .map_err(|_| anyhow::anyhow!("command channel closed"))?;
            }

            // Send instrument parameter overrides. Start from the post-preset
            // baseline so values reflect the active preset's actual settings.
            let mut inst_values: Vec<f32> = inst_baselines.clone();
            for (name, &value) in &plug_config.params {
                if let Some(info) = inst_params.iter().find(|p| p.name == *name) {
                    cmd_tx
                        .send(plugin::chain::GraphCommand::SetParameter {
                            inst: inst_idx,
                            slot: 0,
                            param_index: info.index,
                            value: value as f32,
                        })
                        .map_err(|_| anyhow::anyhow!("command channel closed"))?;
                    if let Some(v) = inst_values.get_mut(info.index as usize) {
                        *v = value as f32;
                    }
                    log::info!("Set instrument '{}' = {}", name, value);
                } else {
                    log::warn!(
                        "Unknown instrument parameter '{}' (available: {})",
                        name,
                        inst_params
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }

            Some(tui::LoadedPlugin {
                name: inst_name,
                id: instrument_source,
                is_instrument: true,
                params: inst_params,
                param_defaults: inst_baselines,
                param_values: inst_values,
                presets: inst_presets,
                current_preset: inst_current_preset,
                mix: 1.0,
            })
        } else {
            None
        };

        // Load effects
        let mut loaded_effects: Vec<tui::LoadedPlugin> = Vec::new();
        for (fx_idx, effect_config) in inst_config.effects.iter().enumerate() {
            let effect_source =
                session::resolve_plugin_path(&effect_config.plugin, session_dir);
            if let Some(sp) = &splash {
                sp.status(format!(
                    "Loading {}…",
                    tui::splash::display_name(&effect_config.plugin)
                ));
            }
            let mut effect =
                plugin::load(&effect_source, sample_rate_f, max_block_size, &runtime)?;
            loaded_slots += 1;
            if let Some(sp) = &splash {
                sp.progress(loaded_slots);
            }
            log::info!(
                "Loaded effect for inst={} fx={}: {}",
                inst_idx,
                fx_idx,
                effect.name()
            );

            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let effect_presets = effect.presets();
            let effect_current_preset = effect_config.preset.clone();

            // Snapshot post-preset values as the save-filter baseline.
            let effect_params = effect.parameters();
            let effect_baselines: Vec<f32> = effect_params
                .iter()
                .map(|p| effect.get_parameter(p.index).unwrap_or(p.default))
                .collect();
            let effect_name = effect.name().to_string();

            cmd_tx
                .send(plugin::chain::GraphCommand::InsertEffect {
                    inst: inst_idx,
                    index: fx_idx,
                    effect,
                    mix: effect_config.mix,
                })
                .map_err(|_| anyhow::anyhow!("command channel closed"))?;

            // Send parameter overrides for this effect (slot = fx_idx + 1).
            // Start from the post-preset baseline.
            let mut fx_values: Vec<f32> = effect_baselines.clone();
            for (name, &value) in &effect_config.params {
                if let Some(info) = effect_params.iter().find(|p| p.name == *name) {
                    cmd_tx
                        .send(plugin::chain::GraphCommand::SetParameter {
                            inst: inst_idx,
                            slot: fx_idx + 1,
                            param_index: info.index,
                            value: value as f32,
                        })
                        .map_err(|_| anyhow::anyhow!("command channel closed"))?;
                    if let Some(v) = fx_values.get_mut(info.index as usize) {
                        *v = value as f32;
                    }
                    log::info!("Set effect {} '{}' = {}", fx_idx, name, value);
                } else {
                    log::warn!(
                        "Unknown parameter '{}' for effect {} (available: {})",
                        name,
                        fx_idx,
                        effect_params
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
            }

            loaded_effects.push(tui::LoadedPlugin {
                name: effect_name,
                id: effect_source,
                is_instrument: false,
                params: effect_params,
                param_defaults: effect_baselines,
                param_values: fx_values,
                presets: effect_presets,
                current_preset: effect_current_preset,
                mix: effect_config.mix as f32,
            });
        }

        // Build the lane's modulators now that every plugin (and its params)
        // is loaded, so targets can resolve against any slot in the chain.
        let chain_params: Vec<Vec<plugin::ParameterInfo>> =
            std::iter::once(loaded_instrument.as_ref().map(|p| p.params.clone()).unwrap_or_default())
                .chain(loaded_effects.iter().map(|fx| fx.params.clone()))
                .collect();
        let loaded_modulators = load_lane_modulators(inst_config, &chain_params, inst_idx, &cmd_tx)?;

        // Apply instrument transpose (the TUI re-sends this on startup, but
        // play mode has no other path that applies it).
        if inst_config.transpose != 0 {
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetTranspose {
                inst: inst_idx,
                semitones: inst_config.transpose,
            });
        }

        // Load pattern if configured.
        let loaded_pattern = inst_config.pattern.as_ref().map(|p| {
            // Build Pattern and send to audio graph.
            let pattern_events: Vec<crate::plugin::chain::PatternEvent> = p.events.iter().map(|&(frame, status, note, vel)| {
                crate::plugin::chain::PatternEvent {
                    frame,
                    status,
                    note,
                    velocity: vel,
                }
            }).collect();
            let beats_per_sec = p.bpm / 60.0;
            let length_samples = (p.length_beats / beats_per_sec * sample_rate_f) as u64;
            let pattern = crate::plugin::chain::Pattern {
                events: pattern_events,
                length_samples,
            };
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPattern {
                inst: inst_idx,
                pattern,
                base_note: p.base_note,
                in_key: p.in_key,
            });
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetGlobalBpm { bpm: p.bpm });
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLength {
                inst: inst_idx,
                beats: p.length_beats,
            });
            if p.enabled {
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternEnabled {
                    inst: inst_idx,
                    enabled: true,
                });
            }
            if !p.looping {
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLooping {
                    inst: inst_idx,
                    looping: false,
                });
            }
            tui::LoadedPattern {
                bpm: p.bpm,
                length_beats: p.length_beats,
                looping: p.looping,
                base_note: p.base_note,
                events: p.events.clone(),
                enabled: p.enabled,
                in_key: p.in_key,
            }
        });

        let (pitch_bend_range, remap) = inst_config
            .instrument
            .as_ref()
            .map(|p| (p.pitch_bend_range, p.remap.clone()))
            .unwrap_or((2.0, Default::default()));
        let volume = inst_config
            .instrument
            .as_ref()
            .map_or(1.0, |p| p.volume as f32);
        loaded_instruments.push(tui::LoadedInstrument {
            range: inst_config.range,
            transpose: inst_config.transpose,
            volume,
            instrument: loaded_instrument,
            effects: loaded_effects,
            modulators: loaded_modulators,
            pattern: loaded_pattern,
            pitch_bend_range,
            remap,
            group: inst_config.group,
        });
    }

    // Now that all member instruments are loaded, build each group's
    // modulators (member-target param names resolve against the member chains).
    for (g_idx, group_config) in config.groups.iter().enumerate() {
        if group_config.modulators.is_empty() {
            continue;
        }
        // Member chains in ordinal order (members = lanes with group == g_idx).
        let member_chains: Vec<Vec<Vec<plugin::ParameterInfo>>> = loaded_instruments
            .iter()
            .filter(|li| li.group == Some(g_idx))
            .map(|li| {
                let mut chain =
                    vec![li.instrument.as_ref().map(|p| p.params.clone()).unwrap_or_default()];
                chain.extend(li.effects.iter().map(|fx| fx.params.clone()));
                chain
            })
            .collect();
        // Member modulators in ordinal order (for group→member-mod cross-mod).
        let member_mods: Vec<&[tui::LoadedModulator]> = loaded_instruments
            .iter()
            .filter(|li| li.group == Some(g_idx))
            .map(|li| li.modulators.as_slice())
            .collect();
        let bus_params: Vec<Vec<plugin::ParameterInfo>> = loaded_groups[g_idx]
            .effects
            .iter()
            .map(|p| p.params.clone())
            .collect();
        let mods = load_group_modulators(
            group_config,
            g_idx,
            &member_chains,
            &member_mods,
            &bus_params,
            &cmd_tx,
        )?;
        loaded_groups[g_idx].modulators = mods;
    }

    // All initial commands queued — now start the audio stream
    engine.play()?;

    // Loading done: stop the splash (restores the terminal) and re-enable
    // logging before the TUI applies its own suppression logic.
    drop(splash);
    log::set_max_level(prev_log_level);

    // --- Branch: TUI view vs plain play mode ---
    if use_tui {
        let session_path = Some(std::path::PathBuf::from(source));
        tui::run(loaded_instruments, loaded_groups, cmd_tx, midi_tx, runtime, sample_rate_f, max_block_size, session_path, pattern_rx, held.clone(), piano_filter.clone())?;
    } else {
        // --- Plain play mode (original) ---

        // Probe keyboard enhancement support (must be done before entering raw mode)
        let kitty_supported =
            crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

        // Install a panic hook that restores terminal state before the panic
        // message prints, so the user's terminal isn't left in raw mode.
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if kitty_supported {
                let _ = crossterm::execute!(std::io::stderr(), PopKeyboardEnhancementFlags);
            }
            let _ = crossterm::terminal::disable_raw_mode();
            original_hook(info);
        }));

        // Enter raw mode
        crossterm::terminal::enable_raw_mode()?;

        // Push Kitty keyboard flags if supported
        if kitty_supported {
            crossterm::execute!(
                std::io::stderr(),
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                        | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                )
            )?;
            log::info!("Kitty keyboard protocol enabled (press/release detection active)");
        } else {
            log::warn!(
                "Terminal does not support Kitty keyboard protocol — virtual piano disabled (hardware MIDI still works)"
            );
        }

        // Create virtual piano
        let mut virt_piano = piano::VirtualPiano::new(midi_tx, kitty_supported, held.clone(), piano_filter.clone());

        log::info!("Playing. Ctrl+Q or Ctrl+C to quit.");

        let mut last_poll = Instant::now();

        loop {
            // Poll crossterm events with 10ms timeout
            if event::poll(Duration::from_millis(10))? {
                if let Event::Key(key_event) = event::read()? {
                    // Ctrl+C or Ctrl+Q → quit
                    if key_event
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        match key_event.code {
                            KeyCode::Char('c') | KeyCode::Char('q') => break,
                            _ => {}
                        }
                    }
                    // Pass to virtual piano
                    virt_piano.handle_key_event(key_event);
                }
            }

            // Drain returned plugins so they are dropped on the main thread
            while return_rx.try_recv().is_ok() {}

            // Poll for new MIDI devices every ~1s
            if last_poll.elapsed() >= Duration::from_secs(1) {
                midi_mgr.poll_new_devices();
                last_poll = Instant::now();
            }
        }

        // Cleanup
        virt_piano.all_notes_off();

        if kitty_supported {
            crossterm::execute!(std::io::stderr(), PopKeyboardEnhancementFlags).ok();
        }
        crossterm::terminal::disable_raw_mode()?;
    }

    log::info!("Stopping...");

    // Shutdown order matters: stop audio first (so callback can't call plugin),
    // then drop MIDI connections, then returned plugins are dropped last.
    engine.stop();
    drop(midi_mgr);

    // Drain any remaining returned plugins
    while return_rx.try_recv().is_ok() {}

    Ok(())
}

