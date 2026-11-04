//! Modules: effects and instruments, with the audio cables between them, that are saved to a file and loaded into another session (step 104, the first of "design a set of modules, save them as a
//! stand-alone module that you can then load inside another instrument"; step 105 made a module any selection of plugins and not only the chain of one lane: "save the graph of fx or instruments").
//!
//! A module is a **session file of lanes that hold plugins**: each lane has an instrument or none and its effects in the order of its chain, each plugin as a session file says one (the plugin, the preset it
//! has, the values that are not the plugin's own, the mix of an effect and the volume of an instrument, the knobs pinned to its node, and whether the stage before an effect feeds it), the audio cables that
//! are between those plugins, and (step 106) **the modulators that were chosen with them**, each with its settings, the lane of the module whose notes it hears (or every note) and the cables that it has to
//! the module's plugins and to the settings of the module's other modulators (what goes into the module and what comes out of it is the cables that a person draws once the module is in a session). The files are in one directory, the
//! library (`Controller::modules_dir`), one file for each module, and the name of a module is the name of its file without `.toml`. The catalog lists the modules with the plugins, as `module:<name>`
//! (`Catalog::set_modules`), so that what adds a plugin that stands by itself (`AddFreeEffect`) adds a module too: a lane of its own for each of the module's, bare, as `AddFreeEffect` makes it, with the
//! cables between them. The lane is how the model holds a plugin until P5c dissolves it: a module of three effects that were each in a lane of their own, with two cables between them, is three lanes and
//! two cables, as the session it was taken from had them.
//!
//! This file is the part that touches no controller: the names, the library's listing, the text of a module from the model, and what a text says. The controller's `save_module_intent`,
//! `save_selection_intent` and `load_module` (in `apply.rs`) do the rest.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::app::graph::Port;
use crate::app::load::build_mod_source;
use crate::app::model::{ModSourceSlot, NodeId, NodePath, SessionModel};
use crate::app::save;
use crate::ids::{Driven, ModGain};
use crate::plugin::PluginInfo;
use crate::plugin::chain::{CableFrom, CableTo, ModTargetKind, ModulatorLocation};
use crate::session::{self, CablePlace, EffectConfig, Feed, PluginConfig, Route, SaveCable, SaveInstrumentSlot, SaveModTarget, SaveModulator, SaveSources};

/// What the catalog's id of a module begins with: `module:Space` is the module that the file `Space.toml` of the library is.
pub const PREFIX: &str = "module:";

/// The id that the catalog gives the module called `name`.
pub fn id_of(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The name of the module that the catalog's `id` says, or none for the id of a plugin.
pub fn name_of(id: &str) -> Option<&str> {
    id.strip_prefix(PREFIX)
}

/// The name a module is saved under, as a file name that is a name and not a path (`session::plain_file_name`: no folder, none of the characters that a file cannot have, a name that Windows
/// does not keep for itself, at most 255 bytes) without the `.toml` that the file has. The refusal says what is wrong with the name.
pub fn clean_name(given: &str) -> Result<String, String> {
    let file = session::plain_file_name(given)?;
    // (`plain_file_name` adds the extension: a name that already has it is the same module.)
    let stem = file.strip_suffix(".toml").unwrap_or(&file);
    // (A name that ends in the extension itself is a file that the library would not list under it: the listing takes the name off the file, and the name that it makes has to be the one that was given.)
    if stem.ends_with(".toml") {
        return Err("the name of a module does not end in .toml: that is the extension of its file".to_string());
    }
    Ok(stem.to_string())
}

/// Where the module called `name` is in the library `dir`.
pub fn path_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.toml"))
}

/// A module as a file says it: lanes of plugins, the audio cables between the plugins, and the modulators with their cables.
pub struct Module {
    pub lanes: Vec<ModuleLane>,
    pub cables: Vec<ModuleCable>,
    pub modulators: Vec<ModuleModulator>,
}

/// A modulator of a module (step 106): what it is and how it is set (as the loader makes it of a file's table), the lane of the module whose notes it hears, and its cables in the order of the file.
pub struct ModuleModulator {
    pub source: ModSourceSlot,
    /// The lane of the module that it hears (a place among the module's lanes); none is every note.
    pub hears: Option<usize>,
    pub targets: Vec<ModuleTarget>,
}

/// A cable of a modulator of a module: what it drives, and how far.
pub struct ModuleTarget {
    pub drives: ModuleDrives,
    pub depth: f32,
}

/// What a cable of a module's modulator drives. A plugin is named by its place in the module (`ModuleOutput`: the instrument of a lane, or an effect of its chain), a modulator by its place among the module's,
/// and the depth of a cable by the place that the cable has among its modulator's, as the file lists them.
#[derive(Clone, Debug, PartialEq)]
pub enum ModuleDrives {
    /// A parameter of a plugin of the module, by the parameter's name (a plugin that has no such parameter has its cable left out when the module is added).
    Param { plugin: ModuleOutput, name: String },
    /// The volume of a lane's instrument or the mix of an effect.
    Gain { plugin: ModuleOutput, gain: ModGain },
    /// A setting of another of the module's modulators: `port` is its rate or a stage of its envelope.
    Setting { modulator: usize, port: Port },
    /// The depth of the cable that another of the module's modulators has at place `target` among its cables.
    Depth { modulator: usize, target: usize },
}

/// A lane of a module: an instrument, if it has one, and the effects of its chain in order.
pub struct ModuleLane {
    pub instrument: Option<PluginConfig>,
    pub effects: Vec<EffectConfig>,
}

impl Module {
    /// How many plugins the module is made of (the instruments and the effects): what the catalog lists it with where a plugin has its parameters.
    pub fn plugin_count(&self) -> usize {
        self.lanes.iter().map(|lane| usize::from(lane.instrument.is_some()) + lane.effects.len()).sum()
    }
}

/// Where an audio cable of a module starts: what a lane's instrument puts out, or what an effect puts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleOutput {
    /// The instrument of the lane (a place of the module's lanes).
    Instrument { lane: usize },
    /// An effect of a lane's chain.
    Effect { lane: usize, effect: usize },
}

/// Where an audio cable of a module ends: an effect's input, or its second input (`Plugin::side_input`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleInput {
    pub lane: usize,
    pub effect: usize,
    /// Whether it is the second input of that effect.
    pub side: bool,
}

/// An audio cable of a module, from the output of one plugin to the input of an effect, at a level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModuleCable {
    pub from: ModuleOutput,
    pub to: ModuleInput,
    pub gain: f32,
}

/// What a module's text says, or why it is not a module: a session file of lanes that have effects or an instrument, with no group and no source of notes, no cable that is not between two of its plugins (a
/// cable starts at an instrument or an effect and ends at an effect or at the second input of one), and modulators whose cables drive plugins and modulators that the module has and whose ears are lanes that it has.
pub fn parse(text: &str) -> Result<Module, String> {
    let config = session::parse(text).map_err(|error| error.to_string())?;
    if config.instruments.is_empty() {
        return Err("a module is made of lanes of effects and instruments, and this file has no lanes".to_string());
    }
    // (A lane's own modulators are moved to the session's own when a file is read, `session::parse`: a module that has any has `free_modulators`, which are the module's.)
    if !config.groups.is_empty() || config.keys.is_some() || !config.devices.is_empty() {
        return Err("a module is effects and instruments with the cables between them and the modulators that go with them, and nothing else: it has no group and no source of notes".to_string());
    }
    if let Some(empty) = config.instruments.iter().position(|lane| lane.instrument.is_none() && lane.effects.is_empty()) {
        return Err(format!("lane {empty} of the module has no instrument and no effect: a module has at least one plugin in each lane"));
    }
    let lanes = &config.instruments;
    let mut cables = Vec::new();
    for (i, cable) in config.cables.iter().enumerate() {
        let not_between = || format!("cable[{i}] is not between two plugins of the module: a module has the cables that are inside it and no others");
        let from = match cable.from {
            CablePlace::Instrument(lane) => ModuleOutput::Instrument { lane },
            CablePlace::Effect { instrument: lane, effect } => ModuleOutput::Effect { lane, effect },
            _ => return Err(not_between()),
        };
        let to = match cable.to {
            CablePlace::Effect { instrument: lane, effect } => ModuleInput { lane, effect, side: false },
            CablePlace::EffectSide { instrument: lane, effect } => ModuleInput { lane, effect, side: true },
            _ => return Err(not_between()),
        };
        let has = |lane: usize, effect: Option<usize>| match (lanes.get(lane), effect) {
            (Some(lane), Some(effect)) => effect < lane.effects.len(),
            (Some(lane), None) => lane.instrument.is_some(),
            (None, _) => false,
        };
        let start_is_there = match from {
            ModuleOutput::Instrument { lane } => has(lane, None),
            ModuleOutput::Effect { lane, effect } => has(lane, Some(effect)),
        };
        if !start_is_there || !has(to.lane, Some(to.effect)) {
            return Err(format!("cable[{i}] names a plugin that the module has not"));
        }
        cables.push(ModuleCable { from, to, gain: cable.gain.unwrap_or(1.0) });
    }
    let modulators = modulators_of(&config)?;
    let lanes = config.instruments.into_iter().map(|lane| ModuleLane { instrument: lane.instrument, effects: lane.effects }).collect();
    Ok(Module { lanes, cables, modulators })
}

/// The modulators that a module's file has (`[[modulator]]`), with each place that they name looked for among the module's own: a plugin that a cable drives, a lane that a modulator hears, a modulator that a cable
/// drives the settings of and a cable that another has. A place that is not there refuses the module (as a cable of audio does), where a parameter that a plugin has not is left out when the module is added.
fn modulators_of(config: &session::SessionConfig) -> Result<Vec<ModuleModulator>, String> {
    let lanes = &config.instruments;
    let count = config.free_modulators.len();
    let plugin_at = |text: &str, whose: &str| -> Result<ModuleOutput, String> {
        match CablePlace::parse(text) {
            Some(CablePlace::Instrument(lane)) if lanes.get(lane).is_some_and(|lane| lane.instrument.is_some()) => Ok(ModuleOutput::Instrument { lane }),
            Some(CablePlace::Effect { instrument: lane, effect }) if lanes.get(lane).is_some_and(|lane| effect < lane.effects.len()) => Ok(ModuleOutput::Effect { lane, effect }),
            _ => Err(format!("{whose} names {text:?}, which is not a plugin of the module")),
        }
    };
    let mut modulators = Vec::new();
    for (i, modulator) in config.free_modulators.iter().enumerate() {
        let hears = match modulator.hears.as_deref() {
            None => None,
            Some(text) => match CablePlace::parse(text) {
                Some(CablePlace::Instrument(lane)) if lane < lanes.len() => Some(lane),
                _ => return Err(format!("modulator[{i}].hears is {text:?}, which is not a lane of the module: a modulator hears one of the module's lanes, or every note")),
            },
        };
        let mut targets = Vec::new();
        for (j, target) in modulator.targets.iter().enumerate() {
            let whose = format!("modulator[{i}].target[{j}]");
            let other = |index: usize| -> Result<usize, String> {
                if index >= count || index == i {
                    return Err(format!("{whose} drives modulator {index}, which is {}: a cable of a module drives another of the module's modulators", if index == i { "itself" } else { "not in the module" }));
                }
                Ok(index)
            };
            let drives = if let Some(name) = &target.gain {
                let gain = ModGain::parse(name).ok_or_else(|| format!("{whose} drives {name:?}, which is not a volume, a mix or a pan"))?;
                let plugin = plugin_at(target.to.as_deref().unwrap_or(""), &whose)?;
                match (plugin, gain) {
                    (ModuleOutput::Instrument { .. }, ModGain::Volume) | (ModuleOutput::Effect { .. }, ModGain::Mix) => ModuleDrives::Gain { plugin, gain },
                    _ => return Err(format!("{whose} drives the {} of a plugin that has none: a lane's instrument has a volume and an effect a mix", gain.name())),
                }
            } else if let Some(name) = &target.param {
                ModuleDrives::Param { plugin: plugin_at(target.to.as_deref().unwrap_or(""), &whose)?, name: name.clone() }
            } else if let Some(index) = target.mod_rate {
                ModuleDrives::Setting { modulator: other(index)?, port: Port::Rate }
            } else if let Some(pair) = &target.mod_depth {
                let (index, cable) = (other(pair.first().copied().unwrap_or(0))?, pair.get(1).copied().unwrap_or(0));
                if cable >= config.free_modulators[index].targets.len() {
                    return Err(format!("{whose} drives the depth of cable {cable} of modulator {index}, which has {} cables", config.free_modulators[index].targets.len()));
                }
                ModuleDrives::Depth { modulator: index, target: cable }
            } else if let Some(index) = target.mod_attack {
                ModuleDrives::Setting { modulator: other(index)?, port: Port::Attack }
            } else if let Some(index) = target.mod_decay {
                ModuleDrives::Setting { modulator: other(index)?, port: Port::Decay }
            } else if let Some(index) = target.mod_sustain {
                ModuleDrives::Setting { modulator: other(index)?, port: Port::Sustain }
            } else if let Some(index) = target.mod_release {
                ModuleDrives::Setting { modulator: other(index)?, port: Port::Release }
            } else {
                return Err(format!("{whose} drives nothing: it names no parameter, volume, mix or setting of a modulator"));
            };
            targets.push(ModuleTarget { drives, depth: target.depth as f32 });
        }
        modulators.push(ModuleModulator { source: build_mod_source(modulator).0, hears, targets });
    }
    Ok(modulators)
}

/// The nodes of the chain of effects of the lane `node` is of (the node of a lane, or of an effect in its chain): what a module that is the chain of a lane is made of. Refused, with the reason, for a node
/// that is neither, a group's bus, and a lane that has no effect.
pub fn chain_of(model: &SessionModel, node: NodeId) -> Result<Vec<NodeId>, String> {
    let inst = match model.locate(node) {
        Some(NodePath::Instrument(inst) | NodePath::Effect { inst, .. }) => inst,
        Some(NodePath::GroupEffect { .. } | NodePath::Group(_)) => return Err("a module is the chain of a lane: a group's bus is not saved".to_string()),
        _ => return Err("a module is made of the chain of a lane".to_string()),
    };
    let lane = &model.instruments[inst];
    if lane.effects.is_empty() {
        return Err("a lane that has no effect has no chain to save".to_string());
    }
    Ok(lane.effects.iter().map(|effect| effect.node).collect())
}

/// The text of a module made of the chain of the lane `node` is of: `chain_of` and then `to_text_of` (the effects as they are now, in order, and the audio cables between them).
pub fn to_text(model: &SessionModel, node: NodeId) -> Result<String, String> {
    to_text_of(model, &chain_of(model, node)?)
}

/// What is chosen of one lane of the session: whether its instrument is, and which of its effects (by their places in its chain).
#[derive(Default)]
struct Chosen {
    instrument: bool,
    effects: BTreeSet<usize>,
}

/// The text of a module made of the plugins in `nodes` (step 105: the instrument of a lane is the lane's node, an effect is its own): each lane that has one of them is a lane of the module, with the
/// instrument if it was chosen and the effects that were, in the order of their chain, each as it is now; and the audio cables that both of whose ends are chosen, in the order of their ids. What
/// is not chosen is left out, and what joins it to what is chosen is the session's and not the module's: the cable that goes into the module or out of it, the way out of a lane, the link that comes
/// from a stage that was not chosen (an effect that is the first of the module's lane, or follows one that was left out, is not fed by the chain: `feed = "none"`, but for the first of a lane that has no
/// instrument, which has nothing before it that would feed it either way). **The modulators that are chosen go with them** (step 106): each as it is now, with the cables that it has to the plugins that are chosen
/// (a parameter by its name, a lane's volume, an effect's mix) and to the settings of the other modulators that are chosen (a cable that drives the depth of a cable goes only if that cable does), and the lane of the module whose notes it hears
/// if it heard a lane that is chosen (else every note); what it drives that is not chosen is left out. Refused, with the reason, for a node that is a group, a group's effect or a source of notes, a lane that has
/// no instrument, nothing at all, and modulators with no plugin to go with.
pub fn to_text_of(model: &SessionModel, nodes: &[NodeId]) -> Result<String, String> {
    let mut chosen: BTreeMap<usize, Chosen> = BTreeMap::new();
    let mut chosen_modulators: BTreeSet<usize> = BTreeSet::new();
    for &node in nodes {
        match model.locate(node) {
            Some(NodePath::Instrument(inst)) => {
                if model.instruments[inst].instrument.is_none() {
                    return Err("a lane that has no instrument has none to save: choose its effects".to_string());
                }
                chosen.entry(inst).or_default().instrument = true;
            }
            Some(NodePath::Effect { inst, index }) => {
                chosen.entry(inst).or_default().effects.insert(index);
            }
            Some(NodePath::Group(_) | NodePath::GroupEffect { .. }) => return Err("a module is made of effects and instruments: a group's bus is not saved".to_string()),
            Some(NodePath::Modulator { index, .. }) => {
                chosen_modulators.insert(index);
            }
            Some(NodePath::Source(_)) => return Err("a source of notes is not part of a module: a module is made of effects and instruments, and the modulators that go with them".to_string()),
            Some(NodePath::Module(_)) => return Err("a module that was added is not a plugin: choose the plugins and modulators in it".to_string()),
            None => return Err(format!("there is no node {}", node.0)),
        }
    }
    if nodes.is_empty() {
        return Err("nothing is chosen: a module is made of the effects and instruments that are".to_string());
    }
    if chosen.is_empty() {
        return Err("a modulator alone is not a module: choose the effects or instruments that it goes with".to_string());
    }
    // (Where each chosen plugin is in the module, which is how the file names it in its cables.)
    let lane_at: BTreeMap<usize, usize> = chosen.keys().enumerate().map(|(place, inst)| (*inst, place)).collect();
    let mut effect_at: BTreeMap<NodeId, (usize, usize)> = BTreeMap::new();
    let mut instrument_at: BTreeMap<NodeId, usize> = BTreeMap::new();
    // (Where the effects are drawn is the page's of that session, and not the module's: a module is laid out where it is put.)
    let no_places = BTreeMap::new();
    let mut slots = Vec::new();
    for (inst, lane_chosen) in &chosen {
        let lane = &model.instruments[*inst];
        let mut effects = Vec::new();
        let mut before: Option<usize> = None;
        for &index in &lane_chosen.effects {
            let effect = &lane.effects[index];
            let mut saved = save::effect_to_save(effect, &no_places);
            // (The stage before it in the module's chain is the one before it in the session's only if that was chosen too; the first of a lane that has no instrument has nothing before it either way, which the cut below leaves alone.)
            let same_stage = match before {
                Some(previous) => previous + 1 == index,
                None => index == 0,
            };
            if !same_stage && (before.is_some() || lane_chosen.instrument) {
                saved.feed = Feed::None;
            }
            effect_at.insert(effect.node, (lane_at[inst], effects.len()));
            effects.push(saved);
            before = Some(index);
        }
        if lane_chosen.instrument {
            instrument_at.insert(lane.node, lane_at[inst]);
        }
        slots.push(SaveInstrumentSlot {
            range: None,
            transpose: 0,
            muted: false,
            solo: false,
            route: Route::None,
            instrument: if lane_chosen.instrument { lane.instrument.as_ref().map(|plugin| save::instrument_to_save(lane, plugin)) } else { None },
            effects,
            pattern: None,
            group: None,
            ui: None,
        });
    }
    let place_of = |node: NodeId| effect_at.get(&node).copied();
    // (The audio cables that have both ends among what is chosen, in the order of their ids; one that starts at an instrument that was not chosen, at a group, or ends at a group's bus or the mix is the session's.)
    let cables: Vec<SaveCable> = model
        .cables
        .iter()
        .filter_map(|cable| {
            let from = match cable.from {
                CableFrom::Instrument(node) => {
                    let inst = model.instruments.iter().position(|lane| lane.node == node)?;
                    CablePlace::Instrument(*lane_at.get(&inst).filter(|_| chosen[&inst].instrument)?)
                }
                CableFrom::Effect(node) => {
                    let (instrument, effect) = place_of(node)?;
                    CablePlace::Effect { instrument, effect }
                }
                CableFrom::Group(_) => return None,
            };
            let to = match cable.to {
                CableTo::Effect(node) => {
                    let (instrument, effect) = place_of(node)?;
                    CablePlace::Effect { instrument, effect }
                }
                CableTo::EffectSide(node) => {
                    let (instrument, effect) = place_of(node)?;
                    CablePlace::EffectSide { instrument, effect }
                }
                CableTo::GroupBus(_) | CableTo::Master => return None,
            };
            Some(SaveCable { from, to, gain: cable.gain })
        })
        .collect();
    let modulators = modulators_of_text(model, &chosen_modulators, &lane_at, &instrument_at, &effect_at);
    let text = session::to_toml(&slots, &[], &cables, &SaveSources { modulators, ..SaveSources::default() }, None, None, None).map_err(|error| error.to_string())?;
    Ok(format!("# A module saved by Tang (effects and instruments, with the audio cables between them and the modulators that go with them): add it to a session as an effect that stands by itself, and draw the cables that go in and out of it.\n{text}"))
}

/// The modulators that are chosen (`chosen`: their places in the session's own rack) as a module's file writes them, in the order of the rack. A cable stays if what it drives is in the module: a plugin that is chosen
/// (`instrument_at` for a lane's instrument, `effect_at` for an effect, by their places among the module's lanes), a modulator that is chosen, or a cable of such a modulator that stays (a cable that drives the depth of a
/// cable goes with it: the cables are looked at until none more goes). What the file says of a cable is the place that it has in the module: a plugin's, a modulator's among the module's, and the place of a cable among those that
/// stay.
fn modulators_of_text(
    model: &SessionModel,
    chosen: &BTreeSet<usize>,
    lane_at: &BTreeMap<usize, usize>,
    instrument_at: &BTreeMap<NodeId, usize>,
    effect_at: &BTreeMap<NodeId, (usize, usize)>,
) -> Vec<SaveModulator> {
    let at = ModulatorLocation::Free;
    let rack = &model.free_modulators;
    let order: Vec<usize> = chosen.iter().copied().collect();
    let place_in_module: BTreeMap<usize, usize> = order.iter().enumerate().map(|(place, index)| (*index, place)).collect();
    let plugin_place = |node: NodeId| -> Option<CablePlace> {
        effect_at.get(&node).map(|&(instrument, effect)| CablePlace::Effect { instrument, effect }).or_else(|| instrument_at.get(&node).map(|&lane| CablePlace::Instrument(lane)))
    };
    // (For each cable of each of these modulators: whether it stays, and, for one that drives the depth of another, which cable that is: its modulator's place in the rack and its own among that modulator's.)
    let mut stays: Vec<Vec<bool>> = Vec::new();
    let mut depth_of: Vec<Vec<Option<(usize, usize)>>> = Vec::new();
    for &index in &order {
        let (mut modulator_stays, mut modulator_depth_of) = (Vec::new(), Vec::new());
        for target in &rack[index].targets {
            let (keep, of) = match (model.kind_of(at, index, target.aim), target.aim) {
                (None, _) => (false, None),
                // (A value that the host keeps is a plugin's too: `kind_of` says that the node has it, and a plugin that is chosen is an instrument, which has a volume, or an effect, which has a mix.)
                (Some(_), Driven::Param { plugin: node, .. } | Driven::Gain { node, .. }) => (plugin_place(node).is_some(), None),
                (Some(_), Driven::Setting { modulator, .. }) => (matches!(model.locate(modulator), Some(NodePath::Modulator { index, .. }) if place_in_module.contains_key(&index)), None),
                (Some(_), Driven::Depth(edge)) => match model.locate_edge(edge) {
                    Some((_, owner, cable)) if place_in_module.contains_key(&owner) => (true, Some((owner, cable))),
                    _ => (false, None),
                },
            };
            modulator_stays.push(keep);
            modulator_depth_of.push(of);
        }
        stays.push(modulator_stays);
        depth_of.push(modulator_depth_of);
    }
    loop {
        let mut changed = false;
        for (place, cables) in depth_of.iter().enumerate() {
            for (target, of) in cables.iter().enumerate() {
                let Some((owner, cable)) = *of else { continue };
                if stays[place][target] && !stays[place_in_module[&owner]][cable] {
                    stays[place][target] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // (The place that each cable that stays has among the cables of its modulator that stay: what `mod_depth` names a cable by.)
    let rank: Vec<Vec<usize>> = stays.iter().map(|cables| cables.iter().scan(0, |kept, stays| Some(if *stays { *kept += 1; *kept - 1 } else { *kept })).collect()).collect();
    order
        .iter()
        .enumerate()
        .map(|(place, &index)| {
            let modulator = &rack[index];
            let targets = modulator
                .targets
                .iter()
                .enumerate()
                .filter(|(target, _)| stays[place][*target])
                .filter_map(|(_, cable)| {
                    let sibling = |mod_index: usize| place_in_module.get(&mod_index).copied();
                    let (kind, to) = match model.kind_of(at, index, cable.aim)? {
                        kind @ ModTargetKind::FreeParam { plugin, .. } => (kind, Some(plugin_place(plugin)?)),
                        kind @ ModTargetKind::Gain { node, .. } => (kind, Some(plugin_place(node)?)),
                        ModTargetKind::ModulatorRate { mod_index } => (ModTargetKind::ModulatorRate { mod_index: sibling(mod_index)? }, None),
                        ModTargetKind::ModulatorAttack { mod_index } => (ModTargetKind::ModulatorAttack { mod_index: sibling(mod_index)? }, None),
                        ModTargetKind::ModulatorDecay { mod_index } => (ModTargetKind::ModulatorDecay { mod_index: sibling(mod_index)? }, None),
                        ModTargetKind::ModulatorSustain { mod_index } => (ModTargetKind::ModulatorSustain { mod_index: sibling(mod_index)? }, None),
                        ModTargetKind::ModulatorRelease { mod_index } => (ModTargetKind::ModulatorRelease { mod_index: sibling(mod_index)? }, None),
                        ModTargetKind::ModulatorDepth { mod_index, target_index } => {
                            let owner = sibling(mod_index)?;
                            (ModTargetKind::ModulatorDepth { mod_index: owner, target_index: *rank[owner].get(target_index)? }, None)
                        }
                    };
                    Some(SaveModTarget { kind, label: cable.param_name.clone(), depth: cable.depth, to })
                })
                .collect();
            // (It hears a lane of the module if the lane that it heard is chosen, and every note if not: the lane's place is the module's.)
            let hears = modulator.ear.and_then(|ear| model.instruments.iter().position(|lane| lane.node == ear)).and_then(|lane| lane_at.get(&lane)).map(|&lane| CablePlace::Instrument(lane));
            SaveModulator { source: save::source_to_save(&modulator.source), targets, ui: None, hears }
        })
        .collect()
}

/// The modules of the library `dir`, as the catalog lists them: each file `<name>.toml` that is a module, by its name, with how many plugins it has (the instruments and the effects) where a plugin has its parameters. A
/// file that is not a module (or cannot be read) is left out, and said in the log; a directory that is not there has none.
pub fn list(dir: &Path) -> Vec<PluginInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PluginInfo> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.strip_suffix(".toml")?.to_string();
            // (Only a name that this program could have saved under: the id goes to a client, and is loaded by it.)
            if clean_name(&name).ok().as_deref() != Some(name.as_str()) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                return None;
            }
            match std::fs::read_to_string(&path).map_err(|error| error.to_string()).and_then(|text| parse(&text)) {
                Ok(module) => Some(PluginInfo { id: id_of(&name), name, is_instrument: false, param_count: module.plugin_count(), preset_count: 0, path: path.to_string_lossy().into_owned(), scan_ms: 0 }),
                Err(error) => {
                    log::warn!("{} is not a module and is left out of the library: {error}", path.display());
                    None
                }
            }
        })
        .collect();
    found.sort_by_key(|module| module.name.to_lowercase());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_is_known_by_the_id_that_the_catalog_gives_it_and_a_plugin_is_not() {
        assert_eq!(id_of("Space"), "module:Space");
        assert_eq!(name_of("module:Space"), Some("Space"));
        assert_eq!(name_of("builtin:reverb"), None);
        assert_eq!(name_of("module:"), Some(""));
    }

    #[test]
    fn the_name_of_a_module_is_a_plain_file_name_without_its_extension() {
        assert_eq!(clean_name("Space"), Ok("Space".to_string()));
        assert_eq!(clean_name("  Space  "), Ok("Space".to_string()));
        assert_eq!(clean_name("Space.toml"), Ok("Space".to_string()));
        assert_eq!(clean_name("Big room 2"), Ok("Big room 2".to_string()));
        for bad in ["", "   ", ".toml", "a/b", "..\\b", "a:b", "what?", "con", "tab\tname"] {
            assert!(clean_name(bad).is_err(), "{bad:?}");
        }
        assert!(clean_name(&"x".repeat(300)).is_err());
        // The name of the file is the name and the extension: a name with the extension twice is a file that the library lists under another name, so it is not one.
        for twice in ["a.toml.toml", "Space.toml.toml"] {
            assert!(clean_name(twice).is_err_and(|why| why.contains("does not end in .toml")), "{twice:?}");
        }
        assert_eq!(clean_name("a.tomlx"), Ok("a.tomlx".to_string()), "only the extension is the extension");
        assert_eq!(clean_name("x.toml "), Ok("x".to_string()), "the spaces round a name go first");
    }

    /// The effect `effect` of lane `lane` of a module, as a cable starts at it.
    fn effect_out(lane: usize, effect: usize) -> ModuleOutput {
        ModuleOutput::Effect { lane, effect }
    }

    /// The input (or the second input) of the effect `effect` of lane `lane` of a module, as a cable ends at it.
    fn effect_in(lane: usize, effect: usize, side: bool) -> ModuleInput {
        ModuleInput { lane, effect, side }
    }

    #[test]
    fn a_module_that_is_a_chain_of_effects_is_read_as_one_with_its_cables_and_a_second_input() {
        let chain = "[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.5\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"instrument[0].effect[1]\"\ngain = 0.5\n";
        let module = parse(chain).unwrap();
        assert_eq!(module.lanes.len(), 1);
        assert!(module.lanes[0].instrument.is_none());
        assert_eq!(module.lanes[0].effects.iter().map(|effect| effect.plugin.as_str()).collect::<Vec<_>>(), ["builtin:filter", "builtin:reverb"]);
        assert_eq!(module.plugin_count(), 2);
        assert_eq!(module.cables, [ModuleCable { from: effect_out(0, 0), to: effect_in(0, 1, false), gain: 0.5 }]);
        let side = format!("{chain}[[cable]]\nfrom = \"instrument[0].effect[1]\"\nto = \"instrument[0].effect[0].side\"\n");
        assert_eq!(parse(&side).unwrap().cables[1], ModuleCable { from: effect_out(0, 1), to: effect_in(0, 0, true), gain: 1.0 });
    }

    #[test]
    fn a_module_that_is_a_graph_of_lanes_with_an_instrument_and_cables_between_them_is_read_as_one() {
        let graph = "[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.5\npreset = \"Pad\"\npinned = [\"detune\"]\n[instrument.params]\ndetune = 3.0\n\
                     [[instrument.effect]]\nplugin = \"builtin:filter\"\n\
                     [[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\n\
                     [[cable]]\nfrom = \"instrument[0]\"\nto = \"instrument[1].effect[0]\"\ngain = 0.25\n\
                     [[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"instrument[1].effect[1].side\"\n";
        let module = parse(graph).unwrap();
        assert_eq!(module.lanes.len(), 2);
        let instrument = module.lanes[0].instrument.as_ref().expect("the first lane has an instrument");
        assert_eq!((instrument.plugin.as_str(), instrument.volume, instrument.preset.as_deref(), instrument.params.get("detune"), instrument.pinned.as_slice()), ("builtin:osc", 0.5, Some("Pad"), Some(&3.0), ["detune".to_string()].as_slice()));
        assert_eq!((module.lanes[0].effects.len(), module.lanes[1].effects.len(), module.lanes[1].instrument.is_none()), (1, 2, true));
        assert_eq!(module.plugin_count(), 4, "an instrument and three effects");
        assert_eq!(
            module.cables,
            [
                ModuleCable { from: ModuleOutput::Instrument { lane: 0 }, to: effect_in(1, 0, false), gain: 0.25 },
                ModuleCable { from: effect_out(0, 0), to: effect_in(1, 1, true), gain: 1.0 },
            ]
        );
        // A module may be nothing but an instrument.
        assert_eq!(parse("[[instrument]]\nplugin = \"builtin:osc\"\n").unwrap().plugin_count(), 1);
    }

    #[test]
    fn what_is_not_a_module_is_refused_with_the_reason() {
        let refused = |text: &str| parse(text).err().unwrap_or_else(|| "was read".to_string());
        assert!(refused("").contains("no lanes"), "{}", refused(""));
        assert!(refused("[[instrument]]\n").contains("no instrument and no effect"), "{}", refused("[[instrument]]\n"));
        assert!(refused("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\n").contains("lane 1 of the module has no instrument and no effect"), "an empty lane among others");
        let one = "[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n";
        assert!(refused(&format!("[[group]]\n{one}")).contains("nothing else"));
        // (A source of notes and a device are not the module's; a modulator is, since step 106.)
        assert!(refused(&format!("[keys]\n{one}")).contains("nothing else"), "{}", refused(&format!("[keys]\n{one}")));
        assert!(refused(&format!("[[device]]\nname = \"Launchkey\"\n{one}")).contains("nothing else"));
        assert_eq!(parse(&format!("[[modulator]]\n{one}")).map(|module| module.modulators.len()), Ok(1), "a modulator that drives nothing is one all the same");
        // A lane's own modulator is moved to the session's own when a file is read, `session::parse`, and hears the lane that it was in: the older file's module has it so.
        let moved = parse(&format!("{one}[[instrument.modulator]]\ntype = \"lfo\"\n")).unwrap();
        assert_eq!((moved.modulators.len(), moved.modulators[0].hears), (1, Some(0)));
        // A cable that is not between two plugins of the module is refused, whatever it joins: the mix, a group, a lane (which has no input), a source of notes (which starts a cable of notes).
        let between = |from: &str, to: &str| format!("{one}[[cable]]\nfrom = \"{from}\"\nto = \"{to}\"\n");
        for (from, to) in [
            ("instrument[0].effect[0]", "master"),
            ("instrument[0].effect[0]", "group[0]"),
            ("instrument[0].effect[0]", "instrument[0]"),
            ("group[0]", "instrument[0].effect[0]"),
            ("master", "instrument[0].effect[0]"),
            ("keys", "instrument[0]"),
            ("instrument[0].effect[0].side", "instrument[0].effect[0]"),
        ] {
            assert!(refused(&between(from, to)).contains("cable[0] is not between two plugins"), "{from} to {to}: {}", refused(&between(from, to)));
        }
        // A cable that names a plugin that the module has not: past the last effect of the lane, a lane that is not there, and an instrument that the lane does not have, at either end.
        for (from, to) in [
            ("instrument[0].effect[0]", "instrument[0].effect[1]"),
            ("instrument[0].effect[1]", "instrument[0].effect[0]"),
            ("instrument[0].effect[0]", "instrument[0].effect[1].side"),
            ("instrument[1].effect[0]", "instrument[0].effect[0]"),
            ("instrument[0].effect[0]", "instrument[1].effect[0]"),
            ("instrument[1].effect[0]", "instrument[0].effect[0].side"),
            ("instrument[0].effect[0]", "instrument[1].effect[0].side"),
            ("instrument[0]", "instrument[0].effect[0]"),
        ] {
            assert!(refused(&between(from, to)).contains("cable[0] names a plugin that the module has not"), "{from} to {to}: {}", refused(&between(from, to)));
        }
        // The instrument of a lane that has one starts a cable, and the second lane's effect ends it.
        let two = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[cable]]\nfrom = \"instrument[0]\"\nto = \"instrument[1].effect[0]\"\n";
        assert!(parse(two).is_ok(), "{}", refused(two));
        assert!(refused("this is not toml [").contains("TOML") || !refused("this is not toml [").is_empty());
    }

    /// Two lanes: the first has an oscillator and a filter, the second a math effect and no instrument.
    const TWO_LANES: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:math\"\n";

    #[test]
    fn a_module_that_has_modulators_is_read_with_the_lane_each_hears_and_what_its_cables_drive() {
        let text = format!(
            "{TWO_LANES}\
             [[modulator]]\ntype = \"lfo\"\nrate = 2.0\nretrigger = true\nhears = \"instrument[1]\"\n\
             [[modulator.target]]\nto = \"instrument[0].effect[0]\"\nparam = \"cutoff\"\ndepth = 0.25\n\
             [[modulator.target]]\nto = \"instrument[0]\"\ngain = \"volume\"\ndepth = -0.5\n\
             [[modulator.target]]\nto = \"instrument[1].effect[0]\"\ngain = \"mix\"\n\
             [[modulator.target]]\nmod_attack = 1\ndepth = 0.3\n\
             [[modulator]]\ntype = \"envelope\"\nattack = 0.2\n\
             [[modulator.target]]\nmod_depth = [0, 0]\ndepth = 0.1\n\
             [[modulator.target]]\nmod_rate = 0\n\
             [[modulator]]\ntype = \"macro\"\nvalue = 3.0\n"
        );
        let module = parse(&text).unwrap();
        assert_eq!(module.modulators.len(), 3);
        let (lfo, envelope, knob) = (&module.modulators[0], &module.modulators[1], &module.modulators[2]);
        assert!(matches!(lfo.source, ModSourceSlot::Lfo { rate, retrigger: true, .. } if rate == 2.0), "{:?}", lfo.source);
        assert_eq!((lfo.hears, envelope.hears, knob.hears), (Some(1), None, None), "the lane that an ear names is a lane of the module, and none is every note");
        let drives = |modulator: &ModuleModulator| modulator.targets.iter().map(|target| (target.drives.clone(), target.depth)).collect::<Vec<_>>();
        assert_eq!(
            drives(lfo),
            [
                (ModuleDrives::Param { plugin: ModuleOutput::Effect { lane: 0, effect: 0 }, name: "cutoff".to_string() }, 0.25),
                (ModuleDrives::Gain { plugin: ModuleOutput::Instrument { lane: 0 }, gain: ModGain::Volume }, -0.5),
                (ModuleDrives::Gain { plugin: ModuleOutput::Effect { lane: 1, effect: 0 }, gain: ModGain::Mix }, 0.5),
                (ModuleDrives::Setting { modulator: 1, port: Port::Attack }, 0.3),
            ]
        );
        assert_eq!(drives(envelope), [(ModuleDrives::Depth { modulator: 0, target: 0 }, 0.1), (ModuleDrives::Setting { modulator: 0, port: Port::Rate }, 0.5)]);
        assert!(knob.targets.is_empty());
        // What the loader makes of a table is what a module has: the knob that a file says is at 3 is held to the 1 that a knob goes to.
        assert!(matches!(knob.source, ModSourceSlot::Macro { value, .. } if value == 1.0), "{:?}", knob.source);
        assert_eq!(module.plugin_count(), 3, "a module's plugins are the effects and the instruments: the modulators are not counted");
    }

    #[test]
    fn a_macro_of_a_module_has_its_name_and_whether_its_knob_is_shown_and_one_that_has_neither_is_a_macro_that_is_shown() {
        let text = format!("{TWO_LANES}[[modulator]]\ntype = \"macro\"\nname = \"Brightness\"\nexpose = false\n[[modulator]]\ntype = \"macro\"\nvalue = 0.5\n");
        let module = parse(&text).unwrap();
        assert!(matches!(&module.modulators[0].source, ModSourceSlot::Macro { name, expose: false, .. } if name == "Brightness"), "{:?}", module.modulators[0].source);
        assert!(matches!(&module.modulators[1].source, ModSourceSlot::Macro { name, expose: true, value } if name.is_empty() && *value == 0.5), "{:?}", module.modulators[1].source);
    }

    #[test]
    fn what_a_modulator_of_a_module_names_that_the_module_has_not_is_refused_with_its_place() {
        let refused = |text: String| parse(&text).err().unwrap_or_else(|| "was read".to_string());
        let with = |modulator: &str| format!("{TWO_LANES}[[modulator]]\n{modulator}");
        let cable = |lines: &str| with(&format!("[[modulator.target]]\n{lines}"));
        // The lane that it hears: one that the module has not, a group, something that is no place.
        for ear in ["instrument[2]", "group[0]", "keys", "nonsense"] {
            let why = refused(with(&format!("hears = \"{ear}\"\n")));
            assert!(why.contains("modulator[0].hears") && why.contains("not a lane of the module"), "{ear}: {why}");
        }
        assert!(parse(&with("hears = \"instrument[1]\"\n")).is_ok(), "a lane that has no instrument is a lane of the module all the same");
        // The plugin that a cable drives: an effect or a lane that is not there, an instrument that the lane has not, a bus, the mix, nothing and no `to` at all.
        for to in ["instrument[2].effect[0]", "instrument[0].effect[1]", "instrument[1]", "group[0].effect[0]", "master", "nonsense", ""] {
            let why = refused(cable(&format!("to = \"{to}\"\nparam = \"cutoff\"\n")));
            assert!(why.contains("modulator[0].target[0] names") && why.contains("not a plugin of the module"), "{to:?}: {why}");
        }
        assert!(refused(cable("param = \"cutoff\"\n")).contains("not a plugin of the module"), "no `to`");
        assert!(parse(&cable("to = \"instrument[0]\"\nparam = \"detune\"\n")).is_ok(), "a parameter of the oscillator");
        // A volume or a mix: a word that is none of them, a mix of an instrument, a volume of an effect and the pan of anything (a group's, which the module has not).
        assert!(refused(cable("to = \"instrument[0]\"\ngain = \"level\"\n")).contains("not a volume, a mix or a pan"));
        for (to, gain) in [("instrument[0]", "mix"), ("instrument[0].effect[0]", "volume"), ("instrument[0].effect[0]", "pan"), ("instrument[0]", "pan")] {
            let why = refused(cable(&format!("to = \"{to}\"\ngain = \"{gain}\"\n")));
            assert!(why.contains("modulator[0].target[0] drives the") && why.contains("of a plugin that has none"), "{to} {gain}: {why}");
        }
        // A modulator of the module that does not exist, itself, and the depth of a cable that is not there.
        assert!(refused(cable("mod_rate = 0\n")).contains("itself"));
        assert!(refused(cable("mod_rate = 1\n")).contains("modulator 1, which is not in the module"));
        assert!(refused(cable("mod_release = 4\n")).contains("not in the module"));
        assert!(refused(cable("mod_depth = [0, 0]\n")).contains("itself"));
        let two = format!("{TWO_LANES}[[modulator]]\n[[modulator.target]]\nmod_depth = [1, 1]\n[[modulator]]\n[[modulator.target]]\nmod_rate = 0\n");
        let why = refused(two);
        assert!(why.contains("the depth of cable 1 of modulator 1, which has 1 cables"), "{why}");
        // A cable that drives nothing that it names.
        assert!(refused(cable("depth = 0.5\n")).contains("modulator[0].target[0] drives nothing"));
        // The cables are told apart by the order in which they are looked at, as the loader does: a modulator's cable that names a modulator, a stage and a cable of the module is read.
        let fine = format!("{TWO_LANES}[[modulator]]\n[[modulator.target]]\nmod_decay = 1\n[[modulator.target]]\nmod_sustain = 1\n[[modulator.target]]\nmod_release = 1\n[[modulator]]\n[[modulator.target]]\nmod_depth = [0, 2]\n");
        let module = parse(&fine).unwrap();
        let ports: Vec<Port> = module.modulators[0].targets.iter().map(|target| if let ModuleDrives::Setting { port, .. } = target.drives { port } else { panic!("not a setting") }).collect();
        assert_eq!(ports, [Port::Decay, Port::Sustain, Port::Release]);
        assert_eq!(module.modulators[1].targets[0].drives, ModuleDrives::Depth { modulator: 0, target: 2 });
    }

    #[test]
    fn the_library_lists_the_files_that_are_modules_by_name_and_leaves_the_others_out() {
        let dir = tempfile::tempdir().unwrap();
        let chain = |plugins: &[&str]| plugins.iter().fold("[[instrument]]\n".to_string(), |text, plugin| format!("{text}[[instrument.effect]]\nplugin = \"{plugin}\"\n"));
        std::fs::write(dir.path().join("Space.toml"), chain(&["builtin:filter", "builtin:reverb"])).unwrap();
        std::fs::write(dir.path().join("alpha.toml"), chain(&["builtin:delay"])).unwrap();
        // (A graph of two lanes with an instrument is listed with how many plugins it has, the instrument and the three effects.)
        std::fs::write(dir.path().join("Voice.toml"), "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\n").unwrap();
        std::fs::write(dir.path().join("broken.toml"), "[[instrument]]\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a module").unwrap();
        // (A file that is valid and has a name that no module is saved under: the listing would call it `twice.toml`, which is not a name that this program makes of `twice.toml`.)
        std::fs::write(dir.path().join("twice.toml.toml"), chain(&["builtin:delay"])).unwrap();
        std::fs::create_dir(dir.path().join("folder.toml")).unwrap();
        let listed = list(dir.path());
        assert_eq!(
            listed.iter().map(|module| (module.id.as_str(), module.name.as_str(), module.param_count, module.preset_count, module.is_instrument)).collect::<Vec<_>>(),
            [("module:alpha", "alpha", 1, 0, false), ("module:Space", "Space", 2, 0, false), ("module:Voice", "Voice", 4, 0, false)]
        );
        assert!(list(&dir.path().join("not-there")).is_empty());
    }
}
