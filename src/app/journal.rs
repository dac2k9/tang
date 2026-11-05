//! What puts back what was taken out (`docs/web-ui.md` §5, D37).
//!
//! A removal is undone by putting back what it took, where it was, under its ids and the telemetry slots it had: a cable, with what it was centred on,
//! as `SessionModel::remove_edge` hands it back, a modulator with its own cables, as `SessionModel::remove_modulator_at` does, an effect (a lane's or
//! a bus's) with the plugin that was in the chain, and a lane with everything it has; the model and the audio thread are given them again
//! (`SessionModel::insert_target_at`, `insert_modulator_at`, `insert_effect_at`, `insert_bus_effect_at` and `insert_lane_at`; `GraphCommand::InsertModTarget`,
//! `InsertModulator`, `InsertEffect`, `InsertGroupEffect` and `InsertInstrument`). What an operation of the controller takes out is written down as it goes,
//! in a `Journal`, and a step of the history that holds one
//! puts everything back in the reverse order (`Controller::rewind`): the last thing taken out is the first put back, so that each goes back into the
//! session as it was when its turn to go came, and the places that the things taken out after it had are the places they have again.
//!
//! The plugin that is taken out of a chain is the audio thread's to hand back: the controller makes a channel for it (`RemoveEffect { keep }`) and the
//! journal holds the other end (`Kept::Asked`), so that undo puts the very plugin back, with whatever it holds, at once; if it did not come (the audio thread
//! is not running, or the history let go of it, `History` keeps only the newest few) the plugin is loaded again with its preset and the values the user set.
//! A lane is handed back whole (`RemoveInstrument { keep }`, `KeptLane`): its plugins, its modulators and what they have in hand, its pattern; the history lets
//! go of a lane's plugins, and not of the lane, when it keeps too many, and the plugins are loaded again into it. A lane that the audio thread did not hand
//! back cannot be had again, and the undo is refused.
//!
//! An instrument that another one replaces is taken out of its lane as an effect is taken out of a chain (`SwapInstrument { keep }`), and put back by another swap,
//! with the lane's note remap, which goes with the instrument it was for; the cables that drove the old instrument's parameters went before it.
//!
//! A preset that is loaded overwrites what the plugin has (the preset it had and its parameters' values and baselines), and stays in the plugin: what it overwrote is written
//! down as it goes (`Inverse::Preset`), and undo has the audio thread load the preset it had again and gives each of the rows it overwrote its value.

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};

use crate::app::apply::{Done, PluginAt};
use crate::app::controller::{has_audio_output, outputs_fit_the_chain, ChainAt, Controller};
use crate::app::intent::{Applied, IntentError};
use crate::app::model::{mod_source_slot_to_graph, modulator_slot_commands, AudioCable, EventCable, GroupNode, InstrumentNode, ModTargetSlot, ModuleInstance, ModulatorSlot, NodeId, NodePath, ParamKind, PluginSlot, RemovedCable, SourceKind, SourceNode};
use crate::plugin::chain::{GraphCommand, KeptGroup, KeptLane, ModTarget, ModulatorLocation, NoteRemapper};
use crate::plugin::{self, Plugin};
use crate::session::RemapTarget;

/// One thing that was taken out, and what it takes to put it back.
#[derive(Debug)]
pub(super) enum Inverse {
    /// A cable, which goes back to its place in its modulator.
    Cable(RemovedCable),
    /// A modulator with its own cables, which goes back to its place in its rack: `bases` is what each of the cables was centred on, in order (what the
    /// user had set what it drives to).
    Modulator { at: ModulatorLocation, index: usize, modulator: ModulatorSlot, bases: Vec<f32> },
    /// An effect of a lane's chain or of a group's bus, which goes back to its place in the chain: `slot` is the model's plugin (its node, its values, its mix),
    /// and `kept` the plugin that was in the chain, or what puts one back.
    Effect { at: ChainAt, index: usize, slot: PluginSlot, kept: Kept<Box<dyn Plugin>> },
    /// A lane, which goes back among the lanes at `index`: `lane` is the model's node with everything the model has of it (its plugins' slots, its modulators and their cables,
    /// its pattern, its group), and `kept` the lane that the audio thread handed back, with its plugins and whatever they hold, or what puts one back.
    Lane { index: usize, lane: InstrumentNode, kept: Kept<KeptLane> },
    /// A group, which goes back among the groups at `index`: `group` is the model's node (its bus effects' slots, its modulators, its meter), `members` the lanes that were in it
    /// (by node, in the order of the lanes) and `kept` the group that the audio thread handed back, with its bus effects and whatever they hold, or what puts one back. (The cables
    /// of its modulators went before it, each written down as a cable, and come back after it.)
    Group { index: usize, group: GroupNode, members: Vec<NodeId>, kept: Kept<KeptGroup> },
    /// The instrument of a lane (by its node) that another one replaced, or none, for a lane that had no instrument and has been filled: undo gives the lane what it had, and
    /// empties it if it had nothing. (The cables that drove the old instrument's parameters went before it, each written down as a cable, and come back after it.)
    Instrument { lane: NodeId, before: Option<Replaced> },
    /// The preset that a plugin (by its node: a lane's instrument or one of its effects) had before another was loaded, and the rows that the load overwrote (`rows_a_preset_changes`):
    /// undo has the audio thread load the preset again, if it has one by that name, and gives the rows back what they had.
    Preset { node: NodeId, preset: Option<String>, rows: Vec<RowBefore> },
    /// An audio cable, which goes back among the audio cables under the id it had, with the ends and the level it had. (It is taken out before the nodes that it starts and
    /// ends at, and so is put back after them.)
    AudioCable(AudioCable),
    /// A cable of notes, which goes back among them under the id it had, from the source and to the lane that it was between. (It is taken out before the lane that it ends at, or the source that it
    /// starts at, and so is put back after it.)
    EventCable(EventCable),
    /// A source of notes, which goes back among the sources at `index`, under the node it had. (The cables of notes that started at it were taken out before it, each written down, and come back after it.)
    Source { index: usize, source: SourceNode },
    /// A module (step 108) as it was before a member left it or it went with all its members: undo gives it back, as it was, at its place among the modules (one that is still there, with a member less, is
    /// replaced by what it was). It is written down before the member that left, so that it is put back after it.
    Module { index: usize, module: ModuleInstance },
    /// Something went that cannot be put back one thing at a time (cables that drive each other's depths, `RemovedCable::still_driven`): what
    /// the operation did is not a step of the history, which is forgotten as it is for what cannot be undone.
    NoWayBack,
}

/// What was taken out of the audio thread's graph (a plugin, `Box<dyn Plugin>`, or a lane, `KeptLane`), and whether the controller has the very one to put back.
pub(super) enum Kept<T> {
    /// The audio thread was asked to hand it to this channel: it is there, or on its way.
    Asked(Receiver<T>),
    /// The very one, in hand.
    InHand(T),
    /// None is kept (nothing asked for one, or the history let go of it): a plugin is loaded again, and a lane that was not kept cannot be put back.
    Nothing,
}

impl<T> fmt::Debug for Kept<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kept::Asked(_) => "Kept::Asked",
            Kept::InHand(_) => "Kept::InHand",
            Kept::Nothing => "Kept::Nothing",
        })
    }
}

/// A row of a plugin's parameters as the model had it before a preset was loaded: the value, and what it is compared with when the file is saved.
#[derive(Debug)]
pub(super) struct RowBefore {
    pub(super) index: u32,
    pub(super) value: f32,
    pub(super) default: f32,
}

/// An instrument that was taken out of its lane by a replacement: the model's plugin (its values, its preset, its pins), the lane's note remap, which goes with the
/// instrument it was for (the audio thread drops the remapper when the instrument goes) and the pitch bend range that belongs to it, and the plugin that was in the
/// lane, or what puts one back.
#[derive(Debug)]
pub(super) struct Replaced {
    pub(super) slot: PluginSlot,
    pub(super) remap: HashMap<String, RemapTarget>,
    pub(super) pitch_bend_range: f64,
    pub(super) kept: Kept<Box<dyn Plugin>>,
}

/// What an operation took out, in the order it did.
#[derive(Debug, Default)]
pub struct Journal {
    inverses: Vec<Inverse>,
}

impl Journal {
    /// Write down one more thing that was taken out.
    pub(super) fn push(&mut self, inverse: Inverse) {
        self.inverses.push(inverse);
    }

    /// How many plugins it keeps to put back (asked for, or in hand): an effect's, an instrument that was replaced, and those of a lane, its instrument and its effects.
    pub fn plugins_kept(&self) -> usize {
        self.inverses
            .iter()
            .map(|inverse| match inverse {
                Inverse::Effect { kept: Kept::Asked(_) | Kept::InHand(_), .. } => 1,
                Inverse::Instrument { before: Some(Replaced { kept: Kept::Asked(_) | Kept::InHand(_), .. }), .. } => 1,
                Inverse::Lane { lane, kept: Kept::Asked(_), .. } => plugins_of(lane),
                Inverse::Lane { kept: Kept::InHand(kept), .. } => kept.plugin_count(),
                Inverse::Group { group, kept: Kept::Asked(_), .. } => group.effects.len(),
                Inverse::Group { kept: Kept::InHand(kept), .. } => kept.plugin_count(),
                _ => 0,
            })
            .sum()
    }

    /// Let go of the plugins it keeps: they are loaded again if what it took out is put back. (A plugin that is dropped is dropped here, on the thread that
    /// holds the history, as the main thread drops what the audio thread hands back.) A lane keeps its place for them: it is not let go of, unless it has not come.
    pub fn let_go_of_plugins(&mut self) {
        for inverse in &mut self.inverses {
            match inverse {
                Inverse::Effect { kept, .. } => *kept = Kept::Nothing,
                Inverse::Instrument { before: Some(Replaced { kept, .. }), .. } => *kept = Kept::Nothing,
                Inverse::Lane { kept, .. } => {
                    let lane = match std::mem::replace(kept, Kept::Nothing) {
                        Kept::Asked(channel) => channel.try_recv().ok(),
                        Kept::InHand(lane) => Some(lane),
                        Kept::Nothing => None,
                    };
                    if let Some(mut lane) = lane {
                        drop(lane.take_plugins());
                        *kept = Kept::InHand(lane);
                    }
                }
                Inverse::Group { kept, .. } => {
                    let group = match std::mem::replace(kept, Kept::Nothing) {
                        Kept::Asked(channel) => channel.try_recv().ok(),
                        Kept::InHand(group) => Some(group),
                        Kept::Nothing => None,
                    };
                    if let Some(mut group) = group {
                        drop(group.take_plugins());
                        *kept = Kept::InHand(group);
                    }
                }
                _ => {}
            }
        }
    }

    /// Whether something was taken out and all of it can be put back.
    pub fn can_be_put_back(&self) -> bool {
        !self.inverses.is_empty() && !self.inverses.iter().any(|inverse| matches!(inverse, Inverse::NoWayBack))
    }
}

/// How many plugins a lane has: its instrument and its effects.
fn plugins_of(lane: &InstrumentNode) -> usize {
    usize::from(lane.instrument.is_some()) + lane.effects.len()
}

/// What a lane is called in the words of a button and of a refusal: "the lane of Oscillator"; with no instrument (step 87) what it holds, "Reverb" for the one effect
/// that stands by itself in it and "a chain of effects" for more, and "an empty lane" if it holds nothing.
pub(super) fn lane_words(lane: &InstrumentNode) -> String {
    match (&lane.instrument, lane.effects.as_slice()) {
        (Some(instrument), _) => format!("the lane of {}", instrument.name),
        (None, []) => "an empty lane".to_string(),
        (None, [effect]) => effect.name.clone(),
        (None, _) => "a chain of effects".to_string(),
    }
}

/// A cable as the audio thread is told of it when it goes in: under its id, driving what it drove, and centred on `base_value`.
fn cable_for_the_audio_thread(slot: &ModTargetSlot, base_value: f32) -> ModTarget {
    ModTarget { edge: slot.edge, aim: slot.aim, depth: slot.depth, base_value, param_min: slot.param_min, param_max: slot.param_max, effective: slot.effective }
}

impl Controller {
    /// Write down, if an intent is being carried out, that something was taken out. (What the TUI and the tests take out by themselves, with no intent,
    /// is not written down: nobody asked for a step.)
    pub(super) fn took_out(&mut self, inverse: Inverse) {
        if let Some(journal) = &mut self.journal {
            journal.push(inverse);
        }
    }

    /// A structural edit that no intent asks for (what the TUI and the tests call by themselves) is no step of the history, and a step that took something
    /// out puts it back by the places it had, which the edit changes: the history is forgotten. (While an intent is carried out, which is a step of the history
    /// or forgets it as it should, the journal is there, and nothing is forgotten here.)
    pub(super) fn forget_the_history_unless_an_intent_asks(&mut self) {
        if self.journal.is_none() {
            self.history.clear();
        }
    }

    /// A channel for what a removal takes out of the audio thread's graph (a plugin of a chain, or a lane), when an intent is being carried out: the sender is for the
    /// command (`RemoveEffect { keep }`, `RemoveInstrument { keep }`), the `Kept` is what the journal holds. (Nothing, for a call that no intent asks for: nobody puts
    /// that back.)
    pub(super) fn channel_for<T>(&self) -> (Option<Sender<T>>, Kept<T>) {
        if self.journal.is_none() {
            return (None, Kept::Nothing);
        }
        let (keep, kept) = crossbeam_channel::bounded(1);
        (Some(keep), Kept::Asked(kept))
    }

    /// The plugin to put back: the very one if the audio thread handed it back (it is waited for a moment, `recovery_wait`, if it has not yet), else the plugin
    /// loaded again, with the preset it had and the values the user set. Refused, with the reason, if that does not load or `fits` (what the place it goes to asks
    /// of a plugin: an effect's outputs are the chain's, an instrument has some) says it does not.
    fn recover_plugin(
        &self,
        slot: &mut PluginSlot,
        kept: Kept<Box<dyn Plugin>>,
        deadline: Instant,
        fits: fn(&dyn Plugin, &PluginSlot) -> Result<(), IntentError>,
    ) -> Result<Box<dyn Plugin>, IntentError> {
        let handed_back = match kept {
            Kept::InHand(plugin) => Some(plugin),
            Kept::Asked(channel) => channel.recv_deadline(deadline).ok(),
            Kept::Nothing => None,
        };
        let loaded = match handed_back {
            Some(plugin) => plugin,
            None => self.reload_plugin(slot)?,
        };
        fits(&*loaded, slot)?;
        Ok(loaded)
    }

    /// The lane to put back: the very lane if the audio thread handed it back (it is waited for a moment, `recovery_wait`, if it has not yet), with its plugins loaded
    /// again, as `recover_plugin` loads one, if it gave them up (the history lets go of them, to save memory). Refused, with the reason, if the lane did not come or a
    /// plugin cannot be had.
    fn recover_lane(&self, lane: &mut InstrumentNode, kept: Kept<KeptLane>, deadline: Instant) -> Result<KeptLane, IntentError> {
        let handed_back = match kept {
            Kept::InHand(kept) => Some(kept),
            Kept::Asked(channel) => channel.recv_deadline(deadline).ok(),
            Kept::Nothing => None,
        };
        let Some(mut handed_back) = handed_back else {
            return Err(IntentError::Invalid(format!("{} was not handed back by the audio thread", lane_words(lane))));
        };
        if handed_back.plugin_count() < plugins_of(lane) {
            let instrument = match &mut lane.instrument {
                Some(slot) => {
                    let loaded = self.reload_plugin(slot)?;
                    has_audio_output(&*loaded)?;
                    Some(loaded)
                }
                None => None,
            };
            let effects = lane
                .effects
                .iter_mut()
                .map(|slot| {
                    let loaded = self.reload_plugin(slot)?;
                    outputs_fit_the_chain(&slot.name, loaded.audio_output_count())?;
                    Ok(loaded)
                })
                .collect::<Result<Vec<_>, IntentError>>()?;
            handed_back.put_plugins(instrument, effects);
        }
        Ok(handed_back)
    }

    /// The group to put back: the very group if the audio thread handed it back (it is waited for a moment, `recovery_wait`, if it has not yet), with its bus effects loaded again,
    /// as `recover_plugin` loads one, if it gave them up (the history lets go of them, to save memory). Refused, with the reason, if the group did not come or a plugin cannot be had.
    fn recover_group(&self, index: usize, group: &mut GroupNode, kept: Kept<KeptGroup>, deadline: Instant) -> Result<KeptGroup, IntentError> {
        let handed_back = match kept {
            Kept::InHand(kept) => Some(kept),
            Kept::Asked(channel) => channel.recv_deadline(deadline).ok(),
            Kept::Nothing => None,
        };
        let Some(mut handed_back) = handed_back else {
            return Err(IntentError::Invalid(format!("{} was not handed back by the audio thread", group.display_name(index))));
        };
        if handed_back.plugin_count() < group.effects.len() {
            let effects = group
                .effects
                .iter_mut()
                .map(|slot| {
                    let loaded = self.reload_plugin(slot)?;
                    outputs_fit_the_chain(&slot.name, loaded.audio_output_count())?;
                    Ok(loaded)
                })
                .collect::<Result<Vec<_>, IntentError>>()?;
            handed_back.put_plugins(effects);
        }
        Ok(handed_back)
    }

    /// The plugin of `slot`, loaded again from what it was loaded from, with the preset it had and then the values that the model has, which are the user's. The slot has the new plugin's window from then on (step 112: a
    /// plugin that is loaded again is another instance, and has an editor of its own in the registry of the windows).
    fn reload_plugin(&self, slot: &mut PluginSlot) -> Result<Box<dyn Plugin>, IntentError> {
        let mut loaded = plugin::load(&slot.id, self.sample_rate, self.max_block_size, &self.runtime).map_err(|e| {
            log::error!("Failed to load plugin '{}': {e}", slot.id);
            IntentError::Invalid(format!("{} could not be loaded", slot.name))
        })?;
        slot.editor = loaded.editor_id();
        if let Some(preset) = slot.current_preset.as_ref().and_then(|name| slot.presets.iter().find(|preset| preset.name == *name)) {
            if let Err(e) = loaded.load_preset(&preset.id) {
                log::warn!("{}: preset '{}' did not load again: {e}", slot.name, preset.name);
            }
        }
        for param in slot.params.iter().filter(|param| !matches!(param.kind, ParamKind::Separator)) {
            // (A plugin that does not take a value that it gave is not worth refusing the undo for.)
            if let Err(e) = loaded.set_parameter(param.index, param.value) {
                log::debug!("{}: parameter {} did not take {}: {e}", slot.name, param.index, param.value);
            }
        }
        Ok(loaded)
    }

    /// Put back what `journal` says was taken out, the last thing first, as one revision: the structure is said to have changed. Refused, with nothing
    /// changed, if what is to come back is there already (a node or a cable has the id it had: another front-end has put it back by itself), or if a plugin
    /// cannot be had again.
    pub(super) fn rewind(&mut self, mut journal: Journal) -> Result<Applied, IntentError> {
        for inverse in &journal.inverses {
            match inverse {
                Inverse::Cable(cable) => self.refuse_a_cable_that_is_there(&cable.slot)?,
                Inverse::AudioCable(cable) => {
                    if self.model.edge_in_use(cable.id) {
                        return Err(IntentError::Invalid(format!("cable {} is there already", cable.id.0)));
                    }
                }
                Inverse::EventCable(cable) => {
                    if self.model.edge_in_use(cable.id) {
                        return Err(IntentError::Invalid(format!("cable {} is there already", cable.id.0)));
                    }
                }
                Inverse::Source { source, .. } => {
                    if self.model.locate(source.node).is_some() {
                        return Err(IntentError::Invalid(format!("node {} is there already", source.node.0)));
                    }
                    // (The keys are one source, and a device has one node: a session that has another of the kind now is not given a second.)
                    if self.model.sources.iter().any(|there| there.kind == source.kind) {
                        return Err(IntentError::Invalid(match &source.kind {
                            SourceKind::Keys => "the keys are a source already".to_string(),
                            SourceKind::Device(name) => format!("{name} is a source already"),
                        }));
                    }
                }
                Inverse::Effect { slot, .. } => {
                    if self.model.locate(slot.node).is_some() {
                        return Err(IntentError::Invalid(format!("node {} is there already", slot.node.0)));
                    }
                }
                Inverse::Modulator { modulator, .. } => self.refuse_a_modulator_that_is_there(modulator)?,
                Inverse::Lane { lane, .. } => {
                    let plugins = lane.instrument.iter().chain(&lane.effects);
                    for node in std::iter::once(lane.node).chain(plugins.map(|plugin| plugin.node)) {
                        if self.model.locate(node).is_some() {
                            return Err(IntentError::Invalid(format!("node {} is there already", node.0)));
                        }
                    }
                }
                Inverse::Group { group, members, .. } => {
                    for node in std::iter::once(group.node).chain(group.effects.iter().map(|effect| effect.node)) {
                        if self.model.locate(node).is_some() {
                            return Err(IntentError::Invalid(format!("node {} is there already", node.0)));
                        }
                    }
                    // Its members are lanes that are in no group (what was done after it was undone first, so that they are where they were).
                    for member in members {
                        match self.model.locate(*member) {
                            Some(NodePath::Instrument(inst)) if self.model.instruments[inst].group.is_none() => {}
                            Some(NodePath::Instrument(_)) => return Err(IntentError::Invalid(format!("node {} is in a group already", member.0))),
                            _ => return Err(IntentError::Invalid(format!("node {} is not there to be put in the group", member.0))),
                        }
                    }
                }
                Inverse::Instrument { lane, .. } => {
                    if !matches!(self.model.locate(*lane), Some(NodePath::Instrument(_))) {
                        return Err(IntentError::Invalid(format!("node {} is not there to have its instrument put back", lane.0)));
                    }
                }
                Inverse::Preset { node, .. } => {
                    if self.lane_plugin_of(*node).is_none() {
                        return Err(IntentError::Invalid(format!("node {} is not there to have its preset put back", node.0)));
                    }
                }
                // (A module comes back as it was, whether it is gone or has a member less now: nothing of it is in the way.)
                Inverse::Module { .. } | Inverse::NoWayBack => {}
            }
        }
        // The plugins first: what can fail fails before anything is changed. (What was recovered stays in hand if a later one cannot be had, and goes with the
        // step, which is dropped.)
        let deadline = Instant::now() + self.recovery_wait;
        for inverse in &mut journal.inverses {
            match inverse {
                Inverse::Effect { slot, kept, .. } => {
                    let plugin = self.recover_plugin(slot, std::mem::replace(kept, Kept::Nothing), deadline, |plugin, slot| outputs_fit_the_chain(&slot.name, plugin.audio_output_count()))?;
                    *kept = Kept::InHand(plugin);
                }
                Inverse::Instrument { before: Some(Replaced { slot, kept, .. }), .. } => {
                    let plugin = self.recover_plugin(slot, std::mem::replace(kept, Kept::Nothing), deadline, |plugin, _| has_audio_output(plugin))?;
                    *kept = Kept::InHand(plugin);
                }
                Inverse::Lane { lane, kept, .. } => {
                    let recovered = self.recover_lane(lane, std::mem::replace(kept, Kept::Nothing), deadline)?;
                    *kept = Kept::InHand(recovered);
                }
                Inverse::Group { index, group, kept, .. } => {
                    let recovered = self.recover_group(*index, group, std::mem::replace(kept, Kept::Nothing), deadline)?;
                    *kept = Kept::InHand(recovered);
                }
                _ => {}
            }
        }
        for inverse in journal.inverses.into_iter().rev() {
            self.put_back(inverse);
        }
        // (Nothing was taken out by this.)
        self.taken_last = Journal::default();
        Ok(self.conclude(Done::Restructured))
    }

    /// Where the plugin of `node` is, if the node is a lane's instrument or one of its effects and the plugin is there (a lane that has no instrument is a node, and a bus effect has
    /// no presets to load).
    fn lane_plugin_of(&self, node: NodeId) -> Option<PluginAt> {
        let at = self.locate_plugin(node).ok()?;
        match at {
            PluginAt::Lane { inst, slot: 0 } => self.model.instruments.get(inst)?.instrument.is_some().then_some(at),
            PluginAt::Lane { .. } => Some(at),
            PluginAt::Bus { .. } => None,
        }
    }

    /// Refused if the modulator's node, or the id of one of its cables, is in use.
    fn refuse_a_modulator_that_is_there(&self, modulator: &ModulatorSlot) -> Result<(), IntentError> {
        if self.model.locate(modulator.node).is_some() {
            return Err(IntentError::Invalid(format!("node {} is there already", modulator.node.0)));
        }
        for cable in &modulator.targets {
            self.refuse_a_cable_that_is_there(cable)?;
        }
        Ok(())
    }

    fn refuse_a_cable_that_is_there(&self, cable: &ModTargetSlot) -> Result<(), IntentError> {
        if self.model.edge_in_use(cable.edge) {
            Err(IntentError::Invalid(format!("cable {} is there already", cable.edge.0)))
        } else {
            Ok(())
        }
    }

    /// Put one thing back: in the model, and on the audio thread.
    fn put_back(&mut self, inverse: Inverse) {
        match inverse {
            Inverse::Cable(RemovedCable { at, mod_index, target_index, slot, base_value, .. }) => {
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let Some(place) = self.model.insert_target_at(at, mod_index, target_index, slot) else { return };
                // What it drove is where it was when the cable went (the history is undone in the reverse order), unless the session was changed without the history.
                if self.model.kind_of_target(at, mod_index, place).is_none() {
                    log::warn!("a cable was not put back: what it drove is not there");
                    if let Some(rack) = self.model.modulators_mut(at) {
                        rack[mod_index].targets.remove(place);
                    }
                    return;
                }
                let target = cable_for_the_audio_thread(&self.model.modulators(at).expect("the rack it went into")[mod_index].targets[place], base_value);
                let _ = self.cmd_tx.send(GraphCommand::InsertModTarget { location: at, mod_index, target_index: place, target });
            }
            Inverse::AudioCable(cable) => {
                // (Its ends are back, if the nodes it was at were taken out with it: they were put back before it, as it was taken out before them. A cable that the wiring
                // cannot take now, because the session was changed without the history, is not put back.)
                if let Some(problem) = self.model.cable_problem(cable.from, cable.to) {
                    log::warn!("cable {} was not put back: {problem}", cable.id.0);
                    return;
                }
                self.model.insert_audio_cable(cable);
                let _ = self.cmd_tx.send(GraphCommand::AddCable { id: cable.id, from: cable.from, to: cable.to, gain: cable.gain });
            }
            Inverse::EventCable(cable) => {
                // (The lane it ended at is back, as it was put back before it, and so is the source it started at. One of them that is not there now, because the session was changed
                // without the history, leaves the cable out.)
                if self.model.source(cable.from).is_none() || !matches!(self.model.locate(cable.to), Some(NodePath::Instrument(_))) {
                    log::warn!("cable {} was not put back: what it was between is not there", cable.id.0);
                    return;
                }
                self.model.insert_event_cable(cable);
            }
            Inverse::Source { index, source } => {
                self.model.insert_source_at(index, source);
            }
            Inverse::Module { index, module } => {
                self.model.insert_module_at(index, module);
            }
            Inverse::Modulator { at, index, modulator, bases } => {
                debug_assert_eq!(modulator.targets.len(), bases.len(), "a base for each cable");
                let source = mod_source_slot_to_graph(&modulator.source);
                let (output, position, node, ear) = (modulator.output, modulator.position, modulator.node, modulator.ear);
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let Some(place) = self.model.insert_modulator_at(at, index, modulator) else { return };
                // Its cables are told as they are, with it in: all of its own are there, so that a cable to the depth of another of them has it.
                let mut cables: Vec<ModTarget> = Vec::new();
                let mut lost: Vec<usize> = Vec::new();
                for (target_index, base) in bases.into_iter().enumerate() {
                    let slot = &self.model.modulators(at).expect("the rack it went into")[place].targets[target_index];
                    if self.model.kind_of(at, place, slot.aim).is_some() {
                        cables.push(cable_for_the_audio_thread(slot, base));
                    } else {
                        lost.push(target_index);
                    }
                }
                // (A cable that drives nothing that is there now, because the session was changed without the history, is not put back: not in the model either.)
                for target_index in lost.into_iter().rev() {
                    log::warn!("a cable was not put back: what it drove is not there");
                    if let Some(rack) = self.model.modulators_mut(at) {
                        rack[place].targets.remove(target_index);
                    }
                }
                let _ = self.cmd_tx.send(GraphCommand::InsertModulator { location: at, index: place, source, node });
                // It publishes what it puts out (and where it is) where it did, and its own cables come after it, in the order they had.
                for command in modulator_slot_commands(at, place, output, position, ear) {
                    let _ = self.cmd_tx.send(command);
                }
                for target in cables {
                    let _ = self.cmd_tx.send(GraphCommand::AddModTarget { location: at, mod_index: place, target });
                }
            }
            Inverse::Effect { at, index, slot, kept: Kept::InHand(effect) } => {
                let (mix, node, cut) = (f64::from(slot.mix), slot.node, !slot.fed);
                // (The place it went to, which is the last for one past the end: the audio thread is told the same. A chain that is not there takes nothing, and
                // the plugin is dropped here.)
                match at {
                    ChainAt::Lane(inst) => {
                        let Some(place) = self.model.insert_effect_at(inst, index, slot) else { return };
                        let _ = self.cmd_tx.send(GraphCommand::InsertEffect { inst, index: place, effect, mix, node });
                    }
                    ChainAt::Bus(group) => {
                        let Some(place) = self.model.insert_bus_effect_at(group, index, slot) else { return };
                        let _ = self.cmd_tx.send(GraphCommand::InsertGroupEffect { group, index: place, effect, mix, node });
                    }
                }
                // (The audio thread forgot the cut when the effect went: the effect comes back with the feed the model kept for it.)
                if cut {
                    let _ = self.cmd_tx.send(GraphCommand::SetEffectFeed { node, fed: false });
                }
            }
            Inverse::Lane { index, lane, kept: Kept::InHand(kept) } => {
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let place = self.model.insert_lane_at(index, lane);
                let _ = self.cmd_tx.send(GraphCommand::InsertInstrument { index: place, lane: kept });
                // (Its pattern player has the tempo that was there when it went: the lanes are all told the one there is now.)
                let _ = self.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: self.model.bpm });
            }
            Inverse::Group { index, group, members, kept: Kept::InHand(kept) } => {
                // (The members are lanes, where they were: what was done after the removal was undone first.)
                let places: Vec<usize> = members
                    .iter()
                    .filter_map(|node| match self.model.locate(*node) {
                        Some(NodePath::Instrument(inst)) => Some(inst),
                        _ => None,
                    })
                    .collect();
                // (The place it went to, which is the last for one past the end: the audio thread is told the same.)
                let place = self.model.insert_group_at(index, group, &places);
                let _ = self.cmd_tx.send(GraphCommand::InsertGroup { index: place, group: kept, members: places });
            }
            Inverse::Instrument { lane, before } => {
                let Some(NodePath::Instrument(inst)) = self.model.locate(lane) else { return };
                match before {
                    Some(Replaced { slot, remap, pitch_bend_range, kept: Kept::InHand(instrument) }) => {
                        // (The lane's note remapper is made again from what the model has, as the loader makes it.)
                        let remapper = if remap.is_empty() {
                            None
                        } else {
                            NoteRemapper::from_config(&remap, pitch_bend_range).map_err(|e| log::warn!("The note remap of a lane did not come back: {e}")).ok()
                        };
                        let inst_buf = (0..instrument.audio_output_count()).map(|_| Vec::new()).collect();
                        let node = &mut self.model.instruments[inst];
                        node.instrument = Some(slot);
                        node.remap = remap;
                        node.pitch_bend_range = pitch_bend_range;
                        let _ = self.cmd_tx.send(GraphCommand::SwapInstrument { inst, instrument, inst_buf, remapper, keep: None });
                    }
                    // (A lane that had none is empty again: the audio thread hands the instrument that was put in to the return channel.)
                    None => {
                        let node = &mut self.model.instruments[inst];
                        node.instrument = None;
                        node.forget_remap();
                        let _ = self.cmd_tx.send(GraphCommand::ClearInstrument { inst, keep: None });
                    }
                    Some(_) => {}
                }
            }
            Inverse::Preset { node, preset, rows } => {
                let Some(at) = self.lane_plugin_of(node) else { return };
                let PluginAt::Lane { inst, slot } = at else { return };
                let Some(plugin) = self.plugin_at(&at) else { return };
                // The model has the preset it had and the rows as they were; the preset is loaded again on the audio thread if the plugin still has one by that name (a name that a
                // session file gave and the plugin has not is kept as the name), and then each row is given its value, whatever the preset made of it.
                let preset_id = preset.as_ref().and_then(|name| plugin.presets.iter().find(|candidate| candidate.name == *name)).map(|found| found.id.clone());
                plugin.current_preset = preset;
                for before in &rows {
                    if let Some(row) = plugin.params.iter_mut().find(|row| row.index == before.index) {
                        row.value = before.value;
                        row.default = before.default;
                    }
                }
                if let Some(preset_id) = preset_id {
                    let _ = self.cmd_tx.send(GraphCommand::LoadPreset { inst, slot, preset_id });
                }
                for before in rows {
                    let _ = self.cmd_tx.send(GraphCommand::SetParameter { inst, slot, param_index: before.index, value: before.value });
                }
            }
            // (`rewind` has every plugin, every lane and every group in hand by now.)
            Inverse::Effect { .. } | Inverse::Lane { .. } | Inverse::Group { .. } | Inverse::NoWayBack => {}
        }
    }
}
