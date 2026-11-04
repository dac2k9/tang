//! Loading a session: instantiate its plugins, tell the audio thread what to run
//! (over the command channel), and return the session model.
//!
//! One loader for every front-end: the TUI and `play` both go through
//! `load_session`, so a session means the same thing in all of them. It reports
//! progress through a callback instead of drawing anything, which is how the TUI
//! splash follows along.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crossbeam_channel::Sender;

use crate::app::controller::outputs_fit_the_chain;
use crate::app::model::{
    AudioCable, BPM_MAX, BPM_MIN, CABLE_GAIN_MAX, CEILING_MAX, CEILING_MIN, Driven, EdgeId, EventCable, FREE, GroupNode, InstrumentNode, MASTER_VOLUME_MAX, MAX_NAME, MasterNode, ModGain, ModSourceSlot,
    ModTargetSlot, ModuleInstance, ModulatorSlot, NEW_CABLE_GAIN, NodeId, NodeIds,
    ParamKind, ParamSlot, PatternState, PluginSlot, SYNC_DIVISIONS, SessionModel, SourceKind, SourceNode, format_from_id, mod_source_slot_to_graph, modulator_slot_commands,
    setting_of, sync_from_name, sync_name, tempo_of_patterns,
};
use crate::plugin::chain::{CableFrom, CableProblem, CableTo, GraphCommand, ModulatorLocation};
use crate::session::{Feed, NodeUi, Route};
use crate::telemetry::SlotAllocator;
use crate::{plugin, session};

/// What `load_session` is doing, for a front-end that wants to show progress
/// (the TUI splash). Only the plugin slots of instrument lanes are reported, as
/// before: `done` counts instruments and their effects, not group bus effects.
pub enum Progress<'a> {
    /// About to load this plugin, named as in the session file.
    Loading(&'a str),
    /// `done` plugin slots have been loaded so far.
    Loaded { done: usize },
}

/// A loaded plugin's parameters, as the loader tracks them.
struct Params {
    /// Every parameter the plugin reports, including the ones the model hides (a
    /// VST3's "(locked)" entries). Modulator targets resolve names against this.
    all: Vec<plugin::ParameterInfo>,
    /// Each parameter's post-preset value, before any session-file override: the
    /// baseline the model compares against to decide what needs saving.
    baselines: Vec<f32>,
    /// Each parameter's current value: the baseline plus the session's overrides.
    values: Vec<f32>,
}

impl Params {
    /// Read the values of a freshly loaded, preset-applied plugin.
    fn snapshot(plugin: &mut dyn plugin::Plugin) -> Self {
        let all = plugin.parameters();
        let baselines: Vec<f32> = all
            .iter()
            .map(|p| plugin.get_parameter(p.index).unwrap_or(p.default))
            .collect();
        Params { all, values: baselines.clone(), baselines }
    }

    /// The parameter list modulator targets resolve against, each parameter's
    /// `default` replaced by what it is set to now (after the preset and the
    /// session's overrides): a target is centred on that, as it is when the user
    /// binds one in the TUI, not on what the plugin starts at.
    fn for_targets(&self) -> Vec<plugin::ParameterInfo> {
        self.all
            .iter()
            .zip(&self.values)
            .map(|(info, &value)| plugin::ParameterInfo { default: value, ..info.clone() })
            .collect()
    }
}

/// Apply the session's parameter overrides, in name order so the command stream is
/// the same every run. `send` is called with each parameter the plugin has and its new
/// value, which is also recorded in `params.values`. Returns the names the plugin does
/// not have.
fn apply_overrides(
    params: &mut Params,
    overrides: &HashMap<String, f64>,
    mut send: impl FnMut(&plugin::ParameterInfo, f32) -> anyhow::Result<()>,
) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<&String> = overrides.keys().collect();
    names.sort();
    let mut unknown = Vec::new();
    for name in names {
        let value = overrides[name] as f32;
        match params.all.iter().find(|p| p.name == *name) {
            Some(info) => {
                send(info, value)?;
                if let Some(v) = params.values.get_mut(info.index as usize) {
                    *v = value;
                }
            }
            None => unknown.push(name.clone()),
        }
    }
    Ok(unknown)
}

fn names(params: &[plugin::ParameterInfo]) -> String {
    params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
}

/// The model's slot for a plugin that has just been loaded and left alone: every
/// parameter at the value the plugin reports (its declared default if it reports
/// none), no preset, fully wet. `node` is the node the plugin sits in. (A session's
/// plugins get theirs from `plugin_slot`, with the values the loader read back in
/// the same way.)
pub fn fresh_slot(plugin: &mut dyn plugin::Plugin, id: &str, node: NodeId) -> PluginSlot {
    let params = Params::snapshot(plugin);
    let mut slot = plugin_slot(
        node,
        plugin.name().to_string(),
        id.to_string(),
        plugin.is_instrument(),
        &params,
        plugin.presets(),
        None,
        1.0,
    );
    slot.side = plugin.side_input();
    slot.takes_notes = plugin.takes_notes();
    slot
}

/// The model's slot for a loaded plugin. Parameters a plugin marks "(locked)" are
/// hidden, and a parameter with value labels becomes an enum.
#[allow(clippy::too_many_arguments)]
fn plugin_slot(
    node: NodeId,
    name: String,
    id: String,
    is_instrument: bool,
    params: &Params,
    presets: Vec<plugin::Preset>,
    current_preset: Option<String>,
    mix: f32,
) -> PluginSlot {
    let slots = params
        .all
        .iter()
        .zip(&params.baselines)
        .zip(&params.values)
        .filter(|((p, _), _)| !p.name.starts_with("(locked)"))
        .map(|((p, &baseline), &value)| ParamSlot {
            kind: p.labels.clone().map_or(ParamKind::Float, ParamKind::Enum),
            name: p.name.clone(),
            index: p.index,
            min: p.min,
            max: p.max,
            default: baseline,
            value,
            unit: p.unit.clone(),
            group: p.group.clone(),
            step_count: p.step_count,
            used_when: p.used_when.clone(),
        })
        .collect();
    PluginSlot {
        node,
        name,
        format: format_from_id(&id),
        id,
        is_instrument,
        params: slots,
        presets,
        current_preset,
        mix,
        fed: true,
        side: None,
        takes_notes: true,
        pinned: Vec::new(),
    }
}

fn send(cmd_tx: &Sender<GraphCommand>, cmd: GraphCommand) -> anyhow::Result<()> {
    cmd_tx.send(cmd).map_err(|_| anyhow::anyhow!("command channel closed"))
}

/// What the loader hands out as it builds nodes: their ids, the telemetry
/// slots modulators and their targets publish into, and where the session says
/// the nodes are drawn.
struct Allocators<'a> {
    ids: &'a mut NodeIds,
    slots: &'a mut SlotAllocator,
    ui: &'a mut BTreeMap<NodeId, NodeUi>,
}

/// Note where a node is drawn, if the session says.
fn place(ui: &mut BTreeMap<NodeId, NodeUi>, node: NodeId, position: Option<NodeUi>) {
    if let Some(position) = position {
        ui.insert(node, position);
    }
}

/// Load `config`: instantiate every plugin, send the audio thread the commands
/// that build the graph (groups first, then lanes with their plugins, parameters,
/// modulators, transpose and pattern, then group modulators), and return the
/// session model. Blocks on the command channel when it is full, so the audio
/// thread must be (or soon be) draining it.
#[allow(clippy::too_many_arguments)]
pub fn load_session(
    config: &session::SessionConfig,
    session_dir: &Path,
    sample_rate: f32,
    max_block_size: usize,
    runtime: &plugin::Runtime,
    cmd_tx: &Sender<GraphCommand>,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<SessionModel> {
    // Every modulator of a loaded session is the session's own (`session::parse` makes them so: `SessionConfig::move_modulators_to_the_session`): one that is still in the table of a lane,
    // of one of its effects or of a group would be left out without a word.
    for (i, lane) in config.instruments.iter().enumerate() {
        if !lane.modulators.is_empty() || lane.effects.iter().any(|effect| !effect.modulators.is_empty()) {
            anyhow::bail!("instrument[{i}] has modulators of its own: `SessionConfig::move_modulators_to_the_session` makes them the session's");
        }
    }
    for (g, group) in config.groups.iter().enumerate() {
        if !group.modulators.is_empty() {
            anyhow::bail!("group[{g}] has modulators of its own: `SessionConfig::move_modulators_to_the_session` makes them the session's");
        }
    }
    let mut loaded_slots = 0usize;
    let mut ids = NodeIds::default();
    let mut slots = SlotAllocator::default();
    let mut ui: BTreeMap<NodeId, NodeUi> = BTreeMap::new();
    place(&mut ui, NodeId::MASTER, config.master_ui);
    // The parameter lists modulator targets resolve names against: per lane (slot 0 =
    // the instrument, then its effects) and per group bus effect.
    let mut lane_params: Vec<Vec<Vec<plugin::ParameterInfo>>> = Vec::new();
    let mut bus_params: Vec<Vec<Vec<plugin::ParameterInfo>>> = Vec::new();

    // Set up submix groups first, so instruments can reference them by index.
    let mut groups: Vec<GroupNode> = Vec::new();
    for (g_idx, group_config) in config.groups.iter().enumerate() {
        let group_node = ids.alloc();
        place(&mut ui, group_node, group_config.ui);
        send(cmd_tx, GraphCommand::AddGroup { node: group_node })?;
        let meter = slots.meter();
        if let Some(slots) = meter {
            send(cmd_tx, GraphCommand::SetGroupMeter { group: g_idx, slots })?;
        }
        if (group_config.volume - 1.0).abs() > f32::EPSILON {
            send(cmd_tx, GraphCommand::SetGroupVolume { group: g_idx, value: group_config.volume })?;
        }
        if group_config.pan.abs() > f32::EPSILON {
            send(cmd_tx, GraphCommand::SetGroupPan { group: g_idx, value: group_config.pan })?;
        }
        if group_config.muted {
            send(cmd_tx, GraphCommand::SetGroupMute { group: g_idx, muted: true })?;
        }
        if group_config.solo {
            send(cmd_tx, GraphCommand::SetGroupSolo { group: g_idx, solo: true })?;
        }
        // (The cables come last, so a way out that is cut is cut before one that needs it so is drawn.)
        if group_config.route == Route::None {
            send(cmd_tx, GraphCommand::SetGroupRoute { group: g_idx, route: Route::None })?;
        }
        let mut group_effects: Vec<PluginSlot> = Vec::new();
        let mut group_bus_params = Vec::new();
        for (fx_idx, effect_config) in group_config.effects.iter().enumerate() {
            let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
            let mut effect = plugin::load(&effect_source, sample_rate, max_block_size, runtime)?;
            let side = effect.side_input();
            outputs_fit_the_chain(effect.name(), effect.audio_output_count())
                .map_err(|refusal| anyhow::anyhow!("group[{g_idx}].effect[{fx_idx}]: {refusal}"))?;
            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let presets = effect.presets();
            let mut params = Params::snapshot(effect.as_mut());
            let name = effect.name().to_string();
            let effect_node = ids.alloc();
            send(
                cmd_tx,
                GraphCommand::InsertGroupEffect {
                    group: g_idx,
                    index: fx_idx,
                    effect,
                    mix: effect_config.mix,
                    node: effect_node,
                },
            )?;
            // (The engine starts every effect with its feed, so only what a file cuts is sent. The cables come last, so a link that is cut is cut before a cable that needs it so is drawn.)
            if effect_config.feed == Feed::None {
                send(cmd_tx, GraphCommand::SetEffectFeed { node: effect_node, fed: false })?;
            }
            apply_overrides(&mut params, &effect_config.params, |info, value| {
                let _ = cmd_tx.send(GraphCommand::SetGroupParameter {
                    group: g_idx,
                    index: fx_idx,
                    param_index: info.index,
                    value,
                });
                Ok(())
            })?;
            place(&mut ui, effect_node, effect_config.ui);
            let mut slot = plugin_slot(
                effect_node,
                name,
                effect_source,
                false,
                &params,
                presets,
                effect_config.preset.clone(),
                effect_config.mix as f32,
            );
            slot.pin_names(&effect_config.pinned);
            slot.fed = effect_config.feed.is_fed();
            slot.side = side;
            group_effects.push(slot);
            group_bus_params.push(params.for_targets());
        }
        bus_params.push(group_bus_params);
        groups.push(GroupNode {
            node: group_node,
            // As the rename popup stores it: trimmed, and nothing is no name.
            name: group_config.name.as_deref().map(str::trim).filter(|name| !name.is_empty()).map(String::from),
            volume: group_config.volume,
            pan: group_config.pan,
            muted: group_config.muted,
            solo: group_config.solo,
            route: group_config.route,
            effects: group_effects,
            meter,
        });
    }

    // Set up the graph structure: add instrument lanes.
    let mut instruments: Vec<InstrumentNode> = Vec::new();
    for (inst_idx, inst_config) in config.instruments.iter().enumerate() {
        let lane_node = ids.alloc();
        place(&mut ui, lane_node, inst_config.ui);
        send(cmd_tx, GraphCommand::AddInstrument { range: inst_config.range, node: lane_node })?;
        let lane_meter = slots.meter();
        if let Some(slots) = lane_meter {
            send(cmd_tx, GraphCommand::SetLaneMeter { inst: inst_idx, slots })?;
        }

        // Assign group membership (groups were created above).
        if inst_config.group.is_some() {
            send(cmd_tx, GraphCommand::SetLaneGroup { inst: inst_idx, group: inst_config.group })?;
        }

        // The chain's parameter lists, slot 0 first.
        let mut chain_params: Vec<Vec<plugin::ParameterInfo>> = Vec::new();

        // Load instrument (if present)
        let instrument_slot = if let Some(plug_config) = &inst_config.instrument {
            let instrument_source = session::resolve_plugin_path(&plug_config.plugin, session_dir);
            progress(Progress::Loading(&plug_config.plugin));
            let mut instrument =
                plugin::load(&instrument_source, sample_rate, max_block_size, runtime)?;
            loaded_slots += 1;
            progress(Progress::Loaded { done: loaded_slots });
            log::info!(
                "Loaded instrument for inst={}: {}",
                inst_idx,
                instrument.name()
            );

            if let Some(ref preset_name) = plug_config.preset {
                session::apply_preset(&mut instrument, preset_name);
            }
            let presets = instrument.presets();

            // Snapshot post-preset parameter values; these become the baseline the
            // model compares against to decide which params are user-edited and
            // need to be saved.
            let mut params = Params::snapshot(instrument.as_mut());

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

            let name = instrument.name().to_string();
            let takes_notes = instrument.takes_notes();
            let inst_buf = (0..instrument.audio_output_count())
                .map(|_| Vec::new())
                .collect();
            send(
                cmd_tx,
                GraphCommand::SwapInstrument { inst: inst_idx, instrument, inst_buf, remapper, keep: None },
            )?;

            // Set volume if not default
            if (plug_config.volume - 1.0).abs() > f64::EPSILON {
                send(
                    cmd_tx,
                    GraphCommand::SetVolume { inst: inst_idx, value: plug_config.volume as f32 },
                )?;
            }

            // Send instrument parameter overrides. They start from the post-preset
            // baseline so values reflect the active preset's actual settings.
            let unknown = apply_overrides(&mut params, &plug_config.params, |info, value| {
                send(
                    cmd_tx,
                    GraphCommand::SetParameter {
                        inst: inst_idx,
                        slot: 0,
                        param_index: info.index,
                        value,
                    },
                )?;
                log::info!("Set instrument '{}' = {}", info.name, value);
                Ok(())
            })?;
            for name in unknown {
                log::warn!(
                    "Unknown instrument parameter '{}' (available: {})",
                    name,
                    names(&params.all)
                );
            }

            chain_params.push(params.for_targets());
            let mut slot = plugin_slot(
                lane_node,
                name,
                instrument_source,
                true,
                &params,
                presets,
                plug_config.preset.clone(),
                1.0,
            );
            slot.pin_names(&plug_config.pinned);
            slot.takes_notes = takes_notes;
            Some(slot)
        } else {
            chain_params.push(Vec::new());
            None
        };

        // Load effects
        let mut effects: Vec<PluginSlot> = Vec::new();
        for (fx_idx, effect_config) in inst_config.effects.iter().enumerate() {
            let effect_source = session::resolve_plugin_path(&effect_config.plugin, session_dir);
            progress(Progress::Loading(&effect_config.plugin));
            let mut effect = plugin::load(&effect_source, sample_rate, max_block_size, runtime)?;
            let side = effect.side_input();
            loaded_slots += 1;
            progress(Progress::Loaded { done: loaded_slots });
            // (The audio thread turns away an effect that has other outputs than the chain has channels, and the model would keep it: the session is refused.)
            outputs_fit_the_chain(effect.name(), effect.audio_output_count())
                .map_err(|refusal| anyhow::anyhow!("instrument[{inst_idx}].effect[{fx_idx}]: {refusal}"))?;
            log::info!(
                "Loaded effect for inst={} fx={}: {}",
                inst_idx,
                fx_idx,
                effect.name()
            );

            if let Some(ref preset_name) = effect_config.preset {
                session::apply_preset(&mut effect, preset_name);
            }
            let presets = effect.presets();

            // Snapshot post-preset values as the save-filter baseline.
            let mut params = Params::snapshot(effect.as_mut());
            let name = effect.name().to_string();

            let effect_node = ids.alloc();
            send(
                cmd_tx,
                GraphCommand::InsertEffect {
                    inst: inst_idx,
                    index: fx_idx,
                    effect,
                    mix: effect_config.mix,
                    node: effect_node,
                },
            )?;
            if effect_config.feed == Feed::None {
                send(cmd_tx, GraphCommand::SetEffectFeed { node: effect_node, fed: false })?;
            }

            // Send parameter overrides for this effect (slot = fx_idx + 1).
            // They start from the post-preset baseline.
            let unknown = apply_overrides(&mut params, &effect_config.params, |info, value| {
                send(
                    cmd_tx,
                    GraphCommand::SetParameter {
                        inst: inst_idx,
                        slot: fx_idx + 1,
                        param_index: info.index,
                        value,
                    },
                )?;
                log::info!("Set effect {} '{}' = {}", fx_idx, info.name, value);
                Ok(())
            })?;
            for name in unknown {
                log::warn!(
                    "Unknown parameter '{}' for effect {} (available: {})",
                    name,
                    fx_idx,
                    names(&params.all)
                );
            }

            chain_params.push(params.for_targets());
            place(&mut ui, effect_node, effect_config.ui);
            let mut slot = plugin_slot(
                effect_node,
                name,
                effect_source,
                false,
                &params,
                presets,
                effect_config.preset.clone(),
                effect_config.mix as f32,
            );
            slot.pin_names(&effect_config.pinned);
            slot.fed = effect_config.feed.is_fed();
            slot.side = side;
            effects.push(slot);
        }

        // Mute and solo: what the engine has to begin with is neither.
        if inst_config.muted {
            send(cmd_tx, GraphCommand::SetMute { inst: inst_idx, muted: true })?;
        }
        if inst_config.solo {
            send(cmd_tx, GraphCommand::SetSolo { inst: inst_idx, solo: true })?;
        }
        if inst_config.route == Route::None {
            send(cmd_tx, GraphCommand::SetLaneRoute { inst: inst_idx, route: Route::None })?;
        }

        // Apply instrument transpose.
        if inst_config.transpose != 0 {
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetTranspose {
                inst: inst_idx,
                semitones: inst_config.transpose,
            });
        }

        // Load pattern if configured.
        let pattern = inst_config.pattern.as_ref().map(|p| {
            // Build Pattern and send to audio graph.
            let pattern_events: Vec<plugin::chain::PatternEvent> = p
                .events
                .iter()
                .map(|&(frame, status, note, vel)| plugin::chain::PatternEvent {
                    frame,
                    status,
                    note,
                    velocity: vel,
                })
                .collect();
            let beats_per_sec = p.bpm / 60.0;
            let length_samples = (p.length_beats / beats_per_sec * sample_rate) as u64;
            // (The length in beats first: a length that is set changes the length of a pattern that is there, in proportion, and there is none yet.)
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLength {
                inst: inst_idx,
                beats: p.length_beats,
            });
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPattern {
                inst: inst_idx,
                pattern: plugin::chain::Pattern { events: pattern_events, length_samples },
                base_note: p.base_note,
                in_key: p.in_key,
            });
            // The engine enables a pattern that has events as soon as it receives it, so
            // say what the session wants, including "off".
            let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternEnabled {
                inst: inst_idx,
                enabled: p.enabled,
            });
            if !p.looping {
                let _ = cmd_tx.send(plugin::chain::GraphCommand::SetPatternLooping {
                    inst: inst_idx,
                    looping: false,
                });
            }
            PatternState {
                bpm: p.bpm,
                length_beats: p.length_beats,
                looping: p.looping,
                base_note: p.base_note,
                events: p.events.clone(),
                enabled: p.enabled,
                recording: false,
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
        instruments.push(InstrumentNode {
            node: lane_node,
            range: inst_config.range,
            transpose: inst_config.transpose,
            volume,
            muted: inst_config.muted,
            solo: inst_config.solo,
            route: inst_config.route,
            instrument: instrument_slot,
            effects,
            pattern,
            pitch_bend_range,
            remap,
            group: inst_config.group,
            meter: lane_meter,
        });
        lane_params.push(chain_params);
    }

    // The modulators come last: their cables name plugins of any lane or group by place, so all of them are loaded by now (and a session that has none uses no id for them).
    let free_modulators = load_free_modulators(
        &config.free_modulators,
        &FreeSession { lane_params: &lane_params, lanes: &instruments, bus_params: &bus_params, groups: &groups },
        cmd_tx,
        &mut Allocators { ids: &mut ids, slots: &mut slots, ui: &mut ui },
    )?;

    // The tempo of a session is its `bpm`, or that of its first recorded pattern if it has none, held to the tempos there can be.
    // The audio thread is told it once, and only if the session says one (it starts at `DEFAULT_BPM`): each pattern says the
    // tempo that it was recorded at, and the last of them was what the audio thread was left with.
    let bpm = config.bpm.map_or_else(|| tempo_of_patterns(&instruments), |bpm| bpm.clamp(BPM_MIN, BPM_MAX));
    if config.bpm.is_some() || instruments.iter().any(|lane| lane.pattern.is_some()) {
        let _ = cmd_tx.send(plugin::chain::GraphCommand::SetGlobalBpm { bpm });
    }
    // The output mix: what a `[master]` table says, held to the ranges (a file can say what the intents refuse), and what it is without one. The audio thread starts with a volume of 1, no limiter
    // and the default ceiling, and is told only if the session has something else.
    let master = master_of(config.master);
    if master != (MasterNode { limiter: false, ..MasterNode::default() }) {
        let _ = cmd_tx.send(crate::app::controller::master_command(&master, sample_rate));
    }
    let mut model = SessionModel { instruments, groups, ids, slots, ui, bpm, master, cables: Vec::new(), sources: Vec::new(), event_cables: Vec::new(), free_modulators, modules: Vec::new() };
    // The sources of notes, and then the cables: every lane, group, effect and source that one of them is at is there by now.
    load_sources(config, &mut model);
    load_cables(&config.cables, &mut model, cmd_tx)?;
    // The modules last (a record of what is there already, with a node of its own, so that a session that has none uses no id for them).
    load_modules(config, &mut model);
    Ok(model)
}

/// Make the modules that the file says came together (`[[module]]`, step 108), each a record of the lanes and the modulators that it names by their places (`instrument[1]`, `modulator[0]`) with a node of its own. It
/// is a record of what is there, which a front-end draws as one box, and no part of the sound: a layout is not worth refusing a session for, so a place that is not in the session is left out (and logged), so is a node
/// that is in a module already (a node is in one at most), and a module that has no member left is not made.
fn load_modules(config: &session::SessionConfig, model: &mut SessionModel) {
    for (at, module) in config.modules.iter().enumerate() {
        let mut members: Vec<NodeId> = Vec::new();
        for member in &module.members {
            let node = match member {
                session::MemberPlace::Lane(lane) => model.instruments.get(*lane).map(|lane| lane.node),
                session::MemberPlace::Modulator(index) => model.free_modulators.get(*index).map(|modulator| modulator.node),
            };
            match node {
                None => log::warn!("module[{at}].members: {member} is not in the session: left out"),
                Some(node) if members.contains(&node) || model.module_of(node).is_some() => log::warn!("module[{at}].members: {member} is in a module already: left out"),
                Some(node) => members.push(node),
            }
        }
        if members.is_empty() {
            log::warn!("module[{at}] has no member that is in the session: left out");
            continue;
        }
        let node = model.alloc_node();
        place(&mut model.ui, node, module.ui);
        model.modules.push(ModuleInstance { node, name: module.name.clone(), members });
    }
}

/// The output mix that a session has: what its `[master]` table says, and for each key that is not there what the master is without it (a volume of 1, the limiter in the sound, a ceiling of
/// `DEFAULT_CEILING`). A number is held to the range of the intent that sets it.
fn master_of(config: Option<session::MasterConfig>) -> MasterNode {
    let config = config.unwrap_or_default();
    let default = MasterNode::default();
    MasterNode {
        volume: config.volume.map_or(default.volume, |volume| volume.clamp(0.0, MASTER_VOLUME_MAX)),
        limiter: config.limiter.unwrap_or(default.limiter),
        ceiling: config.ceiling.map_or(default.ceiling, |ceiling| ceiling.clamp(CEILING_MIN, CEILING_MAX)),
    }
}

/// Put the session's sources of notes in the model, as nodes: the keys first (if the file has a `[keys]` table, or a cable that starts at `keys`, which then makes the node), and then the devices
/// in the order of the file, which is how a cable names them (`device[0]`). The audio thread is told nothing: what a lane hears is the controller's to send (`app::sources`).
fn load_sources(config: &session::SessionConfig, model: &mut SessionModel) {
    let cable_from_the_keys = config.cables.iter().any(|cable| cable.from == session::CablePlace::Keys);
    if config.keys.is_some() || cable_from_the_keys {
        let node = model.alloc_node();
        place(&mut model.ui, node, config.keys.as_ref().and_then(|keys| keys.ui));
        model.sources.push(SourceNode { node, kind: SourceKind::Keys });
    }
    for device in &config.devices {
        let node = model.alloc_node();
        place(&mut model.ui, node, device.ui);
        model.sources.push(SourceNode { node, kind: SourceKind::Device(device.name.clone()) });
    }
}

/// Draw the session's audio cables, in the order of the file: each end is a place of the file (`instrument[0].effect[1]`, `group[0]`, `master`), found among the nodes of the model, and the
/// cable gets the next id of the series that the modulation cables have. A place that is not in the session, a start or an end that is not what a cable can start or end at, a cable
/// between two places that another one is between already and a cable that would make the audio go round refuse the session, with the cable's place in the file (`cable[2].to: …`): a
/// cable that is left out would change the sound without a word. A level is held to what an intent holds it to.
fn load_cables(config: &[session::CableConfig], model: &mut SessionModel, cmd_tx: &Sender<GraphCommand>) -> anyhow::Result<()> {
    for (i, cable) in config.iter().enumerate() {
        if cable.from.is_source() {
            load_event_cable(i, cable, model)?;
            continue;
        }
        let from = cable_start(cable.from, model).map_err(|why| anyhow::anyhow!("cable[{i}].from: {why}"))?;
        let to = cable_end(cable.to, model).map_err(|why| anyhow::anyhow!("cable[{i}].to: {why}"))?;
        if model.cables.iter().any(|drawn| (drawn.from, drawn.to) == (from, to)) {
            anyhow::bail!("cable[{i}]: there is a cable from {} to {} already", cable.from, cable.to);
        }
        if let Some(problem) = model.cable_problem(from, to) {
            let words = match problem {
                CableProblem::Loop(_) => "that cable would make the audio go round: it would feed what it comes from".to_string(),
                other => other.to_string(),
            };
            anyhow::bail!("cable[{i}]: {words}");
        }
        let gain = cable.gain.unwrap_or(NEW_CABLE_GAIN).clamp(0.0, CABLE_GAIN_MAX);
        let id = model.alloc_edge();
        model.insert_audio_cable(AudioCable { id, from, to, gain });
        send(cmd_tx, GraphCommand::AddCable { id, from, to, gain })?;
    }
    Ok(())
}

/// Draw a cable of notes: from a source (`keys`, `device[0]`) to a lane (`instrument[1]`), under the next id of the series that the other cables have. A source that is not in the
/// session, an end that is not a lane and a cable between two nodes that another is between already refuse the session, with the cable's place in the file, as the audio cables do.
fn load_event_cable(i: usize, cable: &session::CableConfig, model: &mut SessionModel) -> anyhow::Result<()> {
    use session::CablePlace as Place;
    let from = match cable.from {
        Place::Keys => model.sources.iter().find(|source| source.kind == SourceKind::Keys),
        Place::Device(device) => model.sources.iter().filter(|source| matches!(source.kind, SourceKind::Device(_))).nth(device),
        _ => None,
    }
    .map(|source| source.node)
    .ok_or_else(|| anyhow::anyhow!("cable[{i}].from: {} is not in the session", cable.from))?;
    let to = match cable.to {
        Place::Instrument(lane) => {
            let node = model.instruments.get(lane).map(|lane| lane.node).ok_or_else(|| anyhow::anyhow!("cable[{i}].to: {} is not in the session", cable.to))?;
            if !model.lane_takes_notes(lane) {
                anyhow::bail!("cable[{i}].to: {} takes no notes: an audio input has no input of events", cable.to);
            }
            node
        }
        other => anyhow::bail!("cable[{i}].to: {other} has no input of notes: a cable of notes ends at a lane (instrument[0])"),
    };
    if model.event_cables.iter().any(|drawn| (drawn.from, drawn.to) == (from, to)) {
        anyhow::bail!("cable[{i}]: there is a cable from {} to {} already", cable.from, cable.to);
    }
    let id = model.alloc_edge();
    model.insert_event_cable(EventCable { id, from, to });
    Ok(())
}

/// The node that a cable starts at, if the place is one that a cable can start at and it is in the model.
fn cable_start(place: session::CablePlace, model: &SessionModel) -> Result<CableFrom, String> {
    use session::CablePlace as Place;
    let missing = || format!("{place} is not in the session");
    match place {
        Place::Instrument(lane) => Ok(CableFrom::Instrument(model.instruments.get(lane).ok_or_else(missing)?.node)),
        Place::Effect { instrument, effect } => Ok(CableFrom::Effect(model.instruments.get(instrument).and_then(|lane| lane.effects.get(effect)).ok_or_else(missing)?.node)),
        Place::Group(group) => Ok(CableFrom::Group(model.groups.get(group).ok_or_else(missing)?.node)),
        Place::BusEffect { group, effect } => Ok(CableFrom::Effect(model.groups.get(group).and_then(|bus| bus.effects.get(effect)).ok_or_else(missing)?.node)),
        Place::EffectSide { .. } | Place::BusEffectSide { .. } => Err(format!("{place} is an input: a cable starts at an output of audio")),
        Place::Master => Err("master has no output of audio".to_string()),
        Place::Keys | Place::Device(_) => Err(format!("{place} is a source of notes, which has no output of audio")),
    }
}

/// The node that a cable ends at, if the place is one that a cable can end at and it is in the model.
fn cable_end(place: session::CablePlace, model: &SessionModel) -> Result<CableTo, String> {
    use session::CablePlace as Place;
    let missing = || format!("{place} is not in the session");
    match place {
        Place::Instrument(lane) => {
            model.instruments.get(lane).ok_or_else(missing)?;
            Err(format!("{place} has no input of audio: a cable ends at an effect, a group or master"))
        }
        Place::Effect { instrument, effect } => Ok(CableTo::Effect(model.instruments.get(instrument).and_then(|lane| lane.effects.get(effect)).ok_or_else(missing)?.node)),
        Place::EffectSide { instrument, effect } => {
            let slot = model.instruments.get(instrument).and_then(|lane| lane.effects.get(effect)).ok_or_else(missing)?;
            slot.side.map(|_| CableTo::EffectSide(slot.node)).ok_or_else(|| format!("{place}: that effect has no second input"))
        }
        Place::Group(group) => Ok(CableTo::GroupBus(model.groups.get(group).ok_or_else(missing)?.node)),
        Place::BusEffect { group, effect } => Ok(CableTo::Effect(model.groups.get(group).and_then(|bus| bus.effects.get(effect)).ok_or_else(missing)?.node)),
        Place::BusEffectSide { group, effect } => {
            let slot = model.groups.get(group).and_then(|bus| bus.effects.get(effect)).ok_or_else(missing)?;
            slot.side.map(|_| CableTo::EffectSide(slot.node)).ok_or_else(|| format!("{place}: that effect has no second input"))
        }
        Place::Master => Ok(CableTo::Master),
        Place::Keys | Place::Device(_) => Err(format!("{place} is a source of notes, which has no input of audio")),
    }
}

/// Make the modulators of the rack at `location` from their configs (`nodes` are their nodes, `ears` whose notes each hears, and `plans`
/// and `rack` say which of their cables are made), and tell the audio thread of each, in the order of the file: the modulator, the slots it publishes to (and whose notes it hears), and its cables.
#[allow(clippy::too_many_arguments)]
fn make_rack(
    location: ModulatorLocation,
    configs: &[&session::ModulatorConfig],
    nodes: &[NodeId],
    ears: &[Option<NodeId>],
    plans: &[Vec<Option<Plan>>],
    rack: &Rack,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
    alloc: &mut Allocators,
) -> anyhow::Result<Vec<ModulatorSlot>> {
    let mut loaded = Vec::new();
    for (mod_idx, mod_config) in configs.iter().enumerate() {
        let (source_slot, desc) = build_mod_source(mod_config);
        cmd_tx
            .send(plugin::chain::GraphCommand::InsertModulator { location, index: mod_idx, source: mod_source_slot_to_graph(&source_slot), node: nodes[mod_idx] })
            .map_err(|_| anyhow::anyhow!("command channel closed"))?;
        let (output, position) = ModulatorSlot::alloc_slots(alloc.slots, &source_slot);
        let ear = ears.get(mod_idx).copied().flatten();
        for command in modulator_slot_commands(location, mod_idx, output, position, ear) {
            send(cmd_tx, command)?;
        }
        let targets = make_cables(location, mod_idx, &mod_config.targets, plans, rack, cmd_tx, alloc)?;
        place(alloc.ui, nodes[mod_idx], mod_config.ui);
        loaded.push(ModulatorSlot { node: nodes[mod_idx], source: source_slot, targets, output, position, ear });
        log::info!("Loaded modulator {mod_idx} of {location:?}: {desc}");
    }
    Ok(loaded)
}

/// What a cable of a file is, once the nodes and the cables that it names are known: what it drives, what it is called, the range it is held to and what it is centred on (what the
/// file gives the field or the parameter that it drives).
struct Plan {
    drives: Drives,
    label: String,
    param_min: f32,
    param_max: f32,
    base_value: f32,
}

/// What a cable of a file drives.
enum Drives {
    /// A plugin's parameter, a modulator's setting, or the depth of a cable that is made already (a member's): by the node and the port.
    Known { aim: Driven },
    /// The depth of the cable that the file has at place `target` of the modulator `modulator` of the same rack, which has no id until the cables of the rack are made.
    DepthOfSibling { modulator: usize, target: usize },
}

/// The cables of a rack that are made, once the file's mistakes are out: for each modulator and each cable that the file has for it, the cable's id and the place that it has among the
/// cables of its modulator that are made (what the audio thread's racks name a cable's depth by), or none if it is not made.
struct Rack {
    cables: Vec<Vec<Option<(EdgeId, usize)>>>,
}

/// Find which of the cables of a rack are made, and give each its id, in the order of the file (a cable has an id from the same series as the cables that were loaded before).
/// A cable that has no plan has said why. One that drives the depth of a cable that is not made is left out as well, and says so: what it names is not there, which is a cable
/// that the audio thread would have kept and that drives nothing (and a place that another cable of the modulator would be taken for).
fn resolve_rack(plans: &[Vec<Option<Plan>>], ids: &mut NodeIds) -> Rack {
    let mut made: Vec<Vec<bool>> = plans.iter().map(|cables| cables.iter().map(Option::is_some).collect()).collect();
    loop {
        let mut changed = false;
        for (modulator, cables) in plans.iter().enumerate() {
            for (target, plan) in cables.iter().enumerate() {
                let Some(Plan { drives: Drives::DepthOfSibling { modulator: of, target: at }, .. }) = plan else { continue };
                if made[modulator][target] && !made.get(*of).and_then(|cables| cables.get(*at)).copied().unwrap_or(false) {
                    log::warn!("Modulator {modulator} target {target} drives the depth of cable {at} of modulator {of}, which is not there: left out");
                    made[modulator][target] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let cables = made
        .iter()
        .map(|modulator| {
            let mut place = 0;
            modulator
                .iter()
                .map(|made| {
                    made.then(|| {
                        let cable = (ids.alloc_edge(), place);
                        place += 1;
                        cable
                    })
                })
                .collect()
        })
        .collect();
    Rack { cables }
}

/// Make the cables of the modulator `mod_idx` of the rack at `location`, as the plans say, and tell the audio thread of each (in the order of the file, as the cables are in the rack).
fn make_cables(
    location: ModulatorLocation,
    mod_idx: usize,
    configs: &[session::ModTargetConfig],
    plans: &[Vec<Option<Plan>>],
    rack: &Rack,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
    alloc: &mut Allocators,
) -> anyhow::Result<Vec<ModTargetSlot>> {
    let mut made = Vec::new();
    for (target, config) in configs.iter().enumerate() {
        let (Some(plan), Some((edge, _))) = (&plans[mod_idx][target], rack.cables[mod_idx][target]) else { continue };
        let aim = match &plan.drives {
            Drives::Known { aim, .. } => *aim,
            Drives::DepthOfSibling { modulator, target } => {
                let (edge, _) = rack.cables[*modulator][*target].expect("a cable that is made drives the depth of one that is");
                Driven::Depth(edge)
            }
        };
        let effective = alloc.slots.alloc();
        let depth = config.depth as f32;
        let cable = plugin::chain::ModTarget { edge, aim, depth, base_value: plan.base_value, param_min: plan.param_min, param_max: plan.param_max, effective };
        send(
            cmd_tx,
            plugin::chain::GraphCommand::AddModTarget { location, mod_index: mod_idx, target: cable },
        )?;
        log::info!("Modulator {mod_idx} target: '{}' depth={}", plan.label, config.depth);
        made.push(ModTargetSlot { edge, param_name: plan.label.clone(), aim, depth, param_min: plan.param_min, param_max: plan.param_max, effective });
    }
    Ok(made)
}

/// What the cable `config` of the modulator `me` of a rack is, if it drives a field of another modulator of the rack, or the depth of one of its cables (`nodes` are the nodes of the
/// rack's modulators, and `group_base` where the modulators that the config belongs to begin in it): `info` says the value, the least and the greatest of a field of the modulator at an
/// index, which the cable is centred on. `None`, and the log says why, for a cable that names no field, a modulator that is not there or the modulator itself (a cable like that does
/// nothing).
fn plan_sibling_target(
    config: &session::ModTargetConfig,
    me: usize,
    group_base: usize,
    nodes: &[NodeId],
    nothing_named: &str,
    info: impl Fn(usize, plugin::chain::CrossModField) -> (f32, f32, f32),
) -> Option<Plan> {
    use plugin::chain::CrossModField as F;
    let (index, field) = if let Some(mi) = config.mod_rate {
        (mi + group_base, F::Rate)
    } else if let Some(ref pair) = config.mod_depth {
        (pair.first().copied().unwrap_or(0) + group_base, F::Depth(pair.get(1).copied().unwrap_or(0)))
    } else if let Some(mi) = config.mod_attack {
        (mi + group_base, F::Attack)
    } else if let Some(mi) = config.mod_decay {
        (mi + group_base, F::Decay)
    } else if let Some(mi) = config.mod_sustain {
        (mi + group_base, F::Sustain)
    } else if let Some(mi) = config.mod_release {
        (mi + group_base, F::Release)
    } else {
        log::warn!("{nothing_named}");
        return None;
    };
    if index >= nodes.len() || index == me {
        log::warn!("Modulator {me} has a cable to modulator {index}, which is {}: left out", if index == me { "itself" } else { "not there" });
        return None;
    }
    let label = match field {
        F::Rate => format!("Mod {index} rate"),
        F::Depth(target) => format!("Mod {index} depth {target}"),
        F::Attack => format!("Mod {index} attack"),
        F::Decay => format!("Mod {index} decay"),
        F::Sustain => format!("Mod {index} sustain"),
        F::Release => format!("Mod {index} release"),
    };
    let (base_value, param_min, param_max) = info(index, field);
    let drives = match field {
        F::Depth(target) => Drives::DepthOfSibling { modulator: index, target },
        _ => Drives::Known { aim: Driven::Setting { modulator: nodes[index], field: setting_of(field)? } },
    };
    Some(Plan { drives, label, param_min, param_max, base_value })
}

/// What a macro is called, from the text a file has (step 110): the text without its control characters and the spaces round it, cut at `MAX_NAME` characters, so that every name in the model is one that `RenameMacro` takes (undo of a
/// rename puts the old name back by that intent, and a name that it refused could not come back). Nothing for a name of nothing: the macro is called `Macro`.
fn macro_name_of(text: &str) -> String {
    let name: String = text.chars().filter(|c| !c.is_control()).collect();
    name.trim().chars().take(MAX_NAME).collect::<String>().trim_end().to_string()
}

/// Build the model's source for a modulator config, with a description for the
/// log. The audio thread's `ModSource` is derived from it (`mod_source_slot_to_graph`). A module's modulators are made of it too (`app::module`).
pub(super) fn build_mod_source(mod_config: &session::ModulatorConfig) -> (ModSourceSlot, String) {
    match mod_config.mod_type.as_str() {
        "envelope" => {
            let source = ModSourceSlot::Envelope {
                attack: mod_config.attack as f32,
                decay: mod_config.decay as f32,
                sustain: mod_config.sustain as f32,
                release: mod_config.release as f32,
            };
            (source, "ADSR envelope".to_string())
        }
        "midi" => {
            let bind = if let Some(cc) = mod_config.midi_cc {
                Some(plugin::chain::MidiBindSource::Cc(cc))
            } else if mod_config.midi_pitch_bend {
                Some(plugin::chain::MidiBindSource::PitchBend)
            } else if mod_config.midi_aftertouch {
                Some(plugin::chain::MidiBindSource::Aftertouch)
            } else if mod_config.midi_velocity {
                Some(plugin::chain::MidiBindSource::Velocity)
            } else if mod_config.midi_key {
                Some(plugin::chain::MidiBindSource::Key)
            } else {
                None
            };
            // Unbound on load ⇒ re-arm so the next control binds.
            let source = ModSourceSlot::MidiLearn { source: bind, learning: bind.is_none() };
            let desc = match bind {
                Some(plugin::chain::MidiBindSource::Cc(n)) => format!("MIDI CC {n}"),
                Some(plugin::chain::MidiBindSource::PitchBend) => "MIDI pitch bend".to_string(),
                Some(plugin::chain::MidiBindSource::Aftertouch) => "MIDI aftertouch".to_string(),
                Some(plugin::chain::MidiBindSource::Velocity) => "MIDI velocity".to_string(),
                Some(plugin::chain::MidiBindSource::Key) => "MIDI key".to_string(),
                None => "MIDI (learning)".to_string(),
            };
            (source, desc)
        }
        "macro" => {
            // (The number is held to the knob's range: a file can say what the knob cannot be at.)
            let (min, max) = crate::app::intent::ModulatorField::Value.range();
            let value = (mod_config.value as f32).clamp(min, max);
            let name = macro_name_of(mod_config.name.as_deref().unwrap_or_default());
            (ModSourceSlot::Macro { value, name, expose: mod_config.expose.unwrap_or(true) }, format!("macro at {value:.2}"))
        }
        "steps" => {
            // (What the file gives is held to what a step sequencer can be: a value to -1 to 1, a glide to 0 to 1 and a length to 1 to 16. A step that is
            // not given is nothing, and a file that gives no list at all has a new one's staircase.)
            use crate::app::model::{STEP_MAX, STEP_MIN};
            use crate::plugin::chain::MAX_STEPS;
            let mut values = ModSourceSlot::staircase();
            let length = match &mod_config.steps {
                Some(given) => {
                    values = [0.0; MAX_STEPS];
                    for (value, given) in values.iter_mut().zip(given) {
                        *value = (*given as f32).clamp(STEP_MIN, STEP_MAX);
                    }
                    given.len()
                }
                None => MAX_STEPS,
            };
            let length = mod_config.length.unwrap_or(length).clamp(1, MAX_STEPS);
            let rate = mod_config.rate.map_or(8.0, |rate| rate as f32);
            let glide = (mod_config.glide as f32).clamp(0.0, 1.0);
            let sync = sync_of(mod_config);
            let source = ModSourceSlot::Steps { values, length, rate, glide, retrigger: mod_config.retrigger.unwrap_or(true), sync };
            let desc = if sync == 0 { format!("{length} steps at {rate:.1}Hz") } else { format!("{length} steps, one in {}", sync_name(sync)) };
            (source, desc)
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
            let rate = mod_config.rate.unwrap_or(1.0);
            // (A note does not start an LFO again unless the file says so, as it never did; the fade is held to what one can be.)
            let (min, max) = crate::app::intent::ModulatorField::Fade.range();
            let fade = (mod_config.fade as f32).clamp(min, max);
            let sync = sync_of(mod_config);
            let source = ModSourceSlot::Lfo { waveform, rate: rate as f32, retrigger: mod_config.retrigger.unwrap_or(false), fade, sync };
            let desc = if sync == 0 { format!("{} {:.1}Hz", waveform.name(), rate) } else { format!("{} {}", waveform.name(), sync_name(sync)) };
            (source, desc)
        }
    }
}

/// The place in the list of note lengths that the file ties a modulator's rate to the tempo by: 0 if it says none (`free` or nothing at all). A name that is no note length is none too,
/// and the log says so (as a waveform that is not one is a sine: a modulator is not worth refusing a session for).
fn sync_of(config: &session::ModulatorConfig) -> usize {
    let Some(name) = &config.sync else { return 0 };
    sync_from_name(name).unwrap_or_else(|| {
        log::warn!("Unknown sync '{name}', defaulting to free (it is one of: {FREE}, {})", SYNC_DIVISIONS.map(|(name, _)| name).join(", "));
        0
    })
}

/// (base value, min, max) of a field of a modulator, for a cable from one modulator of a rack to another, before the rack is loaded: the modulator is
/// described by its config, which has the value the file gives the field (the modulator may not be there, and
/// then the field has its default). The cable's modulation is centred on it.
fn sibling_field_info(config: Option<&session::ModulatorConfig>, field: plugin::chain::CrossModField) -> (f32, f32, f32) {
    match config {
        Some(config) => field_info(&build_mod_source(config).0, |ti| config.targets.get(ti).map(|t| t.depth as f32), field),
        None => field_info(&ModSourceSlot::new_lfo(), |_| None, field),
    }
}

/// (base value, min, max) of `field` of a modulator that has `source` and whose cables have the depths
/// `depth_of` says: where a cable that drives the field is centred, and the range it is held to. A field
/// the kind of modulator does not have has the default.
fn field_info(source: &ModSourceSlot, depth_of: impl Fn(usize) -> Option<f32>, field: plugin::chain::CrossModField) -> (f32, f32, f32) {
    use plugin::chain::CrossModField;
    match field {
        CrossModField::Rate => (source.setting(crate::app::intent::ModulatorField::Rate).unwrap_or(1.0), 0.01, 50.0),
        CrossModField::Attack => {
            let v = if let ModSourceSlot::Envelope { attack, .. } = source { *attack } else { 0.01 };
            (v, 0.001, 10.0)
        }
        CrossModField::Decay => {
            let v = if let ModSourceSlot::Envelope { decay, .. } = source { *decay } else { 0.3 };
            (v, 0.001, 10.0)
        }
        CrossModField::Sustain => {
            let v = if let ModSourceSlot::Envelope { sustain, .. } = source { *sustain } else { 0.7 };
            (v, 0.0, 1.0)
        }
        CrossModField::Release => {
            let v = if let ModSourceSlot::Envelope { release, .. } = source { *release } else { 0.5 };
            (v, 0.001, 10.0)
        }
        CrossModField::Depth(ti) => (depth_of(ti).unwrap_or(0.5), 0.0, 1.0),
    }
}

/// The plugins of a session, for the cables of its modulators to name by their places: each lane's chain (slot 0 the instrument, then its effects) and each group's bus, with the
/// parameters that the cables resolve names against.
struct FreeSession<'a> {
    lane_params: &'a [Vec<Vec<plugin::ParameterInfo>>],
    lanes: &'a [InstrumentNode],
    bus_params: &'a [Vec<Vec<plugin::ParameterInfo>>],
    groups: &'a [GroupNode],
}

/// The parameters of the plugin at place `slot` of chain `of` (a lane's chain has its instrument at 0; a lane that has none has an empty list there), or none.
fn params_at(chains: &[Vec<Vec<plugin::ParameterInfo>>], of: usize, slot: usize) -> &[plugin::ParameterInfo] {
    chains.get(of).and_then(|chain| chain.get(slot)).map_or(&[], Vec::as_slice)
}

impl FreeSession<'_> {
    /// The lane or the group that a free modulator's `hears` names, by its node: `instrument[0]` (the node of the lane, which is its instrument's) or `group[0]`. `Err` says why a text is not one.
    fn heard_at(&self, text: &str) -> Result<NodeId, String> {
        use session::CablePlace as Place;
        let place = Place::parse(text).ok_or_else(|| format!("{text:?} is not a place of the session"))?;
        let missing = || format!("{place} is not in the session");
        match place {
            Place::Instrument(lane) => self.lanes.get(lane).map(|lane| lane.node).ok_or_else(missing),
            Place::Group(group) => self.groups.get(group).map(|bus| bus.node).ok_or_else(missing),
            _ => Err(format!("{place} is not a lane or a group")),
        }
    }

    /// The node of the lane, the effect or the group that `place` names, with the value that the loaded session gives its `gain` (a cable to it is centred on that): the volume of a lane that
    /// has an instrument (`instrument[0]`) or of a group (`group[0]`), the mix of an effect (`instrument[0].effect[1]`, `group[0].effect[0]`) or the pan of a group. `Err` says why a place has none.
    fn gain_at(&self, place: session::CablePlace, gain: ModGain) -> Result<(NodeId, f32), String> {
        use session::CablePlace as Place;
        let missing = || format!("{place} is not in the session");
        match (place, gain) {
            (Place::Instrument(lane), ModGain::Volume) => {
                let lane = self.lanes.get(lane).ok_or_else(missing)?;
                if lane.instrument.is_none() {
                    return Err(format!("{place} has no instrument, and so no volume to drive"));
                }
                Ok((lane.node, lane.volume))
            }
            (Place::Effect { instrument, effect }, ModGain::Mix) => {
                let slot = self.lanes.get(instrument).and_then(|lane| lane.effects.get(effect)).ok_or_else(missing)?;
                Ok((slot.node, slot.mix))
            }
            (Place::BusEffect { group, effect }, ModGain::Mix) => {
                let slot = self.groups.get(group).and_then(|bus| bus.effects.get(effect)).ok_or_else(missing)?;
                Ok((slot.node, slot.mix))
            }
            (Place::Group(group), ModGain::Volume) => self.groups.get(group).map(|bus| (bus.node, bus.volume)).ok_or_else(missing),
            (Place::Group(group), ModGain::Pan) => self.groups.get(group).map(|bus| (bus.node, bus.pan)).ok_or_else(missing),
            _ => Err(format!("{place} has no {}", gain.name())),
        }
    }

    /// The plugin that `place` names, with its node and its parameters: `instrument[0]` is a lane's instrument, `instrument[0].effect[1]` an effect of its chain and `group[0].effect[1]` an
    /// effect of a group's bus. `Err` says why a place is not one.
    fn plugin_at(&self, place: session::CablePlace) -> Result<(NodeId, &[plugin::ParameterInfo]), String> {
        use session::CablePlace as Place;
        let missing = || format!("{place} is not in the session");
        match place {
            Place::Instrument(lane) => {
                let slot = self.lanes.get(lane).ok_or_else(missing)?.instrument.as_ref().ok_or_else(|| format!("{place} has no instrument"))?;
                Ok((slot.node, params_at(self.lane_params, lane, 0)))
            }
            Place::Effect { instrument, effect } => {
                let slot = self.lanes.get(instrument).and_then(|lane| lane.effects.get(effect)).ok_or_else(missing)?;
                Ok((slot.node, params_at(self.lane_params, instrument, effect + 1)))
            }
            Place::BusEffect { group, effect } => {
                let slot = self.groups.get(group).and_then(|bus| bus.effects.get(effect)).ok_or_else(missing)?;
                Ok((slot.node, params_at(self.bus_params, group, effect)))
            }
            Place::EffectSide { .. } | Place::BusEffectSide { .. } | Place::Group(_) | Place::Master | Place::Keys | Place::Device(_) => Err(format!("{place} is not a plugin: it is the second input of one")),
        }
    }
}

/// What a free modulator's cable can name: the plugins of the session (by place) and the free modulators (their nodes and configs).
struct FreeTargets<'a> {
    session: &'a FreeSession<'a>,
    nodes: &'a [NodeId],
    siblings: &'a [session::ModulatorConfig],
}

/// What the cable `config` of the free modulator `me` is: a parameter of a plugin anywhere in the session (`to` says where, as a cable's places are written: `instrument[0].effect[1]`), or a
/// field of another free modulator, or the depth of one of its cables. `None`, and the log says why, for a cable that cannot be made.
fn plan_free_target(config: &session::ModTargetConfig, me: usize, free: &FreeTargets) -> Option<Plan> {
    if let Some(ref gain_name) = config.gain {
        return plan_gain_target(config, gain_name, free);
    }
    let Some(ref param_name) = config.param else {
        let info = |index: usize, field| sibling_field_info(free.siblings.get(index), field);
        return plan_sibling_target(config, me, 0, free.nodes, "Free modulator target has no param or mod_* field, skipping", info);
    };
    let Some(place) = config.to.as_deref().and_then(session::CablePlace::parse) else {
        log::warn!("Free modulator target param '{param_name}' has no `to` that names a plugin of the session: left out");
        return None;
    };
    let (plugin, params) = match free.session.plugin_at(place) {
        Ok(found) => found,
        Err(why) => {
            log::warn!("Free modulator target param '{param_name}': {why}: left out");
            return None;
        }
    };
    let Some(info) = params.iter().find(|p| p.name == *param_name) else {
        log::warn!("Free modulator target param '{param_name}' not found at {place}");
        return None;
    };
    Some(Plan {
        drives: Drives::Known { aim: Driven::Param { plugin, index: info.index } },
        label: info.name.clone(),
        param_min: info.min,
        param_max: info.max,
        base_value: info.default,
    })
}

/// What a cable of a file that drives a host-side value is (`gain = "volume"`, `"mix"` or `"pan"`, and `to`, whose it is): the value that the loaded session gives it is what the cable is centred
/// on, and its range is the value's own. `None`, and the log says why, for a cable that cannot be made.
fn plan_gain_target(config: &session::ModTargetConfig, gain_name: &str, free: &FreeTargets) -> Option<Plan> {
    let Some(gain) = ModGain::parse(gain_name) else {
        log::warn!("Free modulator target gain '{gain_name}' is not a volume, a mix or a pan: left out");
        return None;
    };
    let Some(place) = config.to.as_deref().and_then(session::CablePlace::parse) else {
        log::warn!("Free modulator target gain '{gain_name}' has no `to` that names a lane, an effect or a group of the session: left out");
        return None;
    };
    let (node, value) = match free.session.gain_at(place, gain) {
        Ok(found) => found,
        Err(why) => {
            log::warn!("Free modulator target gain '{gain_name}': {why}: left out");
            return None;
        }
    };
    let (param_min, param_max) = gain.range();
    Some(Plan { drives: Drives::Known { aim: Driven::Gain { node, gain } }, label: gain.name().to_string(), param_min, param_max, base_value: value })
}

/// Build the session's free modulators (the top-level `[[modulator]]` tables: in no lane and no group) and send them to the audio thread. Their cables name a plugin by its place in the
/// session (`to`) and a parameter by its name, or a field of a sibling free modulator (`mod_*`), and every lane and group is loaded by now, so a place is found whatever it is.
fn load_free_modulators(
    configs: &[session::ModulatorConfig],
    session: &FreeSession,
    cmd_tx: &crossbeam_channel::Sender<plugin::chain::GraphCommand>,
    alloc: &mut Allocators,
) -> anyhow::Result<Vec<ModulatorSlot>> {
    // The nodes of all of them now, so that the cable of one can name another that comes after it.
    let nodes: Vec<NodeId> = configs.iter().map(|_| alloc.ids.alloc()).collect();
    let free = FreeTargets { session, nodes: &nodes, siblings: configs };
    let plans: Vec<Vec<Option<Plan>>> = configs.iter().enumerate().map(|(me, mod_config)| mod_config.targets.iter().map(|config| plan_free_target(config, me, &free)).collect()).collect();
    let rack = resolve_rack(&plans, alloc.ids);
    // Whose notes each hears: a place that is not a lane or a group of the session is no ear (the modulator hears every event), and the log says so.
    let ears: Vec<Option<NodeId>> = configs
        .iter()
        .enumerate()
        .map(|(i, config)| {
            let text = config.hears.as_deref()?;
            match session.heard_at(text) {
                Ok(node) => Some(node),
                Err(why) => {
                    log::warn!("modulator[{i}].hears: {why}: it hears every note");
                    None
                }
            }
        })
        .collect();
    let configs: Vec<&session::ModulatorConfig> = configs.iter().collect();
    make_rack(ModulatorLocation::Free, &configs, &nodes, &ears, &plans, &rack, cmd_tx, alloc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{self, DISABLED_PATTERN, GROUPS, LEGACY, MIDI, PINNED, PLACED, Run, SETTINGS, load, try_load};

    #[test]
    fn plugin_slots_hide_locked_parameters_and_turn_labelled_ones_into_enums() {
        let info = |index: u32, name: &str, labels: Option<Vec<&str>>| plugin::ParameterInfo {
            index,
            name: name.to_string(),
            min: 0.0,
            max: 10.0,
            default: 5.0,
            labels: labels.map(|l| l.into_iter().map(String::from).collect()),
            ..Default::default()
        };
        let params = Params {
            all: vec![
                info(0, "cutoff", None),
                info(1, "(locked) bypass", None),
                info(2, "mode", Some(vec!["low", "high"])),
            ],
            baselines: vec![1.0, 2.0, 0.0],
            values: vec![1.5, 2.0, 1.0],
        };
        let slot = plugin_slot(
            NodeId(7),
            "Synth".to_string(),
            "vst3:Synth".to_string(),
            true,
            &params,
            vec![],
            Some("Init".to_string()),
            1.0,
        );
        assert_eq!((slot.name.as_str(), slot.format.as_str(), slot.id.as_str()), ("Synth", "VST3", "vst3:Synth"));
        assert!(slot.is_instrument);
        assert_eq!(slot.current_preset.as_deref(), Some("Init"));
        let rows: Vec<_> = slot.params.iter().map(|p| (p.index, p.name.as_str(), p.default, p.value)).collect();
        assert_eq!(rows, [(0, "cutoff", 1.0, 1.5), (2, "mode", 0.0, 1.0)], "the locked parameter is hidden");
        assert!(matches!(slot.params[0].kind, ParamKind::Float));
        assert!(matches!(&slot.params[1].kind, ParamKind::Enum(l) if l == &["low", "high"]));
        // The loader still resolves modulator targets against the full list.
        assert_eq!(params.all.len(), 3);
    }

    #[test]
    fn a_group_named_with_nothing_or_with_spaces_has_no_name() {
        // As in the rename popup: the name is trimmed, and nothing is no name, so the
        // header reads "Group N" rather than blank.
        let model = load(
            "[[group]]\nname = \"\"\n[[group]]\nname = \"   \"\n[[group]]\nname = \" Pad \"\n[[group]]\nname = \"Lead\"\n[[group]]\n",
        )
        .model;
        let names: Vec<Option<&str>> = model.groups.iter().map(|g| g.name.as_deref()).collect();
        assert_eq!(names, [None, None, Some("Pad"), Some("Lead"), None]);
    }

    #[test]
    fn every_modulation_target_gets_an_edge_id_of_its_own() {
        let model = load(GROUPS).model;
        let edges: Vec<_> = model.free_modulators.iter().flat_map(|m| &m.targets).map(|t| t.edge).collect();
        assert!(edges.len() >= 6, "the scene has targets in lanes and in groups: {}", edges.len());
        let distinct: std::collections::HashSet<_> = edges.iter().collect();
        assert_eq!(distinct.len(), edges.len());
    }

    #[test]
    fn a_slot_keeps_each_parameters_unit_section_and_steps() {
        let info = |index: u32, name: &str, unit: Option<&str>, group: &[&str], step_count: u32| plugin::ParameterInfo {
            index,
            name: name.to_string(),
            max: 10.0,
            unit: unit.map(String::from),
            group: group.iter().map(|g| g.to_string()).collect(),
            step_count,
            ..Default::default()
        };
        let params = Params {
            all: vec![info(0, "gain", Some("dB"), &["Output", "Master"], 0), info(1, "steps", None, &[], 5)],
            baselines: vec![0.0, 0.0],
            values: vec![0.0, 0.0],
        };
        let slot = plugin_slot(NodeId(1), "P".to_string(), "builtin:osc".to_string(), false, &params, vec![], None, 1.0);
        let rows: Vec<_> = slot.params.iter().map(|p| (p.unit.as_deref(), p.group.clone(), p.step_count)).collect();
        assert_eq!(rows, [(Some("dB"), vec!["Output".to_string(), "Master".to_string()], 0), (None, vec![], 5)]);
    }

    #[test]
    fn a_fresh_slot_describes_the_plugin_at_its_defaults() {
        let load = |id: &str| plugin::load(id, 48_000.0, 256, &Default::default()).unwrap();

        let reverb = load("builtin:reverb");
        let mut reverb = reverb;
        let slot = fresh_slot(reverb.as_mut(), "builtin:reverb", NodeId(3));
        assert_eq!(slot.node, NodeId(3));
        assert_eq!((slot.name.as_str(), slot.id.as_str(), slot.format.as_str()), ("Reverb", "builtin:reverb", "Built-in"));
        assert!(!slot.is_instrument);
        assert_eq!((slot.mix, slot.current_preset.as_deref()), (1.0, None));
        assert!(!slot.presets.is_empty(), "the plugin's presets come along");
        assert_eq!(slot.params.len(), reverb.parameters().len());
        for (row, info) in slot.params.iter().zip(reverb.parameters()) {
            assert_eq!((row.index, row.name.as_str(), row.min, row.max), (info.index, info.name.as_str(), info.min, info.max));
            assert_eq!((row.default, row.value), (info.default, info.default), "{}", info.name);
        }

        let osc = load("builtin:osc");
        let mut osc = osc;
        let slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(4));
        assert!(slot.is_instrument);
        let waveform = slot.params.iter().find(|p| p.name == "waveform").unwrap();
        assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 5), "labelled parameters are enums");
    }

    /// A plugin whose parameters are not at their declared defaults when it is loaded
    /// (as a VST3 may be, with the state it starts in): `live` is what it reports.
    struct Live {
        live: Vec<Option<f32>>,
    }

    impl plugin::Plugin for Live {
        fn name(&self) -> &str {
            "Live"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn sample_rate(&self) -> f32 {
            48_000.0
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
            Ok(())
        }
        fn parameters(&self) -> Vec<plugin::ParameterInfo> {
            (0..self.live.len() as u32)
                .map(|index| plugin::ParameterInfo { index, name: format!("p{index}"), min: 0.0, max: 1.0, default: 0.25, ..Default::default() })
                .collect()
        }
        fn get_parameter(&mut self, index: u32) -> Option<f32> {
            self.live[index as usize]
        }
        fn set_parameter(&mut self, _: u32, _: f32) -> anyhow::Result<()> {
            Ok(())
        }
        fn presets(&self) -> Vec<plugin::Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_fresh_slot_shows_what_the_plugin_is_set_to_not_what_it_declares() {
        // (Finding f14.) The session's plugins are read back by the loader; so is a
        // plugin added from the selector, and the value is also what is not saved as an edit.
        let mut live = Live { live: vec![Some(0.75), None, Some(0.0)] };
        let slot = fresh_slot(&mut live, "clap:live", NodeId(5));
        let rows: Vec<(f32, f32)> = slot.params.iter().map(|p| (p.value, p.default)).collect();
        assert_eq!(rows, [(0.75, 0.75), (0.25, 0.25), (0.0, 0.0)], "the live value, and the declared default where it reports none");
    }

    #[test]
    fn every_modulator_and_every_parameter_it_drives_gets_a_telemetry_slot() {
        // `load` also checks the model's slots (all different, none reserved).
        let run = load(GROUPS);
        let model = &run.model;
        // The modulators are the session's own: the second lane's first, and then the group's two.
        let (lane_lfo, lfo, env) = (&model.free_modulators[0], &model.free_modulators[1], &model.free_modulators[2]);
        assert_eq!(model.free_modulators.len(), 3);
        assert!(model.free_modulators.iter().all(|m| m.output.is_some()));
        // The group LFO's three parameter targets publish what their parameters are
        // set to, and so do its cross-mod target and the envelope's two, which drive
        // a modulator's own settings (f132).
        let effective: Vec<Vec<bool>> = [lfo, env].iter().map(|m| m.targets.iter().map(|t| t.effective.is_some()).collect()).collect();
        assert_eq!(effective, [vec![true, true, true, true], vec![true, true]]);

        // Slots 0 and 1 are the master meter's. Then, in the order the loader sends
        // things: the group's meter (2, 3), lane 0's (4, 5), lane 1's (6, 7) and lane 2's (8, 9);
        // then the modulators, each output followed by its targets (the ones the loader
        // skipped took no slot).
        let output = |m: &ModulatorSlot| m.output.unwrap().index();
        assert_eq!(output(lane_lfo), 10);
        assert_eq!(output(lfo), 11);
        let driven: Vec<u32> = lfo.targets.iter().filter_map(|t| t.effective).map(|s| s.index()).collect();
        assert_eq!(driven, [12, 13, 14, 15]);
        assert_eq!(output(env), 16);
        let driven: Vec<u32> = env.targets.iter().filter_map(|t| t.effective).map(|s| s.index()).collect();
        assert_eq!(driven, [17, 18]);
        assert_eq!(model.slots.high_water(), 19);

        // The audio thread was told, right after each node was added.
        assert_eq!(
            run.telemetry,
            [
                "SetGroupMeter group=0 peak=2 clips=3",
                "SetLaneMeter inst=0 peak=4 clips=5",
                "SetLaneMeter inst=1 peak=6 clips=7",
                "SetLaneMeter inst=2 peak=8 clips=9",
                "SetModulatorOutput Free index=0 slot=10",
                "SetModulatorOutput Free index=1 slot=11",
                "SetModulatorOutput Free index=2 slot=16",
            ]
        );
    }

    #[test]
    fn a_lane_modulators_cable_to_a_setting_of_another_gets_a_slot_named_at_the_port_it_ends_at() {
        // (f132: only a cable to a parameter did; the engine publishes what a modulator's own setting was set to now.)
        let model = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nrate = 1.0\n[[instrument.modulator.target]]\nmod_attack = 1\ndepth = 0.2\n\
             [[instrument.modulator]]\ntype = \"envelope\"\n",
        )
        .model;
        let (lfo, envelope) = (&model.free_modulators[0], &model.free_modulators[1]);
        let slot = lfo.targets[0].effective.expect("a cable to a setting has a slot");
        let layout = model.telemetry_layout();
        let named: Vec<_> = layout.modulation.iter().filter(|e| e.kind == crate::app::layout::SlotKind::Effective).map(|e| (e.slot, e.node, e.port)).collect();
        assert_eq!(named, [(slot.index(), envelope.node, crate::app::graph::Port::Attack)]);
    }

    /// What the loader sent, without the ears that it told the modulators (`a_moved_modulator_hears_the_node_of_the_lane_or_the_group_it_was_in_and_the_audio_thread_is_told_with_it` holds those).
    fn without_ears(run: &Run) -> Vec<String> {
        run.commands.iter().filter(|c| !c.starts_with("SetModulatorEar")).cloned().collect()
    }

    /// The `AddFreeModTarget` lines of a load.
    fn target_commands(run: &crate::app::testutil::Run) -> Vec<&str> {
        run.commands.iter().map(String::as_str).filter(|c| c.starts_with("Add") && c.contains("ModTarget")).collect()
    }

    #[test]
    fn a_lane_target_is_centred_on_what_the_session_sets_the_parameter_to() {
        // The oscillator's `detune` and the filter's `cutoff` are set, and an LFO
        // drives each: it wobbles around the value the session set, not around the
        // plugin's default (0 and 1).
        let run = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\ndetune = 3.0\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.35\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.5\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.5\n",
        );
        assert_eq!(
            target_commands(&run),
            testutil::told_all(&run.model, &[
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 0, param_index: 1 } depth=0.500 base=3.000 range=[-24.000, 24.000]",
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
            ])
        );
    }

    #[test]
    fn a_target_is_centred_on_the_value_a_preset_gave_the_parameter() {
        // A parameter the preset moves off its default, and one the session sets on
        // top of the preset: the first is centred on the preset's value, the second
        // on the session's.
        let mut reverb = crate::plugin::builtin::load("builtin:reverb", 48_000.0, 256).unwrap();
        let params = reverb.parameters();
        let named = |name: &str| params.iter().find(|p| p.name == name).unwrap().clone();
        let (damping, size) = (named("damping"), named("size"));
        let preset = reverb
            .presets()
            .into_iter()
            .find(|p| {
                reverb.load_preset(&p.id).unwrap();
                reverb.get_parameter(damping.index) != Some(damping.default)
            })
            .expect("a preset that moves `damping`");
        reverb.load_preset(&preset.id).unwrap();
        let preset_damping = reverb.get_parameter(damping.index).unwrap();

        let run = load(&format!(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.effect]]\nplugin = \"builtin:reverb\"\npreset = \"{}\"\n[instrument.effect.params]\nsize = 0.9\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"damping\"\ndepth = 0.5\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"size\"\ndepth = 0.5\n",
            preset.name
        ));
        let commands = target_commands(&run);
        let base = |c: &str| c.split("base=").nth(1).unwrap().split(' ').next().unwrap().to_string();
        assert_eq!(base(commands[0]), format!("{preset_damping:.3}"), "{}", commands[0]);
        assert_ne!(base(commands[0]), format!("{:.3}", damping.default));
        assert_eq!(base(commands[1]), "0.900", "{}", commands[1]);
        let _ = size;
    }

    #[test]
    fn the_cables_of_a_modulator_that_hears_a_group_are_centred_on_the_session_s_values_too() {
        // A member's filter `cutoff` and the bus reverb's `damping` are set, and a
        // group LFO drives each.
        let run = load(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[group.effect.params]\ndamping = 0.6\n\
             [[group.modulator]]\n\
             [[group.modulator.target]]\nmember = 0\nslot = 1\nparam = \"cutoff\"\ndepth = 0.5\n\
             [[group.modulator.target]]\nbus = 0\nparam = \"damping\"\ndepth = 0.5\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n[instrument.effect.params]\ncutoff = 0.35\n",
        );
        assert_eq!(
            target_commands(&run),
            testutil::told_all(&run.model, &[
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]",
                "AddFreeModTarget mod=0 kind=BusParam { group: 0, effect_index: 0, param_index: 2 } depth=0.500 base=0.600 range=[0.000, 1.000]",
            ])
        );
    }

    #[test]
    fn a_parameter_the_session_leaves_alone_keeps_its_default_as_the_centre() {
        let run = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.5\n",
        );
        assert_eq!(
            target_commands(&run),
            testutil::told_all(&run.model, &["AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.500 base=1.000 range=[0.000, 1.000]"])
        );
    }

    #[test]
    fn each_modulator_is_told_its_own_slot_by_its_index() {
        // MIDI: six modulators that hear lane 0, the first and the fifth with one target.
        let run = load(MIDI);
        assert_eq!(
            run.telemetry,
            [
                "SetLaneMeter inst=0 peak=2 clips=3",
                "SetModulatorOutput Free index=0 slot=4",
                // slot 5 is the first modulator's target
                "SetModulatorOutput Free index=1 slot=6",
                "SetModulatorOutput Free index=2 slot=7",
                "SetModulatorOutput Free index=3 slot=8",
                "SetModulatorOutput Free index=4 slot=9",
                // slot 10 is the fifth modulator's target
                "SetModulatorOutput Free index=5 slot=11",
            ]
        );
        let outputs: Vec<u32> = run.model.free_modulators.iter().map(|m| m.output.unwrap().index()).collect();
        assert_eq!(outputs, [4, 6, 7, 8, 9, 11]);
    }

    #[test]
    fn a_step_sequencer_is_told_where_to_publish_its_position_right_after_its_output() {
        // Two step sequencers, the second with a cable, around an LFO.
        let run = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.modulator]]\ntype = \"steps\"\n\
             [[instrument.modulator]]\n\
             [[instrument.modulator]]\ntype = \"steps\"\n\
             [[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.5\n",
        );
        assert_eq!(
            run.telemetry,
            [
                "SetLaneMeter inst=0 peak=2 clips=3",
                "SetModulatorOutput Free index=0 slot=4",
                "SetModulatorPosition Free index=0 slot=5",
                "SetModulatorOutput Free index=1 slot=6",
                "SetModulatorOutput Free index=2 slot=7",
                "SetModulatorPosition Free index=2 slot=8",
                // slot 9 is the second step sequencer's target
            ]
        );
        let slots: Vec<(u32, Option<u32>)> =
            run.model.free_modulators.iter().map(|m| (m.output.unwrap().index(), m.position.map(|slot| slot.index()))).collect();
        assert_eq!(slots, [(4, Some(5)), (6, None), (7, Some(8))]);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_sequencer_that_hears_a_group_is_told_where_to_publish_its_position_right_after_its_output() {
        let run = load(
            "[[group]]\n[[group.modulator]]\ntype = \"steps\"\n[[group.modulator]]\n[[group.modulator]]\ntype = \"steps\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n",
        );
        let slots: Vec<(u32, Option<u32>)> =
            run.model.free_modulators.iter().map(|m| (m.output.unwrap().index(), m.position.map(|slot| slot.index()))).collect();
        let [(first, Some(first_position)), (second, None), (third, Some(third_position))] = slots[..] else { panic!("{slots:?}") };
        assert_eq!((first_position, third_position), (first + 1, third + 1));
        let modulators: Vec<&String> = run.telemetry.iter().filter(|line| line.contains("Modulator")).collect();
        assert_eq!(
            modulators,
            [
                &format!("SetModulatorOutput Free index=0 slot={first}"),
                &format!("SetModulatorPosition Free index=0 slot={first_position}"),
                &format!("SetModulatorOutput Free index=1 slot={second}"),
                &format!("SetModulatorOutput Free index=2 slot={third}"),
                &format!("SetModulatorPosition Free index=2 slot={third_position}"),
            ]
        );
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_session_with_no_modulators_uses_a_meter_for_each_lane_and_nothing_else() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n");
        assert_eq!(run.model.slots.high_water(), 4, "the master meter's two, and the lane's");
        assert_eq!(run.telemetry, ["SetLaneMeter inst=0 peak=2 clips=3"]);
        let meter = run.model.instruments[0].meter.unwrap();
        assert_eq!((meter.peak.index(), meter.clips.index()), (2, 3));
        assert!(run.model.groups.is_empty());
    }

    #[test]
    fn a_group_and_its_lanes_each_get_a_meter_as_they_are_added() {
        let run = load(
            "[[group]]\n[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n[[instrument]]\nplugin = \"builtin:osc\"\n",
        );
        assert_eq!(
            run.telemetry,
            [
                "SetGroupMeter group=0 peak=2 clips=3",
                "SetGroupMeter group=1 peak=4 clips=5",
                "SetLaneMeter inst=0 peak=6 clips=7",
                "SetLaneMeter inst=1 peak=8 clips=9",
            ]
        );
        let slots = |m: Option<crate::telemetry::MeterSlots>| m.map(|m| (m.peak.index(), m.clips.index()));
        assert_eq!(run.model.groups.iter().map(|g| slots(g.meter)).collect::<Vec<_>>(), [Some((2, 3)), Some((4, 5))]);
        assert_eq!(run.model.instruments.iter().map(|l| slots(l.meter)).collect::<Vec<_>>(), [Some((6, 7)), Some((8, 9))]);
    }

    #[test]
    fn a_modulator_slot_command_follows_the_command_that_made_the_modulator() {
        for scene in [MIDI, GROUPS] {
            let (tx, rx) = crossbeam_channel::unbounded();
            let model = crate::app::testutil::load_onto(scene, &tx);
            let sent: Vec<String> = rx.try_iter().map(|c| crate::app::testutil::describe(&c)).collect();
            for (i, line) in sent.iter().enumerate() {
                if line.starts_with("SetModulatorOutput") {
                    let before = &sent[i - 1];
                    assert!(before.starts_with("InsertFreeModulator"), "{line} follows {before}");
                }
            }
            assert_eq!(sent.iter().filter(|l| l.starts_with("SetModulatorOutput")).count(), model.free_modulators.len());
        }
    }

    #[test]
    fn parameter_overrides_apply_in_name_order() {
        // Eight names make an accidentally sorted hash order a 1 in 40320 chance.
        let letters = ["h", "c", "f", "a", "g", "d", "b", "e"];
        let mut params = Params {
            all: letters
                .iter()
                .enumerate()
                .map(|(i, n)| plugin::ParameterInfo { index: i as u32, name: n.to_string(), ..Default::default() })
                .collect(),
            baselines: vec![0.0; 8],
            values: vec![0.0; 8],
        };
        let mut overrides: HashMap<String, f64> =
            letters.iter().enumerate().map(|(i, n)| (n.to_string(), i as f64 + 1.0)).collect();
        overrides.insert("zzz".to_string(), 9.0);
        let mut sent = Vec::new();
        let unknown = apply_overrides(&mut params, &overrides, |info, value| {
            sent.push((info.name.clone(), value));
            Ok(())
        })
        .unwrap();
        let names: Vec<&str> = sent.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e", "f", "g", "h"]);
        assert_eq!(unknown, ["zzz"]);
        // Each value lands at its parameter's index: "h" is index 0 and was given 1.0.
        assert_eq!(params.values, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn basic_example_loads_one_lane_with_a_reverb() {
        let run = load(include_str!("../../examples/basic.toml"));
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertEffect inst=0 index=0 plugin=Reverb mix=0.250",
            ]
        );
        assert_eq!(run.progress, ["loading builtin:sine", "loaded 1", "loading builtin:reverb", "loaded 2"]);
        let lane = &run.model.instruments[0];
        assert_eq!(lane.instrument.as_ref().unwrap().name, "Oscillator");
        assert_eq!(lane.effects[0].name, "Reverb");
        assert_eq!(lane.effects[0].current_preset.as_deref(), Some("Arcadia Dream Hall"));
        assert_eq!(lane.effects[0].mix, 0.25);
    }

    #[test]
    fn split_example_loads_two_ranged_lanes_with_vibrato() {
        let run = load(include_str!("../../examples/split.toml"));
        assert_eq!(
            without_ears(&run),
            testutil::told_all(&run.model, &[
                "AddInstrument range=Some((12, 59))",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "AddInstrument range=Some((60, 108))",
                "SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false",
                // The modulators come after the lanes, the first lane's and then the second's.
                "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 4.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 0, param_index: 1 } depth=0.040 base=0.000 range=[-24.000, 24.000]",
                "InsertFreeModulator index=1 source=Lfo { waveform: Sine, rate: 7.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=1 kind=LaneParam { lane: 1, slot: 0, param_index: 1 } depth=0.150 base=0.000 range=[-24.000, 24.000]",
            ])
        );
        assert_eq!(run.model.instruments.len(), 2);
        assert_eq!(run.model.instruments[0].range, Some((12, 59)));
        assert_eq!(run.model.free_modulators[1].targets[0].depth, 0.15);
    }

    #[test]
    fn echo_example_loads_a_filter_and_a_ping_pong_delay_with_an_lfo_each() {
        let run = load(include_str!("../../examples/echo.toml"));
        assert_eq!(
            without_ears(&run),
            testutil::told_all(&run.model, &[
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "SetVolume inst=0 value=0.300",
                "SetParameter inst=0 slot=0 param=6 value=0.300",
                "SetParameter inst=0 slot=0 param=0 value=3.000",
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000",
                "SetParameter inst=0 slot=1 param=0 value=0.550",
                "SetParameter inst=0 slot=1 param=1 value=2.000",
                "InsertEffect inst=0 index=1 plugin=Delay mix=0.400",
                "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 0.2, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.300 base=0.550 range=[0.000, 1.000]",
                // (The second LFO moves the delay's `time`, which the preset has at 0.3 s: that is where it is centred, over a range of 0.001 to 2 s.)
                "InsertFreeModulator index=1 source=Lfo { waveform: Triangle, rate: 0.3, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=1 kind=LaneParam { lane: 0, slot: 2, param_index: 0 } depth=0.004 base=0.300 range=[0.001, 2.000]",
            ])
        );
        let delay = &run.model.instruments[0].effects[1];
        assert_eq!((delay.name.as_str(), delay.current_preset.as_deref(), delay.mix), ("Delay", Some("Ping-Pong"), 0.4));
    }

    /// A group with a macro at 0.5, and a lane in it with a macro at 0.25 that drives the oscillator's `detune`.
    const MACROS: &str = r#"
[[group]]
[[group.modulator]]
type = "macro"
value = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "macro"
value = 0.25
[[instrument.modulator.target]]
param = "detune"
depth = 0.4
"#;

    #[test]
    fn a_macro_is_loaded_at_the_value_the_file_gives_it_in_a_lane_and_in_a_group_and_the_engine_is_given_it() {
        let run = load(MACROS);
        // (The lane's is the first of the session's own, and the group's the second.)
        assert!(matches!(run.model.free_modulators[0].source, ModSourceSlot::Macro { value, .. } if value == 0.25));
        assert!(matches!(run.model.free_modulators[1].source, ModSourceSlot::Macro { value, .. } if value == 0.5));
        assert!(run.commands.contains(&"InsertFreeModulator index=0 source=Macro { value: 0.25 }".to_string()), "{:#?}", run.commands);
        assert!(run.commands.contains(&"InsertFreeModulator index=1 source=Macro { value: 0.5 }".to_string()), "{:#?}", run.commands);
        // Its cable is a cable like any other.
        assert_eq!(run.model.free_modulators[0].targets.len(), 1);
        let cable = testutil::told(&run.model, "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 0, param_index: 1 } depth=0.400");
        assert!(run.commands.iter().any(|c| c.starts_with(&cable)), "{:#?}", run.commands);
    }

    #[test]
    fn a_macro_that_has_no_value_is_at_rest_and_a_value_beyond_its_range_is_held_in_it() {
        let value_of = |text: &str| match load(text).model.free_modulators[0].source {
            ModSourceSlot::Macro { value, .. } => value,
            ref other => panic!("{other:?}"),
        };
        let lane = |value: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\n{value}");
        assert_eq!(value_of(&lane("")), 0.0);
        assert_eq!(value_of(&lane("value = 1.5\n")), 1.0);
        assert_eq!(value_of(&lane("value = -2.0\n")), 0.0);
        assert_eq!(value_of(&lane("value = 0.75\n")), 0.75);
    }

    #[test]
    fn a_macro_is_loaded_with_the_name_that_the_file_gives_it_and_shows_its_knob_on_a_box_unless_the_file_says_not() {
        let macro_of = |keys: &str| {
            let text = format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\n{keys}");
            match load(&text).model.free_modulators.remove(0).source {
                ModSourceSlot::Macro { name, expose, .. } => (name, expose),
                other => panic!("{other:?}"),
            }
        };
        // (A file that is from before a macro had either is a macro that is called `Macro` and is shown.)
        assert_eq!(macro_of(""), (String::new(), true));
        assert_eq!(macro_of("name = \"Brightness\"\n"), ("Brightness".to_string(), true));
        assert_eq!(macro_of("expose = false\n"), (String::new(), false));
        assert_eq!(macro_of("name = \"Wet\"\nexpose = true\n"), ("Wet".to_string(), true));
        // What a hand wrote is held to what the controller takes, so that no name in the model is one that `RenameMacro` refuses (undo of a rename puts the old name back by it): the spaces round it and the control
        // characters in it go, and it is cut at 64 characters (counted as characters, not bytes), and a name of nothing is none.
        assert_eq!(macro_of("name = \"  Wet  \"\n").0, "Wet");
        assert_eq!(macro_of("name = \"\"\n").0, "");
        assert_eq!(macro_of("name = \"   \"\n").0, "");
        assert_eq!(macro_of("name = \"a\\tb\\nc\"\n").0, "abc");
        assert_eq!(macro_of(&format!("name = \"{}\"\n", "x".repeat(70))).0, "x".repeat(64));
        assert_eq!(macro_of(&format!("name = \"{}\"\n", "å".repeat(70))).0, "å".repeat(64));
        assert_eq!(macro_of(&format!("name = \"{} yyy\"\n", "x".repeat(63))).0, "x".repeat(63), "the space that the cut leaves at the end goes too");
        // The keys of a macro are the macro's: a modulator that is another kind is what it is, whatever it is called.
        let text = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"lfo\"\nname = \"Wobble\"\nexpose = false\n";
        let run = load(text);
        assert!(matches!(run.model.free_modulators[0].source, ModSourceSlot::Lfo { .. }));
        assert_eq!(run.model.free_modulators[0].source.node_name(), "LFO");
    }

    /// A group with a step sequencer that the file describes, and a lane in it with one that the file leaves as a new one is, and an LFO whose cable drives its rate.
    const SEQUENCERS: &str = r#"
[[group]]
[[group.modulator]]
type = "steps"
steps = [0.5, -0.25, 1.0]
rate = 3.5
glide = 0.25
retrigger = false

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "steps"
[[instrument.modulator]]
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.2
"#;

    fn steps_of(source: &ModSourceSlot) -> ([f32; 16], usize, f32, f32, bool) {
        match source {
            ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } => (*values, *length, *rate, *glide, *retrigger),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_step_sequencer_is_loaded_with_what_the_file_gives_it_and_the_engine_is_given_it_at_its_first_step() {
        let run = load(SEQUENCERS);
        let mut values = [0.0; 16];
        values[..3].copy_from_slice(&[0.5, -0.25, 1.0]);
        // The values that are not given are nothing, and as many are in use as are given.
        assert_eq!(steps_of(&run.model.free_modulators[2].source), (values, 3, 3.5, 0.25, false));
        assert!(
            run.commands.contains(
                &"InsertFreeModulator index=2 source=Steps { values: [0.5, -0.25, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], length: 3, rate: 3.5, glide: 0.25, retrigger: false, sync: 0.0, position: 0.0 }"
                    .to_string()
            ),
            "{:#?}",
            run.commands
        );
        // One that the file does not describe is what a new one is: the staircase, all sixteen in use, 8 a second, no glide, started again by a note.
        let ModSourceSlot::Steps { values: staircase, .. } = ModSourceSlot::new_steps() else { unreachable!() };
        assert_eq!(steps_of(&run.model.free_modulators[0].source), (staircase, 16, 8.0, 0.0, true));
        // And a cable that drives its rate is centred on the 8 that it has, where an LFO that has none is at 1.
        let cable = testutil::told(&run.model, "AddFreeModTarget mod=1 kind=ModulatorRate { mod_index: 0 } depth=0.200 base=8.000 range=[0.010, 50.000]");
        assert!(run.commands.iter().any(|c| c.starts_with(&cable)), "{:#?}", run.commands);
        assert_eq!(run.model.free_modulators.len(), 3, "the lane's two and the group's one");
    }

    #[test]
    fn an_lfo_with_a_key_in_the_file_is_loaded_with_it_and_one_that_says_nothing_of_it_runs_free() {
        let lane = |fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"lfo\"\n{fields}");
        let of = |fields: &str| match load(&lane(fields)).model.free_modulators[0].source {
            ModSourceSlot::Lfo { retrigger, fade, .. } => (retrigger, fade),
            ref other => panic!("{other:?}"),
        };
        assert_eq!(of(""), (false, 0.0), "a file that says nothing of it is an LFO that runs free, as every file made one");
        assert_eq!(of("retrigger = true\nfade = 0.75\n"), (true, 0.75));
        assert_eq!(of("retrigger = false\n"), (false, 0.0));
        // A fade is held to what one can be.
        assert_eq!(of("fade = 9.0\n").1, 5.0);
        assert_eq!(of("fade = -1.0\n").1, 0.0);
        // The engine is given the key with the LFO, which rests at full until a note.
        let run = load(&lane("rate = 2.0\nretrigger = true\nfade = 0.5\n"));
        let inserted = "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: true, fade: 0.5, sync: 0.0, age: inf }".to_string();
        assert!(run.commands.contains(&inserted), "{:#?}", run.commands);
        // A group's LFO has them too.
        let run = load("[[group]]\n[[group.modulator]]\nretrigger = true\nfade = 1.5\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n");
        match run.model.free_modulators[0].source {
            ModSourceSlot::Lfo { retrigger, fade, .. } => assert_eq!((retrigger, fade), (true, 1.5)),
            ref other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_modulator_that_the_file_ties_to_the_tempo_is_loaded_with_the_note_length_and_the_engine_is_given_its_beats_and_the_rate_it_keeps() {
        let lane = |kind: &str, fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"{kind}\"\n{fields}");
        let sync_of = |kind: &str, fields: &str| match load(&lane(kind, fields)).model.free_modulators[0].source {
            ModSourceSlot::Lfo { sync, rate, .. } | ModSourceSlot::Steps { sync, rate, .. } => (sync, rate),
            ref other => panic!("{other:?}"),
        };
        // A file that says nothing of it is as free as every file was.
        assert_eq!(sync_of("lfo", "rate = 2.0\n"), (0, 2.0));
        assert_eq!(sync_of("steps", ""), (0, 8.0));
        // 11 is an eighth note and 12 its triplet, 14 a sixteenth, 1 four bars and 16 a thirty-second; the rate it has is kept.
        assert_eq!(sync_of("lfo", "rate = 2.0\nsync = \"1/8\"\n"), (11, 2.0));
        assert_eq!(sync_of("lfo", "sync = \"1/8 triplet\"\n").0, 12);
        assert_eq!(sync_of("steps", "sync = \"1/16\"\nrate = 3.0\n"), (14, 3.0));
        assert_eq!(sync_of("lfo", "sync = \"4 bars\"\n").0, 1);
        assert_eq!(sync_of("steps", "sync = \"1/32\"\n").0, 16);
        // (The name is read in any case and with any space about it, and `free` is none.)
        assert_eq!(sync_of("lfo", "sync = \"  1/4   DOTTED \"\n").0, 7);
        assert_eq!(sync_of("lfo", "sync = \"free\"\n").0, 0);
        // The engine is given the beats with the modulator, a group's too.
        let run = load(&lane("lfo", "rate = 2.0\nsync = \"1/8\"\n"));
        let inserted = "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.5, age: inf }".to_string();
        assert!(run.commands.contains(&inserted), "{:#?}", run.commands);
        let run = load("[[group]]\n[[group.modulator]]\ntype = \"steps\"\nsync = \"1/16\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n");
        match run.model.free_modulators[0].source {
            ModSourceSlot::Steps { sync, .. } => assert_eq!(sync, 14),
            ref other => panic!("{other:?}"),
        }
        assert!(run.commands.iter().any(|c| c.starts_with("InsertFreeModulator index=0 source=Steps {") && c.contains("sync: 0.25")), "{:#?}", run.commands);
    }

    #[test]
    fn a_sync_that_is_no_note_length_leaves_the_modulator_free_and_a_modulator_that_cannot_follow_the_tempo_ignores_one() {
        let lane = |kind: &str, fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"{kind}\"\n{fields}");
        for unknown in ["1/7", "1/8t", "eighth", "7"] {
            match load(&lane("lfo", &format!("rate = 2.0\nsync = \"{unknown}\"\n"))).model.free_modulators[0].source {
                ModSourceSlot::Lfo { sync, rate, .. } => assert_eq!((sync, rate), (0, 2.0), "{unknown}"),
                ref other => panic!("{other:?}"),
            }
        }
        // An envelope, a macro and a MIDI control have no rate to tie to it.
        for kind in ["envelope", "macro", "midi"] {
            let run = load(&lane(kind, "sync = \"1/8\"\n"));
            assert!(!format!("{:?}", run.model.free_modulators[0].source).contains("sync"), "{kind}");
        }
    }

    #[test]
    fn what_a_file_says_that_a_step_sequencer_cannot_be_is_held_to_what_it_can() {
        let lane = |fields: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\n{fields}");
        let of = |fields: &str| steps_of(&load(&lane(fields)).model.free_modulators[0].source);
        // A value is held to -1 to 1, and so is a glide to 0 to 1.
        let (values, length, ..) = of("steps = [2.0, -3.0, 0.5]\n");
        assert_eq!((&values[..4], length), (&[1.0, -1.0, 0.5, 0.0][..], 3));
        assert_eq!(of("glide = -1.0\n").3, 0.0);
        assert_eq!(of("glide = 2.0\n").3, 1.0);
        assert_eq!(of("glide = 0.5\n").3, 0.5);
        // A length is held to 1 to 16, and is how many there are values if the file does not say; steps past the values given are nothing.
        assert_eq!(of("length = 0\n").1, 1);
        assert_eq!(of("length = 40\n").1, 16);
        assert_eq!(of("steps = [0.5]\nlength = 9\n").1, 9);
        assert_eq!(of("steps = [0.5]\nlength = 9\n").0[1..9], [0.0; 8]);
        assert_eq!(of("steps = [0.5, 0.25]\n").1, 2);
        // A rate is what the file says, and a step sequencer's is 8 and not an LFO's 1 if it says none.
        assert_eq!(of("rate = 0.5\n").2, 0.5);
        assert_eq!(of("").2, 8.0);
        let lfo = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\n";
        assert!(matches!(load(lfo).model.free_modulators[0].source, ModSourceSlot::Lfo { rate, .. } if rate == 1.0));
    }

    #[test]
    fn a_step_sequencers_values_that_are_not_finite_numbers_are_refused_with_their_place() {
        let Err(error) = try_load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\nsteps = [0.5, nan]\n") else { panic!("a value that is not a number is refused") };
        let error = format!("{error:#}");
        assert!(error.contains("instrument[0].modulator[0].steps[1]") && error.contains("is not a finite number"), "{error}");
    }

    #[test]
    fn a_macro_value_that_is_not_a_finite_number_is_refused_with_its_place() {
        let Err(error) = try_load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\nvalue = nan\n") else { panic!("a value that is not a number is refused") };
        let error = format!("{error:#}");
        assert!(error.contains("instrument[0].modulator[0].value") && error.contains("is not a finite number"), "{error}");
    }

    #[test]
    fn a_lane_and_a_group_whose_way_out_a_file_cuts_are_so_in_the_model_and_the_engine_and_the_others_are_not() {
        let run = load(
            "[[group]]\nroute = \"none\"\n[[group]]\n[[group]]\nroute = \"out\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nroute = \"none\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nroute = \"none\"\n",
        );
        let lanes: Vec<Route> = run.model.instruments.iter().map(|lane| lane.route).collect();
        assert_eq!(lanes, [Route::None, Route::Out, Route::None]);
        let groups: Vec<Route> = run.model.groups.iter().map(|group| group.route).collect();
        assert_eq!(groups, [Route::None, Route::Out, Route::Out]);
        // (The engine starts every lane and group with its way out, so only what a file cuts is sent.)
        let sent: Vec<&str> = run.commands.iter().map(String::as_str).filter(|c| c.contains("Route")).collect();
        assert_eq!(sent, ["SetGroupRoute group=0 route=none", "SetLaneRoute inst=0 route=none", "SetLaneRoute inst=2 route=none"]);
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_only_through_a_way_out_that_is_cut_is_loaded_when_it_is_cut_and_refused_when_it_is_not() {
        // The group's output to an effect of a lane that is in the group.
        let text = |route: &str| {
            format!(
                "[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n{route}\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n\
                 [[cable]]\nfrom = \"group[0]\"\nto = \"instrument[0].effect[0]\"\n"
            )
        };
        let error = try_load(&text("")).expect_err("a loop is not loaded").to_string();
        assert!(error.contains("cable[0]") && error.contains("go round"), "{error}");
        let run = load(&text("route = \"none\""));
        assert_eq!(run.model.cables.len(), 1);
        assert_eq!(run.model.instruments[0].route, Route::None);
        // The route is sent before the cable, which the engine would refuse otherwise.
        let sent: Vec<&str> = run.commands.iter().map(String::as_str).filter(|c| c.contains("Route") || c.starts_with("AddCable")).collect();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent[0].starts_with("SetLaneRoute") && sent[1].starts_with("AddCable"), "{sent:?}");
    }

    #[test]
    fn an_effect_whose_feed_a_file_cuts_is_so_in_the_model_and_the_engine_and_the_others_are_not() {
        let run = load(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[[group.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n",
        );
        let lanes: Vec<Vec<bool>> = run.model.instruments.iter().map(|lane| lane.effects.iter().map(|effect| effect.fed).collect()).collect();
        assert_eq!(lanes, [vec![false, true], vec![true]]);
        assert_eq!(run.model.groups[0].effects.iter().map(|effect| effect.fed).collect::<Vec<_>>(), [true, false]);
        // (The engine starts every effect with its feed, so only what a file cuts is sent, by the effect's node, after the effect is put in.)
        let cut = |lane: usize, fx: usize| run.model.instruments[lane].effects[fx].node.0;
        let bus_cut = run.model.groups[0].effects[1].node.0;
        let sent: Vec<&str> = run.commands.iter().map(String::as_str).filter(|c| c.contains("Feed") || c.starts_with("InsertGroupEffect") || c.starts_with("InsertEffect")).collect();
        assert_eq!(
            sent,
            [
                "InsertGroupEffect group=0 index=0 plugin=Reverb mix=1.000".to_string(),
                "InsertGroupEffect group=0 index=1 plugin=Filter mix=1.000".to_string(),
                format!("SetEffectFeed node={bus_cut} fed=false"),
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000".to_string(),
                format!("SetEffectFeed node={} fed=false", cut(0, 0)),
                "InsertEffect inst=0 index=1 plugin=Delay mix=1.000".to_string(),
                "InsertEffect inst=1 index=0 plugin=Filter mix=1.000".to_string(),
            ]
        );
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_only_through_a_chain_link_that_is_cut_is_loaded_when_it_is_cut_and_refused_when_it_is_not() {
        // An effect that sends back to the one before it in its own chain.
        let text = |feed: &str| {
            format!(
                "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\n{feed}\n\
                 [[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"instrument[0].effect[0]\"\n"
            )
        };
        let error = try_load(&text("")).expect_err("a loop is not loaded").to_string();
        assert!(error.contains("cable[0]") && error.contains("go round"), "{error}");
        let run = load(&text("feed = \"none\""));
        assert_eq!(run.model.cables.len(), 1);
        assert!(!run.model.instruments[0].effects[1].fed);
        // The cut is sent before the cable, which the engine would refuse otherwise.
        let sent: Vec<&str> = run.commands.iter().map(String::as_str).filter(|c| c.contains("Feed") || c.starts_with("AddCable")).collect();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent[0].starts_with("SetEffectFeed") && sent[1].starts_with("AddCable"), "{sent:?}");
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_only_through_a_link_into_a_bus_effect_that_is_cut_is_loaded_when_it_is_cut_and_refused_when_it_is_not() {
        // A bus effect that sends back into the lane that is in the group.
        let text = |feed: &str| {
            format!(
                "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n{feed}\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n\
                 [[cable]]\nfrom = \"group[0].effect[0]\"\nto = \"instrument[0].effect[0]\"\n"
            )
        };
        let error = try_load(&text("")).expect_err("a loop is not loaded").to_string();
        assert!(error.contains("cable[0]") && error.contains("go round"), "{error}");
        let run = load(&text("feed = \"none\""));
        assert_eq!(run.model.cables.len(), 1);
        assert!(!run.model.groups[0].effects[0].fed);
    }

    #[test]
    fn a_lane_and_a_group_that_a_file_mutes_or_solos_are_so_in_the_model_and_the_engine_and_the_others_are_not() {
        let run = load(
            "[[group]]\nmute = true\n[[group]]\nsolo = true\n[[group]]\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nmute = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nsolo = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nmute = true\nsolo = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n",
        );
        let lanes: Vec<(bool, bool)> = run.model.instruments.iter().map(|lane| (lane.muted, lane.solo)).collect();
        assert_eq!(lanes, [(true, false), (false, true), (true, true), (false, false)]);
        let groups: Vec<(bool, bool)> = run.model.groups.iter().map(|group| (group.muted, group.solo)).collect();
        assert_eq!(groups, [(true, false), (false, true), (false, false)]);
        // (The engine starts every lane and group neither muted nor soloed, so only what a file turns on is sent.)
        let sent: Vec<&str> = run.commands.iter().map(String::as_str).filter(|c| c.starts_with("SetMute") || c.starts_with("SetSolo") || c.starts_with("SetGroupMute") || c.starts_with("SetGroupSolo")).collect();
        assert_eq!(
            sent,
            [
                "SetGroupMute group=0 muted=true",
                "SetGroupSolo group=1 solo=true",
                "SetMute inst=0 muted=true",
                "SetSolo inst=1 solo=true",
                "SetMute inst=2 muted=true",
                "SetSolo inst=2 solo=true",
            ]
        );
    }

    #[test]
    fn groups_load_first_and_the_modulators_last() {
        let run = load(GROUPS);
        assert_eq!(
            without_ears(&run),
            testutil::told_all(&run.model, &[
                // Groups exist before any lane refers to them, with their bus effects.
                "AddGroup",
                "SetGroupVolume group=0 value=0.800",
                "SetGroupPan group=0 value=-0.300",
                "InsertGroupEffect group=0 index=0 plugin=Reverb mix=0.600",
                "SetGroupParameter group=0 fx=0 param=1 value=0.500",
                // Lanes: members join their group, the third lane stays ungrouped.
                "AddInstrument range=Some((12, 59))",
                "SetLaneGroup inst=0 group=Some(0)",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "SetParameter inst=0 slot=0 param=0 value=1.000",
                "AddInstrument range=None",
                "SetLaneGroup inst=1 group=Some(0)",
                "SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false",
                "InsertEffect inst=1 index=0 plugin=Filter mix=1.000",
                "AddInstrument range=None",
                "SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false",
                // The modulators come last, once every chain is known: the second lane's, and then the group's two. Two cables of the group's never resolve and are skipped: `param` without member / bus,
                // and a bus that does not exist.
                "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 3.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "InsertFreeModulator index=1 source=Lfo { waveform: Triangle, rate: 0.5, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=1 kind=LaneParam { lane: 0, slot: 0, param_index: 1 } depth=0.400 base=0.000 range=[-24.000, 24.000]",
                "AddFreeModTarget mod=1 kind=LaneParam { lane: 1, slot: 1, param_index: 0 } depth=-0.300 base=1.000 range=[0.000, 1.000]",
                "AddFreeModTarget mod=1 kind=BusParam { group: 0, effect_index: 0, param_index: 2 } depth=0.250 base=0.350 range=[0.000, 1.000]",
                "AddFreeModTarget mod=1 kind=ModulatorRate { mod_index: 0 } depth=0.200 base=3.000 range=[0.010, 50.000]",
                "InsertFreeModulator index=2 source=Envelope { attack: 0.02, decay: 0.4, sustain: 0.5, release: 1.0, state: Idle, level: 0.0, notes_held: 0 }",
                "AddFreeModTarget mod=2 kind=ModulatorRate { mod_index: 1 } depth=0.300 base=0.500 range=[0.010, 50.000]",
                "AddFreeModTarget mod=2 kind=ModulatorDepth { mod_index: 1, target_index: 2 } depth=0.100 base=0.250 range=[0.000, 1.000]",
            ])
        );
        // Progress covers the lanes' plugin slots only; the group's bus effect is not counted.
        assert_eq!(
            run.progress,
            [
                "loading builtin:osc", "loaded 1", "loading builtin:osc", "loaded 2",
                "loading builtin:filter", "loaded 3", "loading builtin:osc", "loaded 4",
            ]
        );
        let group = &run.model.groups[0];
        assert_eq!((group.name.as_deref(), group.volume, group.pan), (Some("Pad"), 0.8, -0.3));
        assert_eq!(group.effects.len(), 1);
        assert_eq!(run.model.free_modulators.len(), 3, "the second lane's, and the group's two");
        assert_eq!(run.model.free_modulators[1].targets.len(), 4, "two of the six targets were skipped");
        assert_eq!(run.model.instruments.iter().map(|l| l.group).collect::<Vec<_>>(), [Some(0), Some(0), None]);
    }

    #[test]
    fn each_groups_modulators_are_aimed_at_their_own_members() {
        // Group B's member 0 is lane 1 (the one with a filter), not lane 0.
        let run = load(
            r#"
[[group]]
name = "A"
[[group]]
name = "B"
[[group.modulator]]
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument.effect]]
plugin = "builtin:filter"
"#,
        );
        assert!(
            run.commands.contains(
                &testutil::told(&run.model, "AddFreeModTarget mod=0 kind=LaneParam { lane: 1, slot: 1, param_index: 0 } depth=0.400 base=1.000 range=[0.000, 1.000]")
            ),
            "{:#?}",
            run.commands
        );
        assert_eq!(run.model.free_modulators.len(), 1);
        assert_eq!(run.model.free_modulators[0].targets.len(), 1);
        assert_eq!(run.model.free_modulators[0].ear, Some(run.model.groups[1].node), "it hears the second group");
    }

    #[test]
    fn modulators_nested_under_effects_migrate_into_the_lane() {
        // Older sessions put modulators under an effect. They join the lane's rack after the
        // instrument's own, aimed at that effect (the target's own `slot` is ignored), and their
        // sibling indices are offset by where the effect's modulators start in the merged list.
        let run = load(LEGACY);
        assert_eq!(
            without_ears(&run),
            testutil::told_all(&run.model, &[
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000",
                "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 0, param_index: 1 } depth=0.100 base=0.000 range=[-24.000, 24.000]",
                "InsertFreeModulator index=1 source=Lfo { waveform: Sine, rate: 3.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=1 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.200 base=1.000 range=[0.000, 1.000]",
                "InsertFreeModulator index=2 source=Lfo { waveform: Sine, rate: 4.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=2 kind=ModulatorRate { mod_index: 1 } depth=0.300 base=3.000 range=[0.010, 50.000]",
            ])
        );
        let lane = &run.model.instruments[0];
        assert_eq!(run.model.free_modulators.len(), 3);
        assert_eq!(run.model.kind_of_target(ModulatorLocation::Free, 1, 0), Some(plugin::chain::ModTargetKind::FreeParam { plugin: lane.effects[0].node, param_index: 0 }));
    }

    #[test]
    fn a_position_in_the_session_is_the_place_of_the_node_it_is_on() {
        let model = load(PLACED).model;
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        let place = |node: NodeId| model.ui.get(&node).copied();
        let (group, lane) = (&model.groups[0], &model.instruments[0]);
        assert_eq!(model.master_ui(), at(1200.0, 300.5));
        assert_eq!(place(group.node), at(800.0, 40.0));
        assert_eq!(place(group.effects[0].node), at(1100.0, 40.0));
        assert_eq!(place(model.free_modulators[1].node), at(800.0, -120.0), "the group's modulator is the second of the session's own");
        assert_eq!(place(lane.node), at(0.0, 0.0));
        assert_eq!(place(lane.effects[0].node), at(376.0, 0.25));
        assert_eq!(place(model.free_modulators[0].node), at(376.0, -200.0));
        // The lane the session says nothing of has none, and nothing else is in the table.
        assert_eq!(place(model.instruments[1].node), None);
        assert_eq!(model.ui.len(), 7);
        // A session with no positions has no places.
        assert!(load(GROUPS).model.ui.is_empty());
    }

    /// The index the plugin of a slot has for the parameter of this name.
    fn index_of(slot: &PluginSlot, name: &str) -> u32 {
        slot.params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{} has no parameter {name}", slot.name)).index
    }

    #[test]
    fn the_parameters_a_session_pins_are_pinned_on_the_plugin_they_are_for_in_the_order_they_are_named() {
        let model = load(PINNED).model;
        let (group, lane, plain) = (&model.groups[0], &model.instruments[0], &model.instruments[1]);
        let osc = lane.instrument.as_ref().unwrap();
        assert_eq!(osc.pinned, [index_of(osc, "sustain"), index_of(osc, "detune"), index_of(osc, "waveform")]);
        // A name that the plugin has no parameter of, and a name twice, are no pins (and the order is that of the first time).
        let filter = &lane.effects[0];
        assert_eq!(filter.pinned, [index_of(filter, "resonance"), index_of(filter, "cutoff")]);
        let bus = &group.effects[0];
        assert_eq!(bus.pinned, [index_of(bus, "drive"), index_of(bus, "cutoff")]);
        // The lane the session says nothing of has none, and a session from before pins has none.
        assert!(plain.instrument.as_ref().unwrap().pinned.is_empty());
        assert!(load(GROUPS).model.instruments.iter().flat_map(|lane| lane.instrument.iter().chain(&lane.effects)).all(|slot| slot.pinned.is_empty()));
    }

    #[test]
    fn a_pin_for_a_parameter_the_plugin_hides_is_no_pin() {
        let mut run = load("[[instrument]]\nplugin = \"builtin:osc\"\npinned = [\"sustain\"]\n");
        let osc = run.model.instruments[0].instrument.as_mut().unwrap();
        // (What a plugin marks "(locked)" is not in the model's list of parameters, so no name of it finds one.)
        let sustain = index_of(osc, "sustain");
        assert_eq!(osc.pinned, [sustain], "the session pinned it");
        osc.params.retain(|p| p.name != "sustain");
        // (Pinning is saying which are pinned, not adding to them: the sustain that was pinned is not any more.)
        osc.pin_names(&["sustain".to_string(), "detune".to_string()]);
        assert_eq!(osc.pinned, [index_of(osc, "detune")], "no sustain (index {sustain}) any more");
    }

    #[test]
    fn a_separator_is_no_pin_since_it_is_no_control() {
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        let mut slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7));
        slot.params.push(ParamSlot { name: "Envelope".into(), index: 99, kind: ParamKind::Separator, ..Default::default() });
        slot.pin_names(&["Envelope".to_string(), "detune".to_string()]);
        assert_eq!(slot.pinned, [index_of(&slot, "detune")]);
    }

    #[test]
    fn a_name_that_two_parameters_have_pins_the_first_of_them() {
        // (As the name of a modulator's target is: some plugins name two parameters alike, and a file has names to go by.)
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        let mut slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7));
        let first = index_of(&slot, "detune");
        slot.params.push(ParamSlot { name: "detune".into(), index: 77, ..Default::default() });
        slot.pin_names(&["detune".to_string()]);
        assert_eq!(slot.pinned, [first]);
    }

    #[test]
    fn the_names_of_the_pins_are_the_names_of_the_parameters_pinned_in_the_order_they_were_pinned() {
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        let mut slot = fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7));
        slot.pinned = vec![index_of(&slot, "pan"), index_of(&slot, "attack")];
        assert_eq!(slot.pinned_names(), ["pan", "attack"]);
        // (An index the plugin has no parameter of names nothing.)
        slot.pinned.push(12_345);
        assert_eq!(slot.pinned_names(), ["pan", "attack"]);
    }

    #[test]
    fn a_plugin_that_is_just_loaded_has_nothing_pinned() {
        let mut osc = plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap();
        assert!(fresh_slot(osc.as_mut(), "builtin:osc", NodeId(7)).pinned.is_empty());
    }

    #[test]
    fn a_position_on_a_modulator_that_an_older_session_nested_under_an_effect_is_kept() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect.modulator]]
ui = { x = 5, y = 6 }
"#,
        );
        assert_eq!(run.model.ui.get(&run.model.free_modulators[0].node), Some(&NodeUi { x: 5.0, y: 6.0 }));
    }

    #[test]
    fn a_lane_with_no_plugin_still_has_the_modulators_the_file_gives_it() {
        // A lane the TUI cleared keeps its effects and its modulators, and a save writes them under the bare
        // `[[instrument]]`: the file has to give them back.
        let run = load(
            r#"
[[instrument]]
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator]]
type = "envelope"
"#,
        );
        assert_eq!(
            without_ears(&run),
            testutil::told_all(&run.model, &[
                "AddInstrument range=None",
                "InsertEffect inst=0 index=0 plugin=Filter mix=1.000",
                "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.400 base=1.000 range=[0.000, 1.000]",
                "InsertFreeModulator index=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }",
            ])
        );
        let lane = &run.model.instruments[0];
        assert!(lane.instrument.is_none());
        assert_eq!(run.model.free_modulators.len(), 2);
        assert_eq!(run.model.free_modulators[0].targets[0].param_name.as_str(), "cutoff");
        assert!(matches!(run.model.kind_of_target(ModulatorLocation::Free, 0, 0), Some(plugin::chain::ModTargetKind::FreeParam { plugin, .. }) if plugin == lane.effects[0].node));
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_to_a_modulators_rate_stage_or_depth_is_centred_on_what_the_file_has_there() {
        // m0 drives, one cable each, the rate of m1 (an LFO at 2 Hz), the four stages of m2 (an
        // envelope) and the depth of m1's first cable (0.4). A cable's modulation is centred on the
        // value it drives, and that is the value the file gives the field, not a default.
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
mod_rate = 1
[[instrument.modulator.target]]
mod_attack = 2
[[instrument.modulator.target]]
mod_decay = 2
[[instrument.modulator.target]]
mod_sustain = 2
[[instrument.modulator.target]]
mod_release = 2
[[instrument.modulator.target]]
mod_depth = [1, 0]
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.4
[[instrument.modulator]]
type = "envelope"
attack = 0.2
decay = 0.4
sustain = 0.6
release = 0.8
"#,
        );
        let base = |c: &String| c.split("base=").nth(1).unwrap().split(' ').next().unwrap().to_string();
        let bases: Vec<String> = run.commands.iter().filter(|c| c.starts_with("AddFreeModTarget mod=0 ")).map(base).collect();
        assert_eq!(bases, ["2.000", "0.200", "0.400", "0.600", "0.800", "0.400"]);
    }

    #[test]
    fn a_cable_to_a_modulator_the_file_does_not_have_is_left_out() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
mod_rate = 7
# The one after the last: the rack has one modulator, and its second is not there either.
[[instrument.modulator.target]]
mod_rate = 1
"#,
        );
        // (They would drive nothing, and the model has no cable that does: the audio thread is not told of them either.)
        assert!(run.commands.iter().all(|c| !c.contains("ModTarget")), "{:#?}", run.commands);
        assert!(run.model.free_modulators[0].targets.is_empty());
    }

    #[test]
    fn a_cable_that_names_what_comes_later_is_made_by_the_node_and_the_cable_it_names() {
        use crate::app::model::ModSetting;
        // The first modulator drives the rate of the second, the depth of the second's cable and the attack of the third: all of them come after it.
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
mod_rate = 1
[[instrument.modulator.target]]
mod_depth = [1, 0]
[[instrument.modulator.target]]
mod_attack = 2
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator]]
type = "envelope"
"#,
        );
        let own = &run.model.free_modulators;
        let (second, third) = (own[1].node, own[2].node);
        let aims: Vec<Driven> = own[0].targets.iter().map(|target| target.aim).collect();
        assert_eq!(
            aims,
            [
                Driven::Setting { modulator: second, field: ModSetting::Rate },
                Driven::Depth(own[1].targets[0].edge),
                Driven::Setting { modulator: third, field: ModSetting::Attack },
            ]
        );
        // The audio thread is told the nodes that they aim at: the second modulator, the first of its cables, and the third.
        let told: Vec<&String> = run.commands.iter().filter(|c| c.starts_with("AddFreeModTarget mod=0 ")).collect();
        assert_eq!(told.len(), 3);
        for (command, aim) in told.iter().zip(&aims) {
            assert!(command.contains(&format!("aim={aim:?} ")), "{told:?}");
        }
        for (m, modulator) in own.iter().enumerate() {
            for (t, _) in modulator.targets.iter().enumerate() {
                assert!(run.model.kind_of_target(ModulatorLocation::Free, m, t).is_some(), "modulator {m} cable {t}");
            }
        }
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_to_its_own_modulator_or_to_the_depth_of_a_cable_that_is_not_made_is_left_out_with_the_cables_that_drive_its_depth() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
# Its own rate: it does nothing.
[[instrument.modulator.target]]
mod_rate = 0
# The depth of the sixth cable of the second modulator, which has two.
[[instrument.modulator.target]]
mod_depth = [1, 5]
# The only one that is made.
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator]]
rate = 2.0
# A parameter that the plugin has not.
[[instrument.modulator.target]]
param = "no such parameter"
# The depth of the second cable of the first modulator, which is not made.
[[instrument.modulator.target]]
mod_depth = [0, 1]
[[instrument.modulator]]
rate = 3.0
# The depth of the second cable of the second modulator, which is not made.
[[instrument.modulator.target]]
mod_depth = [1, 1]
"#,
        );
        assert_eq!(run.model.free_modulators.iter().map(|m| m.targets.len()).collect::<Vec<_>>(), [1, 0, 0]);
        assert_eq!(run.commands.iter().filter(|c| c.contains("ModTarget")).count(), 1, "{:#?}", run.commands);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_that_drives_the_depth_of_a_cable_that_is_left_out_is_left_out_whatever_the_order_of_the_file() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
# The depth of the first cable of the second modulator, which is made as far as anyone can tell when this one is looked at...
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
mod_depth = [1, 0]
# ...and which drives the depth of the seventh cable of the third modulator, which has none: it is left out, and so, after it, is the one before.
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
mod_depth = [2, 6]
[[instrument.modulator]]
rate = 3.0
"#,
        );
        assert_eq!(run.model.free_modulators.iter().map(|m| m.targets.len()).collect::<Vec<_>>(), [0, 0, 0]);
        assert!(run.commands.iter().all(|c| !c.contains("ModTarget")), "{:#?}", run.commands);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_to_the_depth_of_a_cable_of_a_modulator_that_an_older_file_nested_in_an_effect_counts_the_modulators_of_the_effect() {
        // The lane's own modulator is the first of the merged list, the effect's two are the second and the third, and the third's cable names "the first cable of the first
        // modulator" of the effect: the second's.
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "detune"

[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect.modulator]]
rate = 3.0
[[instrument.effect.modulator.target]]
param = "cutoff"
[[instrument.effect.modulator]]
rate = 4.0
[[instrument.effect.modulator.target]]
mod_depth = [0, 0]
"#,
        );
        let own = &run.model.free_modulators;
        assert_eq!(own.iter().map(|m| m.targets.len()).collect::<Vec<_>>(), [1, 1, 1]);
        assert_eq!(own[2].targets[0].aim, Driven::Depth(own[1].targets[0].edge));
        run.model.check_ids().unwrap();
    }

    #[test]
    fn the_depth_of_a_cable_is_told_to_the_audio_thread_by_the_cable_that_is_made_and_not_by_its_place_in_the_file() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
# The first cable of the file is left out, so that the second is the first that the modulator has.
[[instrument.modulator.target]]
param = "no such parameter"
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
mod_depth = [0, 1]
"#,
        );
        let own = &run.model.free_modulators;
        assert_eq!(own.iter().map(|m| m.targets.len()).collect::<Vec<_>>(), [1, 1]);
        assert_eq!(own[1].targets[0].aim, Driven::Depth(own[0].targets[0].edge));
        let told: Vec<&String> = run.commands.iter().filter(|c| c.starts_with("AddFreeModTarget mod=1 ")).collect();
        assert_eq!(told.len(), 1, "{:#?}", run.commands);
        assert!(told[0].contains(&format!("aim={:?} ", Driven::Depth(own[0].targets[0].edge))), "{told:?}");
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_a_modulator_that_hears_a_group_to_what_a_member_does_not_have_is_left_out() {
        let run = load(
            r#"
[[group]]
[[group.modulator]]
rate = 1.0
# A modulator that the member has not.
[[group.modulator.target]]
member = 0
mod_rate = 3
# A cable that the member's modulator has not.
[[group.modulator.target]]
member = 0
mod_depth = [0, 4]
# The one that it has.
[[group.modulator.target]]
member = 0
mod_depth = [0, 0]
[[group.modulator.target]]
member = 0
param = "detune"
# A member that is not there.
[[group.modulator.target]]
member = 3
param = "detune"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "volume"
"#,
        );
        // (The lane's own is the first of the session's own, and the group's the second.)
        let group = &run.model.free_modulators[1];
        let member_cable = run.model.free_modulators[0].targets[0].edge;
        assert_eq!(group.targets.iter().map(|t| t.aim).collect::<Vec<_>>().len(), 2);
        assert_eq!(group.targets[0].aim, Driven::Depth(member_cable));
        assert!(matches!(group.targets[1].aim, Driven::Param { .. }));
        assert_eq!(run.commands.iter().filter(|c| c.starts_with("AddFreeModTarget")).count(), 3, "the lane's one and the group's two: {:#?}", run.commands);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn midi_modulators_rearm_for_learning_when_unbound() {
        let run = load(MIDI);
        assert_eq!(
            without_ears(&run),
            testutil::told_all(&run.model, &[
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertFreeModulator index=0 source=MidiLearn { source: Some(Cc(74)), value: 0.0, learning: false }",
                "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 0, param_index: 1 } depth=0.500 base=0.000 range=[-24.000, 24.000]",
                "InsertFreeModulator index=1 source=MidiLearn { source: None, value: 0.0, learning: true }",
                "InsertFreeModulator index=2 source=MidiLearn { source: Some(PitchBend), value: 0.5, learning: false }",
                "InsertFreeModulator index=3 source=MidiLearn { source: Some(Aftertouch), value: 0.0, learning: false }",
                "InsertFreeModulator index=4 source=MidiLearn { source: Some(Velocity), value: 0.0, learning: false }",
                "AddFreeModTarget mod=4 kind=LaneParam { lane: 0, slot: 0, param_index: 2 } depth=0.500 base=1.000 range=[0.000, 1.000]",
                &format!("InsertFreeModulator index=5 source=MidiLearn {{ source: Some(Key), value: {:?}, learning: false }}", 60.0f32 / 127.0),
            ])
        );
    }

    #[test]
    fn lane_and_effect_settings_become_commands_in_chain_order() {
        let run = load(SETTINGS);
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=Some((36, 83))",
                // Instrument: remapper present, host volume, parameter overrides (the unknown one is dropped).
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=true",
                "SetVolume inst=0 value=0.500",
                "SetParameter inst=0 slot=0 param=0 value=2.000",
                // Effects, each followed by its own parameter overrides (preset applied before insertion).
                "InsertEffect inst=0 index=0 plugin=Reverb mix=0.400",
                "SetParameter inst=0 slot=1 param=1 value=0.900",
                "InsertEffect inst=0 index=1 plugin=Filter mix=1.000",
                "SetParameter inst=0 slot=2 param=0 value=0.250",
                // Then transpose and the pattern (120000 / 90 bpm * 2 beats at 48 kHz = 64000 frames).
                "SetTranspose inst=0 semitones=-12",
                "SetPatternLength inst=0 beats=2.0",
                "SetPattern inst=0 events=2 length_samples=64000 base_note=Some(60) in_key=true",
                "SetPatternEnabled inst=0 enabled=true",
                "SetPatternLooping inst=0 looping=false",
                // A lane with no plugin is still a lane.
                "AddInstrument range=None",
                // The tempo is told once, when everything else has been (it is the first pattern's, 90).
                "SetGlobalBpm bpm=90.0",
            ]
        );
        let lane = &run.model.instruments[0];
        assert_eq!((lane.transpose, lane.volume, lane.pitch_bend_range), (-12, 0.5, 4.0));
        assert!(lane.remap.contains_key("G#4"));
        assert!(run.model.instruments[1].instrument.is_none());
        let osc = lane.instrument.as_ref().unwrap();
        let waveform = osc.params.iter().find(|p| p.name == "waveform").unwrap();
        assert_eq!(waveform.value, 2.0, "the override is the value");
        assert_eq!(waveform.default, 0.0, "the plugin's own value stays the baseline");
        assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 5), "enums keep their labels");
        let reverb = &lane.effects[0];
        let size = reverb.params.iter().find(|p| p.name == "size").unwrap();
        assert_eq!(size.value, 0.9);
        assert_ne!(size.default, 0.9, "the preset's value, not the override, is the baseline");
        assert_eq!(reverb.mix, 0.4);
        let untouched: Vec<_> = reverb.params.iter().filter(|p| p.name != "size").collect();
        assert!(!untouched.is_empty());
        assert!(
            untouched.iter().all(|p| p.value == p.default),
            "parameters the session does not override keep their baseline"
        );
        assert!(reverb.params.iter().any(|p| p.name == "decay" && p.value != 0.0));
        assert_eq!(run.progress.len(), 6, "three plugin slots, loading + loaded each");
        let pattern = lane.pattern.as_ref().unwrap();
        assert_eq!(
            (pattern.bpm, pattern.length_beats, pattern.looping, pattern.base_note, pattern.enabled, pattern.in_key),
            (90.0, 2.0, false, Some(60), true, true)
        );
        assert_eq!(pattern.events, [(0, 0x90, 60, 100), (24000, 0x80, 60, 0)]);
        assert!(!pattern.recording, "a loaded pattern is never mid-recording");
    }

    #[test]
    fn a_pattern_the_session_marks_disabled_is_loaded_disabled() {
        // The engine enables a pattern that has events as soon as it receives it, so the
        // loader has to say what the session wants. (The TUI used to paper over this by
        // re-sending the flag at startup; `play` did not.)
        let run = load(DISABLED_PATTERN);
        assert_eq!(
            run.commands,
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "SetPatternLength inst=0 beats=4.0",
                "SetPattern inst=0 events=2 length_samples=115200 base_note=Some(60) in_key=false",
                "SetPatternEnabled inst=0 enabled=false",
                "SetGlobalBpm bpm=100.0",
            ]
        );
    }

    // -- audio cables (P3c of docs/patch-graph.md §11.6) --

    /// `SENDS` with the cables `cables` after it.
    fn sends_with(cables: &str) -> String {
        format!("{}\n{cables}", crate::app::testutil::SENDS)
    }

    fn cable_commands(run: &Run) -> Vec<String> {
        run.commands.iter().filter(|command| command.contains("Cable")).cloned().collect()
    }

    #[test]
    fn the_cables_of_a_file_are_drawn_between_the_ends_it_names_in_the_order_of_the_file_under_the_ids_that_come_next() {
        let run = load(&sends_with(
            r#"
[[cable]]
from = "instrument[1]"
to = "instrument[0].effect[0]"

[[cable]]
from = "instrument[0].effect[1]"
to = "group[0]"
gain = 0.5

[[cable]]
from = "group[0]"
to = "instrument[1].effect[0]"
gain = 1.5

[[cable]]
from = "group[0].effect[0]"
to = "master"
"#,
        ));
        let n = crate::app::testutil::Sends::of(&run.model);
        use crate::plugin::chain::{CableFrom as F, CableTo as T};
        assert_eq!(
            run.model.cables.iter().map(|cable| (cable.id.0, cable.from, cable.to, cable.gain)).collect::<Vec<_>>(),
            [
                (0, F::Instrument(n.lane[1]), T::Effect(n.fx[0]), 1.0),
                (1, F::Effect(n.fx[1]), T::GroupBus(n.group), 0.5),
                (2, F::Group(n.group), T::Effect(n.fx[2]), 1.5),
                (3, F::Effect(n.bus), T::Master, 1.0),
            ]
        );
        run.model.check_ids().unwrap();
        // The audio thread is told each, after every lane and group is there, in the order of the file.
        assert_eq!(
            cable_commands(&run),
            [
                format!("AddCable id=0 from=Instrument({:?}) to=Effect({:?}) gain=1.000", n.lane[1], n.fx[0]),
                format!("AddCable id=1 from=Effect({:?}) to=GroupBus({:?}) gain=0.500", n.fx[1], n.group),
                format!("AddCable id=2 from=Group({:?}) to=Effect({:?}) gain=1.500", n.group, n.fx[2]),
                format!("AddCable id=3 from=Effect({:?}) to=Master gain=1.000", n.bus),
            ]
        );
        let last_lane_or_group = run.commands.iter().rposition(|command| command.starts_with("InsertEffect") || command.starts_with("InsertGroupEffect") || command.starts_with("AddInstrument")).unwrap();
        let first_cable = run.commands.iter().position(|command| command.starts_with("AddCable")).unwrap();
        assert!(first_cable > last_lane_or_group, "{:#?}", run.commands);
    }

    #[test]
    fn a_session_with_no_cables_sends_no_cable_command_and_its_ids_are_as_they_were() {
        let run = load(crate::app::testutil::SENDS);
        assert!(run.model.cables.is_empty());
        assert!(cable_commands(&run).is_empty());
    }

    #[test]
    fn the_cables_take_the_ids_after_the_modulation_cables_of_the_file() {
        let run = load(&format!(
            "{}{}",
            GROUPS,
            "\n[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\n[[cable]]\nfrom = \"instrument[1]\"\nto = \"master\"\n"
        ));
        let modulation: Vec<u32> = run.model.free_modulators.iter().flat_map(|m| m.targets.iter().map(|t| t.edge.0)).collect();
        let audio: Vec<u32> = run.model.cables.iter().map(|cable| cable.id.0).collect();
        assert_eq!(audio.len(), 2);
        assert!(modulation.iter().all(|id| !audio.contains(id)), "{modulation:?} {audio:?}");
        assert!(audio[0] > *modulation.iter().max().unwrap(), "after every modulation cable of the file: {modulation:?} {audio:?}");
        run.model.check_ids().unwrap();
    }

    #[test]
    fn the_level_of_a_cable_in_a_file_is_held_to_nothing_and_twice_that() {
        let run = load(&sends_with(
            "[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\ngain = 9.0\n[[cable]]\nfrom = \"instrument[1]\"\nto = \"master\"\ngain = -1.0\n",
        ));
        assert_eq!(run.model.cables.iter().map(|cable| cable.gain).collect::<Vec<_>>(), [2.0, 0.0]);
        assert_eq!(cable_commands(&run).iter().map(|command| command.rsplit(' ').next().unwrap().to_string()).collect::<Vec<_>>(), ["gain=2.000", "gain=0.000"]);
    }

    #[test]
    fn a_cable_that_the_session_cannot_have_refuses_the_session_with_the_place_of_the_cable() {
        for (cable, why) in [
            ("from = \"instrument[5]\"\nto = \"master\"", "cable[0].from: instrument[5] is not in the session"),
            ("from = \"instrument[0].effect[7]\"\nto = \"master\"", "cable[0].from: instrument[0].effect[7] is not in the session"),
            ("from = \"group[3]\"\nto = \"master\"", "cable[0].from: group[3] is not in the session"),
            ("from = \"group[0].effect[1]\"\nto = \"master\"", "cable[0].from: group[0].effect[1] is not in the session"),
            ("from = \"master\"\nto = \"instrument[0].effect[0]\"", "cable[0].from: master has no output of audio"),
            ("from = \"instrument[0]\"\nto = \"instrument[1]\"", "cable[0].to: instrument[1] has no input of audio"),
            ("from = \"instrument[0]\"\nto = \"group[9]\"", "cable[0].to: group[9] is not in the session"),
            ("from = \"instrument[0].effect[1]\"\nto = \"instrument[0].effect[0]\"", "cable[0]: that cable would make the audio go round"),
            ("from = \"group[0]\"\nto = \"instrument[0].effect[0]\"", "cable[0]: that cable would make the audio go round"),
        ] {
            let error = try_load(&sends_with(&format!("[[cable]]\n{cable}\n"))).expect_err(cable).to_string();
            assert!(error.contains(why), "{error}\nfor:\n{cable}");
        }
        // The second cable is the one that is named, and a cable between the same two places twice is refused.
        let error = try_load(&sends_with("[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\n[[cable]]\nfrom = \"instrument[9]\"\nto = \"master\"\n")).unwrap_err().to_string();
        assert!(error.contains("cable[1].from: instrument[9] is not in the session"), "{error}");
        let error = try_load(&sends_with("[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\n[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\n")).unwrap_err().to_string();
        assert!(error.contains("cable[1]") && error.contains("already"), "{error}");
    }

    #[test]
    fn a_cable_through_a_chain_of_cables_that_comes_back_is_a_loop_and_is_refused_where_it_closes() {
        let error = try_load(&sends_with(
            "[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"instrument[1].effect[0]\"\n[[cable]]\nfrom = \"instrument[1].effect[0]\"\nto = \"instrument[0].effect[1]\"\n[[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"instrument[0].effect[0]\"\n",
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("cable[2]") && error.contains("go round"), "{error}");
    }

    #[test]
    fn what_a_session_with_cables_is_saved_as_is_loaded_as_the_same_cables_and_the_same_file() {
        let text = sends_with(
            "[[cable]]\nfrom = \"instrument[1]\"\nto = \"instrument[0].effect[0]\"\n[[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"group[0]\"\ngain = 0.5\n[[cable]]\nfrom = \"group[0].effect[0]\"\nto = \"master\"\ngain = 1.5\n[[cable]]\nfrom = \"group[0]\"\nto = \"instrument[1].effect[0]\"\ngain = 0.75\n",
        );
        let model = load(&text).model;
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&model);
        let saved = session::to_toml(&instruments, &groups, &cables, &sources, None, model.master_ui(), model.bpm_to_save()).unwrap();
        assert_eq!(saved.matches("[[cable]]").count(), 4, "{saved}");
        assert!(saved.contains("from = \"group[0]\""), "a cable that starts at a group says which: {saved}");
        let again = load(&saved).model;
        assert_eq!(again.cables, model.cables, "the cables are the same, under the same ids");
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&again);
        assert_eq!(session::to_toml(&instruments, &groups, &cables, &sources, None, again.master_ui(), again.bpm_to_save()).unwrap(), saved, "and the file is the same");
    }

    // -- free modulators (`[[modulator]]`, P5b-2) --

    /// What a file's saved text is, as `Ctrl+S` writes it.
    fn saved(model: &SessionModel) -> String {
        let (instruments, groups, cables, sources) = crate::app::save::to_save(model);
        session::to_toml(&instruments, &groups, &cables, &sources, None, model.master_ui(), model.bpm_to_save()).unwrap()
    }

    // -- a second input on an effect (step 95) --

    #[test]
    fn a_cable_at_the_second_input_of_an_effect_is_loaded_told_to_the_audio_thread_and_saved_as_the_file_wrote_it() {
        use crate::plugin::chain::{CableFrom as F, CableTo as T};
        let run = load(testutil::SIDED);
        let n = testutil::Sided::of(&run.model);
        assert_eq!(run.model.cables.iter().map(|cable| (cable.id.0, cable.from, cable.to, cable.gain)).collect::<Vec<_>>(), [(0, F::Instrument(n.lane[1]), T::EffectSide(n.math), 1.0)]);
        run.model.check_ids().unwrap();
        assert_eq!(cable_commands(&run), [format!("AddCable id=0 from=Instrument({:?}) to=EffectSide({:?}) gain=1.000", n.lane[1], n.math)]);
        // The plugins say that they have a second input: the math effects have, the filter has not, and neither has an instrument.
        assert_eq!(run.model.plugin_slot(n.math).unwrap().side, Some(crate::plugin::SideInput { main: "A", side: "B" }));
        assert_eq!(run.model.plugin_slot(n.bus_math).unwrap().side, Some(crate::plugin::SideInput { main: "A", side: "B" }));
        assert_eq!((run.model.plugin_slot(n.filter).unwrap().side, run.model.plugin_slot(n.lane[0]).unwrap().side), (None, None));
        // It is written as it was read, and read again as the same cables and the same file.
        let text = saved(&run.model);
        assert!(text.contains("to = \"instrument[0].effect[1].side\""), "{text}");
        let again = load(&text).model;
        assert_eq!(again.cables, run.model.cables);
        assert_eq!(saved(&again), text);
    }

    #[test]
    fn a_cable_at_the_second_input_of_a_bus_effect_names_it_by_its_group_and_its_place_in_the_bus() {
        let text = format!("{}\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"group[0].effect[0].side\"\ngain = 0.5\n[[cable]]\nfrom = \"instrument[1]\"\nto = \"instrument[0].effect[1]\"\n", testutil::SIDED);
        let run = load(&text);
        let n = testutil::Sided::of(&run.model);
        use crate::plugin::chain::{CableFrom as F, CableTo as T};
        assert_eq!(
            run.model.cables.iter().map(|cable| (cable.from, cable.to, cable.gain)).collect::<Vec<_>>(),
            [(F::Instrument(n.lane[1]), T::EffectSide(n.math), 1.0), (F::Effect(n.filter), T::EffectSide(n.bus_math), 0.5), (F::Instrument(n.lane[1]), T::Effect(n.math), 1.0)],
            "a cable at the first input of an effect and one at its second, from the same lane, are two"
        );
        let saved = saved(&run.model);
        assert!(saved.contains("to = \"group[0].effect[0].side\"") && saved.contains("to = \"instrument[0].effect[1]\"\n"), "{saved}");
        assert_eq!(load(&saved).model.cables, run.model.cables);
    }

    #[test]
    fn a_second_input_that_the_session_cannot_have_refuses_the_session_with_the_place_of_the_cable() {
        for (cable, why) in [
            ("from = \"instrument[1]\"\nto = \"instrument[0].effect[0].side\"", "cable[1].to: instrument[0].effect[0].side: that effect has no second input"),
            ("from = \"instrument[1]\"\nto = \"instrument[0].effect[7].side\"", "cable[1].to: instrument[0].effect[7].side is not in the session"),
            ("from = \"instrument[1]\"\nto = \"group[0].effect[3].side\"", "cable[1].to: group[0].effect[3].side is not in the session"),
            // (A second effect on the bus, a reverb, which is put in after the cable's table: the tables of an array go to the one table of the group that is there.)
            ("from = \"instrument[1]\"\nto = \"group[0].effect[1].side\"\n[[group.effect]]\nplugin = \"builtin:reverb\"", "cable[1].to: group[0].effect[1].side: that effect has no second input"),
            ("from = \"instrument[0].effect[1].side\"\nto = \"master\"", "cable[1].from: instrument[0].effect[1].side is an input: a cable starts at an output of audio"),
            ("from = \"group[0].effect[0].side\"\nto = \"master\"", "cable[1].from: group[0].effect[0].side is an input"),
            ("from = \"instrument[0].effect[1]\"\nto = \"instrument[0].effect[1].side\"", "cable[1]: that cable would make the audio go round"),
            ("from = \"group[0].effect[0]\"\nto = \"instrument[0].effect[1].side\"\n[[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"group[0].effect[0].side\"", "cable[2]: that cable would make the audio go round"),
            ("from = \"instrument[1]\"\nto = \"instrument[0].effect[1].side\"", "cable[1]: there is a cable from instrument[1] to instrument[0].effect[1].side already"),
            ("from = \"instrument[1]\"\nto = \"instrument[0].side\"", "is not a place of the session"),
        ] {
            let error = try_load(&format!("{}\n[[cable]]\n{cable}\n", testutil::SIDED)).expect_err(cable).to_string();
            assert!(error.contains(why), "{error}\nfor:\n{cable}");
        }
    }

    #[test]
    fn a_free_modulators_cable_that_names_the_second_input_of_an_effect_is_left_out_as_one_that_names_what_is_no_plugin_is() {
        // (A free cable that names what is no plugin is loaded without, and the log says so: the session is loaded, with no cable of modulation.)
        let run = load(&format!("{}\n[[modulator]]\n[[modulator.target]]\nto = \"instrument[0].effect[1].side\"\nparam = \"mode\"\n", testutil::SIDED));
        assert!(run.model.free_modulators[0].targets.is_empty(), "the second input is not a plugin: it is one of its inputs");
    }

    #[test]
    fn the_free_modulators_of_a_file_are_the_sessions_own_with_cables_to_the_plugins_they_name_and_the_audio_thread_is_told_by_the_node() {
        use crate::plugin::chain::ModTargetKind as K;
        let run = load(testutil::FREE_MODULATORS);
        let model = &run.model;
        assert_eq!(model.free_modulators.len(), 2);
        assert!(matches!(model.free_modulators[0].source, ModSourceSlot::Lfo { rate, .. } if rate == 0.5));
        assert!(matches!(model.free_modulators[1].source, ModSourceSlot::Envelope { attack, .. } if attack == 0.05));
        let (osc, reverb, bus, filter) = (model.instruments[0].node, model.instruments[0].effects[0].node, model.groups[0].effects[0].node, model.instruments[1].effects[0].node);
        let kinds_of = |index: usize| testutil::kinds_at(model, ModulatorLocation::Free, index);
        let param_index = |plugin: &PluginSlot, name: &str| plugin.params.iter().find(|row| row.name == name).unwrap().index;
        assert_eq!(
            kinds_of(0),
            [
                K::FreeParam { plugin: osc, param_index: param_index(model.instruments[0].instrument.as_ref().unwrap(), "detune") },
                K::FreeParam { plugin: reverb, param_index: param_index(&model.instruments[0].effects[0], "damping") },
                K::FreeParam { plugin: bus, param_index: param_index(&model.groups[0].effects[0], "cutoff") },
                K::ModulatorAttack { mod_index: 1 },
            ]
        );
        assert_eq!(
            kinds_of(1),
            [K::FreeParam { plugin: filter, param_index: param_index(&model.instruments[1].effects[0], "cutoff") }, K::ModulatorDepth { mod_index: 0, target_index: 0 }]
        );
        // (Each cable is told to the audio thread after its modulator, in the order of the file, with the node that it drives.)
        let commands: Vec<&String> = run.commands.iter().filter(|c| c.contains("Free")).collect();
        let inserts: Vec<&&String> = commands.iter().filter(|c| c.starts_with("InsertFreeModulator")).collect();
        assert!(inserts[0].starts_with("InsertFreeModulator index=0 source=Lfo"), "{commands:?}");
        assert!(inserts[1].starts_with("InsertFreeModulator index=1 source=Envelope"), "{commands:?}");
        let cable_commands: Vec<&&String> = commands.iter().filter(|c| c.starts_with("AddFreeModTarget")).collect();
        assert_eq!(cable_commands.len(), 6, "{commands:?}");
        assert!(cable_commands[0].starts_with(&format!("AddFreeModTarget mod=0 aim=Param {{ plugin: NodeId({}), index:", osc.0)), "{cable_commands:?}");
        assert!(cable_commands[3].starts_with(&format!("AddFreeModTarget mod=0 aim=Setting {{ modulator: NodeId({}), field: Attack }}", model.free_modulators[1].node.0)), "{cable_commands:?}");
        assert!(cable_commands[5].starts_with(&format!("AddFreeModTarget mod=1 aim=Depth(EdgeId({}))", model.free_modulators[0].targets[0].edge.0)), "{cable_commands:?}");
        model.check_ids().unwrap();
    }

    #[test]
    fn a_free_cable_names_the_lane_and_the_group_it_drives_by_their_places_whichever_they_are() {
        // The second lane's instrument and effect and the second group's bus effect, which a place that was read as the first of each would not find.
        let model = load(
            r#"
[[modulator]]
[[modulator.target]]
to = "instrument[1]"
param = "detune"
[[modulator.target]]
to = "group[1].effect[0]"
param = "damping"
[[modulator.target]]
to = "instrument[1].effect[0]"
param = "damping"

[[group]]
[[group]]
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#,
        )
        .model;
        let aims: Vec<NodeId> = model.free_modulators[0]
            .targets
            .iter()
            .map(|t| match t.aim {
                Driven::Param { plugin, .. } => plugin,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(aims, [model.instruments[1].node, model.groups[1].effects[0].node, model.instruments[1].effects[0].node]);
    }

    #[test]
    fn the_free_modulators_take_the_nodes_after_the_lanes_and_the_groups_and_the_sources_the_nodes_after_them() {
        let run = load(&format!("[keys]\n{}", testutil::FREE_MODULATORS));
        let model = &run.model;
        let last_lane_node = model.instruments.iter().flat_map(|lane| std::iter::once(lane.node).chain(lane.effects.iter().map(|e| e.node))).max().unwrap();
        let last_bus_node = model.groups[0].effects.iter().map(|e| e.node).max().unwrap();
        assert!(model.free_modulators.iter().all(|m| m.node > last_lane_node.max(last_bus_node)), "after every lane and group");
        assert!(model.sources[0].node > model.free_modulators[1].node, "and before the sources, which are made when the model is");
        // A session with none uses no node for them: its sources are where they were.
        let without = load("[keys]\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert_eq!(without.sources[0].node, NodeId(without.instruments[0].node.0 + 1));
    }

    #[test]
    fn a_free_cable_is_centred_on_what_the_session_sets_the_parameter_it_drives_to() {
        let run = load(
            r#"
[[modulator]]
[[modulator.target]]
to = "instrument[0].effect[0]"
param = "damping"
depth = 0.5
[[modulator.target]]
to = "instrument[0]"
param = "detune"

[[instrument]]
plugin = "builtin:osc"
[instrument.params]
detune = 7.0
[[instrument.effect]]
plugin = "builtin:reverb"
[instrument.effect.params]
damping = 0.9
"#,
        );
        let model = &run.model;
        assert_eq!(model.centre_of(model.free_modulators[0].targets[0].aim), Some(0.9), "an override of the file");
        assert_eq!(model.centre_of(model.free_modulators[0].targets[1].aim), Some(7.0));
        let cables: Vec<&String> = run.commands.iter().filter(|c| c.starts_with("AddFreeModTarget")).collect();
        assert!(cables[0].contains("depth=0.500 base=0.900"), "{cables:?}");
        assert!(cables[1].contains("depth=0.500 base=7.000"), "a depth that the file does not say is half: {cables:?}");
    }

    #[test]
    fn a_free_cable_that_names_what_is_not_there_is_left_out_and_the_others_are_made() {
        let run = load(
            r#"
[[modulator]]
[[modulator.target]]
param = "damping"
depth = 0.1
[[modulator.target]]
to = "master"
param = "damping"
[[modulator.target]]
to = "group[0]"
param = "damping"
[[modulator.target]]
to = "instrument[7]"
param = "damping"
[[modulator.target]]
to = "instrument[2]"
param = "detune"
[[modulator.target]]
to = "instrument[0].effect[0]"
param = "no such parameter"
[[modulator.target]]
to = "not a place"
param = "damping"
[[modulator.target]]
mod_rate = 5
[[modulator.target]]
mod_rate = 0
[[modulator.target]]
to = "instrument[0].effect[0]"
param = "damping"
depth = 0.25

[[group]]
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument]]
"#,
        );
        let cables = &run.model.free_modulators[0].targets;
        assert_eq!(cables.len(), 1, "of ten, the one that names a plugin that is there and a parameter that it has");
        assert_eq!((cables[0].param_name.as_str(), cables[0].depth), ("damping", 0.25));
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_session_with_free_modulators_is_saved_as_the_same_modulators_and_cables_and_the_same_file() {
        let model = load(testutil::FREE_MODULATORS).model;
        let text = saved(&model);
        assert_eq!(text.matches("[[modulator]]").count(), 2, "{text}");
        assert!(text.contains("to = \"instrument[0].effect[0]\""), "a cable says which plugin by its place: {text}");
        assert!(text.contains("to = \"group[0].effect[0]\""), "{text}");
        assert!(text.find("[[group]]").unwrap() < text.find("[[modulator]]").unwrap(), "after the lanes and the groups that its cables drive: {text}");
        let again = load(&text).model;
        assert_eq!(again.free_modulators.len(), 2);
        for (a, b) in again.free_modulators.iter().zip(&model.free_modulators) {
            assert_eq!(format!("{:?}", a.source), format!("{:?}", b.source));
            let cable = |t: &ModTargetSlot| (t.param_name.clone(), t.depth, t.param_min, t.param_max);
            assert_eq!(a.targets.iter().map(cable).collect::<Vec<_>>(), b.targets.iter().map(cable).collect::<Vec<_>>());
        }
        assert_eq!(testutil::kinds_at(&again, ModulatorLocation::Free, 0).len(), 4);
        assert_eq!(saved(&again), text, "and the file is the same");
    }

    /// `GAINS` and a modulator of the session's own that has a cable to every value that a cable can drive in it (a lane's volume, the mix of three effects, a group's volume and pan), and the cables
    /// that a file can say and no session can have (the volume of a lane that has no instrument, a pan of a lane, a volume of an effect, a value that is not one, a place that is not there, no place):
    /// the loader leaves each of those out.
    fn gains_with_cables() -> String {
        format!(
            r#"{}
[[modulator]]
type = "lfo"
hears = "group[0]"

[[modulator.target]]
to = "instrument[0]"
gain = "volume"
depth = 0.3
[[modulator.target]]
to = "instrument[0].effect[0]"
gain = "mix"
depth = -0.4
[[modulator.target]]
to = "instrument[1].effect[0]"
gain = "mix"
[[modulator.target]]
to = "group[0].effect[0]"
gain = "mix"
depth = 0.1
[[modulator.target]]
to = "group[0]"
gain = "volume"
depth = 0.2
[[modulator.target]]
to = "group[0]"
gain = "pan"
depth = 0.6
[[modulator.target]]
to = "instrument[1]"
gain = "volume"
[[modulator.target]]
to = "instrument[0]"
gain = "pan"
[[modulator.target]]
to = "group[0].effect[0]"
gain = "volume"
[[modulator.target]]
to = "instrument[0]"
gain = "loudness"
[[modulator.target]]
to = "instrument[7]"
gain = "volume"
[[modulator.target]]
gain = "mix"
"#,
            testutil::GAINS
        )
    }

    #[test]
    fn a_free_cable_to_a_volume_a_mix_or_a_pan_is_centred_on_what_the_session_sets_it_to_and_one_that_names_nothing_that_has_one_is_left_out() {
        let run = load(&gains_with_cables());
        let model = &run.model;
        assert_eq!(
            modulators_described(model),
            [
                "LFO 1.0Hz sine hears group[0] [instrument[0] volume depth 0.300 centre Some(\"0.700\") range [0.000, 4.000]; instrument[0].effect[0] mix depth -0.400 centre Some(\"0.900\") range [0.000, 1.000]; \
                 instrument[1].effect[0] mix depth 0.500 centre Some(\"0.400\") range [0.000, 1.000]; group[0].effect[0] mix depth 0.100 centre Some(\"0.600\") range [0.000, 1.000]; \
                 group[0] volume depth 0.200 centre Some(\"0.800\") range [0.000, 4.000]; group[0] pan depth 0.600 centre Some(\"-0.250\") range [-1.000, 1.000]]"
            ]
        );
        // The audio thread is told each cable by the node and the value, in the order of the file.
        let told: Vec<&String> = run.commands.iter().filter(|c| c.starts_with("AddFreeModTarget")).collect();
        let (lane, group) = (model.instruments[0].node, model.groups[0].node);
        assert_eq!(told.len(), 6, "{told:?}");
        assert_eq!(told[0], &format!("AddFreeModTarget mod=0 aim={:?} depth=0.300 base=0.700 range=[0.000, 4.000]", Driven::Gain { node: lane, gain: ModGain::Volume }));
        assert_eq!(told[5], &format!("AddFreeModTarget mod=0 aim={:?} depth=0.600 base=-0.250 range=[-1.000, 1.000]", Driven::Gain { node: group, gain: ModGain::Pan }));
        model.check_ids().unwrap();
    }

    #[test]
    fn a_session_with_cables_to_volumes_mixes_and_pans_is_saved_as_the_same_cables_and_the_same_file() {
        let model = load(&gains_with_cables()).model;
        let text = saved(&model);
        assert_eq!(text.matches("gain = ").count(), 6, "the six that drive something: {text}");
        assert!(text.contains("to = \"group[0]\"\ngain = \"pan\""), "a group is a place of its own: {text}");
        assert!(text.contains("to = \"instrument[1].effect[0]\"\ngain = \"mix\""), "{text}");
        assert!(!text.contains("loudness") && !text.contains("instrument[7]"), "what was left out is not written: {text}");
        let again = load(&text).model;
        assert_eq!(modulators_described(&again), modulators_described(&model));
        assert_eq!(saved(&again), text, "and the file is the same");
    }

    /// Three modulators of the session's own, the first hearing the second lane, the second the group and the third every event.
    const FREE_EARS: &str = r#"
[[modulator]]
type = "envelope"
hears = "instrument[1]"
[[modulator]]
hears = "group[0]"
[[modulator]]

[[group]]
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_free_modulator_that_hears_a_lane_or_a_group_has_it_for_an_ear_and_the_audio_thread_is_told_the_node() {
        let run = load(FREE_EARS);
        let model = &run.model;
        assert_eq!(model.free_modulators[0].ear, Some(model.instruments[1].node));
        assert_eq!(model.free_modulators[1].ear, Some(model.groups[0].node));
        assert_eq!(model.free_modulators[2].ear, None);
        let told: Vec<&String> = run.commands.iter().filter(|c| c.starts_with("SetModulatorEar")).collect();
        assert_eq!(told, [&format!("SetModulatorEar Free index=0 ear={}", model.instruments[1].node.0), &format!("SetModulatorEar Free index=1 ear={}", model.groups[0].node.0)]);
        model.check_ids().unwrap();
    }

    #[test]
    fn a_free_modulators_ear_that_is_not_a_lane_or_a_group_of_the_session_is_no_ear_and_a_lanes_modulator_has_none() {
        for text in ["master", "keys", "device[0]", "instrument[7]", "group[3]", "instrument[0].effect[0]", "group[0].effect[0]", "somewhere", ""] {
            let run = load(&format!("[[modulator]]\nhears = {text:?}\n[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n"));
            assert_eq!(run.model.free_modulators[0].ear, None, "{text:?}");
            assert!(!run.commands.iter().any(|c| c.starts_with("SetModulatorEar")), "{text:?}");
        }
        // (A key that a lane's or a group's modulator has is not read: they hear their own.)
        let run = load("[[group]]\n[[group.modulator]]\nhears = \"instrument[0]\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.modulator]]\nhears = \"group[0]\"\n");
        assert_eq!(run.model.free_modulators[0].ear, Some(run.model.instruments[0].node), "the lane's hears the lane");
        assert_eq!(run.model.free_modulators[1].ear, Some(run.model.groups[0].node), "the group's hears the group");
        assert_eq!(run.commands.iter().filter(|c| c.starts_with("SetModulatorEar")).count(), 2);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn what_a_free_modulator_hears_is_saved_as_the_place_of_the_lane_or_the_group_and_loaded_again_and_a_lane_that_is_gone_is_every_event() {
        let model = load(FREE_EARS).model;
        let text = saved(&model);
        assert_eq!(text.matches("hears").count(), 2, "{text}");
        assert!(text.contains("hears = \"instrument[1]\"") && text.contains("hears = \"group[0]\""), "{text}");
        let again = load(&text).model;
        assert_eq!(again.free_modulators.iter().map(|m| m.ear.is_some()).collect::<Vec<_>>(), [true, true, false]);
        assert_eq!(again.free_modulators[0].ear, Some(again.instruments[1].node));
        assert_eq!(saved(&again), text, "and the file is the same");
        // A modulator that hears a node that is neither a lane nor a group that is there (a lane that was taken out, here a modulator) hears every event, and is saved so.
        let mut model = model;
        model.free_modulators[1].ear = Some(model.free_modulators[2].node);
        let text = saved(&model);
        assert_eq!(text.matches("hears").count(), 1, "{text}");
    }

    #[test]
    fn every_kind_of_free_modulator_is_saved_with_the_ear_that_it_has() {
        let mut text = String::new();
        for kind in ["lfo", "envelope", "midi", "macro", "steps"] {
            text += &format!("[[modulator]]\ntype = \"{kind}\"\nhears = \"instrument[0]\"\n");
        }
        text += "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let model = load(&text).model;
        assert_eq!(model.free_modulators.len(), 5);
        assert!(model.free_modulators.iter().all(|m| m.ear == Some(model.instruments[0].node)));
        let written = saved(&model);
        assert_eq!(written.matches("hears = \"instrument[0]\"").count(), 5, "{written}");
        assert_eq!(saved(&load(&written).model), written);
    }

    #[test]
    fn a_session_with_no_free_modulator_writes_no_table_for_one_and_the_place_of_a_free_modulator_is_kept() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert!(!saved(&model).contains("[[modulator]]"));
        let placed = load("[[modulator]]\nui = { x = 12.5, y = -40 }\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert_eq!(placed.ui.get(&placed.free_modulators[0].node), Some(&NodeUi { x: 12.5, y: -40.0 }));
        let text = saved(&placed);
        assert!(text.contains("ui = { x = 12.5, y = -40 }"), "saved as typed: {text}");
        assert_eq!(load(&text).model.ui.values().filter(|ui| **ui == NodeUi { x: 12.5, y: -40.0 }).count(), 1);
    }

    #[test]
    fn a_free_modulator_has_a_slot_for_its_output_and_for_where_each_cable_is_and_the_audio_thread_is_told_them() {
        let run = load(testutil::FREE_MODULATORS);
        let model = &run.model;
        for modulator in &model.free_modulators {
            assert!(modulator.output.is_some(), "its output is published");
            for cable in &modulator.targets {
                assert!(cable.effective.is_some(), "so is what each cable drives");
            }
        }
        assert!(run.telemetry.iter().any(|c| c.starts_with("SetModulatorOutput Free index=0")), "{:?}", run.telemetry);
        assert!(run.telemetry.iter().any(|c| c.starts_with("SetModulatorOutput Free index=1")), "{:?}", run.telemetry);
    }

    // -- cables of notes (P4b of docs/patch-graph.md §11.6) --

    use crate::app::model::NodePath;
    use crate::app::testutil::{Routed, SOURCES};

    #[test]
    fn the_sources_of_a_file_are_nodes_of_the_model_the_keys_first_and_then_the_devices_in_the_order_of_the_file() {
        let run = load(SOURCES);
        let n = Routed::of(&run.model);
        assert_eq!(
            run.model.sources.iter().map(|source| (source.node, source.kind.clone())).collect::<Vec<_>>(),
            [
                (n.keys, SourceKind::Keys),
                (n.devices[0], SourceKind::Device("Launchkey Mini MK4 37".to_string())),
                (n.devices[1], SourceKind::Device("Seaboard".to_string())),
            ]
        );
        // They are nodes of the model, found by their ids, with the places the file gave them, and the ids come after every lane's.
        assert_eq!(run.model.locate(n.keys), Some(NodePath::Source(0)));
        assert_eq!(run.model.locate(n.devices[1]), Some(NodePath::Source(2)));
        assert!(n.keys.0 > n.lane[2].0, "after the nodes of the lanes: {n:?}");
        assert_eq!((run.model.ui.get(&n.keys), run.model.ui.get(&n.devices[0]), run.model.ui.get(&n.devices[1])), (Some(&NodeUi { x: 0.0, y: 40.0 }), Some(&NodeUi { x: 0.0, y: 300.5 }), None));
        run.model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_of_notes_are_drawn_in_the_order_of_the_file_under_the_ids_that_the_audio_cables_have_too_and_the_audio_thread_is_told_nothing_of_them() {
        let run = load(SOURCES);
        let n = Routed::of(&run.model);
        assert_eq!(
            run.model.event_cables.iter().map(|cable| (cable.id.0, cable.from, cable.to)).collect::<Vec<_>>(),
            [(0, n.keys, n.lane[0]), (1, n.devices[0], n.lane[1]), (2, n.devices[1], n.lane[1])]
        );
        assert_eq!(run.model.cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [3], "the audio cable that follows is the next of the one series");
        assert_eq!(cable_commands(&run).len(), 1, "only the audio cable is sent: {:#?}", run.commands);
        assert!(run.commands.iter().all(|command| !command.contains("Source")), "{:#?}", run.commands);
        run.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_from_the_keys_makes_the_node_of_the_keys_when_the_file_has_no_table_for_them_and_a_session_without_either_has_none() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[cable]]\nfrom = \"keys\"\nto = \"instrument[0]\"\n");
        assert_eq!(run.model.sources.iter().map(|source| source.kind.clone()).collect::<Vec<_>>(), [SourceKind::Keys]);
        assert_eq!(run.model.event_cables.len(), 1);
        let none = load(crate::app::testutil::SENDS);
        assert!(none.model.sources.is_empty() && none.model.event_cables.is_empty(), "no node of a source until there is one in the file");
        // A table with nothing in it is the keys too, with no cable.
        let table = load("[keys]\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(table.model.sources.len(), 1);
        assert!(table.model.event_cables.is_empty());
    }

    #[test]
    fn a_session_with_no_source_has_the_ids_it_had() {
        let mut run = load(crate::app::testutil::SENDS);
        let max = run.model.nodes().map(|(node, _)| node.0).max().unwrap();
        assert_eq!(run.model.alloc_node().0, max + 1, "the next id is the one after the last node's: no id was spent on a source that is not there");
        assert_eq!(run.model.alloc_edge().0, 0, "and no edge id either (this session has no cable of any kind)");
    }

    #[test]
    fn a_cable_of_notes_that_the_session_cannot_have_refuses_the_session_with_the_place_of_the_cable() {
        let with = |cable: &str| format!("[[device]]\nname = \"A\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[group]]\n[[cable]]\n{cable}\n");
        for (cable, why) in [
            ("from = \"device[1]\"\nto = \"instrument[0]\"", "cable[0].from: device[1] is not in the session"),
            ("from = \"device[0]\"\nto = \"instrument[3]\"", "cable[0].to: instrument[3] is not in the session"),
            ("from = \"device[0]\"\nto = \"master\"", "cable[0].to: master has no input of notes: a cable of notes ends at a lane"),
            ("from = \"keys\"\nto = \"group[0]\"", "cable[0].to: group[0] has no input of notes"),
            ("from = \"keys\"\nto = \"device[0]\"", "cable[0].to: device[0] has no input of notes"),
            ("from = \"device[0]\"\nto = \"instrument[0].effect[0]\"", "cable[0].to: instrument[0].effect[0] has no input of notes"),
            ("from = \"instrument[0]\"\nto = \"keys\"", "cable[0].to: keys is a source of notes, which has no input of audio"),
            ("from = \"instrument[0]\"\nto = \"device[0]\"", "cable[0].to: device[0] is a source of notes, which has no input of audio"),
        ] {
            let error = try_load(&with(cable)).expect_err(cable).to_string();
            assert!(error.contains(why), "{error}\nfor:\n{cable}");
        }
        // The cable that is named is the one that is wrong, and a cable between the same two places twice is refused.
        let twice = "[[device]]\nname = \"A\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[cable]]\nfrom = \"device[0]\"\nto = \"instrument[0]\"\n[[cable]]\nfrom = \"device[0]\"\nto = \"instrument[0]\"\n";
        let error = try_load(twice).unwrap_err().to_string();
        assert!(error.contains("cable[1]: there is a cable from device[0] to instrument[0] already"), "{error}");
        // A cable from the keys with no table for them makes the node, and one from a device that no table makes is refused.
        let error = try_load("[[instrument]]\nplugin = \"builtin:osc\"\n[[cable]]\nfrom = \"device[0]\"\nto = \"instrument[0]\"\n").unwrap_err().to_string();
        assert!(error.contains("cable[0].from: device[0] is not in the session"), "{error}");
    }

    #[test]
    fn a_cable_of_notes_to_a_lane_whose_instrument_takes_no_notes_refuses_the_session_with_the_place_of_the_cable_and_the_lane_that_has_none_takes_one() {
        // The audio input has no input of events (step 99c): a file that draws a cable of notes to it is refused as the intent is, and the same cable to the lane beside it is made.
        let with = |lane: &str| format!("[keys]\n[[instrument]]\nplugin = \"builtin:input\"\nroute = \"none\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[cable]]\nfrom = \"keys\"\nto = \"{lane}\"\n");
        let error = try_load(&with("instrument[0]")).unwrap_err().to_string();
        assert!(error.contains("cable[0].to: instrument[0] takes no notes: an audio input has no input of events"), "{error}");
        assert_eq!(load(&with("instrument[1]")).model.event_cables.len(), 1);
        // The slot has what the plugin says, from the file as from the controller: the input takes none, the oscillator does, and a lane that has no instrument is one that may be given one.
        let run = load("[[instrument]]\nplugin = \"builtin:input\"\nroute = \"none\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n");
        let says: Vec<bool> = (0..3).map(|lane| run.model.lane_takes_notes(lane)).collect();
        assert_eq!(says, [false, true, true]);
        assert!(run.model.lane_takes_notes(9), "a lane that is not there takes notes, as it will when it is");
    }

    #[test]
    fn what_a_session_with_sources_is_saved_as_is_loaded_as_the_same_sources_cables_and_places_and_the_same_file() {
        let model = load(SOURCES).model;
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&model);
        let saved = session::to_toml(&instruments, &groups, &cables, &sources, None, model.master_ui(), model.bpm_to_save()).unwrap();
        assert!(saved.starts_with("[keys]\nui = { x = 0, y = 40 }\n\n[[device]]\nname = \"Launchkey Mini MK4 37\"\nui = { x = 0, y = 300.5 }\n\n[[device]]\nname = \"Seaboard\"\n\n[[instrument]]"), "{saved}");
        let order: Vec<&str> = saved.lines().filter(|line| line.starts_with("from = ")).collect();
        assert_eq!(order, ["from = \"keys\"", "from = \"device[0]\"", "from = \"device[1]\"", "from = \"instrument[2]\""], "the cables are in the order of their ids, the audio cable among them: {saved}");
        let again = load(&saved).model;
        assert_eq!(again.sources.iter().map(|source| &source.kind).collect::<Vec<_>>(), model.sources.iter().map(|source| &source.kind).collect::<Vec<_>>());
        assert_eq!(again.event_cables, model.event_cables, "the cables of notes are the same, under the same ids");
        assert_eq!(again.cables, model.cables);
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&again);
        assert_eq!(session::to_toml(&instruments, &groups, &cables, &sources, None, again.master_ui(), again.bpm_to_save()).unwrap(), saved, "and the file is the same");
    }

    // -- the modules that were added as one thing (`[[module]]`, step 108) --

    /// Two lanes that came with a module, an LFO that did, a lane that did not, and a second module of one lane.
    const MODULES: &str = r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument]]
route = "none"
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument]]
route = "none"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
route = "none"
[[instrument.effect]]
plugin = "builtin:delay"

[[modulator]]
hears = "instrument[1]"
[[modulator.target]]
to = "instrument[1].effect[0]"
param = "cutoff"

[[module]]
name = "Room"
members = ["instrument[1]", "instrument[2]", "modulator[0]"]
ui = { x = 40, y = 80.5 }

[[module]]
name = "Echo"
members = ["instrument[3]"]
"#;

    #[test]
    fn a_module_that_a_file_has_is_a_record_of_the_lanes_and_the_modulators_it_names_under_a_node_of_its_own_made_after_the_sources_and_the_cables() {
        let run = load(MODULES);
        let model = &run.model;
        let lanes: Vec<NodeId> = model.instruments.iter().map(|lane| lane.node).collect();
        let modulator = model.free_modulators[0].node;
        assert_eq!(model.modules.len(), 2);
        assert_eq!((model.modules[0].name.as_str(), model.modules[0].members.clone()), ("Room", vec![lanes[1], lanes[2], modulator]));
        assert_eq!((model.modules[1].name.as_str(), model.modules[1].members.clone()), ("Echo", vec![lanes[3]]));
        assert_eq!(model.ui.get(&model.modules[0].node), Some(&NodeUi { x: 40.0, y: 80.5 }), "where the box is drawn");
        assert_eq!(model.ui.get(&model.modules[1].node), None);
        let last = model.nodes().map(|(node, _)| node.0).max().unwrap();
        assert_eq!(model.modules[1].node.0, last, "the boxes have the last ids, so that a session that has none uses no id for them");
        assert!(model.modules[0].node < model.modules[1].node && model.modules.iter().all(|module| module.node > modulator));
        assert_eq!(model.locate(model.modules[1].node), Some(NodePath::Module(1)));
        assert!(run.commands.iter().all(|command| !command.contains("odule")), "the audio thread is told nothing of them: {:?}", run.commands);
        model.check_ids().unwrap();
        // A session that has none has none, and its ids are what they were.
        let without = load(&MODULES[..MODULES.find("[[module]]").unwrap()]).model;
        assert!(without.modules.is_empty());
        assert_eq!(without.nodes().map(|(node, _)| node.0).max(), Some(model.modules[0].node.0 - 1));
    }

    #[test]
    fn what_a_module_has_is_saved_as_it_is_loaded_the_same_record_and_place_and_the_same_file() {
        let model = load(MODULES).model;
        let text = saved(&model);
        assert!(text.contains("[[module]]\nname = \"Room\"\nmembers = [\"instrument[1]\", \"instrument[2]\", \"modulator[0]\"]\nui = { x = 40, y = 80.5 }\n\n[[module]]\nname = \"Echo\"\nmembers = [\"instrument[3]\"]\n"), "{text}");
        assert!(text.find("[[modulator]]").unwrap() < text.find("[[module]]").unwrap(), "after the modulators that it names: {text}");
        let again = load(&text).model;
        assert_eq!(again.modules.iter().map(|module| (module.name.clone(), module.members.len())).collect::<Vec<_>>(), [("Room".to_string(), 3), ("Echo".to_string(), 1)]);
        assert_eq!(again.ui.get(&again.modules[0].node), Some(&NodeUi { x: 40.0, y: 80.5 }));
        assert_eq!(saved(&again), text, "and the file is the same");
        // Where a member is is told by its place among the lanes and the modulators, which a lane that is taken out changes: the file that is saved after names the places that it has then.
        let mut fewer = load(MODULES).model;
        let gone = fewer.instruments[0].node;
        assert!(fewer.remove_lane(0).is_some() && gone != fewer.modules[0].node);
        let text = saved(&fewer);
        assert!(text.contains("members = [\"instrument[0]\", \"instrument[1]\", \"modulator[0]\"]") && text.contains("members = [\"instrument[2]\"]"), "{text}");
    }

    #[test]
    fn what_a_module_names_that_is_not_there_or_is_in_a_module_already_is_left_out_and_a_module_with_none_left_is_not_made_and_the_session_loads() {
        let lanes = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[modulator]]\n";
        let modules = |tables: &str| load(&format!("{lanes}{tables}")).model.modules.iter().map(|module| (module.name.clone(), module.members.len())).collect::<Vec<_>>();
        // A lane and a modulator that are not there.
        assert_eq!(modules("[[module]]\nname = \"A\"\nmembers = [\"instrument[0]\", \"instrument[7]\", \"modulator[3]\", \"modulator[0]\"]\n"), [("A".to_string(), 2)]);
        // A node that is in a module already is not in a second, and a node that is named twice is in once.
        assert_eq!(modules("[[module]]\nname = \"A\"\nmembers = [\"instrument[0]\", \"instrument[0]\"]\n[[module]]\nname = \"B\"\nmembers = [\"instrument[0]\", \"instrument[1]\"]\n"), [("A".to_string(), 1), ("B".to_string(), 1)]);
        // A module with no member that is there is not made; one with no name is called by the default; one with no members is not made.
        assert_eq!(modules("[[module]]\nname = \"A\"\nmembers = [\"instrument[9]\"]\n[[module]]\nmembers = [\"instrument[1]\"]\n[[module]]\nname = \"C\"\n"), [("Module".to_string(), 1)]);
        let model = load(&format!("{lanes}[[module]]\nname = \"A\"\nmembers = [\"instrument[9]\"]\n[[module]]\nmembers = [\"instrument[1]\"]\n")).model;
        model.check_ids().unwrap();
        assert_eq!(model.nodes().filter(|(_, path)| matches!(path, NodePath::Module(_))).count(), 1, "the one that was made, and no node was used for the one that was not");
    }

    /// Every `SetGlobalBpm` that a session sent the audio thread, as the tempo it says.
    fn tempo_commands(run: &Run) -> Vec<String> {
        run.commands.iter().filter(|command| command.starts_with("SetGlobalBpm")).cloned().collect()
    }

    // -- the output mix (step 102) --

    const OSC_LANE: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    fn file_of(model: &SessionModel) -> String {
        let (instruments, groups, cables, sources) = crate::app::save::to_save(model);
        session::to_toml(&instruments, &groups, &cables, &sources, None, model.master_ui(), model.bpm_to_save()).unwrap()
    }

    #[test]
    fn a_session_that_says_nothing_of_its_master_has_the_limiter_in_the_sound_and_the_audio_thread_is_told_so_once() {
        let run = load(OSC_LANE);
        assert_eq!(run.model.master, MasterNode { volume: 1.0, limiter: true, ceiling: -1.0 });
        assert_eq!(run.master, ["SetMaster volume=1.000 limiter=true ceiling=-1.0 rate=48000"]);
        assert!(run.commands.iter().all(|command| !command.contains("SetMaster")), "(it is not among the commands the other tests hold a load to)");
        // The same for an empty session, and for one that has a `[master]` table that says only what is already so.
        assert_eq!(load("").master, ["SetMaster volume=1.000 limiter=true ceiling=-1.0 rate=48000"]);
        assert_eq!(load(&format!("[master]\nlimiter = true\nvolume = 1.0\nceiling = -1.0\n{OSC_LANE}")).master.len(), 1);
    }

    #[test]
    fn a_master_table_is_what_the_session_has_and_is_told_whole_to_the_audio_thread() {
        let run = load(&format!("[master]\nvolume = 0.5\nceiling = -6.0\n{OSC_LANE}"));
        assert_eq!(run.model.master, MasterNode { volume: 0.5, limiter: true, ceiling: -6.0 }, "a key that is not there is what the master is without it");
        assert_eq!(run.master, ["SetMaster volume=0.500 limiter=true ceiling=-6.0 rate=48000"]);
        let run = load(&format!("[master]\nlimiter = false\nvolume = 1.5\n{OSC_LANE}"));
        assert_eq!(run.model.master, MasterNode { volume: 1.5, limiter: false, ceiling: -1.0 });
        assert_eq!(run.master, ["SetMaster volume=1.500 limiter=false ceiling=-1.0 rate=48000"]);
    }

    #[test]
    fn a_session_whose_master_is_what_the_audio_thread_starts_with_tells_it_nothing_of_it() {
        // (The engine starts with a volume of 1, no limiter and the default ceiling: it is told only what is another thing.)
        let run = load(&format!("[master]\nlimiter = false\n{OSC_LANE}"));
        assert!(!run.model.master.limiter);
        assert!(run.master.is_empty(), "{:?}", run.master);
        // A ceiling that is not the default is another thing, though the limiter is out of the sound: it is the one that it has when it is put in.
        let run = load(&format!("[master]\nlimiter = false\nceiling = -9.0\n{OSC_LANE}"));
        assert_eq!(run.master, ["SetMaster volume=1.000 limiter=false ceiling=-9.0 rate=48000"]);
    }

    #[test]
    fn a_master_that_a_file_says_over_what_the_intents_allow_is_held_to_it() {
        let run = load(&format!("[master]\nvolume = 9.0\nceiling = 5.0\n{OSC_LANE}"));
        assert_eq!((run.model.master.volume, run.model.master.ceiling), (MASTER_VOLUME_MAX, CEILING_MAX));
        let run = load(&format!("[master]\nvolume = -1.0\nceiling = -99.0\n{OSC_LANE}"));
        assert_eq!((run.model.master.volume, run.model.master.ceiling), (0.0, CEILING_MIN));
    }

    #[test]
    fn a_master_is_saved_as_the_keys_that_are_not_what_it_is_without_them_and_loads_the_same() {
        // Nothing of it is written for a master that is as it is without a table, so that the file of a session that never touched it is the file it was.
        assert!(!file_of(&load(OSC_LANE).model).contains("master"));
        assert!(!file_of(&load(&format!("[master]\nlimiter = true\n{OSC_LANE}")).model).contains("master"), "(what the file said that is the default is not kept)");
        let model = load(&format!("[master]\nvolume = 0.75\nlimiter = false\nceiling = -4.5\n{OSC_LANE}")).model;
        let text = file_of(&model);
        assert!(text.contains("[master]\nvolume = 0.75\nlimiter = false\nceiling = -4.5\n"), "{text}");
        let again = load(&text).model;
        assert_eq!(again.master, model.master);
        assert_eq!(file_of(&again), text, "and the file is the same");
        // Only the key that is not the default is written.
        let text = file_of(&load(&format!("[master]\nceiling = -2.0\n{OSC_LANE}")).model);
        assert!(text.contains("[master]\nceiling = -2.0\n") && !text.contains("limiter") && !text.contains("volume = "), "{text}");
    }

    #[test]
    fn the_tempo_of_a_session_is_its_bpm_and_the_audio_thread_is_told_it_once() {
        let run = load("bpm = 96.5\n[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(run.model.bpm, 96.5);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=96.5"]);
    }

    #[test]
    fn the_bpm_of_a_session_is_the_tempo_whatever_its_patterns_were_recorded_at() {
        // (SETTINGS has a pattern that was recorded at 90: it stays, and its length is counted in it.)
        let run = load(&format!("bpm = 100.0\n{SETTINGS}"));
        assert_eq!(run.model.bpm, 100.0);
        assert_eq!(run.model.instruments[0].pattern.as_ref().unwrap().bpm, 90.0);
        assert!(run.commands.contains(&"SetPattern inst=0 events=2 length_samples=64000 base_note=Some(60) in_key=true".to_string()), "{:#?}", run.commands);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=100.0"]);
    }

    #[test]
    fn the_audio_thread_is_told_the_first_patterns_tempo_once_when_there_is_no_bpm() {
        let lane = |bpm: &str| {
            format!("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n")
        };
        let run = load(&format!("{}{}", lane("90.0"), lane("140.0")));
        assert_eq!(run.model.bpm, 90.0);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=90.0"], "(it was the last pattern's, 140, that the audio thread was left with)");
        assert_eq!(run.model.instruments[1].pattern.as_ref().unwrap().bpm, 140.0, "each pattern keeps its own");
    }

    #[test]
    fn a_session_with_no_tempo_at_all_tells_the_audio_thread_none() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(run.model.bpm, 120.0);
        assert_eq!(tempo_commands(&run), Vec::<String>::new(), "it starts at 120");
    }

    #[test]
    fn a_pattern_recorded_at_a_tempo_there_cannot_be_gives_the_session_the_nearest_one_and_keeps_its_own() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = 5.0\nlength_beats = 4.0\nenabled = true\n");
        assert_eq!(run.model.bpm, 20.0);
        assert_eq!(run.model.instruments[0].pattern.as_ref().unwrap().bpm, 5.0);
        assert_eq!(tempo_commands(&run), ["SetGlobalBpm bpm=20.0"]);
    }

    #[test]
    fn the_bpm_of_a_file_is_held_to_the_tempos_there_can_be() {
        for (text, tempo) in [("0", 20.0), ("-5", 20.0), ("19.9", 20.0), ("20", 20.0), ("133.3", 133.3), ("300", 300.0), ("301", 300.0), ("1e30", 300.0)] {
            let run = load(&format!("bpm = {text}\n[[instrument]]\nplugin = \"builtin:osc\"\n"));
            assert_eq!(run.model.bpm, tempo, "bpm = {text}");
            assert_eq!(tempo_commands(&run), [format!("SetGlobalBpm bpm={tempo:?}")], "bpm = {text}");
        }
    }

    fn tempo_of(session: &str) -> f32 {
        load(session).model.bpm
    }

    #[test]
    fn the_tempo_starts_at_the_first_recorded_patterns() {
        let lane = |bpm: &str| {
            format!(
                "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n"
            )
        };
        let plain = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        // The first lane that has a pattern decides, not the first lane.
        assert_eq!(tempo_of(&format!("{plain}{}{}", lane("90.0"), lane("140.0"))), 90.0);
        assert_eq!(tempo_of(&lane("140.0")), 140.0);
    }

    #[test]
    fn the_tempo_is_120_without_a_recorded_pattern() {
        assert_eq!(tempo_of("[[instrument]]\nplugin = \"builtin:osc\"\n"), 120.0);
        assert_eq!(tempo_of(""), 120.0);
    }

    #[test]
    fn modulator_targets_that_do_not_resolve_are_skipped() {
        let run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "nope"
[[instrument.modulator.target]]
depth = 0.3
[[instrument.modulator.target]]
param = "detune"
slot = 4
"#,
        );
        assert_eq!(
            without_ears(&run),
            [
                "AddInstrument range=None",
                "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false",
                "InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }",
            ],
            "unknown parameter, no field at all, and a slot past the chain are all dropped"
        );
        assert!(run.model.free_modulators[0].targets.is_empty());
    }

    #[test]
    fn an_unknown_plugin_fails_the_load() {
        let err = try_load("[[instrument]]\nplugin = \"builtin:nope\"\n").expect_err("should fail");
        assert!(format!("{err:#}").contains("Unknown built-in plugin"), "{err:#}");
    }

    #[test]
    fn an_effect_with_other_outputs_than_the_chain_has_fails_the_load_and_the_error_says_where_it_is_and_why() {
        // (The audio thread turns such an effect away, so a model that kept it would show an effect that is not there, and the parameters of the effects after it
        // would be told to places the engine has not, or to another effect: f68.)
        let err = try_load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[instrument.effect]]\nplugin = \"builtin:mono-effect\"\n",
        )
        .expect_err("a chain of two channels has no place for it");
        assert_eq!(format!("{err:#}"), "instrument[0].effect[1]: MonoEffect has 1 audio output, and an effect here needs 2");

        // The place is the place in the file: the second lane's first effect.
        let err = try_load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:mono-effect\"\n",
        )
        .expect_err("the same in another lane");
        assert_eq!(format!("{err:#}"), "instrument[1].effect[0]: MonoEffect has 1 audio output, and an effect here needs 2");
    }

    #[test]
    fn a_bus_effect_with_other_outputs_than_the_chain_has_fails_the_load_the_same_way() {
        let err = try_load(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[[group]]\n[[group.effect]]\nplugin = \"builtin:filter\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\n\
             [[group.effect]]\nplugin = \"builtin:mono-effect\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n",
        )
        .expect_err("a group's bus is stereo too");
        // (The second group's third effect: the place of the group and the place of the effect are told apart.)
        assert_eq!(format!("{err:#}"), "group[1].effect[2]: MonoEffect has 1 audio output, and an effect here needs 2");
    }

    // -- Every modulator the session's own (step 94: `SessionConfig::move_modulators_to_the_session`) --

    /// Where the plugin `node` is, as a file names it (`instrument[0].effect[1]`), and not by the node, which a load that makes the modulators in another order gives another.
    fn plugin_place(model: &SessionModel, node: NodeId) -> String {
        match model.locate(node) {
            Some(NodePath::Instrument(lane)) => format!("instrument[{lane}]"),
            Some(NodePath::Effect { inst, index }) => format!("instrument[{inst}].effect[{index}]"),
            Some(NodePath::GroupEffect { group, index }) => format!("group[{group}].effect[{index}]"),
            other => format!("{other:?}"),
        }
    }

    /// Where the lane, the effect or the group `node`, whose volume, mix or pan a cable drives, is, as a file names it: a plugin's place, or `group[0]` for a group.
    fn gain_place(model: &SessionModel, node: NodeId) -> String {
        match model.locate(node) {
            Some(NodePath::Group(group)) => format!("group[{group}]"),
            _ => plugin_place(model, node),
        }
    }

    /// Every modulator of `model` in the order of the session's own rack (the lanes' that the move made, the groups', and the file's own), with whom it hears, what it is and what each of its
    /// cables drives (a plugin by its place, a modulator by its place in this order, and the depth of a cable by the modulator and the place of the cable), the depth it is at and what it
    /// is centred on: what a file says of a modulator, whether it says it in a lane, in a group or among the session's own.
    fn modulators_described(model: &SessionModel) -> Vec<String> {
        let all: Vec<(&ModulatorSlot, Option<String>)> =
            model.free_modulators.iter().map(|m| (m, model.ear_of(m).and_then(|ear| model.heard_place(ear)).map(|place| place.to_string()))).collect();
        let place_of = |node: NodeId| all.iter().position(|(m, _)| m.node == node).expect("a modulator of the session");
        let cable_at = |edge: EdgeId| all.iter().enumerate().find_map(|(at, (m, _))| m.targets.iter().position(|t| t.edge == edge).map(|target| format!("#{at}.{target}"))).expect("a cable of the session");
        all.iter()
            .map(|(m, hears)| {
                let cables: Vec<String> = m
                    .targets
                    .iter()
                    .map(|t| {
                        let drives = match t.aim {
                            Driven::Param { plugin, index } => format!("{}:{index} ({})", plugin_place(model, plugin), t.param_name),
                            Driven::Setting { modulator, field } => format!("#{} {field:?}", place_of(modulator)),
                            Driven::Depth(edge) => format!("depth of {}", cable_at(edge)),
                            Driven::Gain { node, gain } => format!("{} {}", gain_place(model, node), gain.name()),
                        };
                        format!("{drives} depth {:.3} centre {:?} range [{:.3}, {:.3}]", t.depth, model.centre_of(t.aim).map(|centre| format!("{centre:.3}")), t.param_min, t.param_max)
                    })
                    .collect();
                format!("{} hears {} [{}]", m.source.label(), hears.as_deref().unwrap_or("every note"), cables.join("; "))
            })
            .collect()
    }

    /// Sessions with modulators in their lanes and in their groups, in every form that a file has them in: what the rest of these tests load, an older file that nests them in the effects, cables
    /// between siblings, a group's cables to its members and its bus and to the modulators of its members, and cables that name what is not there.
    fn racks_of_every_kind() -> Vec<(&'static str, String)> {
        let nested = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 0.5
[[instrument.modulator.target]]
param = "detune"
depth = 0.5
[[instrument.modulator.target]]
param = "cutoff"
slot = 1
depth = 0.2
[[instrument.modulator]]
type = "envelope"
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.4
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.1
[[instrument.modulator.target]]
mod_rate = 9
depth = 0.9
[[instrument.modulator.target]]
mod_rate = 1
depth = 0.9
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect.modulator]]
rate = 3.0
[[instrument.effect.modulator.target]]
param = "cutoff"
slot = 7
depth = 0.2
[[instrument.effect.modulator]]
rate = 4.0
[[instrument.effect.modulator.target]]
mod_rate = 0
depth = 0.3
[[instrument.effect.modulator.target]]
mod_rate = 5
depth = 0.3
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
type = "steps"
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator]]
rate = 6.0
[[instrument.modulator.target]]
mod_rate = 0
"#
        .to_string();
        let with_free = format!(
            "{GROUPS}\n[[modulator]]\ntype = \"lfo\"\n[[modulator.target]]\nto = \"instrument[0]\"\nparam = \"detune\"\n[[modulator.target]]\nmod_rate = 1\n[[modulator]]\ntype = \"envelope\"\nhears = \"instrument[1]\"\n[[modulator.target]]\nmod_rate = 0\n"
        );
        vec![
            ("the groups' session", GROUPS.to_string()),
            ("modulators nested in an effect, and cables between siblings", nested),
            ("an older file", LEGACY.to_string()),
            ("controls that learn", MIDI.to_string()),
            ("modulators in the session's own rack too", with_free),
            ("a session that has none", "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string()),
        ]
    }

    #[test]
    fn every_modulator_of_a_moved_session_is_the_sessions_own_and_drives_what_it_drove_and_hears_what_its_rack_heard() {
        // What each file says, as the session has it once it is loaded: the modulators of the lanes first (in the order of the lanes, and for an older file's effects after the lane's own),
        // then the groups', then the file's own; each hears the lane or the group it was in, and its cables drive the plugin it named by its place (a member's, the bus's), a sibling by its place in
        // this order, and the depth of a cable by the modulator and the place of the cable. A cable that named what is not there, or the modulator itself, is not made.
        let expected: Vec<(&str, Vec<&str>)> = vec![
            ("the groups' session", vec![
                "LFO 3.0Hz sine hears instrument[1] []",
                "LFO 0.5Hz triangle hears group[0] [instrument[0]:1 (detune) depth 0.400 centre Some(\"0.000\") range [-24.000, 24.000]; instrument[1].effect[0]:0 (cutoff) depth -0.300 centre Some(\"1.000\") range [0.000, 1.000]; group[0].effect[0]:2 (damping) depth 0.250 centre Some(\"0.350\") range [0.000, 1.000]; #0 Rate depth 0.200 centre Some(\"3.000\") range [0.010, 50.000]]",
                "ADSR hears group[0] [#1 Rate depth 0.300 centre Some(\"0.500\") range [0.010, 50.000]; depth of #1.2 depth 0.100 centre Some(\"0.250\") range [0.000, 1.000]]",
            ]),
            ("modulators nested in an effect, and cables between siblings", vec![
                "LFO 0.5Hz sine hears instrument[0] [instrument[0]:1 (detune) depth 0.500 centre Some(\"0.000\") range [-24.000, 24.000]; instrument[0].effect[0]:0 (cutoff) depth 0.200 centre Some(\"1.000\") range [0.000, 1.000]]",
                "ADSR hears instrument[0] [#0 Rate depth 0.400 centre Some(\"0.500\") range [0.010, 50.000]; depth of #0.1 depth 0.100 centre Some(\"0.200\") range [0.000, 1.000]]",
                "LFO 3.0Hz sine hears instrument[0] [instrument[0].effect[0]:0 (cutoff) depth 0.200 centre Some(\"1.000\") range [0.000, 1.000]]",
                "LFO 4.0Hz sine hears instrument[0] [#2 Rate depth 0.300 centre Some(\"3.000\") range [0.010, 50.000]]",
                "Steps 16 8.0Hz hears instrument[1] [instrument[1]:1 (detune) depth 0.500 centre Some(\"0.000\") range [-24.000, 24.000]]",
                "LFO 6.0Hz sine hears instrument[1] [#4 Rate depth 0.500 centre Some(\"8.000\") range [0.010, 50.000]]",
            ]),
            ("an older file", vec![
                "LFO 2.0Hz sine hears instrument[0] [instrument[0]:1 (detune) depth 0.100 centre Some(\"0.000\") range [-24.000, 24.000]]",
                "LFO 3.0Hz sine hears instrument[0] [instrument[0].effect[0]:0 (cutoff) depth 0.200 centre Some(\"1.000\") range [0.000, 1.000]]",
                "LFO 4.0Hz sine hears instrument[0] [#1 Rate depth 0.300 centre Some(\"3.000\") range [0.010, 50.000]]",
            ]),
            ("controls that learn", vec![
                "CC 74 hears instrument[0] [instrument[0]:1 (detune) depth 0.500 centre Some(\"0.000\") range [-24.000, 24.000]]",
                "MIDI (learning…) hears instrument[0] []",
                "Pitch Bend hears instrument[0] []",
                "Aftertouch hears instrument[0] []",
                "Velocity hears instrument[0] [instrument[0]:2 (volume) depth 0.500 centre Some(\"1.000\") range [0.000, 1.000]]",
                "Key hears instrument[0] []",
            ]),
            ("modulators in the session's own rack too", vec![
                "LFO 3.0Hz sine hears instrument[1] []",
                "LFO 0.5Hz triangle hears group[0] [instrument[0]:1 (detune) depth 0.400 centre Some(\"0.000\") range [-24.000, 24.000]; instrument[1].effect[0]:0 (cutoff) depth -0.300 centre Some(\"1.000\") range [0.000, 1.000]; group[0].effect[0]:2 (damping) depth 0.250 centre Some(\"0.350\") range [0.000, 1.000]; #0 Rate depth 0.200 centre Some(\"3.000\") range [0.010, 50.000]]",
                "ADSR hears group[0] [#1 Rate depth 0.300 centre Some(\"0.500\") range [0.010, 50.000]; depth of #1.2 depth 0.100 centre Some(\"0.250\") range [0.000, 1.000]]",
                "LFO 1.0Hz sine hears every note [instrument[0]:1 (detune) depth 0.500 centre Some(\"0.000\") range [-24.000, 24.000]; #4 Rate depth 0.500 centre None range [0.010, 50.000]]",
                "ADSR hears instrument[1] [#3 Rate depth 0.500 centre Some(\"1.000\") range [0.010, 50.000]]",
            ]),
            ("a session that has none", vec![
            ]),
        ];
        let sessions = racks_of_every_kind();
        assert_eq!(sessions.len(), expected.len());
        for ((what, text), (named, lines)) in sessions.iter().zip(&expected) {
            assert_eq!(what, named);
            let run = load(text);
            run.model.check_ids().unwrap();
            assert_eq!(modulators_described(&run.model), *lines, "{what}");
        }
    }

    #[test]
    fn a_session_that_still_has_the_modulators_of_a_lane_an_effect_or_a_group_is_refused_with_the_place_and_the_same_file_moved_loads() {
        // (`session::parse` moves them; a file that is read as it is written (`parse_raw`) is not loaded: they would be left out without a word.)
        let lane = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\n";
        let effect = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect.modulator]]\n";
        let group = "[[group]]\n[[group.modulator]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n";
        for (text, place) in [(lane, "instrument[0]"), (effect, "instrument[0]"), (group, "group[0]")] {
            let config = session::parse_raw(text).unwrap();
            let (tx, _rx) = crossbeam_channel::unbounded();
            let err = load_session(&config, Path::new("."), 48_000.0, 256, &plugin::Runtime::default(), &tx, &mut |_| {}).expect_err("the modulators are still in a table of a lane or a group");
            let said = format!("{err:#}");
            assert!(said.starts_with(&format!("{place} has modulators of its own")), "{said}");
            assert!(try_load(text).is_ok(), "moved, the same file loads: {text}");
        }
    }

    #[test]
    fn a_moved_modulator_hears_the_node_of_the_lane_or_the_group_it_was_in_and_the_audio_thread_is_told_with_it() {
        let moved = load(GROUPS);
        let ear = |index: usize| moved.model.free_modulators[index].ear;
        // The group's two modulators come first in the order of the racks' (the lanes have one, in the second lane: the lanes' are first).
        assert_eq!(ear(0), Some(moved.model.instruments[1].node), "the second lane's own");
        assert_eq!(ear(1), Some(moved.model.groups[0].node));
        assert_eq!(ear(2), Some(moved.model.groups[0].node));
        let told: Vec<&String> = moved.commands.iter().filter(|c| c.starts_with("SetModulatorEar")).collect();
        assert_eq!(told.len(), 3, "{told:?}");
        assert_eq!(told[0], &format!("SetModulatorEar Free index=0 ear={}", moved.model.instruments[1].node.0));
        assert_eq!(told[1], &format!("SetModulatorEar Free index=1 ear={}", moved.model.groups[0].node.0));
        assert_eq!(moved.commands.iter().filter(|c| c.starts_with("InsertFreeModulator")).count(), 3, "{:?}", moved.commands);
    }

    #[test]
    fn a_moved_session_is_saved_as_the_sessions_own_modulators_and_loaded_again_as_the_same_session() {
        for (what, text) in racks_of_every_kind() {
            let moved = load(&text);
            let saved = saved(&moved.model);
            assert!(!saved.contains("instrument.modulator") && !saved.contains("group.modulator"), "{what}: {saved}");
            assert_eq!(saved.contains("[[modulator]]"), !moved.model.free_modulators.is_empty(), "{what}");
            // (A file that is saved is not moved again: what it has is the session's own.)
            let again = load(&saved);
            assert_eq!(modulators_described(&again.model), modulators_described(&moved.model), "{what}");
            assert_eq!(self::saved(&again.model), saved, "{what}: the same file again");
        }
    }

    #[test]
    fn a_closed_command_channel_fails_the_load() {
        let config = session::parse(include_str!("../../examples/basic.toml")).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        drop(rx);
        let err = load_session(
            &config,
            Path::new("."),
            48_000.0,
            256,
            &plugin::Runtime::default(),
            &tx,
            &mut |_| {},
        )
        .expect_err("should fail");
        assert!(format!("{err:#}").contains("command channel closed"), "{err:#}");
    }
}
