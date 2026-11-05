//! Saving a session: turn the model back into what a session file holds.
//!
//! The mirror of `app::load`. The loader reads a file into the model; `to_save`
//! reads the model into the plain structs `session::save` writes. Nothing here
//! touches the disk or the terminal, so any front-end (and a test) can serialize
//! a session.

use crate::app::model::{EdgeId, InstrumentNode, ModSourceSlot, ModulatorSlot, NodeId, ParamSlot, PluginSlot, SessionModel, SourceKind, sync_name};
use crate::plugin::chain::{CableFrom, CableTo, MidiBindSource, ModTargetKind, ModulatorLocation};
use crate::session::{
    CablePlace, Feed, MemberPlace, NodeUi, SaveCable, SaveDevice, SaveEffect, SaveGroup, SaveInstrument, SaveInstrumentSlot, SaveKeys, SaveModSource, SaveModTarget, SaveModule, SaveModulator,
    SavePattern, SaveSources,
};
use std::collections::{BTreeMap, HashMap};

/// The lanes and groups of a session as `session::save` writes them, with where each node is drawn, its cables (the audio cables and the cables of notes) and its sources of notes. The piano
/// scale is not part of the model; it goes to `save` separately, and the master's place is `SessionModel::master_ui`.
pub fn to_save(model: &SessionModel) -> (Vec<SaveInstrumentSlot>, Vec<SaveGroup>, Vec<SaveCable>, SaveSources) {
    let ui = &model.ui;
    let save_groups = model
        .groups
        .iter()
        .map(|g| SaveGroup {
            name: g.name.clone(),
            volume: g.volume,
            pan: g.pan,
            muted: g.muted,
            route: g.route,
            solo: g.solo,
            effects: g.effects.iter().map(|fx| effect_to_save(fx, ui)).collect(),
            ui: ui.get(&g.node).copied(),
        })
        .collect();
    let save_instruments = model
        .instruments
        .iter()
        .map(|lane| SaveInstrumentSlot {
            range: lane.range,
            transpose: lane.transpose,
            muted: lane.muted,
            route: lane.route,
            solo: lane.solo,
            group: lane.group,
            instrument: lane.instrument.as_ref().map(|inst| SaveInstrument {
                plugin: inst.id.clone(),
                volume: lane.volume,
                preset: inst.current_preset.clone(),
                params: overrides(&inst.params),
                pinned: inst.pinned_names(),
                state: inst.state.clone(),
                pitch_bend_range: lane.pitch_bend_range,
                remap: lane.remap.clone(),
            }),
            effects: lane.effects.iter().map(|fx| effect_to_save(fx, ui)).collect(),
            ui: ui.get(&lane.node).copied(),
            pattern: lane.pattern.as_ref().map(|p| SavePattern {
                bpm: p.bpm,
                length_beats: p.length_beats,
                looping: p.looping,
                base_note: p.base_note,
                events: p.events.clone(),
                enabled: p.enabled,
                in_key: p.in_key,
            }),
        })
        .collect();
    (save_instruments, save_groups, cables_to_save(model), sources_to_save(model))
}

/// The sources of notes of a session: the keys, if it has them, and its devices in the order that the file's cables name them in, each with where it is drawn.
fn sources_to_save(model: &SessionModel) -> SaveSources {
    let ui = |node: NodeId| model.ui.get(&node).copied();
    SaveSources {
        keys: model.sources.iter().find(|source| source.kind == SourceKind::Keys).map(|source| SaveKeys { ui: ui(source.node) }),
        devices: model
            .sources
            .iter()
            .filter_map(|source| match &source.kind {
                SourceKind::Device(name) => Some(SaveDevice { name: name.clone(), ui: ui(source.node) }),
                SourceKind::Keys => None,
            })
            .collect(),
        modulators: modulators_to_save(model, ModulatorLocation::Free),
        modules: modules_to_save(model),
        master: model.master_to_save(),
    }
}

/// The modules that were added as one thing (step 108), each naming the lanes and the modulators that came with it by their places among the `[[instrument]]` and the `[[modulator]]` tables, which is how the
/// loader finds them again. A member that is not a lane or a modulator that is there (the model keeps none) is left out.
fn modules_to_save(model: &SessionModel) -> Vec<SaveModule> {
    model
        .modules
        .iter()
        .map(|module| SaveModule {
            name: module.name.clone(),
            members: module
                .members
                .iter()
                .filter_map(|node| {
                    model
                        .instruments
                        .iter()
                        .position(|lane| lane.node == *node)
                        .map(MemberPlace::Lane)
                        .or_else(|| model.free_modulators.iter().position(|modulator| modulator.node == *node).map(MemberPlace::Modulator))
                })
                .collect(),
            ui: model.ui.get(&module.node).copied(),
        })
        .collect()
}

/// Where the effect in `node` is, as the file names a place: an effect of a lane's chain or of a group's bus.
fn effect_place_of(model: &SessionModel, node: NodeId) -> Option<CablePlace> {
    model
        .instruments
        .iter()
        .enumerate()
        .find_map(|(instrument, lane)| lane.effects.iter().position(|effect| effect.node == node).map(|effect| CablePlace::Effect { instrument, effect }))
        .or_else(|| model.groups.iter().enumerate().find_map(|(group, bus)| bus.effects.iter().position(|effect| effect.node == node).map(|effect| CablePlace::BusEffect { group, effect })))
}

/// Where the lane in `node` is, as the file names a place (a lane's instrument shares its node).
fn lane_place_of(model: &SessionModel, node: NodeId) -> Option<CablePlace> {
    model.instruments.iter().position(|lane| lane.node == node).map(CablePlace::Instrument)
}

/// Where the plugin in `node` is, as the file names a place: a lane's instrument, an effect of a lane's chain or of a group's bus. It is how a modulator's cable names what it drives.
fn plugin_place_of(model: &SessionModel, node: NodeId) -> Option<CablePlace> {
    lane_place_of(model, node).or_else(|| effect_place_of(model, node))
}

/// The cables of a session as the file names their places: by the place of a lane, a group or an effect among those of the session, the keys or a device among the sources, in the order of
/// the cables' ids (an audio cable and a cable of notes are of one series).
fn cables_to_save(model: &SessionModel) -> Vec<SaveCable> {
    let effect_place = |node: NodeId| effect_place_of(model, node);
    let lane_place = |node: NodeId| lane_place_of(model, node);
    let group_place = |node: NodeId| model.groups.iter().position(|group| group.node == node).map(CablePlace::Group);
    let source_place = |node: NodeId| {
        let at = model.sources.iter().position(|source| source.node == node)?;
        match model.sources[at].kind {
            SourceKind::Keys => Some(CablePlace::Keys),
            SourceKind::Device(_) => Some(CablePlace::Device(model.sources[..at].iter().filter(|source| matches!(source.kind, SourceKind::Device(_))).count())),
        }
    };
    let audio = model.cables.iter().filter_map(|cable| {
        let from = match cable.from {
            CableFrom::Instrument(node) => lane_place(node),
            CableFrom::Effect(node) => effect_place(node),
            CableFrom::Group(node) => group_place(node),
        };
        let to = match cable.to {
            CableTo::Effect(node) => effect_place(node),
            CableTo::EffectSide(node) => effect_place(node).and_then(|place| match place {
                CablePlace::Effect { instrument, effect } => Some(CablePlace::EffectSide { instrument, effect }),
                CablePlace::BusEffect { group, effect } => Some(CablePlace::BusEffectSide { group, effect }),
                _ => None,
            }),
            CableTo::GroupBus(node) => group_place(node),
            CableTo::Master => Some(CablePlace::Master),
        };
        // (A cable with an end that is not in the session is not written: the controller takes a cable out with what it is at, so this is not met.)
        Some((cable.id, SaveCable { from: from?, to: to?, gain: cable.gain }))
    });
    // (A cable of notes has no level: its `gain` is the 1 that is not written.)
    let notes = model.event_cables.iter().filter_map(|cable| Some((cable.id, SaveCable { from: source_place(cable.from)?, to: lane_place(cable.to)?, gain: 1.0 })));
    let mut cables: Vec<(EdgeId, SaveCable)> = audio.chain(notes).collect();
    cables.sort_by_key(|(id, _)| *id);
    cables.into_iter().map(|(_, cable)| cable).collect()
}

/// The parameters whose value differs from the baseline the loader recorded (the
/// plugin's own value once its preset is applied): what a file keeps as `params`.
fn overrides(params: &[ParamSlot]) -> Vec<(String, f32)> {
    params
        .iter()
        .filter(|p| (p.value - p.default).abs() > f32::EPSILON)
        .map(|p| (p.name.clone(), p.value))
        .collect()
}

/// The instrument of a lane as a module keeps it (step 105): the plugin with its preset, the values that are not its own and the knobs pinned to its node, and the lane's volume. What is the lane's note remap, and
/// the bend range that goes with it, is of the keyboard that plays it and not of the sound: a module has the defaults.
pub(super) fn instrument_to_save(lane: &InstrumentNode, plugin: &PluginSlot) -> SaveInstrument {
    SaveInstrument { plugin: plugin.id.clone(), volume: lane.volume, preset: plugin.current_preset.clone(), params: overrides(&plugin.params), pinned: plugin.pinned_names(), state: plugin.state.clone(), pitch_bend_range: 2.0, remap: HashMap::new() }
}

pub(super) fn effect_to_save(fx: &PluginSlot, ui: &BTreeMap<NodeId, NodeUi>) -> SaveEffect {
    SaveEffect {
        plugin: fx.id.clone(),
        mix: fx.mix,
        feed: if fx.fed { Feed::Chain } else { Feed::None },
        preset: fx.current_preset.clone(),
        params: overrides(&fx.params),
        pinned: fx.pinned_names(),
        state: fx.state.clone(),
        ui: ui.get(&fx.node).copied(),
    }
}

/// What the file says of a modulator's note length: the name of it, or nothing for a modulator that goes at its own rate (as a file that was written before there was one).
fn sync_for_file(sync: usize) -> Option<String> {
    (sync > 0).then(|| sync_name(sync).to_string())
}

/// What a modulator is, and how it is set, as the file writes it (a module's file writes it too).
pub(super) fn source_to_save(source: &ModSourceSlot) -> SaveModSource {
    match source {
        ModSourceSlot::Lfo { waveform, rate, retrigger, fade, sync } => SaveModSource::Lfo { waveform: waveform.name().to_string(), rate: *rate, retrigger: *retrigger, fade: *fade, sync: sync_for_file(*sync) },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => SaveModSource::Envelope { attack: *attack, decay: *decay, sustain: *sustain, release: *release },
        ModSourceSlot::MidiLearn { source, .. } => SaveModSource::MidiLearn {
            cc: match source {
                Some(MidiBindSource::Cc(n)) => Some(*n),
                _ => None,
            },
            pitch_bend: matches!(source, Some(MidiBindSource::PitchBend)),
            aftertouch: matches!(source, Some(MidiBindSource::Aftertouch)),
            velocity: matches!(source, Some(MidiBindSource::Velocity)),
            key: matches!(source, Some(MidiBindSource::Key)),
        },
        ModSourceSlot::Macro { value, name, expose } => SaveModSource::Macro { value: *value, name: name.clone(), expose: *expose },
        ModSourceSlot::Steps { values, length, rate, glide, retrigger, sync } => SaveModSource::Steps { values: values.to_vec(), length: *length, rate: *rate, glide: *glide, retrigger: *retrigger, sync: sync_for_file(*sync) },
    }
}

/// The modulators of the rack at `at`, as the file writes them: each cable by the place that the rack names what it drives by (`SessionModel::kind_of`: the file names a plugin by its
/// place in the session, a sibling by its index).
fn modulators_to_save(model: &SessionModel, at: ModulatorLocation) -> Vec<SaveModulator> {
    let ui = &model.ui;
    let mods: &[ModulatorSlot] = model.modulators(at).map_or(&[], |rack| rack.as_slice());
    mods.iter()
        .enumerate()
        .map(|(index, m)| {
            let source = source_to_save(&m.source);
            SaveModulator {
                source,
                targets: m
                    .targets
                    .iter()
                    // (A cable that drives nothing that is in reach is not in the model: this leaves out none.)
                    .filter_map(|t| {
                        let kind = model.kind_of(at, index, t.aim)?;
                        // (A cable to a plugin names it by its place in the session.)
                        let to = match kind {
                            ModTargetKind::FreeParam { plugin, .. } => Some(plugin_place_of(model, plugin)?),
                            // (A cable to a volume, a mix or a pan names whose it is: a lane, an effect or a group.)
                            ModTargetKind::Gain { node, .. } => Some(plugin_place_of(model, node).or_else(|| model.groups.iter().position(|group| group.node == node).map(CablePlace::Group))?),
                            _ => None,
                        };
                        Some(SaveModTarget { kind, label: t.param_name.clone(), depth: t.depth, to })
                    })
                    .collect(),
                ui: ui.get(&m.node).copied(),
                // (Only the session's own modulators hear a lane or a group; one that hears a node that is gone hears every event, and is written so: no place names a node that is not there.)
                hears: m.ear.and_then(|ear| model.heard_place(ear)),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::model::{ModSourceSlot, SessionModel};
    use crate::app::testutil::{
        CROSS_MOD, DISABLED_PATTERN, GROUPS, LEGACY, MIDI, PINNED, PLACED, SETTINGS, assert_same_model, load, try_load,
    };
    use crate::plugin::chain::MidiBindSource;
    use crate::session;

    const BASIC: &str = include_str!("../../examples/basic.toml");
    const SPLIT: &str = include_str!("../../examples/split.toml");

    /// Values that are not exact in binary, on an instrument and its pattern.
    const ODD_TEMPO: &str = r#"
[[instrument]]
plugin = "builtin:osc"
volume = 0.7
[instrument.pattern]
bpm = 93.7
length_beats = 3.3
base_note = "C4"
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
"#;

    const SCENES: &[(&str, &str)] = &[
        ("basic", BASIC),
        ("split", SPLIT),
        ("groups", GROUPS),
        ("legacy", LEGACY),
        ("midi", MIDI),
        ("settings", SETTINGS),
        ("cross-mod", CROSS_MOD),
        ("disabled-pattern", DISABLED_PATTERN),
        ("odd-tempo", ODD_TEMPO),
        ("placed", PLACED),
        ("pinned", PINNED),
    ];

    /// The file a save of this model writes.
    fn saved(model: &SessionModel) -> String {
        let (instruments, groups, cables, sources) = to_save(model);
        session::to_toml(&instruments, &groups, &cables, &sources, None, model.master_ui(), model.bpm_to_save()).unwrap()
    }

    #[test]
    fn every_scene_survives_a_save_and_a_reload() {
        for (name, text) in SCENES {
            let first = load(text);
            let file = saved(&first.model);
            let second =
                try_load(&file).unwrap_or_else(|e| panic!("{name}: the saved file does not load: {e:#}\n{file}"));
            assert_eq!(first.commands, second.commands, "{name}: the saved file tells the engine something else:\n{file}");
            assert_same_model(name, &first.model, &second.model);
            assert_eq!(file, saved(&second.model), "{name}: saving twice gives a different file");
        }
    }

    // What a saved file looks like. Hand-edited files get rewritten on every save, so
    // the writer has to keep to the style the docs show.

    #[test]
    fn numbers_are_written_the_way_they_were_typed() {
        let file = saved(&load(GROUPS).model);
        for line in ["volume = 0.8", "pan = -0.3", "mix = 0.6", "attack = 0.02", "decay = 0.4", "depth = -0.3"] {
            assert!(file.lines().any(|l| l == line), "no `{line}` in:\n{file}");
        }
        let file = saved(&load(ODD_TEMPO).model);
        for line in ["volume = 0.7", "bpm = 93.7", "length_beats = 3.3"] {
            assert!(file.lines().any(|l| l == line), "no `{line}` in:\n{file}");
        }
        // Whatever the scene: an f32 widened to f64 shows up as a long run of digits.
        for (name, text) in SCENES {
            let file = saved(&load(text).model);
            let noisy: Vec<&str> = file.lines().filter(|l| longest_fraction(l) >= 8).collect();
            assert!(noisy.is_empty(), "{name}: numbers with float noise:\n{}", noisy.join("\n"));
        }
    }

    /// Digits after the longest decimal point on the line.
    fn longest_fraction(line: &str) -> usize {
        line.split('.').skip(1).map(|rest| rest.chars().take_while(char::is_ascii_digit).count()).max().unwrap_or(0)
    }

    #[test]
    fn remapped_keys_are_written_as_inline_tables() {
        let file = saved(&load(SETTINGS).model);
        assert!(file.contains("\"G#4\" = { note = \"G4\", detune = 1.0 }"), "{file}");
    }

    #[test]
    fn mute_and_solo_are_written_where_they_are_on_and_nowhere_else_and_come_back_when_the_file_is_loaded() {
        let text = "[[group]]\nmute = true\n[[group]]\nsolo = true\n[[group]]\n\
                    [[instrument]]\nplugin = \"builtin:osc\"\nsolo = true\n\
                    [[instrument]]\nplugin = \"builtin:osc\"\nmute = true\nsolo = true\n\
                    [[instrument]]\nplugin = \"builtin:osc\"\n";
        let first = load(text);
        let file = saved(&first.model);
        assert_eq!(file.matches("mute = true").count(), 2, "{file}");
        assert_eq!(file.matches("solo = true").count(), 3, "{file}");
        assert!(!file.contains("mute = false") && !file.contains("solo = false"), "{file}");
        let second = try_load(&file).unwrap_or_else(|e| panic!("the saved file does not load: {e:#}\n{file}"));
        let state = |model: &SessionModel| {
            (
                model.instruments.iter().map(|lane| (lane.muted, lane.solo)).collect::<Vec<_>>(),
                model.groups.iter().map(|group| (group.muted, group.solo)).collect::<Vec<_>>(),
            )
        };
        assert_eq!(state(&second.model), state(&first.model));
        assert_eq!(state(&second.model), (vec![(false, true), (true, true), (false, false)], vec![(true, false), (false, true), (false, false)]));
        // A session with neither is written with neither.
        let file = saved(&load("[[instrument]]\nplugin = \"builtin:osc\"\n").model);
        assert!(!file.contains("mute") && !file.contains("solo"), "{file}");
    }

    #[test]
    fn a_cross_mod_depth_target_stays_on_one_line() {
        let file = saved(&load(GROUPS).model);
        // (The group's LFO is the second of the session's own, after the lane's: `mod_depth` names the modulator by its place there.)
        assert!(file.contains("mod_depth = [1, 2]"), "{file}");
    }

    #[test]
    fn a_lane_whose_instrument_was_cleared_keeps_its_modulators_through_a_save_and_a_reload() {
        // What `d` on the instrument of a lane that has effects does: the plugin goes; the lane, its effects and
        // its modulators stay.
        let mut first = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
"#,
        );
        first.model.instruments[0].instrument = None;
        let file = saved(&first.model);
        let second = try_load(&file).unwrap_or_else(|e| panic!("the saved file does not load: {e:#}\n{file}"));
        let rack = |model: &SessionModel| {
            let at = ModulatorLocation::Free;
            model
                .free_modulators
                .iter()
                .enumerate()
                .map(|(m, modulator)| {
                    let targets = modulator.targets.iter().enumerate().map(|(t, cable)| (model.kind_of_target(at, m, t), cable.param_name.clone(), cable.depth));
                    (format!("{:?}", modulator.source), targets.collect::<Vec<_>>())
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(rack(&first.model).len(), 1);
        assert_eq!(rack(&second.model), rack(&first.model), "the modulator and its cable are in the file:\n{file}");
        assert!(second.model.instruments[0].instrument.is_none());
        assert_eq!(second.model.instruments[0].effects.len(), 1);
        assert_eq!(file, saved(&second.model), "and saving it again gives the same file");
    }

    #[test]
    fn an_lfo_is_saved_by_the_name_of_its_waveform_and_is_the_same_when_the_file_is_loaded_again() {
        for waveform in crate::plugin::chain::LfoWaveform::ALL {
            let name = waveform.name();
            let first = load(&format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nwaveform = \"{name}\"\nrate = 3.0\n"));
            let file = saved(&first.model);
            assert!(file.contains(&format!("waveform = \"{name}\"")), "{name}:\n{file}");
            let second = load(&file);
            let source = |run: &crate::app::testutil::Run| format!("{:?}", run.model.free_modulators[0].source);
            assert_eq!(source(&second), source(&first), "{name}");
            assert_eq!(source(&first), format!("Lfo {{ waveform: {waveform:?}, rate: 3.0, retrigger: false, fade: 0.0, sync: 0 }}"), "{name}");
        }
    }

    #[test]
    fn an_lfo_that_retriggers_and_fades_is_saved_with_its_key_and_is_the_same_when_the_file_is_loaded_again_and_one_that_runs_free_has_neither_key() {
        let first = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nrate = 3.0\nretrigger = true\nfade = 0.3\n");
        let file = saved(&first.model);
        assert!(file.contains("retrigger = true") && file.contains("fade = 0.3"), "{file}");
        let second = load(&file);
        let source = |run: &crate::app::testutil::Run| format!("{:?}", run.model.free_modulators[0].source);
        assert_eq!(source(&second), source(&first));
        assert_eq!(source(&first), "Lfo { waveform: Sine, rate: 3.0, retrigger: true, fade: 0.3, sync: 0 }");
        // A session that was written before an LFO had a key is saved as it was.
        let free = saved(&load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nrate = 3.0\n").model);
        assert!(!free.contains("retrigger") && !free.contains("fade"), "{free}");
    }

    #[test]
    fn an_lfo_and_a_step_sequencer_that_follow_the_tempo_are_saved_with_the_name_of_the_note_length_and_the_rate_they_keep_and_are_the_same_when_loaded_again() {
        let session = "[[group]]\n[[group.modulator]]\nrate = 0.5\nsync = \"2 bars\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.modulator]]\nrate = 3.0\nsync = \"1/8 triplet\"\n\
                       [[instrument.modulator]]\ntype = \"steps\"\nrate = 6.5\nsync = \"1/16 dotted\"\n[[instrument.modulator]]\nrate = 2.0\nsync = \"free\"\n";
        let first = load(session);
        let file = saved(&first.model);
        // Each is written by the name, once; the one that is free has no key; the rates are what they were.
        assert_eq!(file.matches("sync = ").count(), 3, "{file}");
        for line in ["sync = \"2 bars\"", "sync = \"1/8 triplet\"", "sync = \"1/16 dotted\"", "rate = 0.5", "rate = 3.0", "rate = 6.5", "rate = 2.0"] {
            assert!(file.contains(line), "{line}:\n{file}");
        }
        let second = load(&file);
        let sources = |run: &crate::app::testutil::Run| run.model.free_modulators.iter().map(|m| format!("{:?}", m.source)).collect::<Vec<_>>();
        assert_eq!(sources(&second), sources(&first));
        assert_eq!(saved(&second.model), file, "and saving it again gives the same file");
        let sync = |run: &crate::app::testutil::Run| {
            let of = |m: &crate::app::model::ModulatorSlot| match &m.source {
                ModSourceSlot::Lfo { sync, .. } | ModSourceSlot::Steps { sync, .. } => *sync,
                other => panic!("{other:?}"),
            };
            run.model.free_modulators.iter().map(of).collect::<Vec<_>>()
        };
        assert_eq!(sync(&second), [12, 13, 0, 2], "the lane's three, then the group's");
    }

    #[test]
    fn the_places_of_the_nodes_are_saved_on_the_nodes_they_are_for() {
        let file = saved(&load(PLACED).model);
        let lines = |prefix: &str| file.lines().filter(|l| l.starts_with(prefix)).collect::<Vec<_>>();
        assert_eq!(lines("master = "), ["master = { x = 1200, y = 300.5 }"], "{file}");
        assert_eq!(
            lines("ui = "),
            [
                "ui = { x = 0, y = 0 }",
                "ui = { x = 376, y = 0.25 }",
                "ui = { x = 800, y = 40 }",
                "ui = { x = 1100, y = 40 }",
                "ui = { x = 376, y = -200 }",
                "ui = { x = 800, y = -120 }",
            ],
            "the lane and its effect, then the group and its effect, then the modulators of the session's own, the lane's first and the group's after it:\n{file}"
        );
    }

    #[test]
    fn the_place_of_a_node_that_has_gone_is_not_saved() {
        let mut run = load(PLACED);
        let gone = run.model.instruments[0].effects.remove(0).node;
        assert!(run.model.ui.contains_key(&gone), "(ids are not reused, so the table is not tidied)");
        let file = saved(&run.model);
        assert!(!file.contains("y = 0.25"), "{file}");
        assert_eq!(file.lines().filter(|l| l.starts_with("ui = ")).count(), 5, "{file}");
        // And the master stays, with nothing else changed in the file.
        assert!(file.contains("master = { x = 1200, y = 300.5 }"), "{file}");
    }

    #[test]
    fn a_session_with_no_places_saves_none() {
        for (name, text) in SCENES {
            if *name == "placed" {
                continue;
            }
            let file = saved(&load(text).model);
            assert!(!file.lines().any(|l| l.starts_with("ui = ") || l == "[ui]"), "{name}:\n{file}");
        }
    }

    #[test]
    fn the_pinned_parameters_are_saved_by_name_in_the_order_they_were_pinned_on_the_plugins_they_are_for() {
        let file = saved(&load(PINNED).model);
        // (The names the file had that were no pins are gone: the model only has pins.)
        for line in [r#"pinned = ["sustain", "detune", "waveform"]"#, r#"pinned = ["resonance", "cutoff"]"#, r#"pinned = ["drive", "cutoff"]"#] {
            assert_eq!(file.lines().filter(|l| *l == line).count(), 1, "{line} in\n{file}");
        }
        assert_eq!(file.lines().filter(|l| l.starts_with("pinned")).count(), 3, "{file}");
        // Each on its own plugin: the instrument's before its effect's, and the bus effect's with the group.
        let at = |line: &str| file.find(line).unwrap_or_else(|| panic!("{line}\n{file}"));
        assert!(at(r#"pinned = ["sustain""#) < at("[[instrument.effect]]") && at("[[instrument.effect]]") < at(r#"pinned = ["resonance""#), "{file}");
        assert!(at("[[group]]") < at(r#"pinned = ["drive""#), "{file}");
    }

    #[test]
    fn a_session_with_nothing_pinned_saves_no_pins() {
        for (name, text) in SCENES {
            if *name == "pinned" {
                continue;
            }
            let file = saved(&load(text).model);
            assert!(!file.contains("pinned"), "{name}:\n{file}");
        }
    }

    #[test]
    fn an_effect_that_is_moved_takes_its_pins_with_it() {
        let mut run = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["drive"]
[[instrument.effect]]
plugin = "builtin:filter"
pinned = ["cutoff", "resonance"]
"#,
        );
        run.model.instruments[0].effects.swap(0, 1);
        let file = saved(&run.model);
        let first = file.find(r#"pinned = ["cutoff", "resonance"]"#).unwrap();
        let second = file.find(r#"pinned = ["drive"]"#).unwrap();
        assert!(first < second, "{file}");
    }

    #[test]
    fn a_plain_lane_is_saved_as_just_its_plugin() {
        let run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert_eq!(saved(&run.model), "[[instrument]]\nplugin = \"builtin:osc\"\n");
    }

    #[test]
    fn a_parameter_is_saved_only_while_it_differs_from_its_baseline() {
        let mut run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        let sustain = |model: &mut SessionModel| -> usize {
            let params = &mut model.instruments[0].instrument.as_mut().unwrap().params;
            params.iter().position(|p| p.name == "sustain").unwrap()
        };
        let i = sustain(&mut run.model);
        let osc = run.model.instruments[0].instrument.as_mut().unwrap();
        let baseline = osc.params[i].default;
        osc.params[i].value = baseline / 2.0;
        let file = saved(&run.model);
        assert!(file.contains("[instrument.params]") && file.contains("sustain = 0.5"), "{file}");

        let osc = run.model.instruments[0].instrument.as_mut().unwrap();
        osc.params[i].value = baseline;
        assert!(!saved(&run.model).contains("params"), "back at the baseline it is not an override");
    }

    #[test]
    fn an_instruments_preset_is_saved() {
        // No built-in instrument has presets, so set the name the way a preset load does.
        let mut run = load("[[instrument]]\nplugin = \"builtin:osc\"\n");
        run.model.instruments[0].instrument.as_mut().unwrap().current_preset = Some("Warm Pad".into());
        assert!(saved(&run.model).contains("preset = \"Warm Pad\""));
    }

    const MACROS: &str = "[[group]]\n[[group.modulator]]\ntype = \"macro\"\nvalue = 0.5\n\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.modulator]]\ntype = \"macro\"\nvalue = 0.25\n";

    #[test]
    fn a_macro_is_saved_with_its_value_and_loaded_again_the_same_in_a_lane_and_in_a_group() {
        let first = load(MACROS);
        let file = saved(&first.model);
        assert_eq!(file.matches("type = \"macro\"").count(), 2, "{file}");
        assert!(file.contains("value = 0.25") && file.contains("value = 0.5"), "{file}");
        let second = load(&file);
        assert_eq!(first.commands, second.commands);
        assert_eq!(saved(&second.model), file, "a second save is the same file");
    }

    #[test]
    fn a_macro_that_is_called_something_and_is_not_shown_is_saved_with_both_and_one_that_is_neither_is_saved_with_neither() {
        let mut run = load(MACROS);
        // (The lane's macro is the first of the session's own, and the group's the second.)
        run.model.free_modulators[0].source = ModSourceSlot::Macro { value: 0.25, name: "Brightness".to_string(), expose: false };
        let file = saved(&run.model);
        assert_eq!((file.matches("name =").count(), file.matches("expose").count()), (1, 1), "the group's macro has neither, and is not given one: {file}");
        assert!(file.contains("name = \"Brightness\"") && file.contains("expose = false"), "{file}");
        // Loaded again, both are what they were, and a second save is the same file.
        let second = load(&file);
        assert!(matches!(&second.model.free_modulators[0].source, ModSourceSlot::Macro { name, expose: false, .. } if name == "Brightness"), "{:?}", second.model.free_modulators[0].source);
        assert!(matches!(&second.model.free_modulators[1].source, ModSourceSlot::Macro { name, expose: true, .. } if name.is_empty()), "{:?}", second.model.free_modulators[1].source);
        assert_eq!(saved(&second.model), file);
        // A name that is a macro's alone is not written for a modulator that is another kind, and a macro that is shown and has none is as it always was in the file.
        let plain = saved(&load(MACROS).model);
        assert!(!plain.contains("name") && !plain.contains("expose"), "{plain}");
        let lfo = saved(&load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"lfo\"\n").model);
        assert!(!lfo.contains("name") && !lfo.contains("expose"), "{lfo}");
    }

    const SEQUENCERS: &str = "[[group]]\n[[group.modulator]]\ntype = \"steps\"\nsteps = [0.5, -0.5]\nrate = 3.0\nglide = 0.5\nretrigger = false\n\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.modulator]]\ntype = \"steps\"\n";

    #[test]
    fn a_step_sequencer_is_saved_with_all_that_it_has_and_loaded_again_the_same_in_a_lane_and_in_a_group() {
        let first = load(SEQUENCERS);
        let file = saved(&first.model);
        assert_eq!(file.matches("type = \"steps\"").count(), 2, "{file}");
        // Every one is written whole: its sixteen steps (the group's with the zeros that the file left out, and its length of 2), and what it has besides.
        assert_eq!(file.matches("steps = [").count(), 2, "{file}");
        assert!(file.contains("steps = [0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]"), "{file}");
        assert!(file.contains("length = 2") && file.contains("length = 16"), "{file}");
        assert!(file.contains("rate = 3.0") && file.contains("rate = 8.0"), "{file}");
        assert!(file.contains("glide = 0.5") && file.contains("glide = 0.0"), "{file}");
        assert!(file.contains("retrigger = false") && file.contains("retrigger = true"), "{file}");
        let second = load(&file);
        assert_eq!(first.commands, second.commands);
        assert_eq!(saved(&second.model), file, "a second save is the same file");
    }

    #[test]
    fn a_step_sequencer_that_is_changed_is_saved_as_it_is_now() {
        let mut run = load(SEQUENCERS);
        let mut values = [0.0; 16];
        values[..3].copy_from_slice(&[-1.0, 0.125, 0.75]);
        run.model.free_modulators[0].source = ModSourceSlot::Steps { values, length: 3, rate: 12.5, glide: 1.0, retrigger: false, sync: 0 };
        let file = saved(&run.model);
        assert!(file.contains("steps = [-1.0, 0.125, 0.75, 0.0,"), "{file}");
        assert!(file.contains("length = 3") && file.contains("rate = 12.5") && file.contains("glide = 1.0"), "{file}");
        assert_eq!(file.matches("retrigger = false").count(), 2, "{file}");
        assert!(!file.contains("retrigger = true"), "{file}");
    }

    #[test]
    fn a_macro_that_is_turned_is_saved_at_the_value_it_has_and_a_macro_at_rest_says_so() {
        let mut run = load(MACROS);
        // (The lane's macro is the first of the session's own, and the group's the second.)
        run.model.free_modulators[0].source = ModSourceSlot::macro_at(0.8);
        run.model.free_modulators[1].source = ModSourceSlot::macro_at(0.0);
        let file = saved(&run.model);
        assert!(file.contains("value = 0.8") && file.contains("value = 0.0"), "{file}");
        assert!(!file.contains("value = 0.25") && !file.contains("value = 0.5"), "{file}");
    }

    #[test]
    fn the_other_modulators_are_saved_with_no_value() {
        let file = saved(&load(MIDI).model);
        assert!(!file.contains("value ="), "{file}");
    }

    #[test]
    fn a_control_learned_after_loading_is_saved_as_bound() {
        let mut run = load(MIDI);
        // The second modulator is the armed, unbound one (see `MIDI`).
        let m = &mut run.model.free_modulators[1];
        assert!(matches!(m.source, ModSourceSlot::MidiLearn { source: None, learning: true }));
        m.source = ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(7)), learning: false };
        let file = saved(&run.model);
        assert!(file.matches("midi_cc = ").count() == 2 && file.contains("midi_cc = 7"), "{file}");
    }

    #[test]
    fn several_remapped_keys_survive_a_save_and_reload() {
        let text = r#"
[[instrument]]
plugin = "builtin:osc"
pitch_bend_range = 2.0
[instrument.remap]
"G#4" = { note = "G4", detune = 1.0 }
"C#2" = { note = "D2", detune = -0.5 }
"F#3" = { note = "F3", detune = 1.0 }
"#;
        let remap = |model: &SessionModel| {
            let mut keys: Vec<_> =
                model.instruments[0].remap.iter().map(|(k, v)| (k.clone(), v.note.clone(), v.detune)).collect();
            keys.sort_by(|a, b| a.0.cmp(&b.0));
            keys
        };
        let first = load(text);
        let second = load(&saved(&first.model));
        assert_eq!(remap(&first.model).len(), 3);
        assert_eq!(remap(&first.model), remap(&second.model));
        assert_eq!(first.commands, second.commands, "the same detune values still share channels");
    }
}
