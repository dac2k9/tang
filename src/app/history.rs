//! The history of what a person did, and the way back (`docs/web-ui.md` §5, D35).
//!
//! Every change that can be undone leaves a `Step`: the intents that put it back and the intents that do it again, in the
//! words a button has for it. The controller keeps the steps in a `History`, and `Undo` and `Redo` are intents like the others:
//! they apply what a step says, so that the audio thread, the model, the TUI and every browser take them as any other change.
//!
//! A change that goes on (a knob dragged turns it at every frame) is one step, not a hundred: a change of what the last step
//! was of, soon enough after it, is merged into it, and the step goes back to before the first.
//!
//! What is added is taken out by undo and put back by redo **under the ids it had** (`Made`, `NodeIds::reuse`): every step that
//! names the node or the cable goes on naming it, whatever is undone and redone around it.
//!
//! What is taken out is put back by undo from what a `Journal` says was taken, exactly as it was and under its ids (`app::journal`); redo takes it out
//! again, and what that takes is what the next undo puts back.

use std::time::{Duration, Instant};

use crate::app::controller::Controller;
use crate::app::graph::{NoteRange, Port, SourceInfo};
use crate::app::intent::{Applied, Gain, Intent, IntentError, ModulatorField, ModulatorKind};
use crate::app::journal::{Journal, lane_words};
use crate::app::model::{EdgeId, Made, ModSourceSlot, NodeId, NodePath, PluginSlot};
use crate::app::module;
use crate::piano_filter::PianoMode;
use crate::plugin::chain::ModulatorLocation;
use crate::session::{Feed, Route};

/// How long after one change of a thing the next change of it may come and still be the same step.
pub const SAME_STEP: Duration = Duration::from_millis(1000);

/// How many steps are kept. The oldest go.
pub const MAX_STEPS: usize = 200;

/// How many plugins that were taken out the history keeps to put back (a plugin can hold a good deal of memory): the steps that are newer keep theirs, and
/// what an older step takes out is loaded again when it is put back.
pub const MAX_KEPT_PLUGINS: usize = 8;

/// What a change is of, for a change that goes on to be told from the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A parameter of a plugin: its node and its own index.
    Param(NodeId, u32),
    /// A gain of a node.
    Gain(NodeId, Gain),
    /// A setting of a modulator.
    Field(NodeId, ModulatorField),
    /// The steps of a step sequencer, all of them: a stroke across the bars is one change of one thing.
    Steps(NodeId),
    /// The depth of a cable.
    Depth(EdgeId),
    /// The level of an audio cable.
    Level(EdgeId),
    /// The tempo of the session.
    Tempo,
}

/// What a step that took something out holds, to put it back.
#[derive(Debug)]
enum Taken {
    /// The step took nothing out: `undo` is its way back.
    No,
    /// What it took out, which undo puts back (`undo` is empty).
    Held(Journal),
    /// It was undone, and what it took was put back: redo takes it out again, and holds what that takes.
    Spent,
}

/// One thing that was done, and the way back.
#[derive(Debug)]
pub struct Step {
    /// What it was, for a button: "set cutoff".
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by `History::state`, which tells the page what its buttons say"))]
    pub label: String,
    /// The intents that put it back, in the order they are applied.
    pub undo: Vec<Intent>,
    /// The intents that do it again, in the order they are applied.
    pub redo: Vec<Intent>,
    /// What it was of, if it is a change that goes on.
    key: Option<Key>,
    /// When it was last changed.
    last: Instant,
    /// The ids that `undo` makes, in the order it makes them (none, for most).
    undo_made: Made,
    /// The ids that `redo` makes, in the order it makes them: what was added is added again under the ids it had.
    redo_made: Made,
    /// What it took out, if it is a removal.
    taken: Taken,
}

impl Step {
    /// A step of one change: `undo` puts back what it found, `redo` is the change.
    pub fn new(label: impl Into<String>, undo: Intent, redo: Intent, key: Option<Key>, at: Instant) -> Step {
        Step { label: label.into(), undo: vec![undo], redo: vec![redo], key, last: at, undo_made: Made::default(), redo_made: Made::default(), taken: Taken::No }
    }

    /// A step of an add: `redo` is the intent, which made `made` the first time, and `undo` takes it out (the intents that do, in the order they are applied).
    fn added(label: impl Into<String>, undo: Vec<Intent>, redo: Intent, made: Made, at: Instant) -> Step {
        Step { label: label.into(), undo, redo: vec![redo], key: None, last: at, undo_made: Made::default(), redo_made: made, taken: Taken::No }
    }

    /// A step of a removal: `redo` is the intent that took `taken` out, and undo puts it back.
    fn removed(label: impl Into<String>, redo: Intent, taken: Journal, at: Instant) -> Step {
        Step { label: label.into(), undo: Vec::new(), redo: vec![redo], key: None, last: at, undo_made: Made::default(), redo_made: Made::default(), taken: Taken::Held(taken) }
    }

    /// Whether it took something out (which is put back by what was taken, and not by intents).
    fn is_removal(&self) -> bool {
        !matches!(self.taken, Taken::No)
    }

    /// A copy of a step that took nothing out, for a test that needs two of a kind (as nothing the program does makes).
    #[cfg(test)]
    fn copy(&self) -> Step {
        assert!(!self.is_removal(), "what a removal holds is not copied");
        Step {
            label: self.label.clone(),
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            key: self.key,
            last: self.last,
            undo_made: self.undo_made.clone(),
            redo_made: self.redo_made.clone(),
            taken: Taken::No,
        }
    }
}

/// What the buttons need to say: what Undo and Redo would do, or nothing if they would not.
#[cfg(any(test, feature = "web"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryState {
    pub undo: Option<String>,
    pub redo: Option<String>,
}

/// The steps that can be undone, the last on top, and the steps that were undone and can be done again.
#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

impl History {
    /// Something was done. What could have been redone cannot be any more. If it is another change of what the last step
    /// was of, soon after it, the two are one step: it puts back what the first found and does what the last did.
    pub fn record(&mut self, step: Step) {
        self.redo.clear();
        if let (Some(top), Some(key)) = (self.undo.last_mut(), step.key) {
            if top.key == Some(key) && step.last.saturating_duration_since(top.last) <= SAME_STEP {
                top.redo = step.redo;
                top.last = step.last;
                return;
            }
        }
        self.undo.push(step);
        if self.undo.len() > MAX_STEPS {
            self.undo.remove(0);
        }
        self.keep_the_newest_plugins();
    }

    /// Of the plugins that the steps to undo keep, the newest `MAX_KEPT_PLUGINS` stay, and the rest are let go of (a step that cannot keep all of its plugins
    /// keeps none).
    fn keep_the_newest_plugins(&mut self) {
        let mut kept = 0;
        for step in self.undo.iter_mut().rev() {
            if let Taken::Held(journal) = &mut step.taken {
                let plugins = journal.plugins_kept();
                if kept + plugins > MAX_KEPT_PLUGINS {
                    journal.let_go_of_plugins();
                } else {
                    kept += plugins;
                }
            }
        }
    }

    /// The step to undo, taken off (it is gone if it cannot be put back; `kept_undone` keeps it to be redone if it was).
    pub fn take_undo(&mut self) -> Option<Step> {
        self.undo.pop()
    }

    /// The step that was undone, to be redone.
    pub fn kept_undone(&mut self, step: Step) {
        self.redo.push(step);
    }

    /// The step to redo, taken off.
    pub fn take_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    /// The step that was redone, to be undone again: as it was, and not merged into the one before it.
    pub fn kept_redone(&mut self, step: Step) {
        self.undo.push(step);
        self.keep_the_newest_plugins();
    }

    /// Forget everything: something was done that cannot be undone, and what came before cannot be undone past it. True if there
    /// was something to forget.
    pub fn clear(&mut self) -> bool {
        let had = !self.undo.is_empty() || !self.redo.is_empty();
        self.undo.clear();
        self.redo.clear();
        had
    }

    /// What Undo and Redo would do.
    #[cfg(any(test, feature = "web"))]
    pub fn state(&self) -> HistoryState {
        HistoryState { undo: self.undo.last().map(|step| step.label.clone()), redo: self.redo.last().map(|step| step.label.clone()) }
    }

    #[cfg(test)]
    fn undoable(&self) -> usize {
        self.undo.len()
    }

    #[cfg(test)]
    fn redoable(&self) -> usize {
        self.redo.len()
    }
}

/// What a change is to the history.
pub(super) enum Plan {
    /// It can be undone: this is the step.
    Step(Step),
    /// It adds something, and is undone by taking that out: what it made is known once it is carried out, and the step is made then.
    Added { label: String, intent: Intent, at: Instant },
    /// It takes something out, and is undone by putting that back: what it took is known once it is carried out, and the step is made then.
    Removed { label: String, intent: Intent, at: Instant },
    /// It cannot be undone yet, and what came before cannot be undone past it.
    Barrier,
    /// It is no change of the session.
    Nothing,
}

impl Controller {
    /// What Undo and Redo would do.
    #[cfg(any(test, feature = "web"))]
    pub fn history_state(&self) -> HistoryState {
        self.history.state()
    }

    /// Put back what the last step that can be undone did. Refused if there is none, or if what it names is not there any more
    /// (a node taken out since: the step is dropped, and the one before it is the next to be undone).
    pub fn undo(&mut self) -> Result<Applied, IntentError> {
        let Some(mut step) = self.history.take_undo() else {
            return Err(IntentError::Invalid("there is nothing to undo".to_string()));
        };
        let applied = if step.is_removal() {
            let Taken::Held(journal) = std::mem::replace(&mut step.taken, Taken::Spent) else {
                return Err(IntentError::Invalid("what was taken out is not there to be put back".to_string()));
            };
            self.rewind(journal)?
        } else {
            let carried = self.carry_out_all(&step.undo, &step.undo_made)?;
            // (What an add that is undone took out is not put back by anything: redo loads the plugin again. It is not held on to.)
            self.taken_last = Journal::default();
            carried
        };
        self.history.kept_undone(step);
        Ok(applied)
    }

    /// Do again what the last step that was undone did.
    pub fn redo(&mut self) -> Result<Applied, IntentError> {
        let Some(mut step) = self.history.take_redo() else {
            return Err(IntentError::Invalid("there is nothing to redo".to_string()));
        };
        let applied = self.carry_out_all(&step.redo, &step.redo_made)?;
        if step.is_removal() {
            // What it took out this time is what the next undo puts back.
            let taken = std::mem::take(&mut self.taken_last);
            if !taken.can_be_put_back() {
                // (It took out what nothing puts back: the same session as the first time cannot do that, but a session that changed without the history can.)
                self.history.clear();
                return Ok(applied);
            }
            step.taken = Taken::Held(taken);
        }
        self.history.kept_redone(step);
        Ok(applied)
    }

    /// Carry out `intents` one after the other, each a revision of its own, and say what all of them did. What they make is made under
    /// the ids in `made`, in the order they were handed out the first time: what comes back has the ids it had.
    fn carry_out_all(&mut self, intents: &[Intent], made: &Made) -> Result<Applied, IntentError> {
        self.put_ids_back(made)?;
        let carried = self.carry_out_each(intents);
        // (What the intents did not make is not kept for what is made next.)
        self.model.ids.drop_ids_to_put_back();
        carried
    }

    /// The ids in `made` are the ones to come next. Refused if one of them is in use: a node or a cable cannot have two.
    fn put_ids_back(&mut self, made: &Made) -> Result<(), IntentError> {
        if let Some(node) = made.nodes.iter().find(|node| self.model.locate(**node).is_some()) {
            return Err(IntentError::Invalid(format!("node {} is there already", node.0)));
        }
        if let Some(edge) = made.edges.iter().find(|edge| self.model.edge_in_use(**edge)) {
            return Err(IntentError::Invalid(format!("cable {} is there already", edge.0)));
        }
        self.model.ids.reuse(&made.nodes, &made.edges);
        Ok(())
    }

    fn carry_out_each(&mut self, intents: &[Intent]) -> Result<Applied, IntentError> {
        let mut all: Option<Applied> = None;
        for intent in intents {
            let applied = self.carry_out(intent.clone())?;
            all = Some(match all {
                None => applied,
                Some(mut so_far) => {
                    so_far.rev = applied.rev;
                    so_far.events.extend(applied.events);
                    so_far.restructured |= applied.restructured;
                    so_far
                }
            });
        }
        all.ok_or_else(|| IntentError::Invalid("the step has nothing to do".to_string()))
    }

    /// What a lane or a group is called in the words of a step of a mute or a solo, and whether it is muted and whether it is soloed: none for a node that is
    /// neither.
    fn lane_or_group_flags(&self, node: NodeId) -> Option<(String, bool, bool)> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => {
                let lane = &self.model.instruments[inst];
                Some((lane_words(lane), lane.muted, lane.solo))
            }
            NodePath::Group(group) => {
                let bus = &self.model.groups[group];
                Some((bus.display_name(group), bus.muted, bus.solo))
            }
            _ => None,
        }
    }

    /// What a lane or a group is called in the words of a step of its way out, and which way out it has: none for a node that is neither.
    /// The name of the effect `node` (of a lane's chain or of a group's bus) and whether the link into it from the stage before it is there, as `SetFeed` says it. None for a node that is no effect.
    fn effect_feed(&self, node: NodeId) -> Option<(String, Feed)> {
        let slot = match self.model.locate(node)? {
            NodePath::Effect { inst, index } => &self.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &self.model.groups[group].effects[index],
            _ => return None,
        };
        Some((slot.name.clone(), if slot.fed { Feed::Chain } else { Feed::None }))
    }

    /// What a modulator of the session's own rack is called in the words of a step of its ear ("an LFO") and whom it hears now (a lane or a group that is there: one that went is every note, which is
    /// what Undo is to put back and what the graph says): none for a node that is no such modulator.
    fn free_modulator_ear(&self, node: NodeId) -> Option<(&'static str, Option<NodeId>)> {
        match self.model.locate(node)? {
            NodePath::Modulator { at: ModulatorLocation::Free, index } => {
                let modulator = &self.model.free_modulators[index];
                Some((ModulatorKind::of(&modulator.source).label(), self.model.ear_of(modulator)))
            }
            _ => None,
        }
    }

    /// The name that the macro `node` has (nothing for none: it is called `Macro`) and whether its knob is on the box of a module that it is in, as `RenameMacro` and `ExposeMacro` say them: none for a node that is no macro.
    fn macro_properties(&self, node: NodeId) -> Option<(&str, bool)> {
        match self.model.locate(node)? {
            NodePath::Modulator { at: ModulatorLocation::Free, index } => match &self.model.free_modulators[index].source {
                ModSourceSlot::Macro { name, expose, .. } => Some((name, *expose)),
                _ => None,
            },
            _ => None,
        }
    }

    /// What a lane or a group is called in the words of a step of what a modulator hears ("the lane of Oscillator", "Pad"): none for a node that is neither.
    fn lane_or_group_words(&self, node: NodeId) -> Option<String> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => Some(lane_words(&self.model.instruments[inst])),
            NodePath::Group(group) => Some(self.model.groups[group].display_name(group)),
            _ => None,
        }
    }

    fn lane_or_group_route(&self, node: NodeId) -> Option<(String, Route)> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => {
                let lane = &self.model.instruments[inst];
                Some((lane_words(lane), lane.route))
            }
            NodePath::Group(group) => {
                let bus = &self.model.groups[group];
                Some((bus.display_name(group), bus.route))
            }
            _ => None,
        }
    }

    /// What `intent` is to the history, read from the session before it is applied: the step that puts back what it will change
    /// (so it is the value it will overwrite that is read), or that it cannot be undone.
    pub(super) fn plan_history(&self, intent: &Intent) -> Plan {
        let now = (self.clock)();
        let step = |label: String, undo: Intent, key: Option<Key>| Plan::Step(Step::new(label, undo, intent.clone(), key, now));
        let added = |label: String| Plan::Added { label, intent: intent.clone(), at: now };
        match intent {
            // (Done before `plan_history` is asked, by `apply`: they are no steps of the history.)
            Intent::Save | Intent::SaveAs { .. } | Intent::SaveModule { .. } | Intent::SaveSelectionAsModule { .. } | Intent::Undo | Intent::Redo => Plan::Nothing,
            // A pattern is not put back by undo (what a recording took and what a clear took are not kept): these are no steps, and take nothing from the ones there are.
            Intent::SetRecording { .. } | Intent::ClearPattern { .. } | Intent::SetPatternField { .. } => Plan::Nothing,
            Intent::SetParam { node, param, .. } => match self.param_row(*node, *param) {
                Some((name, old)) => step(format!("set {name}"), Intent::SetParam { node: *node, param: *param, value: old }, Some(Key::Param(*node, *param))),
                None => Plan::Nothing,
            },
            Intent::SetGain { node, gain, .. } => match self.gain_value(*node, *gain) {
                Some(old) => step(gain.label().to_string(), Intent::SetGain { node: *node, gain: *gain, value: old }, Some(Key::Gain(*node, *gain))),
                None => Plan::Nothing,
            },
            // (The limiter on the output mix: what is as it is asked to be changes nothing, and no step is kept for it.)
            Intent::SetLimiter { on } if self.model.master.limiter != *on => {
                step(format!("turn the limiter {}", if *on { "on" } else { "off" }), Intent::SetLimiter { on: !*on }, None)
            }
            Intent::SetLimiter { .. } => Plan::Nothing,
            Intent::SetModulatorField { node, field, .. } => match self.field_value(*node, *field) {
                Some(old) => step(format!("set {}", field.label()), Intent::SetModulatorField { node: *node, field: *field, value: old }, Some(Key::Field(*node, *field))),
                None => Plan::Nothing,
            },
            Intent::SetSteps { node, .. } => match self.model.locate(*node).and_then(|path| match path {
                NodePath::Modulator { at, index } => self.model.modulators(at)?.get(index)?.source.steps(),
                _ => None,
            }) {
                Some(old) => step("set steps".to_string(), Intent::SetSteps { node: *node, values: old.to_vec() }, Some(Key::Steps(*node))),
                None => Plan::Nothing,
            },
            Intent::SetEdgeDepth { edge, .. } => match self.edge_depth(*edge) {
                Some(old) => step("set the depth of a cable".to_string(), Intent::SetEdgeDepth { edge: *edge, depth: old }, Some(Key::Depth(*edge))),
                None => Plan::Nothing,
            },
            Intent::SetCableGain { edge, .. } => match self.model.audio_cable(*edge) {
                Some(cable) => step("set the level of an audio cable".to_string(), Intent::SetCableGain { edge: *edge, gain: cable.gain }, Some(Key::Level(*edge))),
                None => Plan::Nothing,
            },
            Intent::SetRange { node, .. } => match self.model.locate(*node) {
                Some(NodePath::Instrument(inst)) => {
                    let old = self.model.instruments[inst].range.map(|(low, high)| NoteRange { low, high });
                    step("set the keys".to_string(), Intent::SetRange { node: *node, range: old }, None)
                }
                _ => Plan::Nothing,
            },
            // (A node that is neither a lane nor a group is refused, and what is as it is asked to be changes nothing: no step is kept for either.)
            Intent::SetMute { node, muted } => match self.lane_or_group_flags(*node) {
                Some((name, was, _)) if was != *muted => {
                    step(format!("{} {name}", if *muted { "mute" } else { "unmute" }), Intent::SetMute { node: *node, muted: was }, None)
                }
                _ => Plan::Nothing,
            },
            Intent::SetSolo { node, solo } => match self.lane_or_group_flags(*node) {
                Some((name, _, was)) if was != *solo => {
                    step(format!("{} {name}", if *solo { "solo" } else { "unsolo" }), Intent::SetSolo { node: *node, solo: was }, None)
                }
                _ => Plan::Nothing,
            },
            // (As a way out is: a node that is no modulator of the session's own rack is refused, and what is as it is asked to be changes nothing: no step is kept for either. A node that is
            // neither a lane nor a group is refused too, and is not a step.)
            Intent::SetModulatorEar { node, ear } => match self.free_modulator_ear(*node) {
                Some((kind, was)) if was != *ear && ear.is_none_or(|heard| self.lane_or_group_words(heard).is_some()) => {
                    let label = match ear.and_then(|heard| self.lane_or_group_words(heard)) {
                        Some(heard) => format!("make {kind} hear {heard}"),
                        None => format!("make {kind} hear every note"),
                    };
                    step(label, Intent::SetModulatorEar { node: *node, ear: was }, None)
                }
                _ => Plan::Nothing,
            },
            // (As a mute is: a node that is neither a lane nor a group is refused, and what is as it is asked to be changes nothing: no step is kept for either.)
            Intent::SetRoute { node, route } => match self.lane_or_group_route(*node) {
                Some((name, was)) if was != *route => {
                    let label = if *route == Route::None { format!("cut the way out of {name}") } else { format!("give {name} its way out") };
                    step(label, Intent::SetRoute { node: *node, route: was }, None)
                }
                _ => Plan::Nothing,
            },
            // (As a way out is: what is not an effect is refused, and what is as it is asked to be changes nothing: no step is kept for either.)
            Intent::SetFeed { node, feed } => match self.effect_feed(*node) {
                Some((name, was)) if was != *feed => {
                    let label = if *feed == Feed::None { format!("cut the input of {name}") } else { format!("give {name} its input back") };
                    step(label, Intent::SetFeed { node: *node, feed: was }, None)
                }
                _ => Plan::Nothing,
            },
            Intent::SetNodeUi { node, .. } => {
                let old = self.model.ui.get(node).copied();
                step("move a node".to_string(), Intent::SetNodeUi { node: *node, ui: old }, None)
            }
            Intent::SetPinned { node, .. } => match self.plugin_slot(*node) {
                Some(plugin) => step("choose the knobs".to_string(), Intent::SetPinned { node: *node, params: plugin.pinned.clone() }, None),
                None => Plan::Nothing,
            },
            Intent::RenameGroup { node, .. } => match self.model.locate(*node) {
                Some(NodePath::Group(group)) => {
                    // (None is the name a group has by default, which a name of nothing gives back.)
                    let old = self.model.groups[group].name.clone().unwrap_or_default();
                    step("rename a group".to_string(), Intent::RenameGroup { node: *node, name: old }, None)
                }
                _ => Plan::Nothing,
            },
            // (As a group's name is: a node that is no macro, and a name that is not one, are refused, and a name that the macro has changes nothing: no step is kept for any of them.)
            Intent::RenameMacro { node, name } => match self.macro_properties(*node) {
                Some((was, _)) if super::apply::macro_name(name).is_ok_and(|new| new != was) => {
                    step("rename a macro".to_string(), Intent::RenameMacro { node: *node, name: was.to_string() }, None)
                }
                _ => Plan::Nothing,
            },
            // (As a mute is: a node that is no macro is refused, and what is as it is asked to be changes nothing: no step is kept for either.)
            Intent::ExposeMacro { node, expose } => match self.macro_properties(*node) {
                Some((_, was)) if was != *expose => {
                    let label = if *expose { "show a macro on its box" } else { "hide a macro from its box" };
                    step(label.to_string(), Intent::ExposeMacro { node: *node, expose: was }, None)
                }
                _ => Plan::Nothing,
            },
            Intent::SetScale { .. } => step("set the scale".to_string(), Intent::SetScale { scale: self.piano_filter.scale().short() }, None),
            Intent::SetLocked { locked } => {
                let label = if *locked { "lock the piano to its scale" } else { "let every note of the piano through" };
                step(label.to_string(), Intent::SetLocked { locked: self.piano_filter.mode() == PianoMode::Locked }, None)
            }
            Intent::SetBpm { .. } => step("set the tempo".to_string(), Intent::SetBpm { bpm: self.model.bpm }, Some(Key::Tempo)),
            Intent::MoveEffect { node, .. } => match self.model.locate(*node) {
                Some(NodePath::Effect { index, .. } | NodePath::GroupEffect { index, .. }) => {
                    step("move an effect".to_string(), Intent::MoveEffect { node: *node, index: index as u32 }, None)
                }
                _ => Plan::Nothing,
            },
            // (The catalog names the plugin, or the intent is refused.)
            Intent::AddEffect { plugin, .. } => self.catalog_name(plugin).map_or(Plan::Nothing, |name| added(format!("add {name}"))),
            // (As an effect that goes in a chain: the words of the button that adds one. Undo takes the lane that holds it out with it.)
            Intent::AddFreeEffect { plugin } => self.catalog_name(plugin).map_or(Plan::Nothing, |name| added(format!("add {name}"))),
            Intent::AddInstrument { plugin } => self.catalog_name(plugin).map_or(Plan::Nothing, |name| added(format!("add {name} as an instrument"))),
            Intent::AddFreeModulator { kind, .. } => added(format!("add {}", kind.label())),
            Intent::AddModEdge { .. } => added("draw a cable".to_string()),
            Intent::AddCable { from, to, .. } => {
                let of_notes = from.port == Port::EventsOut || to.port == Port::EventsIn;
                added(if of_notes { "draw a cable of notes" } else { "draw an audio cable" }.to_string())
            }
            // (A device whose name is not one is refused, and the plan is not looked at: no step is kept for it. The name is called as the controller calls it, without the spaces round it.)
            Intent::AddSource { source } => match source {
                SourceInfo::Keys => added("add Keys".to_string()),
                SourceInfo::Device { name } => added(format!("add {}", name.trim())),
            },
            // A modulator and its cable: the words of the TUI's `m` and `l` (a target that cannot be named is refused with the rest, and no step is kept for it).
            Intent::Modulate { to, kind, .. } => {
                let name = match to.port {
                    Port::Param { index } => self.param_row(to.node, index).map(|(name, _)| name),
                    Port::Rate => Some("rate".to_string()),
                    Port::Attack => Some("attack".to_string()),
                    Port::Decay => Some("decay".to_string()),
                    Port::Sustain => Some("sustain".to_string()),
                    Port::Release => Some("release".to_string()),
                    Port::Depth { .. } => Some("depth".to_string()),
                    Port::Volume => Some("volume".to_string()),
                    Port::Mix => Some("mix".to_string()),
                    Port::Pan => Some("pan".to_string()),
                    Port::ModOut | Port::AudioIn | Port::AudioInSide | Port::AudioOut | Port::EventsIn | Port::EventsOut => None,
                };
                let verb = if matches!(kind, ModulatorKind::MidiLearn) { "learn" } else { "modulate" };
                name.map_or(Plan::Nothing, |name| added(format!("{verb} {name}")))
            }
            Intent::AddGroup { members } => {
                // (A member that is no lane is refused with the rest, so it need not be looked at.)
                let mut lanes = members.iter().filter_map(|member| self.lane_place(*member));
                // A lane that leaves the group it is in goes back to none when the new group is taken out, and not to that one: a step does not bring it back.
                if lanes.any(|inst| self.model.instruments[inst].group.is_some()) { Plan::Barrier } else { added("add a group".to_string()) }
            }
            Intent::SetLaneGroup { node, group } => {
                // (A node that is no lane, and a group that is no group, are refused: no step is kept for them.)
                let Some(inst) = self.lane_place(*node) else { return Plan::Nothing };
                let old = self.model.instruments[inst].group.map(|index| self.model.groups[index].node);
                if old == *group {
                    Plan::Nothing
                } else {
                    let label = if group.is_some() { "put a lane in a group" } else { "take a lane out of its group" };
                    step(label.to_string(), Intent::SetLaneGroup { node: *node, group: old }, None)
                }
            }
            // (A cable that is not there is refused, and the plan is not looked at.)
            Intent::RemoveEdge { edge } => {
                let label = if self.model.audio_cable(*edge).is_some() {
                    "remove an audio cable"
                } else if self.model.event_cable(*edge).is_some() {
                    "remove a cable of notes"
                } else {
                    "remove a cable"
                };
                Plan::Removed { label: label.to_string(), intent: intent.clone(), at: now }
            }
            Intent::RemoveNode { node } => match self.model.locate(*node) {
                // A modulator goes with its own cables and those that address it, and comes back with them (a step of the words of the button that adds one).
                Some(NodePath::Modulator { at, index }) => {
                    let kind = self.model.modulators(at).and_then(|rack| rack.get(index)).map(|modulator| ModulatorKind::of(&modulator.source));
                    kind.map_or(Plan::Nothing, |kind| Plan::Removed { label: format!("remove {}", kind.label()), intent: intent.clone(), at: now })
                }
                // An effect goes with the cables that drive it, and comes back with them: the very plugin if the audio thread handed it back (a step of the
                // words of the button that adds one: its name).
                Some(NodePath::Effect { .. } | NodePath::GroupEffect { .. }) => {
                    self.plugin_slot(*node).map_or(Plan::Nothing, |plugin| Plan::Removed { label: format!("remove {}", plugin.name), intent: intent.clone(), at: now })
                }
                // A lane goes with its modulators and their cables, its plugins and its pattern, after the cables of its group to it, and comes back with them, at its place among
                // the lanes and in its group: the whole lane that the audio thread hands back (a step of the words of the button that takes it out).
                Some(NodePath::Instrument(inst)) => {
                    Plan::Removed { label: format!("remove {}", crate::app::journal::lane_words(&self.model.instruments[inst])), intent: intent.clone(), at: now }
                }
                // A group goes with its bus effects, its modulators and their cables, and its members are in none; it comes back with them: the whole group that the audio thread hands
                // back, at its place among the groups, its members put in it again and its cables after it (a step of the words of the button that takes it out: its name).
                Some(NodePath::Group(group)) => {
                    Plan::Removed { label: format!("remove {}", self.model.groups[group].display_name(group)), intent: intent.clone(), at: now }
                }
                // A source of notes goes with the cables of notes that start at it, and comes back with them, at its place among the sources (a step of the words of the button that adds one).
                Some(NodePath::Source(index)) => {
                    Plan::Removed { label: format!("remove {}", self.model.sources[index].kind.display_name()), intent: intent.clone(), at: now }
                }
                // A module goes with its modulators and its lanes, whole, and comes back with them as one (step 108; a step of the words of the button that adds one: its name).
                Some(NodePath::Module(index)) => Plan::Removed { label: format!("remove {}", self.model.modules[index].name), intent: intent.clone(), at: now },
                // (A node that is not there is refused, and the plan is not looked at.)
                None => Plan::Nothing,
            },
            // A module that is collapsed is an add, in the words of what was done by the name the box has: the record is the one node it makes, and undo takes the record away and leaves what was in it (step 109).
            Intent::CollapseIntoModule { name, .. } => {
                added(format!("collapse into {}", super::apply::module_title(name).unwrap_or_else(|_| crate::session::DEFAULT_MODULE_NAME.to_string())))
            }
            // The box is taken away and what is in it stays: the record alone goes, and comes back as it was (`Inverse::Module`), in the words of what was done by the box's name (a node that is no module is refused).
            Intent::UngroupModule { node } => match self.model.locate(*node) {
                Some(NodePath::Module(index)) => Plan::Removed { label: format!("ungroup {}", self.model.modules[index].name), intent: intent.clone(), at: now },
                _ => Plan::Nothing,
            },
            // The instrument of a lane goes with the cables that drove it, and comes back with them: the very plugin if the audio thread handed it back, and the lane's note remap (a
            // step of the words of the button that puts one in, by the name the catalog has for the new one; an empty lane is filled, and emptied again).
            Intent::ReplaceInstrument { node, plugin } => match (self.model.locate(*node), self.catalog_name(plugin)) {
                (Some(NodePath::Instrument(inst)), Some(name)) => {
                    let label = if self.model.instruments[inst].instrument.is_some() { format!("change the instrument to {name}") } else { format!("put {name} in a lane") };
                    Plan::Removed { label, intent: intent.clone(), at: now }
                }
                // (A node that is no lane, or a plugin that the catalog does not name, is refused, and the plan is not looked at.)
                _ => Plan::Nothing,
            },
            // The instrument of a lane that is taken out goes with the cables that drove it, and comes back with them: the very plugin if the audio thread handed it back, and the
            // lane's note remap (a step of the words "take Oscillator out of its lane"; the lane stays, with the rest of what it has). A node that is no lane, and a lane that has
            // no instrument, are refused, and the plan is not looked at.
            Intent::ClearInstrument { node } => match self.model.locate(*node) {
                Some(NodePath::Instrument(inst)) => self.model.instruments[inst]
                    .instrument
                    .as_ref()
                    .map_or(Plan::Nothing, |plugin| Plan::Removed { label: format!("take {} out of its lane", plugin.name), intent: intent.clone(), at: now }),
                _ => Plan::Nothing,
            },
            // A preset that is loaded overwrites the values of the plugin and the preset it had: undo has the preset loaded again and gives the values back (a step in the words of the
            // preset, by the name the plugin has for the id). A node that has no such plugin, a bus effect (which has no presets to load) and an id the plugin lacks are refused.
            Intent::LoadPreset { node, preset } => match self.model.locate(*node) {
                Some(NodePath::Instrument(_) | NodePath::Effect { .. }) => self
                    .plugin_slot(*node)
                    .and_then(|plugin| plugin.presets.iter().find(|candidate| candidate.id == *preset))
                    .map_or(Plan::Nothing, |found| Plan::Removed { label: format!("load preset {}", found.name), intent: intent.clone(), at: now }),
                _ => Plan::Nothing,
            },
        }
    }

    /// An intent has been carried out: its step is kept, or the history is forgotten.
    pub(super) fn keep_in_history(&mut self, plan: Plan) {
        // (What the intent took out is a step's to keep, or nothing's: it is not left here.)
        let taken = std::mem::take(&mut self.taken_last);
        match plan {
            Plan::Step(step) => self.history.record(step),
            Plan::Added { label, intent, at } => {
                let made = self.made_last.clone();
                match self.takes_out(&intent, &made) {
                    Some(undo) => self.history.record(Step::added(label, undo, intent, made, at)),
                    // (It made something that cannot be named, or what it made is not there: nothing puts it back.)
                    None => {
                        self.history.clear();
                    }
                }
            }
            Plan::Removed { label, intent, at } => {
                if taken.can_be_put_back() {
                    self.history.record(Step::removed(label, intent, taken, at));
                } else {
                    // (Something went that nothing puts back, or nothing went that can be named.)
                    self.history.clear();
                }
            }
            Plan::Barrier => {
                self.history.clear();
            }
            Plan::Nothing => {}
        }
    }

    /// The intents that take out what `added`, an add that is carried out, made: the one node, or the one cable (a free effect: its lane), or the lanes and the modulators of a module, the last that was made first.
    fn takes_out(&self, added: &Intent, made: &Made) -> Option<Vec<Intent>> {
        // (A module that was collapsed made its record and nothing else: undo takes the record away, and the lanes and the modulators in it stay.)
        if let Intent::CollapseIntoModule { .. } = added {
            return match made.nodes.as_slice() {
                [node] if matches!(self.model.locate(*node), Some(NodePath::Module(_))) => Some(vec![Intent::UngroupModule { node: *node }]),
                _ => None,
            };
        }
        // (A module made a lane for each of its own, and the plugins in them, and the cables between the plugins, and then its modulators, which have their cables: each modulator and each lane is taken out, with all that is in
        // it and the cables at it, the modulators first, which were made last.)
        if let Intent::AddFreeEffect { plugin } = added {
            if module::name_of(plugin).is_some() {
                let nodes: Vec<NodeId> = made.nodes.iter().copied().filter(|node| matches!(self.model.locate(*node), Some(NodePath::Instrument(_) | NodePath::Modulator { .. }))).collect();
                return (!nodes.is_empty()).then(|| nodes.into_iter().rev().map(|node| Intent::RemoveNode { node }).collect());
            }
        }
        self.takes_out_one(added, made).map(|intent| vec![intent])
    }

    /// The intent that takes out what `added`, an add that is carried out, made, when it made one thing (a node, a cable, or a lane with its effect).
    fn takes_out_one(&self, added: &Intent, made: &Made) -> Option<Intent> {
        match (added, made.nodes.as_slice(), made.edges.as_slice()) {
            (Intent::AddModEdge { .. }, [], [edge]) if self.model.locate_edge(*edge).is_some() => Some(Intent::RemoveEdge { edge: *edge }),
            (Intent::AddModEdge { .. }, ..) => None,
            (Intent::AddCable { .. }, [], [edge]) if self.model.audio_cable(*edge).is_some() || self.model.event_cable(*edge).is_some() => Some(Intent::RemoveEdge { edge: *edge }),
            (Intent::AddCable { .. }, ..) => None,
            // (The modulator is taken out with its cable, which is the one thing that was made besides it.)
            (Intent::Modulate { .. }, [node], [_]) if self.model.locate(*node).is_some() => Some(Intent::RemoveNode { node: *node }),
            (Intent::Modulate { .. }, ..) => None,
            // (The lane is taken out with the effect in it: the two nodes that were made, the lane's first, and no cable.)
            (Intent::AddFreeEffect { plugin }, [lane, _], []) if module::name_of(plugin).is_none() && self.lane_place(*lane).is_some() => Some(Intent::RemoveNode { node: *lane }),
            (Intent::AddFreeEffect { .. }, ..) => None,
            (_, [node], []) if self.model.locate(*node).is_some() => Some(Intent::RemoveNode { node: *node }),
            _ => None,
        }
    }

    /// The name the catalog has for the plugin `source`.
    fn catalog_name(&self, source: &str) -> Option<String> {
        // (A module of the library is named as a plugin is: `add Space`.)
        self.catalog.plugins().iter().chain(self.catalog.modules()).find(|plugin| plugin.id == source).map(|plugin| plugin.name.clone())
    }

    /// The place of the lane whose node is `node`.
    fn lane_place(&self, node: NodeId) -> Option<usize> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => Some(inst),
            _ => None,
        }
    }

    /// The name and the value of parameter `param` of the plugin in `node`.
    fn param_row(&self, node: NodeId, param: u32) -> Option<(String, f32)> {
        let row = self.plugin_slot(node)?.params.iter().find(|row| row.index == param)?;
        Some((row.name.clone(), row.value))
    }

    /// The plugin in `node`: a lane's instrument, an effect of a lane's chain or of a group's bus.
    fn plugin_slot(&self, node: NodeId) -> Option<&PluginSlot> {
        match self.model.locate(node)? {
            NodePath::Instrument(inst) => self.model.instruments[inst].instrument.as_ref(),
            NodePath::Effect { inst, index } => self.model.instruments[inst].effects.get(index),
            NodePath::GroupEffect { group, index } => self.model.groups[group].effects.get(index),
            NodePath::Group(_) | NodePath::Modulator { .. } | NodePath::Source(_) | NodePath::Module(_) => None,
        }
    }

    /// The host-side gain `gain` of `node`, if it has it.
    fn gain_value(&self, node: NodeId, gain: Gain) -> Option<f32> {
        // (The output mix is a node of the graph and not of the model.)
        if node == NodeId::MASTER {
            return match gain {
                Gain::MasterVolume => Some(self.model.master.volume),
                Gain::LimiterCeiling => Some(self.model.master.ceiling),
                _ => None,
            };
        }
        match (gain, self.model.locate(node)?) {
            (Gain::Volume, NodePath::Instrument(inst)) => Some(self.model.instruments[inst].volume),
            (Gain::Mix, NodePath::Effect { inst, index }) => self.model.instruments[inst].effects.get(index).map(|effect| effect.mix),
            (Gain::Mix, NodePath::GroupEffect { group, index }) => self.model.groups[group].effects.get(index).map(|effect| effect.mix),
            (Gain::GroupVolume, NodePath::Group(group)) => Some(self.model.groups[group].volume),
            (Gain::GroupPan, NodePath::Group(group)) => Some(self.model.groups[group].pan),
            _ => None,
        }
    }

    /// The value of a setting of the modulator in `node`, if its kind has it.
    fn field_value(&self, node: NodeId, field: ModulatorField) -> Option<f32> {
        let NodePath::Modulator { at, index } = self.model.locate(node)? else { return None };
        self.model.modulators(at)?.get(index)?.source.setting(field)
    }

    /// The depth of the cable `edge`.
    fn edge_depth(&self, edge: EdgeId) -> Option<f32> {
        let (at, index, target) = self.model.locate_edge(edge)?;
        Some(self.model.modulators(at)?.get(index)?.targets.get(target)?.depth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The modulators of the session's own that hear the lane `lane`: what the lane's rack was before step 96.
    #[allow(dead_code)]
    fn hearing_lane(model: &SessionModel, lane: usize) -> Vec<&crate::app::model::ModulatorSlot> {
        let node = model.instruments[lane].node;
        model.free_modulators.iter().filter(|m| m.ear == Some(node)).collect()
    }

    /// The modulators of the session's own that hear the group `group`: what the group's rack was before step 96.
    #[allow(dead_code)]
    fn hearing_group(model: &SessionModel, group: usize) -> Vec<&crate::app::model::ModulatorSlot> {
        let node = model.groups[group].node;
        model.free_modulators.iter().filter(|m| m.ear == Some(node)).collect()
    }

    use std::sync::{Arc, Mutex};

    use crate::app::controller::Controller;
    use crate::app::graph::{Endpoint, NoteRange, Port, SourceInfo};
    use crate::app::intent::{Applied, Event, IntentError, ModulatorKind, PatternField};
    use crate::app::model::{Driven, EdgeId, Made, MasterNode, ModSourceSlot, NodePath, SessionModel, SourceKind, SourceNode};
    use crate::app::testutil::{self, SENDS, Sends, StubEngine, described, load, wire};
    use crate::session::{Feed, Route};
    use crate::plugin::chain::{CableFrom, CableTo};
    use crate::session::NodeUi;

    const T0: fn() -> Instant = Instant::now;

    /// A step that sets the lock to `redo` and puts it back to `undo`: what a step is made of does not matter to the history.
    fn step(label: &str, undo: bool, redo: bool, key: Option<Key>, at: Instant) -> Step {
        Step::new(label, Intent::SetLocked { locked: undo }, Intent::SetLocked { locked: redo }, key, at)
    }

    fn knob(n: u32) -> Option<Key> {
        Some(Key::Param(NodeId(1), n))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_new_history_has_nothing_to_undo_or_redo() {
        let mut history = History::default();
        assert_eq!(history.state(), HistoryState { undo: None, redo: None });
        assert!(history.take_undo().is_none());
        assert!(history.take_redo().is_none());
    }

    #[test]
    fn a_step_that_is_recorded_is_what_undo_would_undo_and_the_last_of_them_comes_first() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        history.record(step("set mix", true, false, None, start + ms(5000)));
        assert_eq!(history.state(), HistoryState { undo: Some("set mix".to_string()), redo: None });
        let last = history.take_undo().unwrap();
        assert_eq!((last.label.as_str(), last.undo, last.redo), ("set mix", vec![Intent::SetLocked { locked: true }], vec![Intent::SetLocked { locked: false }]));
        assert_eq!(history.state().undo.as_deref(), Some("set cutoff"));
    }

    #[test]
    fn a_step_that_was_undone_can_be_redone_and_is_then_to_be_undone_again() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert_eq!(history.state(), HistoryState { undo: None, redo: Some("set cutoff".to_string()) });
        let redone = history.take_redo().unwrap();
        history.kept_redone(redone);
        assert_eq!(history.state(), HistoryState { undo: Some("set cutoff".to_string()), redo: None });
    }

    #[test]
    fn a_step_that_is_recorded_after_an_undo_ends_what_could_be_redone() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        history.record(step("set mix", false, true, None, start + ms(5000)));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert_eq!(history.state().redo.as_deref(), Some("set mix"));
        history.record(step("set volume", false, true, None, start + ms(9000)));
        assert_eq!(history.state(), HistoryState { undo: Some("set volume".to_string()), redo: None });
    }

    #[test]
    fn changes_of_one_thing_that_follow_each_other_closely_are_one_step_that_goes_back_to_before_the_first() {
        let start = T0();
        let mut history = History::default();
        // A knob dragged: a change at every frame, in the order they came.
        history.record(Step::new("set cutoff", Intent::SetLocked { locked: false }, Intent::SetLocked { locked: true }, knob(3), start));
        history.record(Step::new("set cutoff", Intent::SetLocked { locked: true }, Intent::SetLocked { locked: false }, knob(3), start + ms(16)));
        history.record(Step::new("set cutoff", Intent::SetLocked { locked: false }, Intent::SetLocked { locked: true }, knob(3), start + ms(32)));
        let only = history.take_undo().unwrap();
        // It puts back what the first change found, and does again what the last did.
        assert_eq!((only.undo, only.redo), (vec![Intent::SetLocked { locked: false }], vec![Intent::SetLocked { locked: true }]));
        assert!(history.take_undo().is_none(), "one step and no more");
    }

    #[test]
    fn a_drag_that_goes_on_is_one_step_for_as_long_as_no_change_is_a_second_after_the_one_before() {
        let start = T0();
        let mut history = History::default();
        for frame in 0..10 {
            // Every 900 ms for nine seconds: each is soon enough after the one before, though the first is far from the last.
            history.record(step("set cutoff", false, true, knob(3), start + ms(900 * frame)));
        }
        assert!(history.take_undo().is_some());
        assert!(history.take_undo().is_none());
    }

    #[test]
    fn changes_of_one_thing_a_second_apart_or_more_are_steps_of_their_own() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, knob(3), start));
        // Exactly a second after the last is still the same step: the window is closed at the end it has.
        history.record(step("set cutoff", true, false, knob(3), start + ms(1000)));
        assert_eq!(history.undoable(), 1);
        history.record(step("set cutoff", false, true, knob(3), start + ms(2001)));
        assert_eq!(history.undoable(), 2);
    }

    #[test]
    fn changes_of_other_things_are_steps_of_their_own_and_so_is_a_change_after_a_step_that_is_not_its_own() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, knob(3), start));
        history.record(step("set resonance", false, true, knob(4), start + ms(10)));
        // Back to the first knob: the last step is the second's, so this is a step of its own.
        history.record(step("set cutoff", false, true, knob(3), start + ms(20)));
        assert_eq!(history.undoable(), 3);
        // A change that has no key is never merged, whatever came before it.
        history.record(step("rename a group", false, true, None, start + ms(30)));
        history.record(step("rename a group", false, true, None, start + ms(40)));
        assert_eq!(history.undoable(), 5);
    }

    #[test]
    fn a_change_after_an_undo_is_a_step_of_its_own_and_ends_the_redo() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, knob(3), start));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        history.record(step("set cutoff", false, true, knob(3), start + ms(10)));
        // The redo that could have been is gone, and the change is a step of its own (it is not merged into what was undone).
        assert_eq!((history.undoable(), history.redoable()), (1, 0));
    }

    #[test]
    fn clearing_the_history_leaves_nothing_to_undo_or_redo() {
        let start = T0();
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, start));
        history.record(step("set mix", false, true, None, start + ms(5000)));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert!(history.clear(), "there was something");
        assert_eq!(history.state(), HistoryState { undo: None, redo: None });
        assert!(!history.clear(), "and now there is not");
    }

    #[test]
    fn clearing_says_there_was_something_when_there_was_only_something_to_redo() {
        let mut history = History::default();
        history.record(step("set cutoff", false, true, None, Instant::now()));
        let undone = history.take_undo().unwrap();
        history.kept_undone(undone);
        assert_eq!((history.undoable(), history.redoable()), (0, 1));
        assert!(history.clear());
        assert_eq!(history.redoable(), 0);
    }

    #[test]
    fn the_oldest_steps_go_when_there_are_more_than_it_keeps() {
        let start = T0();
        let mut history = History::default();
        for n in 0..(MAX_STEPS as u64 + 5) {
            history.record(step(&format!("step {n}"), false, true, None, start + ms(5000 * n)));
        }
        assert_eq!(history.undoable(), MAX_STEPS);
        assert_eq!(history.state().undo.as_deref(), Some(format!("step {}", MAX_STEPS + 4).as_str()));
        // The oldest that is left is the sixth.
        let mut oldest = None;
        while let Some(step) = history.take_undo() {
            oldest = Some(step.label);
        }
        assert_eq!(oldest.as_deref(), Some("step 5"));
    }

    // -- the controller keeps the history --

    /// A group with a bus filter, another with nothing and no name, a lane in the first with a reverb and a filter, an LFO whose cable goes to its detune and an envelope, and a lane of its own.
    const SESSION: &str = r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:filter"

[[group]]

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "lfo"
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.5
[[instrument.modulator]]
type = "envelope"
attack = 0.1
decay = 0.2
sustain = 0.3
release = 0.4
[[instrument.modulator.target]]
param = "volume"
depth = 0.25

[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
type = "lfo"
rate = 3.0
"#;

    /// A clock that a test turns.
    #[derive(Clone)]
    struct Clock(Arc<Mutex<Instant>>);

    impl Clock {
        fn advance(&self, by: Duration) {
            *self.0.lock().unwrap() += by;
        }
    }

    fn controller() -> (StubEngine, Controller, Clock) {
        controller_over(load(SESSION).model)
    }

    /// A controller over `model` and a stub engine, with a clock that the test turns.
    fn controller_over(model: SessionModel) -> (StubEngine, Controller, Clock) {
        // (Room for the commands of a long test that does not take them: with the 64 that `stub_engine` has, a test that sends one more blocks in `send` for ever instead of failing.)
        let (handles, engine) = testutil::stub_engine_of(4_096);
        let mut ctl = Controller::new(model, handles, None);
        let clock = Clock(Arc::new(Mutex::new(Instant::now())));
        let shared = clock.clone();
        ctl.clock = Box::new(move || *shared.0.lock().unwrap());
        (engine, ctl, clock)
    }

    fn sent(engine: &StubEngine) -> Vec<String> {
        described(engine.cmd_rx.try_iter())
    }

    fn node(ctl: &Controller, path: NodePath) -> NodeId {
        ctl.model.nodes().find(|(_, p)| *p == path).map(|(id, _)| id).unwrap()
    }

    fn lane(ctl: &Controller) -> NodeId {
        node(ctl, NodePath::Instrument(0))
    }

    /// The index and value of a parameter of the plugin at `path`.
    fn param(ctl: &Controller, path: NodePath, name: &str) -> (u32, f32) {
        let slot = match path {
            NodePath::Instrument(i) => ctl.model.instruments[i].instrument.as_ref().unwrap(),
            NodePath::Effect { inst, index } => &ctl.model.instruments[inst].effects[index],
            NodePath::GroupEffect { group, index } => &ctl.model.groups[group].effects[index],
            _ => panic!("{path:?} has no plugin"),
        };
        let row = slot.params.iter().find(|r| r.name == name).unwrap();
        (row.index, row.value)
    }

    fn detune(ctl: &Controller) -> (u32, f32) {
        param(ctl, NodePath::Instrument(0), "detune")
    }

    fn set_detune(ctl: &mut Controller, value: f32) -> Applied {
        let (index, _) = detune(ctl);
        ctl.apply(Intent::SetParam { node: lane(ctl), param: index, value }).unwrap()
    }

    fn undo_label(ctl: &Controller) -> Option<String> {
        ctl.history_state().undo
    }

    fn redo_label(ctl: &Controller) -> Option<String> {
        ctl.history_state().redo
    }

    // -- What a modulator that is in no lane hears (SetModulatorEar, step 93) --

    /// `controller()` with the modulators that its session has (two, the lane's, which are the session's own once it is loaded) taken out, and the history empty: a test of a modulator that is made
    /// in it starts with none.
    fn controller_without_modulators() -> (StubEngine, Controller, Clock) {
        let (engine, mut ctl, clock) = controller();
        while !ctl.model.free_modulators.is_empty() {
            assert!(ctl.remove_modulator(ModulatorLocation::Free, 0));
        }
        sent(&engine);
        (engine, ctl, clock)
    }

    #[test]
    fn what_a_free_modulator_hears_is_a_step_in_words_that_say_whom_and_undo_and_redo_set_it_in_the_model_and_the_engine() {
        let (engine, mut ctl, _clock) = controller_without_modulators();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }).unwrap();
        let envelope = ctl.model.free_modulators[0].node;
        let (first, second) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Instrument(1)));
        let ear = |ctl: &Controller| ctl.model.free_modulators[0].ear;
        let set = |ctl: &mut Controller, ear| ctl.apply(Intent::SetModulatorEar { node: envelope, ear }).unwrap();
        sent(&engine);
        set(&mut ctl, Some(first));
        assert_eq!(undo_label(&ctl).as_deref(), Some("make an envelope hear the lane of Oscillator"));
        set(&mut ctl, Some(second));
        set(&mut ctl, None);
        assert_eq!(undo_label(&ctl).as_deref(), Some("make an envelope hear every note"));
        assert_eq!(
            sent(&engine),
            [format!("SetModulatorEar Free index=0 ear={}", first.0), format!("SetModulatorEar Free index=0 ear={}", second.0), "SetModulatorEar Free index=0 ear=none".to_string()]
        );
        // (What is as it is asked to be is applied and is no step.)
        set(&mut ctl, None);
        assert_eq!(ctl.history_state().undo.as_deref(), Some("make an envelope hear every note"));
        sent(&engine);
        for (back_to, text) in [(Some(second), second.0.to_string()), (Some(first), first.0.to_string()), (None, "none".to_string())] {
            ctl.undo().unwrap();
            assert_eq!(ear(&ctl), back_to);
            assert_eq!(sent(&engine), [format!("SetModulatorEar Free index=0 ear={text}")]);
        }
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an envelope"), "the three steps are undone and the modulator is the next");
        for (to, text) in [(Some(first), first.0.to_string()), (Some(second), second.0.to_string()), (None, "none".to_string())] {
            ctl.redo().unwrap();
            assert_eq!(ear(&ctl), to);
            assert_eq!(sent(&engine), [format!("SetModulatorEar Free index=0 ear={text}")]);
        }
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_free_modulator_that_hears_a_lane_is_put_back_by_undo_hearing_it_and_what_is_refused_is_no_step() {
        let (engine, mut ctl, _clock) = controller_without_modulators();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        let lfo = ctl.model.free_modulators[0].node;
        let (first, plugin) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        ctl.apply(Intent::SetModulatorEar { node: lfo, ear: Some(first) }).unwrap();
        let steps = ctl.history_state();
        // Refused: a lane has no ear, and a plugin is not heard.
        assert!(ctl.apply(Intent::SetModulatorEar { node: first, ear: Some(first) }).is_err());
        assert!(ctl.apply(Intent::SetModulatorEar { node: lfo, ear: Some(plugin) }).is_err());
        assert_eq!(ctl.history_state(), steps, "no step for a refusal");
        // Taken out and put back: the lane that it hears is where it was, and it hears it again, in the model and in the audio thread.
        ctl.apply(Intent::RemoveNode { node: lfo }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators[0].ear, Some(first));
        let told = sent(&engine);
        assert!(told.iter().any(|c| *c == format!("SetModulatorEar Free index=0 ear={}", first.0)), "{told:?}");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_that_is_made_to_hear_a_lane_is_one_step_that_undo_takes_out_and_redo_makes_again_under_its_id_hearing_the_same() {
        use crate::app::graph::{Endpoint, Port};
        let (engine, mut ctl, _clock) = controller_without_modulators();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let first = node(&ctl, NodePath::Instrument(0));
        // The "+" of a lane: one intent, one step, in the words of the button.
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: Some(first) }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an envelope"));
        let envelope = ctl.model.free_modulators[0].node;
        // The knob's "~": a modulator and its cable, one step, hearing the lane of the plugin.
        let filter = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        let (resonance, _) = ctl.model.instruments[0].effects[1].params.iter().find(|row| row.name == "resonance").map(|row| (row.index, ())).unwrap();
        ctl.apply(Intent::Modulate { to: Endpoint { node: filter, port: Port::Param { index: resonance } }, kind: ModulatorKind::Lfo, depth: None }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("modulate resonance"));
        let lfo = ctl.model.free_modulators[1].node;
        assert_eq!(ctl.model.free_modulators[1].ear, Some(first));
        let after = ctl.model.graph();
        sent(&engine);
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert!(ctl.model.free_modulators.is_empty(), "both are taken out");
        ctl.redo().unwrap();
        let told = sent(&engine);
        assert!(told.iter().any(|c| *c == format!("SetModulatorEar Free index=0 ear={}", first.0)), "the ear goes with the modulator that redo makes: {told:?}");
        ctl.redo().unwrap();
        assert_eq!(ctl.model.graph(), after, "the graph is what it was, whole");
        // (A telemetry slot is not given again, so the graph is compared by what a client reads: the nodes, the ears and the cables.)
        assert_eq!((ctl.model.free_modulators[0].node, ctl.model.free_modulators[0].ear), (envelope, Some(first)));
        assert_eq!((ctl.model.free_modulators[1].node, ctl.model.free_modulators[1].ear), (lfo, Some(first)));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_parameter_that_was_set_is_put_back_in_the_model_and_the_engine_and_set_again_by_redo() {
        let (engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        set_detune(&mut ctl, 3.0);
        sent(&engine);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (Some("set detune".to_string()), None));

        let undone = ctl.undo().unwrap();
        // It is an intent like the others: a revision, the event, the engine's command.
        assert_eq!(undone, Applied::of(2, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 0.0 }]));
        assert_eq!(detune(&ctl).1, 0.0);
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune_index} value=0.000")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (None, Some("set detune".to_string())));

        let redone = ctl.redo().unwrap();
        assert_eq!(redone, Applied::of(3, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 3.0 }]));
        assert_eq!(detune(&ctl).1, 3.0);
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune_index} value=3.000")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (Some("set detune".to_string()), None));
        assert!(ctl.dirty);
    }

    #[test]
    fn what_a_parameter_was_set_to_after_clamping_is_what_redo_sets_and_what_it_had_is_what_undo_puts_back() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 99.0);
        assert_eq!(detune(&ctl).1, 24.0);
        clock.advance(Duration::from_secs(5));
        set_detune(&mut ctl, -99.0);
        ctl.undo().unwrap();
        // Back to what it had, which was the clamped value, and not to what was asked.
        assert_eq!(detune(&ctl).1, 24.0);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(detune(&ctl).1, -24.0);
    }

    #[test]
    fn a_drag_is_one_step_that_goes_back_to_before_it() {
        let (_engine, mut ctl, clock) = controller();
        for (frame, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            set_detune(&mut ctl, value);
            clock.advance(Duration::from_millis(16 + frame as u64));
        }
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
        assert!(ctl.undo().is_err(), "that was all of it");
        ctl.redo().unwrap();
        assert_eq!(detune(&ctl).1, 4.0);
    }

    #[test]
    fn a_change_a_while_after_the_last_is_a_step_of_its_own() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_millis(1500));
        set_detune(&mut ctl, 2.0);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 1.0);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn changes_of_two_parameters_are_two_steps_in_the_order_they_were_made() {
        let (_engine, mut ctl, _clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        let before = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping").1;
        set_detune(&mut ctl, 3.0);
        ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.9 }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("set damping"));
        ctl.undo().unwrap();
        assert_eq!(param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping").1, before);
        assert_eq!(detune(&ctl).1, 3.0, "the other is as it was");
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn a_parameter_of_a_bus_effect_is_put_back_by_the_groups_command() {
        let (engine, mut ctl, _clock) = controller();
        let bus = NodePath::GroupEffect { group: 0, index: 0 };
        let (cutoff, before) = param(&ctl, bus, "cutoff");
        ctl.apply(Intent::SetParam { node: node(&ctl, bus), param: cutoff, value: 0.5 }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(param(&ctl, bus, "cutoff").1, before);
        assert_eq!(sent(&engine), [format!("SetGroupParameter group=0 fx=0 param={cutoff} value={before:.3}")]);
    }

    #[test]
    fn the_gains_are_put_back_each_by_its_own_command() {
        let (engine, mut ctl, clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let group = node(&ctl, NodePath::Group(0));
        let before = (ctl.model.instruments[0].volume, ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].volume, ctl.model.groups[0].pan);
        for (node, gain, value) in [(lane(&ctl), Gain::Volume, 2.5), (reverb, Gain::Mix, 0.125), (group, Gain::GroupVolume, 0.5), (group, Gain::GroupPan, -0.5)] {
            ctl.apply(Intent::SetGain { node, gain, value }).unwrap();
            clock.advance(Duration::from_secs(5));
        }
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set pan"));
        for _ in 0..4 {
            ctl.undo().unwrap();
        }
        assert_eq!((ctl.model.instruments[0].volume, ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].volume, ctl.model.groups[0].pan), before);
        assert_eq!(
            sent(&engine),
            [
                format!("SetGroupPan group=0 value={:.3}", before.3),
                format!("SetGroupVolume group=0 value={:.3}", before.2),
                format!("SetMix inst=0 slot=1 value={:.3}", before.1),
                format!("SetVolume inst=0 value={:.3}", before.0),
            ]
        );
        for _ in 0..4 {
            ctl.redo().unwrap();
        }
        assert_eq!((ctl.model.instruments[0].volume, ctl.model.instruments[0].effects[0].mix, ctl.model.groups[0].volume, ctl.model.groups[0].pan), (2.5, 0.125, 0.5, -0.5));
    }

    #[test]
    fn the_settings_of_a_modulator_and_the_depth_of_a_cable_are_put_back() {
        let (engine, mut ctl, clock) = controller();
        let lfo = ctl.model.free_modulators[0].node;
        let edge = ctl.model.free_modulators[0].targets[0].edge;
        for (field, value) in [(ModulatorField::Rate, 9.0), (ModulatorField::Waveform, 1.0), (ModulatorField::Waveform, 2.0)] {
            ctl.apply(Intent::SetModulatorField { node: lfo, field, value }).unwrap();
            clock.advance(Duration::from_secs(5));
        }
        ctl.apply(Intent::SetEdgeDepth { edge, depth: -0.75 }).unwrap();
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set the depth of a cable"));
        for _ in 0..4 {
            ctl.undo().unwrap();
        }
        let ModSourceSlot::Lfo { waveform, rate, .. } = ctl.model.free_modulators[0].source else { panic!("an LFO") };
        assert_eq!((format!("{waveform:?}"), rate, ctl.model.free_modulators[0].targets[0].depth), ("Sine".to_string(), 2.0, 0.5));
        assert_eq!(
            sent(&engine),
            [
                "SetFreeModTargetDepth mod=0 target=0 depth=0.500",
                "SetFreeModulatorWaveform mod=0 waveform=Triangle",
                "SetFreeModulatorWaveform mod=0 waveform=Sine",
                "SetFreeModulatorRate mod=0 rate=2.000"
            ]
        );
        for _ in 0..4 {
            ctl.redo().unwrap();
        }
        let ModSourceSlot::Lfo { waveform, rate, .. } = ctl.model.free_modulators[0].source else { panic!("an LFO") };
        assert_eq!((format!("{waveform:?}"), rate, ctl.model.free_modulators[0].targets[0].depth), ("Saw".to_string(), 9.0, -0.75));
    }

    #[test]
    fn a_setting_of_the_key_of_an_lfo_is_a_step_in_the_words_of_the_setting_and_is_put_back() {
        let (engine, mut ctl, clock) = controller();
        let lfo = ctl.model.free_modulators[0].node;
        for (field, value, words) in [(ModulatorField::Retrigger, 1.0, "set retrigger"), (ModulatorField::Fade, 2.5, "set fade")] {
            ctl.apply(Intent::SetModulatorField { node: lfo, field, value }).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(words));
            clock.advance(Duration::from_secs(5));
        }
        sent(&engine);
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        let key = |ctl: &Controller| match ctl.model.free_modulators[0].source {
            ModSourceSlot::Lfo { retrigger, fade, .. } => (retrigger, fade),
            _ => panic!("an LFO"),
        };
        assert_eq!(key(&ctl), (false, 0.0));
        assert_eq!(sent(&engine), ["SetModulatorKey Free index=0 retrigger=true fade=0.000", "SetModulatorKey Free index=0 retrigger=false fade=0.000"]);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(key(&ctl), (true, 2.5));
    }

    #[test]
    fn a_sync_is_a_step_in_the_words_of_the_setting_and_is_put_back_with_the_rate_the_lfo_keeps() {
        let (engine, mut ctl, clock) = controller();
        let lfo = ctl.model.free_modulators[0].node;
        let state = |ctl: &Controller| match ctl.model.free_modulators[0].source {
            ModSourceSlot::Lfo { sync, rate, .. } => (sync, rate),
            _ => panic!("an LFO"),
        };
        let (_, rate) = state(&ctl);
        for value in [11.0, 14.0] {
            ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Sync, value }).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some("set sync"));
            clock.advance(Duration::from_secs(5));
        }
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(state(&ctl), (11, rate));
        ctl.undo().unwrap();
        assert_eq!(state(&ctl), (0, rate), "the rate is what it was all the time");
        assert_eq!(sent(&engine), ["SetModulatorSync Free index=0 beats=0.500", "SetModulatorSync Free index=0 beats=0.000"]);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(state(&ctl), (14, rate));
    }

    #[test]
    fn an_lfo_that_follows_the_tempo_is_put_back_with_its_note_length_when_it_is_taken_out_and_undone() {
        let (engine, mut ctl, clock) = controller();
        let lfo = ctl.model.free_modulators[0].node;
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Sync, value: 11.0 }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RemoveNode { node: lfo }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        let ModSourceSlot::Lfo { sync, .. } = ctl.model.free_modulators[0].source else { panic!("an LFO") };
        assert_eq!(sync, 11, "as it was, and not free as a new one is");
        let commands = sent(&engine);
        assert_eq!(
            commands.first().map(String::as_str),
            Some("InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.5, age: inf }"),
            "{commands:?}"
        );
    }

    #[test]
    fn an_lfo_that_is_taken_out_is_put_back_with_its_key() {
        let (engine, mut ctl, clock) = controller();
        let lfo = ctl.model.free_modulators[0].node;
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Retrigger, value: 1.0 }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Fade, value: 0.5 }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RemoveNode { node: lfo }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        let ModSourceSlot::Lfo { retrigger, fade, .. } = ctl.model.free_modulators[0].source else { panic!("an LFO") };
        assert_eq!((retrigger, fade), (true, 0.5), "as it was, and not as a new one is");
        let commands = sent(&engine);
        assert_eq!(
            commands.first().map(String::as_str),
            Some("InsertFreeModulator index=0 source=Lfo { waveform: Sine, rate: 2.0, phase: 0.0, retrigger: true, fade: 0.5, sync: 0.0, age: inf }"),
            "{commands:?}"
        );
    }

    #[test]
    fn the_stages_of_an_envelope_are_each_put_back_to_what_they_were() {
        let (_engine, mut ctl, clock) = controller();
        let envelope = ctl.model.free_modulators[1].node;
        let stages = [(ModulatorField::Attack, 0.1, 1.5), (ModulatorField::Decay, 0.2, 1.6), (ModulatorField::Sustain, 0.3, 0.9), (ModulatorField::Release, 0.4, 1.7)];
        for (field, _, value) in stages {
            ctl.apply(Intent::SetModulatorField { node: envelope, field, value }).unwrap();
            clock.advance(Duration::from_secs(5));
        }
        assert_eq!(undo_label(&ctl).as_deref(), Some("set release"));
        for (field, was, _) in stages.into_iter().rev() {
            let applied = ctl.undo().unwrap();
            assert_eq!(applied.events, [Event::ModulatorFieldSet { node: envelope, field, value: was }]);
        }
    }

    #[test]
    fn two_settings_of_one_modulator_that_are_changed_in_a_moment_are_two_steps() {
        let (_engine, mut ctl, _clock) = controller();
        let lfo = ctl.model.free_modulators[0].node;
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Rate, value: 9.0 }).unwrap();
        ctl.apply(Intent::SetModulatorField { node: lfo, field: ModulatorField::Waveform, value: 3.0 }).unwrap();
        ctl.undo().unwrap();
        let ModSourceSlot::Lfo { waveform, rate, .. } = ctl.model.free_modulators[0].source else { panic!("an LFO") };
        assert_eq!((format!("{waveform:?}"), rate), ("Sine".to_string(), 9.0));
    }

    #[test]
    fn the_depth_of_a_cable_that_is_dragged_is_one_step() {
        let (_engine, mut ctl, _clock) = controller();
        let edge = ctl.model.free_modulators[0].targets[0].edge;
        for depth in [0.6, 0.7, 0.8, 0.9] {
            ctl.apply(Intent::SetEdgeDepth { edge, depth }).unwrap();
        }
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets[0].depth, 0.5);
        assert!(ctl.undo().is_err());
    }

    #[test]
    fn two_parameters_of_one_plugin_that_are_turned_one_after_the_other_are_two_steps() {
        let (_engine, mut ctl, _clock) = controller();
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        set_detune(&mut ctl, 3.0);
        ctl.apply(Intent::SetParam { node: lane(&ctl), param: volume, value: 0.5 }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((detune(&ctl).1, param(&ctl, NodePath::Instrument(0), "volume").1), (3.0, 1.0));
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn the_volume_and_the_pan_of_a_group_are_two_steps_for_all_they_are_changed_in_a_moment() {
        let (_engine, mut ctl, _clock) = controller();
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::SetGain { node: group, gain: Gain::GroupVolume, value: 0.5 }).unwrap();
        ctl.apply(Intent::SetGain { node: group, gain: Gain::GroupPan, value: 0.25 }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.groups[0].volume, ctl.model.groups[0].pan), (0.5, 0.0));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.groups[0].volume, 1.0);
    }

    #[test]
    fn a_scale_is_put_back_to_the_one_it_was_and_the_lock_has_its_words_for_both_ways() {
        let (_engine, mut ctl, clock) = controller();
        ctl.apply(Intent::SetScale { scale: "F# dorian".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetScale { scale: "A minor".to_string() }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.piano_filter.scale().short(), "F# dorian");
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLocked { locked: false }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("let every note of the piano through"));
    }

    #[test]
    fn a_tempo_is_put_back_to_the_one_it_was_and_a_drag_of_it_is_one_step() {
        let (engine, mut ctl, clock) = controller();
        ctl.apply(Intent::SetBpm { bpm: 90.0 }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetBpm { bpm: 100.0 }).unwrap();
        clock.advance(Duration::from_millis(100));
        ctl.apply(Intent::SetBpm { bpm: 110.0 }).unwrap();
        clock.advance(Duration::from_millis(100));
        ctl.apply(Intent::SetBpm { bpm: 140.0 }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("set the tempo"));
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.bpm, 90.0, "the drag was one step, which put back what it began at");
        assert_eq!(sent(&engine), ["SetGlobalBpm bpm=90.0"]);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.bpm, 120.0);
        assert!(ctl.undo().is_err(), "nothing more to undo");
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(ctl.model.bpm, 140.0);
    }

    #[test]
    fn a_step_that_changes_the_structure_says_so_when_it_is_undone_among_others() {
        let (_engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let mut step = Step::new("do two things", Intent::SetParam { node: lane(&ctl), param: detune_index, value: 1.0 }, Intent::SetParam { node: lane(&ctl), param: detune_index, value: 2.0 }, None, Instant::now());
        step.undo.push(Intent::MoveEffect { node: reverb, index: 1 });
        ctl.history.record(step);
        let undone = ctl.undo().unwrap();
        assert!(undone.restructured, "the second of them did");
        assert_eq!(undone.events.len(), 1, "and the first did not");
    }

    #[test]
    fn the_limiter_and_the_masters_gains_are_steps_in_the_words_of_what_they_do_and_undo_puts_them_back_in_the_model_and_the_engine() {
        let (engine, mut ctl, clock) = controller();
        let master = |engine: &StubEngine| -> Vec<String> {
            engine.cmd_rx.try_iter().filter(|command| matches!(command, crate::plugin::chain::GraphCommand::SetMaster { .. })).map(|command| testutil::describe(&command)).collect()
        };
        let step = |ctl: &mut Controller, intent: Intent| {
            clock.advance(Duration::from_secs(5));
            ctl.apply(intent).unwrap();
        };
        step(&mut ctl, Intent::SetLimiter { on: false });
        assert_eq!(undo_label(&ctl).as_deref(), Some("turn the limiter off"));
        step(&mut ctl, Intent::SetGain { node: NodeId::MASTER, gain: Gain::LimiterCeiling, value: -6.0 });
        assert_eq!(undo_label(&ctl).as_deref(), Some("set the limiter's ceiling"));
        step(&mut ctl, Intent::SetGain { node: NodeId::MASTER, gain: Gain::MasterVolume, value: 0.5 });
        assert_eq!(undo_label(&ctl).as_deref(), Some("set the master volume"));
        step(&mut ctl, Intent::SetLimiter { on: true });
        assert_eq!(undo_label(&ctl).as_deref(), Some("turn the limiter on"));
        master(&engine);

        ctl.undo().unwrap();
        assert_eq!(ctl.model.master, MasterNode { volume: 0.5, limiter: false, ceiling: -6.0 });
        assert_eq!(redo_label(&ctl).as_deref(), Some("turn the limiter on"));
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.master, MasterNode { volume: 1.0, limiter: true, ceiling: -1.0 }, "all of it is as it was");
        assert_eq!(master(&engine).last().map(String::as_str), Some("SetMaster volume=1.000 limiter=true ceiling=-1.0 rate=48000"), "and so is the audio thread");
        // Redo does it again, in order.
        for _ in 0..4 {
            ctl.redo().unwrap();
        }
        assert_eq!(ctl.model.master, MasterNode { volume: 0.5, limiter: true, ceiling: -6.0 });
        assert!(ctl.undo().is_ok() && ctl.redo().is_ok());
    }

    #[test]
    fn a_ceiling_that_is_dragged_is_one_step_and_a_limiter_that_is_as_it_is_asked_to_be_is_none() {
        let (_engine, mut ctl, _clock) = controller();
        for value in [-2.0, -3.0, -4.0, -5.0] {
            ctl.apply(Intent::SetGain { node: NodeId::MASTER, gain: Gain::LimiterCeiling, value }).unwrap();
        }
        ctl.undo().unwrap();
        assert_eq!(ctl.model.master.ceiling, -1.0, "a drag is one step");
        assert!(ctl.undo().is_err(), "and there is nothing more to undo");
        // (The limiter is in the sound already: nothing is kept for asking it to be, and a step that was not made is not put back.)
        ctl.apply(Intent::SetLimiter { on: true }).unwrap();
        assert!(ctl.undo().is_err());
        ctl.apply(Intent::SetLimiter { on: false }).unwrap();
        ctl.apply(Intent::SetLimiter { on: false }).unwrap();
        ctl.undo().unwrap();
        assert!(ctl.model.master.limiter, "one step for the two");
        assert!(ctl.undo().is_err());
    }

    #[test]
    fn a_mute_and_a_solo_are_steps_in_the_words_of_what_they_do_and_undo_puts_the_flag_back_in_the_model_and_the_engine() {
        let (engine, mut ctl, clock) = controller();
        let (instrument, group) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        let step = |ctl: &mut Controller, intent: Intent| {
            clock.advance(Duration::from_secs(5));
            ctl.apply(intent).unwrap();
        };
        step(&mut ctl, Intent::SetMute { node: instrument, muted: true });
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute the lane of Oscillator"));
        step(&mut ctl, Intent::SetSolo { node: instrument, solo: true });
        assert_eq!(undo_label(&ctl).as_deref(), Some("solo the lane of Oscillator"));
        step(&mut ctl, Intent::SetMute { node: group, muted: true });
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute Pad"));
        step(&mut ctl, Intent::SetSolo { node: group, solo: true });
        assert_eq!(undo_label(&ctl).as_deref(), Some("solo Pad"));
        step(&mut ctl, Intent::SetMute { node: instrument, muted: false });
        assert_eq!(undo_label(&ctl).as_deref(), Some("unmute the lane of Oscillator"));
        step(&mut ctl, Intent::SetSolo { node: group, solo: false });
        assert_eq!(undo_label(&ctl).as_deref(), Some("unsolo Pad"));
        sent(&engine);

        // Undo puts each back as it was, in the model and in the engine, the last first.
        ctl.undo().unwrap();
        assert!(ctl.model.groups[0].solo);
        assert_eq!(redo_label(&ctl).as_deref(), Some("unsolo Pad"));
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].muted);
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.instruments[0].muted, ctl.model.instruments[0].solo, ctl.model.groups[0].muted, ctl.model.groups[0].solo), (false, false, false, false));
        assert_eq!(
            sent(&engine),
            [
                "SetGroupSolo group=0 solo=true",
                "SetMute inst=0 muted=true",
                "SetGroupSolo group=0 solo=false",
                "SetGroupMute group=0 muted=false",
                "SetSolo inst=0 solo=false",
                "SetMute inst=0 muted=false",
            ]
        );
        ctl.redo().unwrap();
        assert!(ctl.model.instruments[0].muted);
    }

    #[test]
    fn a_lane_that_has_no_instrument_is_muted_in_the_words_of_an_empty_lane() {
        let (_engine, mut ctl, _clock) = controller();
        ctl.model.instruments[1].instrument = None;
        let empty = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::SetMute { node: empty, muted: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute an empty lane"));
        ctl.apply(Intent::SetSolo { node: empty, solo: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("solo an empty lane"));
    }

    #[test]
    fn two_toggles_of_one_lane_in_a_moment_are_two_steps_and_a_mute_that_changes_nothing_keeps_none() {
        let (_engine, mut ctl, _clock) = controller();
        let (instrument, empty) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)));
        ctl.apply(Intent::SetMute { node: empty, muted: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute the lane of Oscillator"));
        // (Both are in the same moment: a toggle is not merged with the one before it, which would leave a step that does nothing.)
        ctl.apply(Intent::SetMute { node: instrument, muted: true }).unwrap();
        ctl.apply(Intent::SetMute { node: instrument, muted: false }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("unmute the lane of Oscillator"));
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].muted, "the unmute alone");
        ctl.undo().unwrap();
        assert!(!ctl.model.instruments[0].muted, "and the mute alone");
        assert!(ctl.model.instruments[1].muted);
        // Muting what is muted is told and changes nothing, so it keeps no step.
        ctl.apply(Intent::SetMute { node: empty, muted: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute the lane of Oscillator"));
        // (Nor does a solo that is asked not to be what it is not.)
        ctl.apply(Intent::SetSolo { node: empty, solo: false }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute the lane of Oscillator"));
        ctl.undo().unwrap();
        assert!(!ctl.model.instruments[1].muted, "the step that is there is the first mute of the lane, and not a second");
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn a_mute_that_is_refused_leaves_no_step() {
        let (_engine, mut ctl, _clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        assert!(ctl.apply(Intent::SetMute { node: reverb, muted: true }).is_err());
        assert!(ctl.apply(Intent::SetSolo { node: NodeId::MASTER, solo: true }).is_err());
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn cutting_and_giving_back_a_way_out_are_steps_in_the_words_of_the_lane_or_the_group_that_undo_puts_back_in_the_model_and_the_engine() {
        let (engine, mut ctl, clock) = controller();
        let (instrument, group) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        let step = |ctl: &mut Controller, intent: Intent| {
            clock.advance(Duration::from_secs(5));
            ctl.apply(intent).unwrap();
        };
        step(&mut ctl, Intent::SetRoute { node: instrument, route: Route::None });
        assert_eq!(undo_label(&ctl).as_deref(), Some("cut the way out of the lane of Oscillator"));
        step(&mut ctl, Intent::SetRoute { node: group, route: Route::None });
        assert_eq!(undo_label(&ctl).as_deref(), Some("cut the way out of Pad"));
        step(&mut ctl, Intent::SetRoute { node: instrument, route: Route::Out });
        assert_eq!(undo_label(&ctl).as_deref(), Some("give the lane of Oscillator its way out"));
        step(&mut ctl, Intent::SetRoute { node: group, route: Route::Out });
        assert_eq!(undo_label(&ctl).as_deref(), Some("give Pad its way out"));
        sent(&engine);

        // Undo puts each back as it was, in the model and in the engine, the last first.
        ctl.undo().unwrap();
        assert_eq!(ctl.model.groups[0].route, Route::None);
        assert_eq!(redo_label(&ctl).as_deref(), Some("give Pad its way out"));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].route, Route::None);
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.instruments[0].route, ctl.model.groups[0].route), (Route::Out, Route::Out));
        assert_eq!(
            sent(&engine),
            ["SetGroupRoute group=0 route=none", "SetLaneRoute inst=0 route=none", "SetGroupRoute group=0 route=out", "SetLaneRoute inst=0 route=out"]
        );
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[0].route, Route::None, "the first step again");
        assert_eq!(sent(&engine), ["SetLaneRoute inst=0 route=none"]);
    }

    #[test]
    fn a_way_out_that_is_as_it_is_asked_to_be_or_that_is_refused_keeps_no_step_and_what_could_be_redone_is_still_to_redo() {
        let (_engine, mut ctl, _clock) = controller();
        let (instrument, reverb) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        ctl.apply(Intent::SetRoute { node: instrument, route: Route::None }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the way out of the lane of Oscillator"));
        // The way out that the lane has is not a change: it keeps no step, and does not end what could be redone.
        ctl.apply(Intent::SetRoute { node: instrument, route: Route::Out }).unwrap();
        assert_eq!(undo_label(&ctl), None);
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the way out of the lane of Oscillator"));
        // Nor does a way out of what is neither a lane nor a group.
        assert!(ctl.apply(Intent::SetRoute { node: reverb, route: Route::None }).is_err());
        assert!(ctl.apply(Intent::SetRoute { node: NodeId::MASTER, route: Route::None }).is_err());
        assert_eq!(undo_label(&ctl), None);
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the way out of the lane of Oscillator"));
    }

    #[test]
    fn a_lane_whose_way_out_is_cut_that_is_taken_out_comes_back_by_undo_with_its_way_out_cut() {
        let (engine, mut ctl, _clock) = controller();
        let instrument = lane(&ctl);
        ctl.apply(Intent::SetRoute { node: instrument, route: Route::None }).unwrap();
        ctl.apply(Intent::RemoveNode { node: instrument }).unwrap();
        // (There is no audio thread to hand the lane back: reading what was sent answers for it.)
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].route, Route::None, "the lane is handed back whole");
        assert_eq!(ctl.model.instruments[0].node, instrument);
    }

    #[test]
    fn cutting_and_giving_back_a_feed_are_steps_in_the_words_of_the_effect_that_undo_puts_back_in_the_model_and_the_engine() {
        let (engine, mut ctl, clock) = controller();
        let (reverb, filter, bus) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::Effect { inst: 0, index: 1 }), node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }));
        let step = |ctl: &mut Controller, intent: Intent| {
            clock.advance(Duration::from_secs(5));
            ctl.apply(intent).unwrap();
        };
        step(&mut ctl, Intent::SetFeed { node: reverb, feed: Feed::None });
        assert_eq!(undo_label(&ctl).as_deref(), Some("cut the input of Reverb"));
        step(&mut ctl, Intent::SetFeed { node: bus, feed: Feed::None });
        assert_eq!(undo_label(&ctl).as_deref(), Some("cut the input of Filter"));
        step(&mut ctl, Intent::SetFeed { node: reverb, feed: Feed::Chain });
        assert_eq!(undo_label(&ctl).as_deref(), Some("give Reverb its input back"));
        step(&mut ctl, Intent::SetFeed { node: filter, feed: Feed::None });
        sent(&engine);

        // Undo puts each back as it was, in the model and in the engine, the last first.
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].effects[1].fed);
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the input of Filter"));
        ctl.undo().unwrap();
        assert!(!ctl.model.instruments[0].effects[0].fed, "the first effect's feed is cut again, as it was before it was given back");
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].effects.iter().all(|fx| fx.fed) && ctl.model.groups[0].effects[0].fed);
        assert_eq!(
            sent(&engine),
            [
                format!("SetEffectFeed node={} fed=true", filter.0),
                format!("SetEffectFeed node={} fed=false", reverb.0),
                format!("SetEffectFeed node={} fed=true", bus.0),
                format!("SetEffectFeed node={} fed=true", reverb.0),
            ]
        );
        ctl.redo().unwrap();
        assert!(!ctl.model.instruments[0].effects[0].fed, "the first step again");
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=false", reverb.0)]);
    }

    #[test]
    fn a_cable_that_gives_a_link_back_is_the_step_that_gives_it_back_in_the_words_of_the_link_and_undo_cuts_it_again() {
        let (engine, mut ctl, clock) = controller();
        let (instrument, reverb, filter, group) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::Effect { inst: 0, index: 1 }), node(&ctl, NodePath::Group(0)));
        let step = |ctl: &mut Controller, intent: Intent| {
            clock.advance(Duration::from_secs(5));
            ctl.apply(intent).unwrap();
        };
        step(&mut ctl, Intent::SetRoute { node: instrument, route: Route::None });
        step(&mut ctl, Intent::SetFeed { node: reverb, feed: Feed::None });
        sent(&engine);
        // The end of lane 0's chain to its group's bus is its way out, and its instrument to its first effect is that effect's input: a cable drawn between the two ends gives the link
        // back, and is the step of that, in its words (the page draws the links as cables, and the steps of the history are those of the intents that make them).
        step(&mut ctl, Intent::AddCable { from: out(filter), to: into(group), gain: None });
        assert_eq!(undo_label(&ctl).as_deref(), Some("give the lane of Oscillator its way out"));
        step(&mut ctl, Intent::AddCable { from: out(instrument), to: into(reverb), gain: None });
        assert_eq!(undo_label(&ctl).as_deref(), Some("give Reverb its input back"));
        assert_eq!(sent(&engine), ["SetLaneRoute inst=0 route=out".to_string(), format!("SetEffectFeed node={} fed=true", reverb.0)]);
        assert!(ctl.model.cables.is_empty(), "no cable was made: they are the links");
        let linked = |ctl: &Controller| (ctl.model.instruments[0].route, ctl.model.instruments[0].effects[0].fed);
        assert_eq!(linked(&ctl), (Route::Out, true));
        // Undo cuts them again, the last first, and Redo gives them back.
        ctl.undo().unwrap();
        assert_eq!(linked(&ctl), (Route::Out, false));
        assert_eq!(redo_label(&ctl).as_deref(), Some("give Reverb its input back"));
        ctl.undo().unwrap();
        assert_eq!(linked(&ctl), (Route::None, false));
        assert_eq!(sent(&engine), [format!("SetEffectFeed node={} fed=false", reverb.0), "SetLaneRoute inst=0 route=none".to_string()]);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(linked(&ctl), (Route::Out, true));
        // A cable that is refused (the link is there) leaves no step, and what could be redone is still to redo.
        ctl.undo().unwrap();
        assert!(ctl.apply(Intent::AddCable { from: out(filter), to: into(group), gain: None }).is_err());
        assert_eq!(undo_label(&ctl).as_deref(), Some("give the lane of Oscillator its way out"));
        assert_eq!(redo_label(&ctl).as_deref(), Some("give Reverb its input back"));
    }

    #[test]
    fn a_feed_that_is_as_it_is_asked_to_be_or_that_is_refused_keeps_no_step_and_what_could_be_redone_is_still_to_redo() {
        let (_engine, mut ctl, _clock) = controller();
        let (instrument, reverb) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        ctl.apply(Intent::SetFeed { node: reverb, feed: Feed::None }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the input of Reverb"));
        // The feed that the effect has is not a change: it keeps no step, and does not end what could be redone.
        ctl.apply(Intent::SetFeed { node: reverb, feed: Feed::Chain }).unwrap();
        assert_eq!(undo_label(&ctl), None);
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the input of Reverb"));
        // Nor does a feed of what is not an effect.
        assert!(ctl.apply(Intent::SetFeed { node: instrument, feed: Feed::None }).is_err());
        assert!(ctl.apply(Intent::SetFeed { node: NodeId::MASTER, feed: Feed::None }).is_err());
        assert_eq!(undo_label(&ctl), None);
        assert_eq!(redo_label(&ctl).as_deref(), Some("cut the input of Reverb"));
    }

    #[test]
    fn an_effect_whose_feed_is_cut_that_is_taken_out_comes_back_by_undo_with_its_feed_cut_and_the_engine_is_told_so() {
        let (engine, mut ctl, _clock) = controller();
        let (reverb, bus) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }));
        ctl.apply(Intent::SetFeed { node: reverb, feed: Feed::None }).unwrap();
        ctl.apply(Intent::SetFeed { node: bus, feed: Feed::None }).unwrap();
        ctl.apply(Intent::RemoveNode { node: bus }).unwrap();
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        sent(&engine);
        // (The stub engine hands nothing back, so the plugins are loaded again: the cut goes after the plugin, by its node.)
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[0].fed), (reverb, false));
        assert_eq!((ctl.model.groups[0].effects[0].node, ctl.model.groups[0].effects[0].fed), (bus, false));
        let commands = sent(&engine);
        let at = |prefix: &str| commands.iter().position(|command| command.starts_with(prefix)).unwrap_or_else(|| panic!("{prefix} is among {commands:?}"));
        assert_eq!(commands[at("InsertGroupEffect") + 1], format!("SetEffectFeed node={} fed=false", bus.0), "{commands:?}");
        assert_eq!(commands[at("InsertEffect") + 1], format!("SetEffectFeed node={} fed=false", reverb.0), "{commands:?}");
        // An effect that was fed comes back fed, and nothing is said of it.
        ctl.apply(Intent::SetFeed { node: reverb, feed: Feed::Chain }).unwrap();
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].effects[0].fed);
        assert!(!sent(&engine).iter().any(|command| command.starts_with("SetEffectFeed")));
    }

    #[test]
    fn a_lane_that_has_an_effect_whose_feed_is_cut_that_is_taken_out_comes_back_by_undo_with_the_feed_still_cut() {
        let (engine, mut ctl, _clock) = controller();
        let (instrument, filter) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        ctl.apply(Intent::SetFeed { node: filter, feed: Feed::None }).unwrap();
        ctl.apply(Intent::RemoveNode { node: instrument }).unwrap();
        // (There is no audio thread to hand the lane back: reading what was sent answers for it. The audio thread's lane has its cut with it.)
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].effects.iter().map(|fx| fx.fed).collect::<Vec<_>>(), [true, false]);
    }

    #[test]
    fn the_keys_of_a_lane_are_put_back_and_every_note_is_no_range() {
        let (engine, mut ctl, clock) = controller();
        ctl.apply(Intent::SetRange { node: lane(&ctl), range: Some(NoteRange { low: 36, high: 60 }) }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetRange { node: lane(&ctl), range: None }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].range, Some((36, 60)));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].range, None);
        assert_eq!(sent(&engine), ["SetInstrumentRange inst=0 range=Some((36, 60))", "SetInstrumentRange inst=0 range=None"]);
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[0].range, Some((36, 60)));
    }

    #[test]
    fn the_place_of_a_node_is_put_back_and_a_node_that_had_none_has_none_again() {
        let (_engine, mut ctl, clock) = controller();
        let node = lane(&ctl);
        ctl.apply(Intent::SetNodeUi { node, ui: Some(NodeUi { x: 10.0, y: 20.0 }) }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetNodeUi { node, ui: Some(NodeUi { x: 30.0, y: 40.0 }) }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.ui.get(&node), Some(&NodeUi { x: 10.0, y: 20.0 }));
        let applied = ctl.undo().unwrap();
        assert_eq!(applied.events, [Event::NodeUiSet { node, ui: None }]);
        assert_eq!(ctl.model.ui.get(&node), None);
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(ctl.model.ui.get(&node), Some(&NodeUi { x: 30.0, y: 40.0 }));
    }

    #[test]
    fn the_places_of_nodes_that_are_put_one_after_the_other_are_a_step_each() {
        // (Arranging the nodes again is a request for each of them; that is a step each, and not a step that goes back
        // to the place of the first only.)
        let (_engine, mut ctl, _clock) = controller();
        let (a, b) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        ctl.apply(Intent::SetNodeUi { node: a, ui: Some(NodeUi { x: 1.0, y: 1.0 }) }).unwrap();
        ctl.apply(Intent::SetNodeUi { node: b, ui: Some(NodeUi { x: 2.0, y: 2.0 }) }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.ui.contains_key(&a), ctl.model.ui.contains_key(&b)), (true, false));
    }

    #[test]
    fn the_knobs_a_node_shows_are_put_back() {
        let (_engine, mut ctl, clock) = controller();
        let (detune_index, _) = detune(&ctl);
        ctl.apply(Intent::SetPinned { node: lane(&ctl), params: vec![detune_index] }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetPinned { node: lane(&ctl), params: vec![] }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().pinned, [detune_index]);
        ctl.undo().unwrap();
        assert!(ctl.model.instruments[0].instrument.as_ref().unwrap().pinned.is_empty());
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().pinned, [detune_index]);
    }

    #[test]
    fn the_name_of_a_group_is_put_back_and_a_group_that_had_none_has_its_default_name_again() {
        let (_engine, mut ctl, clock) = controller();
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RenameGroup { node: group, name: "Strings".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RenameGroup { node: group, name: String::new() }).unwrap();
        assert_eq!(ctl.model.groups[0].name, None);
        let applied = ctl.undo().unwrap();
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Strings".to_string() }]);
        let applied = ctl.undo().unwrap();
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Pad".to_string() }]);
        assert_eq!(ctl.model.groups[0].name.as_deref(), Some("Pad"));
    }

    #[test]
    fn a_group_that_had_no_name_is_given_none_again_and_not_the_name_it_was_called_by() {
        let (_engine, mut ctl, _clock) = controller();
        ctl.model.groups[0].name = None;
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RenameGroup { node: group, name: "Strings".to_string() }).unwrap();
        let applied = ctl.undo().unwrap();
        assert_eq!(ctl.model.groups[0].name, None);
        assert_eq!(applied.events, [Event::GroupRenamed { node: group, name: "Group 1".to_string() }]);
    }

    // -- The name of a macro and whether a box shows its knob (RenameMacro and ExposeMacro, step 110) --

    /// A macro in the session's own rack, with the name and the flag it has.
    fn a_macro(ctl: &mut Controller) -> NodeId {
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Macro, hears: None }).unwrap();
        ctl.model.free_modulators.last().unwrap().node
    }

    fn macro_has(ctl: &Controller, node: NodeId) -> (String, bool) {
        let Some(NodePath::Modulator { index, .. }) = ctl.model.locate(node) else { panic!("a modulator") };
        match &ctl.model.free_modulators[index].source {
            ModSourceSlot::Macro { name, expose, .. } => (name.clone(), *expose),
            other => panic!("a macro, and not {other:?}"),
        }
    }

    #[test]
    fn the_name_of_a_macro_is_put_back_and_a_macro_that_had_none_has_none_again() {
        let (_engine, mut ctl, _clock) = controller();
        let knob = a_macro(&mut ctl);
        ctl.apply(Intent::RenameMacro { node: knob, name: "Brightness".to_string() }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("rename a macro"));
        // (Two in a row are two steps: a name is not a gesture that goes on, and nothing of it is merged.)
        ctl.apply(Intent::RenameMacro { node: knob, name: "  Cutoff ".to_string() }).unwrap();
        ctl.apply(Intent::RenameMacro { node: knob, name: String::new() }).unwrap();
        assert_eq!(macro_has(&ctl, knob), (String::new(), true));
        ctl.undo().unwrap();
        assert_eq!(macro_has(&ctl, knob).0, "Cutoff");
        ctl.undo().unwrap();
        assert_eq!(macro_has(&ctl, knob).0, "Brightness");
        ctl.undo().unwrap();
        assert_eq!(macro_has(&ctl, knob).0, "", "a macro that had no name has none again, and is not given the one it was called by");
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a macro"), "the three steps were three, and the macro is the next");
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(macro_has(&ctl, knob).0, "");
        assert_eq!(redo_label(&ctl), None);
    }

    #[test]
    fn showing_a_macro_on_its_box_and_hiding_it_are_steps_with_their_own_words_that_undo_and_redo_put_back() {
        let (_engine, mut ctl, _clock) = controller();
        let knob = a_macro(&mut ctl);
        ctl.apply(Intent::ExposeMacro { node: knob, expose: false }).unwrap();
        assert_eq!((undo_label(&ctl).as_deref(), macro_has(&ctl, knob).1), (Some("hide a macro from its box"), false));
        ctl.apply(Intent::ExposeMacro { node: knob, expose: true }).unwrap();
        assert_eq!((undo_label(&ctl).as_deref(), macro_has(&ctl, knob).1), (Some("show a macro on its box"), true));
        ctl.undo().unwrap();
        assert!(!macro_has(&ctl, knob).1);
        ctl.undo().unwrap();
        assert!(macro_has(&ctl, knob).1, "on to begin with");
        ctl.redo().unwrap();
        assert!(!macro_has(&ctl, knob).1);
        // The name is not touched by the flag, nor the flag by the name.
        ctl.apply(Intent::RenameMacro { node: knob, name: "Wet".to_string() }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(macro_has(&ctl, knob), (String::new(), false));
    }

    #[test]
    fn a_macro_that_is_asked_to_be_what_it_is_and_what_is_refused_leave_no_step() {
        let (_engine, mut ctl, _clock) = controller();
        let knob = a_macro(&mut ctl);
        let lane0 = lane(&ctl);
        let steps = |ctl: &Controller| undo_label(ctl);
        let before = steps(&ctl);
        // As it is: not a step, and the one before it is the next to undo.
        ctl.apply(Intent::ExposeMacro { node: knob, expose: true }).unwrap();
        ctl.apply(Intent::RenameMacro { node: knob, name: String::new() }).unwrap();
        ctl.apply(Intent::RenameMacro { node: knob, name: "   ".to_string() }).unwrap();
        assert_eq!(steps(&ctl), before);
        ctl.apply(Intent::RenameMacro { node: knob, name: "Wet".to_string() }).unwrap();
        let before = steps(&ctl);
        ctl.apply(Intent::RenameMacro { node: knob, name: " Wet ".to_string() }).unwrap();
        assert_eq!(steps(&ctl), before, "the name it has, once the spaces are gone, is not a step either");
        // Refused: not a step (and the plan, which is made before the controller says no, is none either).
        for name in ["x".repeat(65), "tab\there".to_string()] {
            assert!(matches!(ctl.plan_history(&Intent::RenameMacro { node: knob, name: name.clone() }), Plan::Nothing), "{name:?}");
            assert!(ctl.apply(Intent::RenameMacro { node: knob, name }).is_err());
        }
        assert!(matches!(ctl.plan_history(&Intent::RenameMacro { node: lane0, name: "x".to_string() }), Plan::Nothing));
        assert!(matches!(ctl.plan_history(&Intent::ExposeMacro { node: lane0, expose: false }), Plan::Nothing));
        assert!(ctl.apply(Intent::RenameMacro { node: lane0, name: "x".to_string() }).is_err());
        assert!(ctl.apply(Intent::ExposeMacro { node: lane0, expose: false }).is_err());
        assert_eq!(steps(&ctl), before);
        ctl.undo().unwrap();
        assert_eq!(macro_has(&ctl, knob).0, "", "one undo takes the one step that there was");
    }

    #[test]
    fn a_macro_that_is_taken_out_and_put_back_has_its_name_and_its_flag() {
        let (_engine, mut ctl, _clock) = controller();
        let knob = a_macro(&mut ctl);
        ctl.apply(Intent::RenameMacro { node: knob, name: "Brightness".to_string() }).unwrap();
        ctl.apply(Intent::ExposeMacro { node: knob, expose: false }).unwrap();
        ctl.apply(Intent::RemoveNode { node: knob }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(macro_has(&ctl, knob), ("Brightness".to_string(), false));
    }

    #[test]
    fn the_scale_and_the_lock_of_the_piano_are_put_back() {
        let (_engine, mut ctl, clock) = controller();
        let before = ctl.piano_filter.scale().short();
        ctl.apply(Intent::SetScale { scale: "F# dorian".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("lock the piano to its scale"));
        ctl.undo().unwrap();
        assert_eq!(ctl.piano_filter.mode(), crate::piano_filter::PianoMode::Highlight);
        let applied = ctl.undo().unwrap();
        assert_eq!(ctl.piano_filter.scale().short(), before);
        assert!(matches!(applied.events.as_slice(), [Event::ScaleSet { .. }]));
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!((ctl.piano_filter.scale().short(), ctl.piano_filter.mode()), ("F# dorian".to_string(), crate::piano_filter::PianoMode::Locked));
    }

    #[test]
    fn an_effect_that_was_moved_is_put_back_in_its_place_and_the_structure_is_said_to_have_changed() {
        let (engine, mut ctl, _clock) = controller();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        ctl.apply(Intent::MoveEffect { node: reverb, index: 1 }).unwrap();
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("move an effect"));
        let undone = ctl.undo().unwrap();
        assert!(undone.restructured && undone.events.is_empty());
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::Effect { inst: 0, index: 0 }));
        assert_eq!(sent(&engine), ["ReorderEffect inst=0 from=1 to=0"]);
        let redone = ctl.redo().unwrap();
        assert!(redone.restructured);
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::Effect { inst: 0, index: 1 }));
    }

    #[test]
    fn a_bus_effect_that_was_moved_is_put_back_by_the_groups_command() {
        let (handles, engine) = testutil::stub_engine();
        let session = SESSION.replace("[[group.effect]]\nplugin = \"builtin:filter\"", "[[group.effect]]\nplugin = \"builtin:filter\"\n[[group.effect]]\nplugin = \"builtin:reverb\"");
        let mut ctl = Controller::new(load(&session).model, handles, None);
        let reverb = node(&ctl, NodePath::GroupEffect { group: 0, index: 1 });
        ctl.apply(Intent::MoveEffect { node: reverb, index: 0 }).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::GroupEffect { group: 0, index: 1 }));
        assert_eq!(sent(&engine), ["ReorderGroupEffect group=0 from=0 to=1"]);
    }

    // -- what cannot be undone, and what cannot be undone any more --

    #[test]
    fn the_intents_undo_and_redo_do_what_the_methods_do_and_are_no_steps_of_the_history_themselves() {
        let (engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        set_detune(&mut ctl, 3.0);
        sent(&engine);
        let undone = ctl.apply(Intent::Undo).unwrap();
        assert_eq!(undone, Applied::of(2, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 0.0 }]));
        assert_eq!(sent(&engine), [format!("SetParameter inst=0 slot=0 param={detune_index} value=0.000")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (None, Some("set detune".to_string())));
        let redone = ctl.apply(Intent::Redo).unwrap();
        assert_eq!(redone, Applied::of(3, vec![Event::ParamSet { node: lane(&ctl), param: detune_index, value: 3.0 }]));
        assert_eq!((undo_label(&ctl), redo_label(&ctl)), (Some("set detune".to_string()), None));
        // Nothing more to redo, and what is refused is no revision.
        assert_eq!(ctl.apply(Intent::Redo), Err(IntentError::Invalid("there is nothing to redo".to_string())));
        assert_eq!(ctl.rev, 3);
        // The undo of the undo is the redo: it is the same step, which went back and forth, and not a new one.
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
        assert!(ctl.apply(Intent::Undo).is_err(), "there was only the one step");
    }

    #[test]
    fn nothing_to_undo_and_nothing_to_redo_are_refused_in_words_and_cost_no_revision() {
        let (engine, mut ctl, _clock) = controller();
        assert_eq!(ctl.undo(), Err(IntentError::Invalid("there is nothing to undo".to_string())));
        assert_eq!(ctl.redo(), Err(IntentError::Invalid("there is nothing to redo".to_string())));
        assert_eq!((ctl.rev, ctl.dirty), (0, false));
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn a_change_after_an_undo_ends_what_could_be_redone() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        ctl.undo().unwrap();
        assert_eq!(redo_label(&ctl).as_deref(), Some("set detune"));
        let (reverb_damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        ctl.apply(Intent::SetParam { node: node(&ctl, NodePath::Effect { inst: 0, index: 0 }), param: reverb_damping, value: 0.1 }).unwrap();
        assert_eq!(redo_label(&ctl), None);
        assert_eq!(ctl.redo(), Err(IntentError::Invalid("there is nothing to redo".to_string())));
    }

    #[test]
    fn what_cannot_be_undone_yet_empties_the_history_so_that_undo_never_does_something_else() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        set_detune(&mut ctl, 2.0);
        ctl.undo().unwrap();
        assert_eq!((undo_label(&ctl).is_some(), redo_label(&ctl).is_some()), (true, true));
        // A group is made of a lane that is in another: nothing puts that back yet (the cables of the group that has the lane go), and what was before it cannot be undone past it.
        ctl.apply(Intent::AddGroup { members: vec![lane(&ctl)] }).unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        assert!(ctl.undo().is_err() && ctl.redo().is_err());
    }

    /// The session of `SESSION` with the built-in plugins in the catalog, so that an effect and an instrument can be added.
    fn controller_with_plugins() -> (StubEngine, Controller, Clock) {
        let (engine, mut ctl, clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        (engine, ctl, clock)
    }

    #[test]
    fn an_effect_that_was_added_is_taken_out_by_undo_and_put_back_by_redo_under_the_id_it_had() {
        let (engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        let before = ctl.model.instruments[0].effects.len();
        sent(&engine);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.instruments[0].effects.last().unwrap().node;
        assert_eq!(ctl.last_made().nodes, [added]);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("add Reverb"), None));
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=2 plugin=Reverb mix=0.500"]);

        let undone = ctl.undo().unwrap();
        assert!(undone.restructured);
        assert_eq!(ctl.model.instruments[0].effects.len(), before);
        assert_eq!(ctl.model.locate(added), None);
        assert_eq!(sent(&engine), ["RemoveEffect inst=0 index=2 keep"]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl).as_deref()), (None, Some("add Reverb")));

        let redone = ctl.redo().unwrap();
        assert!(redone.restructured);
        assert_eq!(ctl.model.instruments[0].effects.last().unwrap().node, added, "the effect that comes back has the id it had");
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=2 plugin=Reverb mix=0.500"]);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("add Reverb"), None));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_effect_on_a_groups_bus_is_added_and_taken_out_the_same_way() {
        let (engine, mut ctl, _clock) = controller_with_plugins();
        let g0 = node(&ctl, NodePath::Group(0));
        let before = ctl.model.groups[0].effects.len();
        sent(&engine);
        ctl.apply(Intent::AddEffect { parent: g0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.groups[0].effects.last().unwrap().node;
        assert_eq!(undo_label(&ctl).as_deref(), Some("add Reverb"));
        ctl.undo().unwrap();
        assert_eq!((ctl.model.groups[0].effects.len(), ctl.model.locate(added)), (before, None));
        ctl.redo().unwrap();
        assert_eq!(ctl.model.groups[0].effects.last().unwrap().node, added);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn what_was_done_to_an_effect_that_was_added_is_put_back_on_the_same_effect_whatever_came_and_went_in_between() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.instruments[0].effects.last().unwrap().node;
        ctl.apply(Intent::SetGain { node: added, gain: Gain::Mix, value: 0.25 }).unwrap();
        clock.advance(Duration::from_secs(5));
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 2 }, "damping");
        ctl.apply(Intent::SetParam { node: added, param: damping, value: 0.75 }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("set damping"));

        // All three go back, the effect with them, and all three come again, on the effect that comes back.
        for _ in 0..3 {
            ctl.undo().unwrap();
        }
        assert_eq!(ctl.model.locate(added), None);
        sent(&engine);
        for _ in 0..3 {
            ctl.redo().unwrap();
        }
        let effect = &ctl.model.instruments[0].effects[2];
        assert_eq!(effect.node, added);
        assert_eq!(effect.mix, 0.25);
        assert_eq!(effect.params.iter().find(|row| row.index == damping).unwrap().value, 0.75);
        assert_eq!(
            sent(&engine),
            [
                "InsertEffect inst=0 index=2 plugin=Reverb mix=0.500".to_string(),
                "SetMix inst=0 slot=3 value=0.250".to_string(),
                format!("SetParameter inst=0 slot=3 param={damping} value=0.750"),
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_and_a_cable_that_were_added_are_taken_out_by_undo_and_come_back_under_their_ids_by_redo() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, filter) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        let (resonance, set) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) }).unwrap();
        let lfo = ctl.last_made().nodes[0];
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an LFO"));
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: filter, port: Port::Param { index: resonance } } }).unwrap();
        let cable = ctl.last_made().edges[0];
        assert_eq!(undo_label(&ctl).as_deref(), Some("draw a cable"));

        ctl.undo().unwrap();
        assert!(ctl.model.locate_edge(cable).is_none());
        assert_eq!(param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance").1, set, "what the cable drove has the value that was set");
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an LFO"));
        ctl.undo().unwrap();
        assert!(ctl.model.locate(lfo).is_none());
        assert_eq!(undo_label(&ctl), None);

        ctl.redo().unwrap();
        assert!(ctl.model.locate(lfo).is_some(), "the modulator comes back under its id");
        ctl.redo().unwrap();
        let (at, index, target) = ctl.model.locate_edge(cable).expect("the cable comes back under its id");
        assert_eq!(ctl.model.modulators(at).unwrap()[index].node, lfo);
        assert_eq!(ctl.model.modulators(at).unwrap()[index].targets[target].edge, cable);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_envelope_and_a_midi_control_have_their_own_words() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: Some(lane0) }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("add an envelope"));
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::MidiLearn, hears: Some(lane0) }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a MIDI control"));
    }

    #[test]
    fn a_velocity_and_a_key_have_their_own_words_going_in_and_out() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        for (kind, added, removed) in [
            (ModulatorKind::Velocity, "add a velocity control", "remove a velocity control"),
            (ModulatorKind::Key, "add a key control", "remove a key control"),
        ] {
            clock.advance(Duration::from_secs(5));
            let node = ctl.apply(Intent::AddFreeModulator { kind, hears: Some(lane0) }).map(|_| ctl.model.free_modulators.last().unwrap().node).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(added));
            clock.advance(Duration::from_secs(5));
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(removed));
            ctl.undo().unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(added), "undone, the control is back and so is the step that made it");
        }
    }

    #[test]
    fn an_instrument_that_was_added_is_a_lane_that_undo_takes_out_and_redo_puts_back_under_its_id_and_what_was_done_to_it_is_done_again() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let lanes = ctl.model.instruments.len();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap();
        let added = ctl.model.instruments.last().unwrap().node;
        assert_eq!(undo_label(&ctl).as_deref(), Some("add Oscillator as an instrument"));
        ctl.apply(Intent::SetNodeUi { node: added, ui: Some(NodeUi { x: 5.0, y: 6.0 }) }).unwrap();
        sent(&engine);

        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments.len(), lanes);
        assert_eq!(ctl.model.locate(added), None);
        assert_eq!(sent(&engine), ["RemoveInstrument inst=2 keep"]);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"), "what was done before the add is still to be undone");
        ctl.redo().unwrap();
        // (The step that moved the lane finds it under the id it had, or it would be refused and dropped.)
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments.last().unwrap().node, added, "the lane comes back under its id");
        assert_eq!(ctl.model.ui.get(&added).copied(), Some(NodeUi { x: 5.0, y: 6.0 }));
        ctl.model.check_ids().unwrap();
        for _ in 0..3 {
            ctl.undo().unwrap();
        }
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn an_effect_that_stands_by_itself_is_a_lane_that_undo_takes_out_with_the_effect_in_it_and_redo_puts_back_under_the_ids_they_had() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let lanes = ctl.model.instruments.len();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddFreeEffect { plugin: "builtin:reverb".to_string() }).unwrap();
        let (lane, effect) = {
            let added = ctl.model.instruments.last().unwrap();
            (added.node, added.effects[0].node)
        };
        assert_eq!(ctl.last_made().nodes, [lane, effect], "the lane's node and then the effect's");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("add Reverb"), None));
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetNodeUi { node: effect, ui: Some(NodeUi { x: 5.0, y: 6.0 }) }).unwrap();
        sent(&engine);

        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments.len(), lanes);
        assert_eq!((ctl.model.locate(lane), ctl.model.locate(effect)), (None, None), "the lane goes, with the effect in it");
        assert_eq!(sent(&engine), [format!("RemoveInstrument inst={lanes} keep")]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl).as_deref()), (None, Some("add Reverb")));
        ctl.redo().unwrap();
        // (The step that moved the effect finds it under the id it had, or it would be refused and dropped.)
        ctl.redo().unwrap();
        let added = ctl.model.instruments.last().unwrap();
        assert_eq!((added.node, added.effects[0].node), (lane, effect), "the lane and the effect come back under the ids they had");
        assert!(added.instrument.is_none());
        assert_eq!(ctl.model.ui.get(&effect).copied(), Some(NodeUi { x: 5.0, y: 6.0 }));
        ctl.model.check_ids().unwrap();
        for _ in 0..2 {
            ctl.undo().unwrap();
        }
        assert_eq!(ctl.model.instruments.len(), lanes);
    }

    #[test]
    fn a_lane_that_holds_effects_and_no_instrument_is_called_in_the_words_of_a_step_by_what_it_holds() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        ctl.apply(Intent::AddFreeEffect { plugin: "builtin:reverb".to_string() }).unwrap();
        let lane = ctl.model.instruments.last().unwrap().node;
        // One effect: its name, since the page shows the effect and not the lane that holds it.
        for (intent, label) in [
            (Intent::SetMute { node: lane, muted: true }, "mute Reverb"),
            (Intent::SetSolo { node: lane, solo: true }, "solo Reverb"),
            // (A free effect has no way out to begin with: it is given one, and then it is cut again.)
            (Intent::SetRoute { node: lane, route: Route::Out }, "give Reverb its way out"),
            (Intent::SetRoute { node: lane, route: Route::None }, "cut the way out of Reverb"),
            (Intent::SetSolo { node: lane, solo: false }, "unsolo Reverb"),
        ] {
            clock.advance(Duration::from_secs(5));
            ctl.apply(intent).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(label));
        }
        // More than one: a chain of effects. None at all is the empty lane that it always was.
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddEffect { parent: lane, plugin: "builtin:filter".to_string() }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetMute { node: lane, muted: false }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("unmute a chain of effects"));
        let lane1 = node(&ctl, NodePath::Instrument(1));
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::ClearInstrument { node: lane1 }).unwrap();
        ctl.apply(Intent::SetMute { node: lane1, muted: true }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("mute an empty lane"));
    }

    #[test]
    fn a_free_effect_that_is_taken_out_with_its_lane_is_put_back_by_undo_whole_and_the_step_is_called_by_the_effect() {
        // (Over an audio thread that is run: it is what hands the lane back.)
        let (mut ctl, mut graph) = controller_over_an_audio_thread(SESSION);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let lanes = ctl.model.instruments.len();
        ctl.apply(Intent::AddFreeEffect { plugin: "builtin:reverb".to_string() }).unwrap();
        let (lane, effect) = {
            let added = ctl.model.instruments.last().unwrap();
            (added.node, added.effects[0].node)
        };
        ctl.apply(Intent::SetGain { node: effect, gain: Gain::Mix, value: 0.8 }).unwrap();
        ctl.apply(Intent::SetMute { node: lane, muted: true }).unwrap();
        graph.drain_commands();
        let before = everything_as_it_is(&ctl, &graph);

        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        graph.drain_commands();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Reverb"));
        assert_eq!(ctl.model.instruments.len(), lanes);

        ctl.undo().unwrap();
        graph.drain_commands();
        assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, "the free effect, put back");
        let back = ctl.model.instruments.last().unwrap();
        assert_eq!((back.node, back.effects[0].node, back.muted), (lane, effect, true), "the lane is back whole, under its id, muted as it was");
        assert!((back.effects[0].mix - 0.8).abs() < 1e-6, "with the mix it had");
        assert_eq!(redo_label(&ctl).as_deref(), Some("remove Reverb"));
        ctl.model.check_ids().unwrap();
        ctl.redo().unwrap();
        graph.drain_commands();
        assert_eq!(ctl.model.instruments.len(), lanes);
    }

    #[test]
    fn a_group_that_was_added_with_lanes_that_were_in_none_is_taken_out_by_undo_and_comes_back_with_them() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane1 = node(&ctl, NodePath::Instrument(1));
        assert_eq!(ctl.model.instruments[1].group, None);
        ctl.apply(Intent::AddGroup { members: vec![lane1] }).unwrap();
        let group = ctl.model.groups.last().unwrap().node;
        assert_eq!(ctl.model.instruments[1].group, Some(2));
        assert_eq!(undo_label(&ctl).as_deref(), Some("add a group"));
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RenameGroup { node: group, name: "Keys".to_string() }).unwrap();

        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!((ctl.model.groups.len(), ctl.model.instruments[1].group), (2, None), "the group is gone and its lane is in none");
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(ctl.model.groups.last().unwrap().node, group, "the group comes back under its id");
        assert_eq!(ctl.model.groups.last().unwrap().name.as_deref(), Some("Keys"));
        assert_eq!(ctl.model.instruments[1].group, Some(2));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_session_that_is_built_by_adds_goes_back_through_each_of_the_structures_it_had_and_forward_through_them_again() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, lane1) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)));
        let (g0, g1) = (node(&ctl, NodePath::Group(0)), node(&ctl, NodePath::Group(1)));
        let (filter, bus) = (node(&ctl, NodePath::Effect { inst: 0, index: 1 }), node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }));
        let (resonance, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        let (cutoff, _) = param(&ctl, NodePath::GroupEffect { group: 0, index: 0 }, "cutoff");
        let (detune, _) = detune(&ctl);
        let to = |node: NodeId, index: u32| Endpoint { node, port: Port::Param { index } };
        // What each add made (the node, or the cable), as it is told, so that a later add can name it.
        let made = |ctl: &mut Controller, intent: Intent| -> Made {
            ctl.apply(intent).unwrap();
            ctl.last_made().clone()
        };

        let mut structures = vec![(ctl.model.graph(), ctl.model.telemetry_layout())];
        let mut record = |ctl: &Controller| structures.push((ctl.model.graph(), ctl.model.telemetry_layout()));
        let fx = made(&mut ctl, Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddEffect { parent: g0, plugin: "builtin:filter".to_string() });
        record(&ctl);
        let lfo = made(&mut ctl, Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: lfo, to: to(filter, resonance) });
        record(&ctl);
        let envelope = made(&mut ctl, Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: Some(lane0) }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: envelope, to: Endpoint { node: lfo, port: Port::Rate } });
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: envelope, to: to(fx, 1) });
        record(&ctl);
        let group_lfo = made(&mut ctl, Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(g0) }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: group_lfo, to: to(lane0, detune) });
        record(&ctl);
        made(&mut ctl, Intent::AddModEdge { modulator: group_lfo, to: to(bus, cutoff) });
        record(&ctl);
        let lane2 = made(&mut ctl, Intent::AddInstrument { plugin: "builtin:osc".to_string() }).nodes[0];
        record(&ctl);
        made(&mut ctl, Intent::AddGroup { members: vec![lane1, lane2] });
        record(&ctl);
        made(&mut ctl, Intent::SetLaneGroup { node: lane1, group: Some(g1) });
        record(&ctl);
        made(&mut ctl, Intent::AddFreeModulator { kind: ModulatorKind::MidiLearn, hears: Some(lane2) });
        record(&ctl);
        let steps = structures.len() - 1;
        assert_eq!(steps, 14);

        // Back through each structure: the nodes, their places, their ids and the cables are what they were, and so are the entries of the layout of the telemetry.
        for step in (0..steps).rev() {
            ctl.undo().unwrap();
            assert_eq!(ctl.model.graph(), structures[step].0, "undone to the structure of {step} adds");
            assert_eq!(ctl.model.telemetry_layout().meters.len(), structures[step].1.meters.len(), "{step}");
            ctl.model.check_ids().unwrap();
        }
        assert_eq!(ctl.history_state().undo, None);
        // And forward again: the graph is what it was, whole, with every id the same (the slots of the telemetry are new ones).
        for (step, (graph, _)) in structures.iter().enumerate().skip(1) {
            ctl.redo().unwrap();
            assert_eq!(&ctl.model.graph(), graph, "done again to the structure of {step} adds");
            ctl.model.check_ids().unwrap();
        }
        assert_eq!(ctl.history_state().redo, None);
    }

    #[test]
    fn an_empty_group_is_added_and_taken_out_too() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        ctl.apply(Intent::AddGroup { members: vec![] }).unwrap();
        assert_eq!(ctl.model.groups.len(), 3);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.groups.len(), 2);
    }

    #[test]
    fn a_group_that_took_a_lane_out_of_another_group_cannot_be_taken_out_again_and_empties_the_history() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let (lane0, lane1) = (lane(&ctl), node(&ctl, NodePath::Instrument(1)));
        assert_eq!((ctl.model.instruments[0].group, ctl.model.instruments[1].group), (Some(0), None));
        // One lane that is in a group is enough, whatever the others are and wherever it is in the list.
        for members in [vec![lane0], vec![lane0, lane1], vec![lane1, lane0]] {
            set_detune(&mut ctl, 1.0);
            clock.advance(Duration::from_secs(5));
            assert!(undo_label(&ctl).is_some());
            ctl.apply(Intent::AddGroup { members: members.clone() }).unwrap();
            assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "{members:?}");
        }
    }

    #[test]
    fn a_lane_put_in_a_group_is_taken_out_by_undo_and_one_that_was_taken_out_is_put_back() {
        let (engine, mut ctl, clock) = controller_with_plugins();
        let (lane1, g1) = (node(&ctl, NodePath::Instrument(1)), node(&ctl, NodePath::Group(1)));
        sent(&engine);
        ctl.apply(Intent::SetLaneGroup { node: lane1, group: Some(g1) }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("put a lane in a group"));
        assert_eq!(ctl.model.instruments[1].group, Some(1));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, None);
        ctl.redo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, Some(1));

        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetLaneGroup { node: lane1, group: None }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("take a lane out of its group"));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, Some(1));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, None);
    }

    #[test]
    fn a_lane_that_moves_from_one_group_to_another_goes_back_to_the_first() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, g1) = (lane(&ctl), node(&ctl, NodePath::Group(1)));
        assert_eq!(ctl.model.instruments[0].group, Some(0));
        ctl.apply(Intent::SetLaneGroup { node: lane0, group: Some(g1) }).unwrap();
        assert_eq!(ctl.model.instruments[0].group, Some(1));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[0].group, Some(0));
    }

    #[test]
    fn a_lane_put_in_the_group_it_is_in_is_no_step_and_leaves_the_history_as_it_was() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lane0, g0) = (lane(&ctl), node(&ctl, NodePath::Group(0)));
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        ctl.apply(Intent::SetLaneGroup { node: lane0, group: Some(g0) }).unwrap();
        assert_eq!(ctl.history_state(), before);
        // (Nor is a lane that is in none taken out of its group.)
        let lane1 = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::SetLaneGroup { node: lane1, group: None }).unwrap();
        assert_eq!(ctl.history_state(), before);
    }

    #[test]
    fn what_is_refused_when_it_is_added_leaves_no_step_and_what_came_before_stays() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        assert!(ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:nothing".to_string() }).is_err());
        assert!(ctl.apply(Intent::AddInstrument { plugin: "builtin:reverb".to_string() }).is_err());
        assert!(ctl.apply(Intent::AddGroup { members: vec![NodeId(9_999)] }).is_err());
        assert!(ctl.apply(Intent::SetLaneGroup { node: lane0, group: Some(NodeId(9_999)) }).is_err());
        assert_eq!(ctl.history_state(), before);
        // (And a refusal leaves no id of its own to put back: the next effect is the first one that was made.)
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        assert_eq!(ctl.last_made().nodes.len(), 1);
    }

    #[test]
    fn a_step_that_adds_what_is_there_already_is_refused_and_dropped_and_nothing_has_two_ids() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        ctl.undo().unwrap();
        // Two of the same step to redo (as nothing the program does makes it): the first puts the effect back, the second finds it there.
        let step = ctl.history.take_redo().unwrap();
        ctl.history.kept_undone(step.copy());
        ctl.history.kept_undone(step);
        ctl.redo().unwrap();
        let effects = ctl.model.instruments[0].effects.len();
        let refused = ctl.redo().unwrap_err();
        assert!(refused.to_string().contains("is there already"), "{refused}");
        assert_eq!(ctl.model.instruments[0].effects.len(), effects, "it was not added twice");
        ctl.model.check_ids().unwrap();
        // The step is dropped: it is not to be redone again, and what was done before it is still to be undone.
        assert_eq!(redo_label(&ctl), None);
    }

    #[test]
    fn a_step_that_draws_a_cable_that_is_there_already_is_refused_and_no_cable_has_two_ids() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let (lfo, filter) = (ctl.model.free_modulators[0].node, node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        let (resonance, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: filter, port: Port::Param { index: resonance } } }).unwrap();
        ctl.undo().unwrap();
        let step = ctl.history.take_redo().unwrap();
        ctl.history.kept_undone(step.copy());
        ctl.history.kept_undone(step);
        ctl.redo().unwrap();
        let cables = ctl.model.free_modulators[0].targets.len();
        let refused = ctl.redo().unwrap_err();
        assert!(refused.to_string().contains("cable") && refused.to_string().contains("is there already"), "{refused}");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), cables);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_that_adds_what_the_tui_has_taken_out_is_refused_with_the_reason_and_dropped() {
        let (_engine, mut ctl, clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let added = ctl.model.instruments[0].effects.last().unwrap().node;
        // Something takes it out without the history (as a front-end that edits the model by itself could): the controller's own method forgets the history.
        let index = ctl.model.instruments[0].effects.len() - 1;
        ctl.model.remove_effect(0, index);
        let refused = ctl.undo().unwrap_err();
        assert_eq!(refused, IntentError::NotFound(added));
        assert_eq!(undo_label(&ctl).as_deref(), Some("set detune"), "the step is dropped and the one before it is next");
    }

    #[test]
    fn an_id_that_an_undone_add_had_is_not_given_to_another_node() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let first = ctl.last_made().nodes[0];
        ctl.undo().unwrap();
        // Something else is added instead of redoing: it is a new node, and what could be redone is gone.
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:filter".to_string() }).unwrap();
        let second = ctl.last_made().nodes[0];
        assert_ne!(first, second);
        assert_eq!(redo_label(&ctl), None);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn every_intent_that_cannot_be_undone_yet_empties_the_history() {
        let (_engine, mut ctl, clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let intents = [
            // A group made of a lane that is in one: the cables of the group that has it go, and a step would not bring them back.
            ("add a group of a lane that is in another", Intent::AddGroup { members: vec![lane(&ctl)] }),
        ];
        for (what, intent) in intents {
            set_detune(&mut ctl, 1.0);
            assert!(undo_label(&ctl).is_some(), "{what}: there was something to undo");
            ctl.apply(intent).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "{what}");
            clock.advance(Duration::from_secs(5));
        }
    }

    #[test]
    fn an_intent_that_is_refused_changes_nothing_in_the_history() {
        let (_engine, mut ctl, _clock) = controller();
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        assert!(ctl.apply(Intent::SetParam { node: lane(&ctl), param: 9999, value: 1.0 }).is_err());
        assert!(ctl.apply(Intent::RemoveNode { node: NodeId(424242) }).is_err());
        assert!(ctl.apply(Intent::AddGroup { members: vec![NodeId(424242)] }).is_err());
        assert_eq!(ctl.history_state(), before);
    }

    #[test]
    fn saving_is_no_change_and_leaves_the_history_as_it_was() {
        let dir = std::env::temp_dir().join(format!("tang-history-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(SESSION).model, handles, Some(dir.join("session.toml")));
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        ctl.apply(Intent::Save).unwrap();
        assert_eq!(ctl.history_state(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn putting_another_instrument_in_a_lane_empties_the_history_whoever_asks_for_it() {
        // (The TUI's `i` calls the controller's method and not the intent; the parameters of the new plugin are not the old one's.)
        let (_engine, mut ctl, _clock) = controller();
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        set_detune(&mut ctl, 1.0);
        ctl.put_instrument(0, "builtin:osc").unwrap();
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    /// The intent that makes a modulator of `kind` for the filter's resonance, with a cable to it.
    fn modulate_resonance(ctl: &Controller, kind: crate::app::intent::ModulatorKind, depth: Option<f32>) -> Intent {
        use crate::app::graph::{Endpoint, Port};
        let filter = node(ctl, NodePath::Effect { inst: 0, index: 1 });
        let (resonance, _) = param(ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        Intent::Modulate { to: Endpoint { node: filter, port: Port::Param { index: resonance } }, kind, depth }
    }

    #[test]
    fn modulating_a_parameter_is_one_step_that_takes_the_modulator_out_with_its_cable_and_puts_both_back_under_their_ids() {
        use crate::app::intent::ModulatorKind;
        let (_engine, mut ctl, clock) = controller_with_plugins();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let before = ctl.model.graph();
        let intent = modulate_resonance(&ctl, ModulatorKind::Lfo, None);
        ctl.apply(intent).unwrap();
        let after = ctl.model.graph();
        assert_ne!(before, after);
        assert_eq!(undo_label(&ctl).as_deref(), Some("modulate resonance"));
        // One undo takes out the modulator and the cable to it, and what came before is next.
        ctl.undo().unwrap();
        assert_eq!(ctl.model.graph(), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("set detune"), Some("modulate resonance")));
        // One redo makes both again, under the ids they had, and the intent is done as it was the first time.
        ctl.redo().unwrap();
        assert_eq!(ctl.model.graph(), after);
        ctl.model.check_ids().unwrap();
        ctl.undo().unwrap();
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0, "and what was done before it is put back after");
    }

    #[test]
    fn a_control_that_is_made_to_learn_is_a_step_in_the_words_of_the_learning_and_keeps_its_depth_when_it_is_done_again() {
        use crate::app::intent::ModulatorKind;
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let intent = modulate_resonance(&ctl, ModulatorKind::MidiLearn, Some(1.0));
        ctl.apply(intent).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("learn resonance"));
        let after = ctl.model.graph();
        ctl.undo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(ctl.model.graph(), after, "the depth is the one it was asked for, not the one a new cable has");
        let rack = &ctl.model.free_modulators;
        assert_eq!(rack.last().map(|m| m.targets[0].depth), Some(1.0));
    }

    #[test]
    fn modulating_a_setting_of_a_modulator_is_a_step_named_for_the_setting_and_one_that_is_refused_is_no_step() {
        use crate::app::graph::{Endpoint, Port};
        use crate::app::intent::ModulatorKind;
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane(&ctl)) }).unwrap();
        let lfo = ctl.last_made().nodes[0];
        ctl.apply(Intent::Modulate { to: Endpoint { node: lfo, port: Port::Rate }, kind: ModulatorKind::Envelope, depth: None }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("modulate rate"));
        // A parameter that the plugin has not is refused, and nothing was made to be undone.
        let before = ctl.model.graph();
        let filter = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        let refused = Intent::Modulate { to: Endpoint { node: filter, port: Port::Param { index: 9_999 } }, kind: ModulatorKind::Lfo, depth: None };
        assert!(ctl.apply(refused).is_err());
        assert_eq!(ctl.model.graph(), before);
        assert_eq!(undo_label(&ctl).as_deref(), Some("modulate rate"), "the step before it is the last one");
    }

    #[test]
    fn emptying_a_lane_of_its_instrument_forgets_the_history_as_putting_another_in_does_and_a_lane_with_none_changes_nothing() {
        // (A call of the controller's method that no intent asks for, as the tests make: what was set on the plugin is addressed by its parameters' places. The intent `ClearInstrument`
        // is a step, below.)
        let (_engine, mut ctl, _clock) = controller();
        set_detune(&mut ctl, 1.0);
        assert!(ctl.clear_instrument(0));
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let before = ctl.history_state();
        assert!(!ctl.clear_instrument(0), "the lane has none now");
        assert_eq!(ctl.history_state(), before, "and the history is as it was");
    }

    #[test]
    fn a_step_whose_node_has_gone_is_refused_with_the_reason_and_is_dropped_and_the_ones_before_it_stay() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 0 }, "damping");
        ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.9 }).unwrap();
        // Something takes the reverb out without the history (as a front-end that edits the model by itself could).
        ctl.model.remove_effect(0, 0);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set damping"));
        assert_eq!(ctl.undo(), Err(IntentError::NotFound(reverb)));
        // The step is gone, and the one before it is what Undo does now.
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("set detune"), None));
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0);
    }

    #[test]
    fn a_step_of_several_intents_applies_them_in_order_each_a_revision_and_says_what_all_of_them_did() {
        let (engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        let group = node(&ctl, NodePath::Group(0));
        let mut step = Step::new(
            "do two things",
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 1.0 },
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 2.0 },
            None,
            Instant::now(),
        );
        step.undo.push(Intent::RenameGroup { node: group, name: "First".to_string() });
        step.redo.push(Intent::RenameGroup { node: group, name: "Second".to_string() });
        ctl.history.record(step);
        let undone = ctl.undo().unwrap();
        assert_eq!(undone.rev, 2, "a revision for each");
        assert_eq!(
            undone.events,
            [Event::ParamSet { node: lane(&ctl), param: detune_index, value: 1.0 }, Event::GroupRenamed { node: group, name: "First".to_string() }]
        );
        assert_eq!(sent(&engine).len(), 1);
        let redone = ctl.redo().unwrap();
        assert_eq!((redone.rev, redone.events.len()), (4, 2));
        assert_eq!((detune(&ctl).1, ctl.model.groups[0].name.as_deref()), (2.0, Some("Second")));
    }

    #[test]
    fn a_step_that_fails_half_way_is_dropped_and_what_was_done_stays() {
        let (_engine, mut ctl, _clock) = controller();
        let (detune_index, _) = detune(&ctl);
        let mut step = Step::new(
            "do two things",
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 1.0 },
            Intent::SetParam { node: lane(&ctl), param: detune_index, value: 2.0 },
            None,
            Instant::now(),
        );
        step.undo.push(Intent::RenameGroup { node: NodeId(424242), name: "Nobody".to_string() });
        ctl.history.record(step);
        assert_eq!(ctl.undo(), Err(IntentError::NotFound(NodeId(424242))));
        assert_eq!(detune(&ctl).1, 1.0, "the first of them was done");
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    #[test]
    fn what_undo_and_redo_do_is_told_to_the_subscribers_as_any_intent_is() {
        let (_engine, mut ctl, _clock) = controller();
        let heard = Arc::new(Mutex::new(Vec::<Applied>::new()));
        let sink = heard.clone();
        ctl.subscribe(Box::new(move |applied: &Applied| sink.lock().unwrap().push(applied.clone())));
        set_detune(&mut ctl, 1.0);
        ctl.undo().unwrap();
        ctl.redo().unwrap();
        let heard = heard.lock().unwrap();
        assert_eq!(heard.iter().map(|a| a.rev).collect::<Vec<_>>(), [1, 2, 3]);
        let values: Vec<f32> = heard.iter().map(|a| match a.events[..] {
            [Event::ParamSet { value, .. }] => value,
            _ => panic!("{a:?}"),
        }).collect();
        assert_eq!(values, [1.0, 0.0, 1.0]);
    }

    #[test]
    fn what_an_add_made_is_what_takes_it_out_and_it_has_to_be_the_one_thing_and_be_there() {
        let (_engine, ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        let (lfo, edge) = (ctl.model.free_modulators[0].node, ctl.model.free_modulators[0].targets[0].edge);
        let add_effect = Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() };
        let add_cable = Intent::AddModEdge { modulator: lfo, to: Endpoint { node: lane0, port: Port::Param { index: 1 } } };
        let nodes = |nodes: &[NodeId]| Made { nodes: nodes.to_vec(), edges: vec![] };
        // One node that is there is taken out; none, two, and one that is not there are not.
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[lfo])), Some(vec![Intent::RemoveNode { node: lfo }]));
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[])), None);
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[lfo, lane0])), None);
        assert_eq!(ctl.takes_out(&add_effect, &nodes(&[NodeId(9_999)])), None);
        // A cable is taken out by its id, and only a cable that is there, and only when it is all that was made.
        let cable = |edges: &[EdgeId]| Made { nodes: vec![], edges: edges.to_vec() };
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[edge])), Some(vec![Intent::RemoveEdge { edge }]));
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[])), None);
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[edge, edge])), None);
        assert_eq!(ctl.takes_out(&add_cable, &cable(&[EdgeId(9_999)])), None);
        assert_eq!(ctl.takes_out(&add_cable, &Made { nodes: vec![lfo], edges: vec![edge] }), None);
        assert_eq!(ctl.takes_out(&add_cable, &nodes(&[lfo])), None);
        // Nodes are not what an add of a node makes when it has made a cable.
        assert_eq!(ctl.takes_out(&add_effect, &cable(&[edge])), None);
        assert_eq!(ctl.takes_out(&add_effect, &Made { nodes: vec![lfo], edges: vec![edge] }), None);
        // An effect that stands by itself made a lane and the effect in it, and it is the lane that is taken out: the first of the two nodes, when that is a lane that is there.
        let add_free = Intent::AddFreeEffect { plugin: "builtin:reverb".to_string() };
        assert_eq!(ctl.takes_out(&add_free, &nodes(&[lane0, lfo])), Some(vec![Intent::RemoveNode { node: lane0 }]));
        // Not one node (the one node that is there is what an effect that goes in a chain makes, and this made two), not three, not a first that is no lane (a modulator is a node that is there) and not one that
        // is not there, and not when a cable was made as well.
        assert_eq!(ctl.takes_out(&add_free, &nodes(&[lane0])), None);
        assert_eq!(ctl.takes_out(&add_free, &nodes(&[lane0, lfo, lfo])), None);
        assert_eq!(ctl.takes_out(&add_free, &nodes(&[lfo, lane0])), None);
        assert_eq!(ctl.takes_out(&add_free, &nodes(&[NodeId(9_999), lfo])), None);
        assert_eq!(ctl.takes_out(&add_free, &Made { nodes: vec![lane0, lfo], edges: vec![edge] }), None);
        // A module made a lane for each of its own (an instrument or an effect or more in each), the cables between the plugins, and then its modulators with their cables: every lane and every modulator that it made is taken
        // out, with all that is in it and the cables at it, the last that was made first (the modulators first, which come last). A module with one lane is the one lane. An effect is what stands for a plugin that it made:
        // it is no lane and no modulator, and goes with its lane.
        let add_module = Intent::AddFreeEffect { plugin: "module:Space".to_string() };
        let remove = |node: NodeId| Intent::RemoveNode { node };
        let lane1 = ctl.model.instruments[1].node;
        let effect = ctl.model.instruments[0].effects[0].node;
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lane0, effect])), Some(vec![remove(lane0)]));
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lane0, effect, effect, effect])), Some(vec![remove(lane0)]));
        assert_eq!(ctl.takes_out(&add_module, &Made { nodes: vec![lane0, effect, effect], edges: vec![edge] }), Some(vec![remove(lane0)]), "its cables go with the lane");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lane0])), Some(vec![remove(lane0)]), "a module may be an instrument alone, which is a lane and nothing in its chain");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lane0, effect, lane1, effect, effect])), Some(vec![remove(lane1), remove(lane0)]), "the lanes, the last first");
        assert_eq!(ctl.takes_out(&add_module, &Made { nodes: vec![lane0, lane1], edges: vec![edge, edge] }), Some(vec![remove(lane1), remove(lane0)]), "with the cables between its lanes");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[effect, lane0, effect])), Some(vec![remove(lane0)]), "a lane wherever it is among what was made");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[])), None);
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[effect, effect])), None, "no lane and no modulator was made: nothing that a module makes");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[NodeId(9_999), effect])), None, "a node that is not there is no lane");
        // The modulators come after the lanes and are taken out before them, the last first, each with its cables; the cables that they drew are among the edges that were made and go with them.
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lane0, effect, lfo])), Some(vec![remove(lfo), remove(lane0)]));
        assert_eq!(ctl.takes_out(&add_module, &Made { nodes: vec![lane0, effect, lane1, lfo], edges: vec![edge, edge, edge] }), Some(vec![remove(lfo), remove(lane1), remove(lane0)]), "the modulators first, then the lanes, the last first");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lfo, lane0])), Some(vec![remove(lane0), remove(lfo)]), "in the reverse of the order that they were made in, whatever it is");
        assert_eq!(ctl.takes_out(&add_module, &nodes(&[lfo, NodeId(9_999)])), Some(vec![remove(lfo)]), "a node that is not there is neither");
    }

    #[test]
    fn an_add_that_made_nothing_that_can_be_named_leaves_no_step_and_empties_the_history() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        set_detune(&mut ctl, 1.0);
        assert!(undo_label(&ctl).is_some());
        ctl.made_last = Made::default();
        let intent = Intent::AddEffect { parent: lane(&ctl), plugin: "builtin:reverb".to_string() };
        ctl.keep_in_history(Plan::Added { label: "add Reverb".to_string(), intent, at: Instant::now() });
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "what came before cannot be undone past what cannot be put back");
    }

    #[test]
    fn ids_that_a_step_did_not_use_are_not_kept_for_what_is_made_next() {
        let (_engine, mut ctl, _clock) = controller_with_plugins();
        let lane0 = lane(&ctl);
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:reverb".to_string() }).unwrap();
        let first = ctl.last_made().nodes.clone();
        ctl.undo().unwrap();
        // The step does something that makes nothing, though it says it makes a node (as a step that was made for something else would).
        let mut step = ctl.history.take_redo().unwrap();
        step.redo = vec![Intent::SetLocked { locked: true }];
        ctl.history.kept_undone(step);
        ctl.redo().unwrap();
        ctl.apply(Intent::AddEffect { parent: lane0, plugin: "builtin:filter".to_string() }).unwrap();
        assert_ne!(ctl.last_made().nodes, first, "the id that was left over is not given to the next node");
        ctl.model.check_ids().unwrap();
    }

    // -- what the web client's tests read --

    /// What a page that is open is to the controller: a subscriber, which a change of structure is told to with a snapshot of a revision of its
    /// own (`Controller::resync`), so that the revisions that come after it are the ones a page is told.
    struct Page;

    impl crate::app::subscribers::EventSink for Page {
        fn applied(&mut self, _: &Applied) {}
        fn resync(&mut self, _: &crate::app::snapshot::Snapshot) {}
    }

    // -- what is taken out is put back --

    /// A lane with a filter, an LFO with three cables (to the oscillator's detune and volume and the filter's cutoff, in that order) and a second
    /// LFO whose cable drives the depth of the first's third.
    const CABLES: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
[[instrument.modulator.target]]
param = "volume"
depth = 0.2
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.3
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
mod_depth = [0, 2]
depth = 0.4
"#;

    /// What a client is told the session is: the nodes and the cables, and the layout of the telemetry. What is put back has to be what it was.
    fn picture(ctl: &Controller) -> (crate::app::graph::Graph, crate::app::layout::TelemetryLayout) {
        let snapshot = ctl.snapshot();
        (snapshot.graph, snapshot.telemetry)
    }

    /// What `Ctrl+S` would write.
    fn written(ctl: &Controller) -> String {
        let (instruments, groups, cables, sources) = crate::app::save::to_save(&ctl.model);
        crate::session::to_toml(&instruments, &groups, &cables, &sources, None, None, None).unwrap()
    }

    fn cable_edge(ctl: &Controller, modulator: usize, cable: usize) -> EdgeId {
        ctl.model.free_modulators[modulator].targets[cable].edge
    }

    #[test]
    fn a_cable_that_was_taken_out_is_put_back_by_undo_where_it_was_and_taken_out_again_by_redo() {
        let (engine, mut ctl, clock) = controller_over(load(CABLES).model);
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let volume = cable_edge(&ctl, 0, 1);
        let (before, saved) = (picture(&ctl), written(&ctl));
        sent(&engine);

        ctl.apply(Intent::RemoveEdge { edge: volume }).unwrap();
        let (taken, saved_taken) = (picture(&ctl), written(&ctl));
        assert_ne!(taken, before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("remove a cable"), None));
        let commands = sent(&engine);
        assert_eq!(commands[0], "RemoveFreeModTarget mod=0 target=1", "{commands:?}");

        let rev = ctl.rev;
        let undone = ctl.undo().unwrap();
        // (An intent like the others: a revision, and the structure said to have changed.)
        assert_eq!((undone.rev, undone.restructured, undone.events.is_empty()), (rev + 1, true, true));
        assert_eq!(picture(&ctl), before, "the cable is where it was, with its id and the slot it publishes to");
        assert_eq!(written(&ctl), saved);
        let commands = sent(&engine);
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert!(commands[0].starts_with(&format!("InsertFreeModTarget mod=0 target=1 aim=Param {{ plugin: {:?}, ", node(&ctl, NodePath::Instrument(0)))), "{commands:?}");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("set detune"), Some("remove a cable")), "what was done before it is still to be undone");
        assert!(ctl.dirty);

        let redone = ctl.redo().unwrap();
        assert!(redone.restructured);
        assert_eq!(picture(&ctl), taken);
        assert_eq!(written(&ctl), saved_taken);
        assert_eq!(sent(&engine)[0], "RemoveFreeModTarget mod=0 target=1");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("remove a cable"), None));

        ctl.undo().unwrap();
        assert_eq!(picture(&ctl), before);
        ctl.undo().unwrap();
        assert_eq!(detune(&ctl).1, 0.0, "the step before it goes back too");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_cables_that_drive_the_depth_of_a_cable_that_goes_are_put_back_with_it_each_where_it_was() {
        let (engine, mut ctl, _clock) = controller_over(load(CABLES).model);
        // The second LFO's only cable drives the depth of the first's third.
        let (cutoff, depth_cable) = (cable_edge(&ctl, 0, 2), cable_edge(&ctl, 1, 0));
        let before = picture(&ctl);
        sent(&engine);
        ctl.apply(Intent::RemoveEdge { edge: cutoff }).unwrap();
        assert!(ctl.model.locate_edge(depth_cable).is_none(), "it went with the cable it drove");
        assert_eq!(&sent(&engine)[..2], ["RemoveFreeModTarget mod=1 target=0", "RemoveFreeModTarget mod=0 target=2"]);

        ctl.undo().unwrap();
        assert_eq!(picture(&ctl), before);
        let commands = sent(&engine);
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert!(commands[0].starts_with(&format!("InsertFreeModTarget mod=0 target=2 aim=Param {{ plugin: {:?}, ", node(&ctl, NodePath::Effect { inst: 0, index: 0 }))), "the cable first: {commands:?}");
        assert!(commands[1].starts_with(&testutil::told(&ctl.model, "InsertFreeModTarget mod=1 target=0 kind=ModulatorDepth { mod_index: 0, target_index: 2 }")), "then what drives its depth: {commands:?}");
        ctl.redo().unwrap();
        assert!(ctl.model.locate_edge(cutoff).is_none() && ctl.model.locate_edge(depth_cable).is_none());
        ctl.undo().unwrap();
        assert_eq!(picture(&ctl), before);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_that_is_put_back_is_put_back_on_the_audio_thread_too_as_the_model_has_it() {
        use crate::plugin::chain::AudioGraph;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(CABLES, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut ctl = Controller::new(model, handles, None);
        graph.drain_commands();
        // What the model says every cable is and drives, and what the audio thread has.
        let in_the_model = |ctl: &Controller| -> Vec<(crate::plugin::chain::ModulatorLocation, usize, EdgeId, Driven)> {
            let at = crate::plugin::chain::ModulatorLocation::Free;
            ctl.model.free_modulators.iter().enumerate().flat_map(|(m, modulator)| modulator.targets.iter().map(move |t| (at, m, t.edge, t.aim))).collect()
        };
        let on_the_audio_thread = |graph: &AudioGraph| -> Vec<_> { graph.cable_bases().into_iter().map(|(at, m, edge, aim, _)| (at, m, edge, aim)).collect() };
        let before = (graph.cable_bases(), in_the_model(&ctl));
        assert_eq!(on_the_audio_thread(&graph), before.1);

        for (modulator, cable) in [(0, 0), (0, 1), (0, 2), (1, 0)] {
            let edge = cable_edge(&ctl, modulator, cable);
            ctl.apply(Intent::RemoveEdge { edge }).unwrap();
            graph.drain_commands();
            assert_eq!(on_the_audio_thread(&graph), in_the_model(&ctl), "cable {cable} of modulator {modulator} taken out");
            assert_ne!(graph.cable_bases(), before.0);
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_eq!((graph.cable_bases(), in_the_model(&ctl)), before, "cable {cable} of modulator {modulator} put back: the same cables, each centred on what it was");
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_eq!(on_the_audio_thread(&graph), in_the_model(&ctl), "cable {cable} of modulator {modulator} taken out again");
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_eq!((graph.cable_bases(), in_the_model(&ctl)), before);
        }
    }

    /// A group with a bus filter and two LFOs, one that drives the bus filter's resonance and the member's detune, and one that drives the depth of the
    /// member's cable to its volume; a member with an LFO whose cables go to its detune and volume.
    const GROUP_CABLES: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 0
param = "resonance"
depth = 0.1
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.2
[[group.modulator]]
rate = 2.0
[[group.modulator.target]]
member = 0
mod_depth = [0, 1]
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
param = "pan"
depth = 0.4
[[instrument.modulator.target]]
param = "volume"
depth = 0.5
"#;

    #[test]
    fn the_cables_of_a_groups_modulators_are_put_back_on_the_audio_thread_too_and_the_cables_that_drive_the_depth_of_a_members_cable_with_it() {
        use crate::plugin::chain::AudioGraph;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(GROUP_CABLES, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut ctl = Controller::new(model, handles, None);
        graph.drain_commands();
        let before = (graph.cable_bases(), picture(&ctl), written(&ctl));
        let rack_of_groups = |ctl: &Controller| -> Vec<Vec<EdgeId>> { hearing_group(&ctl.model, 0).iter().map(|m| m.targets.iter().map(|t| t.edge).collect()).collect() };
        let (group_cables, member_cables) = (rack_of_groups(&ctl), ctl.model.free_modulators[0].targets.iter().map(|t| t.edge).collect::<Vec<_>>());
        assert_eq!((group_cables[0].len(), group_cables[1].len(), member_cables.len()), (2, 1, 2));
        // Each cable of the group's first modulator, the group's cable to the depth of a member's cable, and the member's two (the second of which that one drives).
        for edge in [group_cables[0][0], group_cables[0][1], group_cables[1][0], member_cables[1], member_cables[0]] {
            ctl.apply(Intent::RemoveEdge { edge }).unwrap();
            graph.drain_commands();
            assert_ne!(graph.cable_bases(), before.0, "{edge:?} taken out");
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_eq!((graph.cable_bases(), picture(&ctl), written(&ctl)), before, "{edge:?} put back");
            ctl.redo().unwrap();
            graph.drain_commands();
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_eq!((graph.cable_bases(), picture(&ctl), written(&ctl)), before, "{edge:?} taken out again and put back again");
        }
    }

    #[test]
    fn a_redo_that_takes_out_what_nothing_puts_back_forgets_the_history_as_a_removal_that_does_so_the_first_time_would() {
        let (_engine, mut ctl, clock) = controller_over(load(CABLES).model);
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let volume = cable_edge(&ctl, 0, 1);
        ctl.apply(Intent::RemoveEdge { edge: volume }).unwrap();
        ctl.undo().unwrap();
        // Something that no step made (a front-end that edits the model by itself) makes the cable and another drive each other's depths.
        let (first, second) = (ctl.model.free_modulators[0].targets[1].edge, ctl.model.free_modulators[1].targets[0].edge);
        ctl.model.free_modulators[0].targets[1].aim = crate::app::model::Driven::Depth(second);
        ctl.model.free_modulators[1].targets[0].aim = crate::app::model::Driven::Depth(first);
        let redone = ctl.redo().unwrap();
        assert!(redone.restructured, "it was done");
        assert_eq!(ctl.model.free_modulators[1].targets.len(), 0, "and the other cable went with the cable");
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "but it cannot be undone, so nothing can");
    }

    #[test]
    fn a_cable_that_is_put_back_is_centred_on_what_the_user_had_set_what_it_drives_to() {
        use crate::plugin::chain::AudioGraph;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(CABLES, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut ctl = Controller::new(model, handles, None);
        let (detune_index, _) = detune(&ctl);
        ctl.apply(Intent::SetParam { node: lane(&ctl), param: detune_index, value: 7.0 }).unwrap();
        graph.drain_commands();
        let edge = cable_edge(&ctl, 0, 0);
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        // The user turns the knob that the cable no longer drives, and then the cable comes back: it is centred on the new value, which is what is set.
        ctl.undo().unwrap();
        graph.drain_commands();
        let detune_of_the_lane = Driven::Param { plugin: lane(&ctl), index: detune_index };
        let centre = |graph: &AudioGraph| graph.cable_bases().into_iter().find(|(_, _, _, aim, _)| *aim == detune_of_the_lane).map(|(.., base)| base);
        assert_eq!(centre(&graph), Some(7.0), "the value that was set when the cable went");
    }

    #[test]
    fn a_cable_that_is_put_back_publishes_the_value_it_drives_into_the_slot_it_had() {
        use crate::plugin::chain::{AudioGraph, LfoWaveform};
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(CABLES, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let mut ctl = Controller::new(model, handles, None);
        // The first cable drives the oscillator's detune: its slot is where the effective value of the parameter is told.
        let first = &ctl.model.free_modulators[0].targets[0];
        let (edge, slot) = (first.edge, first.effective.expect("the cable has a slot"));
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets[0].effective, Some(slot), "the model has the slot it had");
        ctl.telemetry.store(slot, f32::NAN);
        graph.drain_commands();
        let mut out = vec![vec![0.0f32; 64]; 2];
        graph.process(&[], &mut out).unwrap();
        // The LFO is a 1 Hz sine, and the cable pulls the detune (a parameter of 48 semitones, set to 0) by a tenth of its range.
        let output = LfoWaveform::Sine.eval(1.0 * 64.0 / 48_000.0);
        let published = ctl.telemetry.load(slot);
        assert!((published - 0.1 * output * 48.0).abs() < 1e-3, "{published} for an LFO output of {output}");
    }

    #[test]
    fn cables_that_drive_each_others_depths_cannot_be_put_back_and_empty_the_history_as_what_cannot_be_undone_does() {
        let (_engine, mut ctl, _clock) = controller_over(load(testutil::CABLES_THAT_DRIVE_EACH_OTHER).model);
        set_detune(&mut ctl, 1.0);
        assert!(undo_label(&ctl).is_some());
        let first = cable_edge(&ctl, 0, 0);
        ctl.apply(Intent::RemoveEdge { edge: first }).unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1, "both went");
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        assert!(ctl.undo().is_err());
    }

    #[test]
    fn a_cable_that_is_there_already_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (_engine, mut ctl, clock) = controller_over(load(CABLES).model);
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let volume = cable_edge(&ctl, 0, 1);
        let slot_of_volume = {
            let target = &ctl.model.free_modulators[0].targets[1];
            crate::app::model::ModTargetSlot {
                edge: target.edge,
                param_name: target.param_name.clone(),
                aim: target.aim,
                depth: target.depth,
                param_min: target.param_min,
                param_max: target.param_max,
                effective: target.effective,
            }
        };
        ctl.apply(Intent::RemoveEdge { edge: volume }).unwrap();
        // Something puts a cable of that id in without the history (as a front-end that edits the model by itself could).
        ctl.model.insert_target_at(crate::plugin::chain::ModulatorLocation::Free, 0, 1, slot_of_volume).unwrap();
        let before = picture(&ctl);
        let error = ctl.undo().unwrap_err();
        assert_eq!(error, IntentError::Invalid(format!("cable {} is there already", volume.0)));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("set detune"), None), "the step is gone, the one before it is the next");
    }

    #[test]
    fn a_cable_that_was_taken_out_by_the_controller_and_not_by_an_intent_leaves_nothing_to_put_back_and_forgets_the_history() {
        let (_engine, mut ctl, _clock) = controller_over(load(CABLES).model);
        set_detune(&mut ctl, 1.0);
        let volume = cable_edge(&ctl, 0, 1);
        assert!(ctl.remove_edge(volume));
        assert!(ctl.journal.is_none(), "nothing is written down unless an intent is being carried out");
        // (Nobody asked for a step, and the places of the cables of the others may have moved: what a step took out is put back by the places it had.)
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    #[test]
    fn a_cable_that_is_refused_leaves_the_history_as_it_was() {
        let (_engine, mut ctl, _clock) = controller_over(load(CABLES).model);
        set_detune(&mut ctl, 1.0);
        let before = ctl.history_state();
        assert_eq!(ctl.apply(Intent::RemoveEdge { edge: EdgeId(9_999) }).unwrap_err(), IntentError::EdgeNotFound(EdgeId(9_999)));
        assert_eq!(ctl.history_state(), before);
    }

    #[test]
    fn reordering_the_lanes_forgets_the_history_as_what_a_removal_put_back_would_go_by_the_place_of_its_lane() {
        let (_engine, mut ctl, clock) = controller();
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let edge = cable_edge(&ctl, 0, 0);
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("set detune"), Some("remove a cable")));
        // A swap that is refused changes nothing, the history included.
        assert!(!ctl.swap_lanes(0, 0) && !ctl.swap_lanes(0, 7));
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("set detune"), Some("remove a cable")));
        // (No intent asks for it, and the cable that was taken out would be put back into whatever lane is the first by then.)
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert!(ctl.swap_lanes(0, 1));
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        assert!(ctl.undo().is_err() && ctl.redo().is_err());
    }

    /// A lane in a group that has two bus effects, and a lane in none.
    const TWO_BUS_EFFECTS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_structural_edit_that_no_intent_asks_for_forgets_the_history_and_one_that_is_refused_does_not() {
        use crate::plugin::chain::ModulatorLocation::Free;
        type Edit = fn(&mut Controller) -> bool;
        // (What a step took out is put back by the places it had, which these change: nothing in the history can be trusted with them. The edits that an intent
        // asks for are steps, or forget the history themselves.)
        let edits: [(&str, &str, Edit); 10] = [
            ("take a cable out", SESSION, |ctl| {
                let edge = ctl.model.free_modulators[0].targets[0].edge;
                ctl.remove_edge(edge)
            }),
            ("take a modulator out", SESSION, |ctl| ctl.remove_modulator(Free, 1)),
            ("take an effect out", SESSION, |ctl| ctl.remove_effect(0, 0)),
            ("take a bus effect out", TWO_BUS_EFFECTS, |ctl| ctl.remove_bus_effect(0, 0)),
            ("take a lane out", SESSION, |ctl| ctl.remove_lane(1)),
            ("take a group out", SESSION, |ctl| ctl.remove_group(1)),
            ("move an effect", SESSION, |ctl| ctl.move_effect(0, 0, 1)),
            ("move a bus effect", TWO_BUS_EFFECTS, |ctl| ctl.move_bus_effect(0, 0, 1)),
            ("put a lane in a group", SESSION, |ctl| ctl.set_lane_group(1, Some(0))),
            ("swap two lanes", SESSION, |ctl| ctl.swap_lanes(0, 1)),
        ];
        for (what, session, edit) in edits {
            let (_engine, mut ctl, _clock) = controller_over(load(session).model);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            assert!(undo_label(&ctl).is_some(), "{what}: there is something to forget");
            assert!(edit(&mut ctl), "{what}");
            assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "{what}");
        }
        // One that does nothing (the place is not there, or the same) leaves the history as it was.
        let refused: [(&str, Edit); 10] = [
            ("a cable that is not there", |ctl| ctl.remove_edge(EdgeId(9_999))),
            ("a modulator that is not there", |ctl| ctl.remove_modulator(Free, 9)),
            ("an effect that is not there", |ctl| ctl.remove_effect(0, 9)),
            ("a bus effect that is not there", |ctl| ctl.remove_bus_effect(0, 9)),
            ("a lane that is not there", |ctl| ctl.remove_lane(9)),
            ("a group that is not there", |ctl| ctl.remove_group(9)),
            ("an effect moved to its own place", |ctl| ctl.move_effect(0, 0, 0)),
            ("a bus effect moved to its own place", |ctl| ctl.move_bus_effect(0, 0, 0)),
            ("a lane put in a group that is not there", |ctl| ctl.set_lane_group(0, Some(9))),
            ("a lane swapped with itself", |ctl| ctl.swap_lanes(0, 0)),
        ];
        for (what, edit) in refused {
            let (_engine, mut ctl, _clock) = controller();
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            assert!(!edit(&mut ctl), "{what}");
            assert_eq!(undo_label(&ctl).as_deref(), Some("lock the piano to its scale"), "{what}: the history is as it was");
        }
    }

    // -- a modulator that is taken out is put back --

    /// A lane with a filter and five modulators. (0) An LFO whose cables go to the oscillator's detune and the filter's cutoff. (1) An envelope whose cables go
    /// to the oscillator's volume and to the rate of the first LFO. (2) An LFO whose cables drive the envelope's attack and the depth of the envelope's first
    /// cable. (3) A MIDI control that is waiting for the next to come in, with a cable to the oscillator's pan. (4) A MIDI control that is bound to CC 74, with a
    /// cable to the filter's resonance.
    const MODULATORS: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.3

[[instrument.modulator]]
type = "envelope"
attack = 0.02
decay = 0.4
sustain = 0.6
release = 0.8
[[instrument.modulator.target]]
param = "volume"
depth = 0.5
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.2

[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
mod_attack = 1
depth = 0.3
[[instrument.modulator.target]]
mod_depth = [1, 0]
depth = 0.4

[[instrument.modulator]]
type = "midi"
[[instrument.modulator.target]]
param = "pan"
depth = 0.7

[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator.target]]
slot = 1
param = "resonance"
depth = 0.9
"#;

    /// A controller over `session` and an audio thread that is given what the controller sends it, and says what it has when it is asked.
    fn controller_over_an_audio_thread(session: &str) -> (Controller, crate::plugin::chain::AudioGraph) {
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(session, &handles.cmd_tx);
        let mut graph = crate::plugin::chain::AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let ctl = Controller::new(model, handles, None);
        graph.drain_commands();
        (ctl, graph)
    }

    /// What `modulators_as_they_are` says: the picture, the file, and the audio thread's modulators and what their cables are centred on.
    type Modulators = (
        (crate::app::graph::Graph, crate::app::layout::TelemetryLayout),
        String,
        Vec<String>,
        Vec<(crate::plugin::chain::ModulatorLocation, usize, EdgeId, Driven, f32)>,
    );

    /// What the modulators are, as each place that says so tells it: the picture a client is given (the nodes and cables with their ids, and the layout of
    /// the telemetry), what `Ctrl+S` would write, and the audio thread's own: every modulator with what it drives and where it publishes, and what each cable
    /// is centred on.
    fn modulators_as_they_are(ctl: &Controller, graph: &crate::plugin::chain::AudioGraph) -> Modulators {
        // (Every cable drives something: a file that has one that does not, to the depth of a cable that is not there, is loaded without it.)
        (picture(ctl), written(ctl), graph.modulator_dump(), graph.cable_bases())
    }

    /// The same places say the same: each of them on its own, so that a failure says which.
    fn assert_same_modulators(now: &Modulators, then: &Modulators, what: &str) {
        assert_eq!(now.0 .0, then.0 .0, "{what}: the nodes and cables of the picture");
        assert_eq!(now.0 .1, then.0 .1, "{what}: the layout of the telemetry");
        assert_eq!(now.1, then.1, "{what}: the file");
        assert_eq!(now.2, then.2, "{what}: the modulators of the audio thread");
        assert_eq!(now.3, then.3, "{what}: what the cables of the audio thread are centred on");
    }

    /// The nodes of the modulators of the session, in the order the model lists them.
    fn modulator_nodes(ctl: &Controller) -> Vec<NodeId> {
        ctl.model.nodes().filter(|(_, path)| matches!(path, NodePath::Modulator { .. })).map(|(id, _)| id).collect()
    }

    /// Every modulator of `session`, one at a time (each in a controller of its own, after a step of another kind): taken out by an intent, put back by undo,
    /// taken out again by redo and put back again. Each time the model, the picture a client is given, the file and the audio thread are as they were (or as
    /// they were when it was taken out). How many were put back, and how many were not (cables that drive each other's depths went with them, so that nothing
    /// can be put back and the history is forgotten).
    fn every_modulator_is_put_back(name: &str, session: &str) -> (usize, usize) {
        let count = modulator_nodes(&controller_over_an_audio_thread(session).0).len();
        let (mut put_back, mut forgotten) = (0, 0);
        for n in 0..count {
            let (mut ctl, mut graph) = controller_over_an_audio_thread(session);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let node = modulator_nodes(&ctl)[n];
            let before = modulators_as_they_are(&ctl, &graph);
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            graph.drain_commands();
            let taken = modulators_as_they_are(&ctl, &graph);
            assert_ne!(taken.0, before.0, "modulator {n} of {name}: it is gone from the picture");
            assert!(ctl.model.locate(node).is_none(), "modulator {n}: it is gone from the model");
            if ctl.history_state() == (HistoryState { undo: None, redo: None }) {
                assert!(ctl.undo().is_err(), "modulator {n}: nothing to undo");
                forgotten += 1;
                continue;
            }
            assert!(undo_label(&ctl).is_some_and(|label| label.starts_with("remove ")), "modulator {n}: {:?}", undo_label(&ctl));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &before, &format!("modulator {n} of {name} put back"));
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &taken, &format!("modulator {n} of {name} taken out again"));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &before, &format!("modulator {n} of {name} put back again"));
            ctl.model.check_ids().unwrap();
            put_back += 1;
        }
        (put_back, forgotten)
    }

    #[test]
    fn every_modulator_of_every_session_is_put_back_by_undo_in_the_model_the_picture_the_file_and_on_the_audio_thread() {
        let mut total = 0;
        let sessions = [
            ("MODULATORS", MODULATORS),
            ("GROUP_CABLES", GROUP_CABLES),
            ("CABLES", CABLES),
            ("GROUPS", testutil::GROUPS),
            ("CROSS_MOD", testutil::CROSS_MOD),
            ("CROSS_MOD_RACKS", testutil::CROSS_MOD_RACKS),
            ("LANES_IN_GROUPS", testutil::LANES_IN_GROUPS),
            ("CABLES_THAT_DRIVE_CABLES", testutil::CABLES_THAT_DRIVE_CABLES),
            ("FREE_MODULATORS", testutil::FREE_MODULATORS),
        ];
        for (name, session) in sessions {
            let (put_back, forgotten) = every_modulator_is_put_back(name, session);
            assert_eq!(forgotten, 0, "{name}");
            total += put_back;
        }
        assert!(total > 20, "{total} modulators is not enough to say much");
    }

    #[test]
    fn modulators_whose_cables_drive_each_others_depths_are_not_put_back_and_the_history_is_forgotten_as_it_is_for_what_cannot_be_undone() {
        // (Only a file can make them: a cable is made to drive a cable that is there already.)
        assert_eq!(every_modulator_is_put_back("CABLES_THAT_DRIVE_EACH_OTHER", testutil::CABLES_THAT_DRIVE_EACH_OTHER), (0, 2));
    }

    const MACROS: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\nvalue = 0.25\n[[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.4\n";

    fn macro_value(ctl: &Controller) -> f32 {
        match ctl.model.free_modulators[0].source {
            ModSourceSlot::Macro { value, .. } => value,
            ref other => panic!("{other:?} is not a macro"),
        }
    }

    #[test]
    fn the_value_of_a_macro_is_put_back_and_done_again_and_a_drag_of_it_is_one_step() {
        let (engine, mut ctl, clock) = controller_over(load(MACROS).model);
        let knob = ctl.model.free_modulators[0].node;
        for value in [0.4, 0.6, 0.8] {
            ctl.apply(Intent::SetModulatorField { node: knob, field: ModulatorField::Value, value }).unwrap();
            clock.advance(Duration::from_millis(50));
        }
        sent(&engine);
        assert_eq!(undo_label(&ctl).as_deref(), Some("set value"));
        ctl.undo().unwrap();
        assert_eq!(macro_value(&ctl), 0.25);
        assert_eq!(sent(&engine), ["SetFreeModulatorSource mod=0 source=Macro { value: 0.25 }"]);
        assert_eq!(undo_label(&ctl), None, "the drag was one step");
        ctl.redo().unwrap();
        assert_eq!(macro_value(&ctl), 0.8);
        assert_eq!(sent(&engine), ["SetFreeModulatorSource mod=0 source=Macro { value: 0.8 }"]);
    }

    #[test]
    fn a_macro_that_is_taken_out_is_put_back_at_the_value_it_had_with_its_cable_in_the_words_of_the_button_that_adds_one() {
        let (engine, mut ctl, clock) = controller_over(load(MACROS).model);
        let knob = ctl.model.free_modulators[0].node;
        ctl.apply(Intent::SetModulatorField { node: knob, field: ModulatorField::Value, value: 0.8 }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RemoveNode { node: knob }).unwrap();
        assert!(ctl.model.free_modulators.is_empty());
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove a macro"));
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(macro_value(&ctl), 0.8, "the value it had, and not the one it was loaded with");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1, "with its cable");
        assert_eq!(ctl.model.free_modulators[0].node, knob, "under the id it had");
        let commands = sent(&engine);
        assert_eq!(commands.first().map(String::as_str), Some("InsertFreeModulator index=0 source=Macro { value: 0.8 }"), "{commands:?}");
    }

    const SEQUENCERS: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\nsteps = [0.5, -0.25, 1.0]\nrate = 3.5\nglide = 0.25\nretrigger = false\n[[instrument.modulator.target]]\nparam = \"detune\"\ndepth = 0.4\n[[instrument.modulator]]\ntype = \"steps\"\n";

    fn lane_steps(ctl: &Controller, index: usize) -> ([f32; 16], usize, f32, f32, bool) {
        match ctl.model.free_modulators[index].source {
            ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } => (values, length, rate, glide, retrigger),
            ref other => panic!("{other:?} is not a step sequencer"),
        }
    }

    /// The command that gives the engine these steps, as `sent` says it (for the step sequencer at the first place of the lane's rack).
    fn steps_line(values: [f32; 16], length: usize, glide: f32, retrigger: bool) -> String {
        format!("SetModulatorSteps Free index=0 values={values:?} length={length} glide={glide:.3} retrigger={retrigger}")
    }

    #[test]
    fn the_steps_of_a_step_sequencer_are_put_back_and_done_again_and_a_stroke_across_them_is_one_step() {
        let (engine, mut ctl, clock) = controller_over(load(SEQUENCERS).model);
        let sequencer = ctl.model.free_modulators[0].node;
        let (first, ..) = lane_steps(&ctl, 0);
        let mut painted = first;
        // A stroke: one bar after the other, a moment between, each a change of what the sequencer goes through.
        for (bar, value) in [(0, 0.9), (1, 0.8), (2, 0.7)] {
            painted[bar] = value;
            ctl.apply(Intent::SetSteps { node: sequencer, values: painted.to_vec() }).unwrap();
            clock.advance(Duration::from_millis(50));
        }
        assert_eq!(undo_label(&ctl).as_deref(), Some("set steps"));
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(lane_steps(&ctl, 0).0, first, "all of the stroke is put back at once");
        assert_eq!(sent(&engine), [steps_line(first, 3, 0.25, false)]);
        assert_eq!(undo_label(&ctl), None, "the stroke was one step");
        ctl.redo().unwrap();
        assert_eq!(lane_steps(&ctl, 0).0, painted);
        assert_eq!(sent(&engine), [steps_line(painted, 3, 0.25, false)]);
    }

    #[test]
    fn the_steps_of_one_step_sequencer_and_of_another_are_two_steps_and_so_are_strokes_a_while_apart() {
        let (_engine, mut ctl, clock) = controller_over(load(SEQUENCERS).model);
        let (first, second) = (ctl.model.free_modulators[0].node, ctl.model.free_modulators[1].node);
        let steps = |value: f32| vec![value; 16];
        ctl.apply(Intent::SetSteps { node: first, values: steps(0.1) }).unwrap();
        ctl.apply(Intent::SetSteps { node: second, values: steps(0.2) }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::SetSteps { node: second, values: steps(0.3) }).unwrap();
        let mut labels = Vec::new();
        while let Some(label) = undo_label(&ctl) {
            labels.push(label);
            ctl.undo().unwrap();
        }
        assert_eq!(labels, ["set steps", "set steps", "set steps"]);
        assert_eq!(lane_steps(&ctl, 0).0[..4], [0.5, -0.25, 1.0, 0.0]);
        assert_eq!(lane_steps(&ctl, 1).0, ModSourceSlot::staircase());
    }

    #[test]
    fn a_setting_of_a_step_sequencer_is_a_step_in_the_words_of_the_setting_and_is_put_back() {
        let (engine, mut ctl, clock) = controller_over(load(SEQUENCERS).model);
        let sequencer = ctl.model.free_modulators[0].node;
        let (values, ..) = lane_steps(&ctl, 0);
        for (field, value, words) in [
            (ModulatorField::Glide, 0.75, "set glide"),
            (ModulatorField::Length, 9.0, "set length"),
            (ModulatorField::Retrigger, 1.0, "set retrigger"),
            (ModulatorField::Rate, 20.0, "set rate"),
        ] {
            ctl.apply(Intent::SetModulatorField { node: sequencer, field, value }).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(words));
            clock.advance(Duration::from_secs(5));
        }
        sent(&engine);
        for _ in 0..4 {
            ctl.undo().unwrap();
        }
        assert_eq!(lane_steps(&ctl, 0), (values, 3, 3.5, 0.25, false));
        assert_eq!(
            sent(&engine),
            [
                "SetFreeModulatorRate mod=0 rate=3.500".to_string(),
                steps_line(values, 9, 0.75, false),
                steps_line(values, 3, 0.75, false),
                steps_line(values, 3, 0.25, false),
            ]
        );
    }

    #[test]
    fn a_step_sequencer_that_is_taken_out_is_put_back_with_the_steps_it_had_and_its_cable_in_the_words_of_the_button_that_adds_one() {
        let (engine, mut ctl, clock) = controller_over(load(SEQUENCERS).model);
        let sequencer = ctl.model.free_modulators[0].node;
        let mut values = [0.0; 16];
        values[..3].copy_from_slice(&[0.9, 0.8, 0.7]);
        ctl.apply(Intent::SetSteps { node: sequencer, values: values.to_vec() }).unwrap();
        ctl.apply(Intent::SetModulatorField { node: sequencer, field: ModulatorField::Length, value: 2.0 }).unwrap();
        ctl.apply(Intent::SetModulatorField { node: sequencer, field: ModulatorField::Sync, value: 14.0 }).unwrap();
        clock.advance(Duration::from_secs(5));
        ctl.apply(Intent::RemoveNode { node: sequencer }).unwrap();
        assert_eq!(ctl.model.free_modulators.len(), 1);
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove a step sequencer"));
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(lane_steps(&ctl, 0), (values, 2, 3.5, 0.25, false), "as it was, and not as it was loaded");
        assert!(matches!(ctl.model.free_modulators[0].source, ModSourceSlot::Steps { sync: 14, .. }), "and following the tempo by the note length it had");
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1, "with its cable");
        assert_eq!(ctl.model.free_modulators[0].node, sequencer, "under the id it had");
        let commands = sent(&engine);
        let inserted = format!("InsertFreeModulator index=0 source=Steps {{ values: {values:?}, length: 2, rate: 3.5, glide: 0.25, retrigger: false, sync: 0.25, position: 0.0 }}");
        assert_eq!(commands.first(), Some(&inserted), "{commands:?}");
    }

    #[test]
    fn a_modulator_that_is_taken_out_is_a_step_in_the_words_of_the_button_that_adds_one_like_it() {
        let (_engine, mut ctl, _clock) = controller_over(load(MODULATORS).model);
        let nodes = modulator_nodes(&ctl);
        let words = ["remove an LFO", "remove an envelope", "remove an LFO", "remove a MIDI control", "remove a MIDI control"];
        assert_eq!(nodes.len(), words.len());
        for (node, words) in nodes.into_iter().zip(words) {
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            assert_eq!(undo_label(&ctl).as_deref(), Some(words));
        }
        // Each of them is put back, the last first.
        for words in words.into_iter().rev() {
            assert_eq!(undo_label(&ctl).as_deref(), Some(words));
            ctl.undo().unwrap();
        }
        assert_eq!(undo_label(&ctl), None);
        assert_eq!(redo_label(&ctl).as_deref(), Some("remove an LFO"), "the first that was taken out is the last that was put back");
    }

    #[test]
    fn what_a_modulator_drove_has_the_value_that_was_set_when_it_goes_and_it_is_centred_on_it_when_it_comes_back() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(MODULATORS);
        let (detune_index, _) = detune(&ctl);
        ctl.apply(Intent::SetParam { node: lane(&ctl), param: detune_index, value: 3.0 }).unwrap();
        let first_lfo = ctl.model.free_modulators[0].node;
        ctl.apply(Intent::RemoveNode { node: first_lfo }).unwrap();
        graph.drain_commands();
        // The cable of the envelope to the rate of that LFO went with it, and what the LFO drove stays where the user put it.
        assert_eq!(detune(&ctl).1, 3.0);
        ctl.undo().unwrap();
        graph.drain_commands();
        let centre = |aim: Driven| graph.cable_bases().into_iter().find(|(_, _, _, a, _)| *a == aim).map(|(.., base)| base);
        assert_eq!(centre(Driven::Param { plugin: lane(&ctl), index: detune_index }), Some(3.0), "the value that was set when it went");
    }

    #[test]
    fn a_modulator_is_taken_out_after_the_cables_that_address_it_and_put_back_before_them_with_its_own_cables_after_it() {
        let (engine, mut ctl, _clock) = controller_over(load(MODULATORS).model);
        let (volume, _) = param(&ctl, NodePath::Instrument(0), "volume");
        let (envelope, output) = (ctl.model.free_modulators[1].node, ctl.model.free_modulators[1].output.unwrap());
        engine.drain();
        ctl.apply(Intent::RemoveNode { node: envelope }).unwrap();
        let (commands, slots) = engine.drain();
        // The second LFO's cables to the envelope's attack and to the depth of its first cable go first (the second is the first by then), then the envelope,
        // and what it drove has the value that was set again.
        assert_eq!(commands[..3], ["RemoveFreeModTarget mod=2 target=0", "RemoveFreeModTarget mod=2 target=0", "RemoveFreeModulator index=1"]);
        assert_eq!(commands.len(), 5, "{commands:?}");
        assert!(commands[3].starts_with(&format!("SetParameter inst=0 slot=0 param={volume} ")), "{commands:?}");
        assert_eq!(commands[4], "SetFreeModulatorRate mod=0 rate=1.000");
        assert!(slots.is_empty());

        ctl.undo().unwrap();
        let (commands, slots) = engine.drain();
        // The envelope, the place where it publishes, its own two cables in the order they had, and then the cables that went before it, the last first.
        assert_eq!(slots, [format!("SetModulatorOutput Free index=1 slot={}", output.index())]);
        assert_eq!(commands.len(), 6, "{commands:?}");
        assert!(commands[0].starts_with("InsertFreeModulator index=1 source=Envelope { attack: 0.02, decay: 0.4, sustain: 0.6, release: 0.8,"), "{commands:?}");
        assert_eq!(commands[1], format!("SetModulatorEar Free index=1 ear={}", ctl.model.instruments[0].node.0), "it hears the lane that it heard");
        assert!(commands[2].starts_with(&testutil::told(&ctl.model, &format!("AddFreeModTarget mod=1 kind=LaneParam {{ lane: 0, slot: 0, param_index: {volume} }}"))), "{commands:?}");
        assert!(commands[3].starts_with(&testutil::told(&ctl.model, "AddFreeModTarget mod=1 kind=ModulatorRate { mod_index: 0 }")), "{commands:?}");
        assert!(commands[4].starts_with(&testutil::told(&ctl.model, "InsertFreeModTarget mod=2 target=0 kind=ModulatorDepth { mod_index: 1, target_index: 0 }")), "{commands:?}");
        assert!(commands[5].starts_with(&testutil::told(&ctl.model, "InsertFreeModTarget mod=2 target=0 kind=ModulatorAttack { mod_index: 1 }")), "{commands:?}");
    }

    /// A lane with a step sequencer and an LFO after it.
    const SEQUENCER: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\n[[instrument.modulator]]\n";

    #[test]
    fn a_step_sequencer_that_is_taken_out_is_put_back_publishing_its_output_and_its_position_where_it_did() {
        let (engine, mut ctl, _clock) = controller_over(load(SEQUENCER).model);
        let sequencer = &ctl.model.free_modulators[0];
        let (node, output, position) = (sequencer.node, sequencer.output.unwrap(), sequencer.position.unwrap());
        engine.drain();
        ctl.apply(Intent::RemoveNode { node }).unwrap();
        engine.drain();
        ctl.undo().unwrap();
        let (_commands, slots) = engine.drain();
        assert_eq!(
            slots,
            [
                format!("SetModulatorOutput Free index=0 slot={}", output.index()),
                format!("SetModulatorPosition Free index=0 slot={}", position.index()),
            ]
        );
        assert_eq!(ctl.model.free_modulators[0].position, Some(position));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_sequencer_that_is_added_is_taken_out_by_undo_and_made_again_by_redo_with_slots_of_its_own() {
        let (engine, mut ctl, _clock) = controller_over(load(SEQUENCER).model);
        let lane = ctl.model.instruments[0].node;
        engine.drain();
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Steps, hears: Some(lane) }).unwrap();
        let first = &ctl.model.free_modulators[2];
        let (first_output, first_position) = (first.output.unwrap().index(), first.position.unwrap().index());
        assert_eq!(first_position, first_output + 1);
        let (_commands, slots) = engine.drain();
        assert_eq!(
            slots,
            [
                format!("SetModulatorOutput Free index=2 slot={first_output}"),
                format!("SetModulatorPosition Free index=2 slot={first_position}"),
            ]
        );
        ctl.undo().unwrap();
        engine.drain();
        ctl.redo().unwrap();
        let again = &ctl.model.free_modulators[2];
        let (output, position) = (again.output.unwrap().index(), again.position.unwrap().index());
        assert_eq!(position, output + 1);
        assert!(output > first_position, "the slots of what was taken out are not given again");
        let (_commands, slots) = engine.drain();
        assert_eq!(
            slots,
            [format!("SetModulatorOutput Free index=2 slot={output}"), format!("SetModulatorPosition Free index=2 slot={position}")]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_that_is_put_back_publishes_what_it_puts_out_into_the_slot_it_had() {
        use crate::plugin::chain::LfoWaveform;
        let (mut ctl, mut graph) = controller_over_an_audio_thread(MODULATORS);
        // The second LFO, a 3 Hz sine.
        let (node, slot) = (ctl.model.free_modulators[2].node, ctl.model.free_modulators[2].output.expect("it has a slot"));
        ctl.apply(Intent::RemoveNode { node }).unwrap();
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators[2].output, Some(slot), "the model has the slot it had");
        ctl.telemetry.store(slot, f32::NAN);
        graph.drain_commands();
        let mut out = vec![vec![0.0f32; 64]; 2];
        graph.process(&[], &mut out).unwrap();
        let published = ctl.telemetry.load(slot);
        let output = LfoWaveform::Sine.eval(3.0 * 64.0 / 48_000.0);
        assert!((published - output).abs() < 1e-3, "{published} for an LFO output of {output}");
    }

    #[test]
    fn a_modulator_that_is_there_already_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        for (what, own_cable) in [("the modulator", false), ("a cable of it", true)] {
            let (_engine, mut ctl, _clock) = controller_over(load(MODULATORS).model);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            // The control that is bound to CC 74, and its one cable.
            let (node, cable) = (ctl.model.free_modulators[4].node, &ctl.model.free_modulators[4].targets[0]);
            let copy = crate::app::model::ModTargetSlot {
                edge: cable.edge,
                param_name: cable.param_name.clone(),
                aim: cable.aim,
                depth: cable.depth,
                param_min: cable.param_min,
                param_max: cable.param_max,
                effective: cable.effective,
            };
            let edge = cable.edge;
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            // Something puts it there without the history (as a front-end that edits the model by itself could): the node, or another modulator with the cable.
            let other = ctl.model.alloc_node();
            let spare = if own_cable {
                crate::app::model::ModulatorSlot { node: other, source: ModSourceSlot::new_lfo(), targets: vec![copy], output: None, position: None, ear: None }
            } else {
                crate::app::model::ModulatorSlot { node, source: ModSourceSlot::new_lfo(), targets: Vec::new(), output: None, position: None, ear: None }
            };
            ctl.model.insert_modulator_at(crate::plugin::chain::ModulatorLocation::Free, 4, spare).unwrap();
            let before = picture(&ctl);
            let error = ctl.undo().unwrap_err();
            let reason = if own_cable { format!("cable {} is there already", edge.0) } else { format!("node {} is there already", node.0) };
            assert_eq!(error, IntentError::Invalid(reason), "{what}");
            assert_eq!(picture(&ctl), before, "{what}");
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "{what}: the step is gone, the one before it is next");
        }
    }

    #[test]
    fn a_modulator_that_was_taken_out_by_the_controller_and_not_by_an_intent_leaves_nothing_to_put_back_and_forgets_the_history() {
        let (_engine, mut ctl, _clock) = controller_over(load(MODULATORS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert!(ctl.remove_modulator(crate::plugin::chain::ModulatorLocation::Free, 4));
        assert!(ctl.journal.is_none(), "nothing is written down unless an intent is being carried out");
        // (The places of the others may have moved: a step that took something out would put it back by the places it had.)
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
    }

    #[test]
    fn a_modulator_that_was_added_and_taken_out_comes_back_under_its_id_and_what_was_done_to_it_goes_on_naming_it() {
        let (_engine, mut ctl, _clock) = controller();
        let (lane0, filter) = (lane(&ctl), node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        let (resonance, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        let start = picture(&ctl);
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) }).unwrap();
        let lfo = ctl.last_made().nodes[0];
        ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: filter, port: Port::Param { index: resonance } } }).unwrap();
        let edge = ctl.last_made().edges[0];
        let (with_cable, saved) = (picture(&ctl), written(&ctl));
        ctl.apply(Intent::RemoveNode { node: lfo }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove an LFO"));
        ctl.undo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (with_cable.clone(), saved));
        // What was done to it before is undone by the ids it has again: the cable is taken out by its id, and then the LFO by its.
        ctl.undo().unwrap();
        assert!(ctl.model.locate_edge(edge).is_none());
        ctl.undo().unwrap();
        assert_eq!(picture(&ctl), start);
        // And forwards: both are added again, under the ids they had (the telemetry slots they publish to are new), and the LFO is taken out again by the
        // id it has had all along.
        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!(picture(&ctl).0, with_cable.0);
        let again = picture(&ctl);
        ctl.redo().unwrap();
        assert!(ctl.model.locate(lfo).is_none());
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove an LFO"));
        ctl.undo().unwrap();
        assert_eq!(picture(&ctl), again);
        ctl.model.check_ids().unwrap();
    }

    // -- an effect that is taken out is put back --

    /// A group with a reverb and a filter on its bus, whose modulator has a cable to the filter and one to the third effect of its member's chain; a lane in it with
    /// a filter, a reverb (a mix of its own) and a filter, whose first modulator has cables to the first and third effect and to the oscillator and whose second drives
    /// the depth of the cable to the third; and a lane of its own with a reverb.
    const EFFECTS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 1
param = "cutoff"
depth = 0.2
[[group.modulator.target]]
member = 0
slot = 3
param = "resonance"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
mix = 0.25
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.1
[[instrument.modulator.target]]
slot = 3
param = "cutoff"
depth = 0.4
[[instrument.modulator.target]]
param = "detune"
depth = 0.5
[[instrument.modulator]]
rate = 3.0
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.6

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    /// What `everything_as_it_is` says: the modulators, and the chains of the audio thread (`AudioGraph::chain_dump`).
    type Everything = (Modulators, Vec<Vec<(String, usize, f32)>>, String);

    /// What is on the audio thread and in the model, as every place that says so tells it: the modulators (`modulators_as_they_are`) and the chains, which
    /// say where each plugin is in memory, so that a plugin that was taken out and put back is told from one that was loaded again.
    fn everything_as_it_is(ctl: &Controller, graph: &crate::plugin::chain::AudioGraph) -> Everything {
        (
            modulators_as_they_are(ctl, graph),
            graph.chain_dump(),
            format!("{:?}\n{:?}\n{:?}\n{:?}\n{:?}", graph.lanes(), graph.instrument_dump(), graph.remappers(), graph.groups(), graph.audio_cables()),
        )
    }

    fn assert_same_everything(now: &Everything, then: &Everything, what: &str) {
        assert_same_modulators(&now.0, &then.0, what);
        assert_eq!(now.1, then.1, "{what}: the chains of the audio thread (the very plugins, in their places, at their mixes)");
        assert_eq!(now.2, then.2, "{what}: the lanes of the audio thread, and where their instruments are");
    }

    /// The nodes of the effects of the session, a lane's and a bus's, in the order the model lists them.
    fn effect_nodes_of(ctl: &Controller) -> Vec<NodeId> {
        ctl.model.nodes().filter(|(_, path)| matches!(path, NodePath::Effect { .. } | NodePath::GroupEffect { .. })).map(|(id, _)| id).collect()
    }

    /// Every effect of `session`, one at a time (each in a controller of its own, after a step of another kind): taken out by an intent, put back by undo, taken
    /// out again by redo and put back again. Each time the model, the picture a client is given, the file and the audio thread are as they were, and the plugin
    /// that is back is the very one that went (the audio thread is run after each, as it runs on its own: it is what hands the plugin back). How many were put
    /// back, and how many were not (cables that drive each other's depths went with them).
    fn every_effect_is_put_back(name: &str, session: &str) -> (usize, usize) {
        let count = effect_nodes_of(&controller_over_an_audio_thread(session).0).len();
        let (mut put_back, mut forgotten) = (0, 0);
        for n in 0..count {
            let (mut ctl, mut graph) = controller_over_an_audio_thread(session);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let node = effect_nodes_of(&ctl)[n];
            let before = everything_as_it_is(&ctl, &graph);
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            graph.drain_commands();
            let taken = everything_as_it_is(&ctl, &graph);
            assert_ne!(taken.0 .0 .0, before.0 .0 .0, "effect {n} of {name}: it is gone from the picture");
            assert_ne!(taken.1, before.1, "effect {n} of {name}: it is gone from the audio thread");
            if ctl.history_state() == (HistoryState { undo: None, redo: None }) {
                assert!(ctl.undo().is_err(), "effect {n}: nothing to undo");
                forgotten += 1;
                continue;
            }
            assert!(undo_label(&ctl).is_some_and(|label| label.starts_with("remove ")), "effect {n}: {:?}", undo_label(&ctl));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("effect {n} of {name} put back"));
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &taken, &format!("effect {n} of {name} taken out again"));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("effect {n} of {name} put back again"));
            ctl.model.check_ids().unwrap();
            put_back += 1;
        }
        (put_back, forgotten)
    }

    #[test]
    fn every_effect_of_every_session_is_put_back_by_undo_as_the_very_plugin_in_the_model_the_picture_the_file_and_on_the_audio_thread() {
        let sessions = [
            ("EFFECTS", EFFECTS),
            ("LONG_CHAINS", testutil::LONG_CHAINS),
            ("CHAINS", testutil::CHAINS),
            ("GROUPS", testutil::GROUPS),
            ("LANES_IN_GROUPS", testutil::LANES_IN_GROUPS),
            ("CROSS_MOD_RACKS", testutil::CROSS_MOD_RACKS),
            ("FREE_MODULATORS", testutil::FREE_MODULATORS),
        ];
        let mut total = 0;
        for (name, session) in sessions {
            let (put_back, forgotten) = every_effect_is_put_back(name, session);
            assert_eq!(forgotten, 0, "{name}");
            total += put_back;
        }
        assert!(total > 20, "{total} effects is not enough to say much");
    }

    #[test]
    fn an_effect_is_a_step_in_the_words_of_its_plugin_and_a_bus_effect_too() {
        let (_engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        let (lane_filter, bus_reverb) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }));
        ctl.apply(Intent::RemoveNode { node: lane_filter }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Filter"));
        ctl.apply(Intent::RemoveNode { node: bus_reverb }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Reverb"));
        ctl.undo().unwrap();
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("remove Filter"), Some("remove Reverb")));
        ctl.undo().unwrap();
        assert_eq!(undo_label(&ctl), None);
    }

    #[test]
    fn an_effect_is_taken_out_after_the_cables_that_drive_it_with_a_channel_to_be_handed_to_and_put_back_before_them_and_the_places_of_the_others_are_told() {
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        // The lane's first modulator has cables to the first and the third effect and to the oscillator; the second drives the depth of the cable to the third; the
        // group has one to the third effect too. The third effect (a filter) goes.
        let third = node(&ctl, NodePath::Effect { inst: 0, index: 2 });
        engine.drain();
        ctl.apply(Intent::RemoveNode { node: third }).unwrap();
        let (commands, _) = engine.drain();
        // Its cables (the lane's own first, the one that drives the depth of a cable before the cable, then the group's), and then the effect with a channel; no
        // value is given back (it goes with the plugin).
        assert_eq!(
            commands,
            ["RemoveFreeModTarget mod=1 target=0", "RemoveFreeModTarget mod=0 target=1", "RemoveFreeModTarget mod=2 target=1", "RemoveEffect inst=0 index=2 keep"]
        );
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        // The stub engine hands nothing back, so the plugin is loaded again (the audio thread would hand the very one): it goes in at its place with its mix, and
        // then the cables, the last that went first, each with what it was centred on.
        assert_eq!(commands.len(), 4, "{commands:?}");
        assert!(commands[0].starts_with("InsertEffect inst=0 index=2 plugin=Filter mix="), "{commands:?}");
        assert!(commands[1].starts_with(&format!("InsertFreeModTarget mod=2 target=1 aim=Param {{ plugin: {third:?}, ")), "{commands:?}");
        assert!(commands[2].starts_with(&format!("InsertFreeModTarget mod=0 target=1 aim=Param {{ plugin: {third:?}, ")), "{commands:?}");
        assert!(commands[3].starts_with(&testutil::told(&ctl.model, "InsertFreeModTarget mod=1 target=0 kind=ModulatorDepth { mod_index: 0, target_index: 1 }")), "{commands:?}");
    }

    #[test]
    fn a_bus_effect_is_taken_out_and_put_back_by_the_groups_commands() {
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        let bus_filter = node(&ctl, NodePath::GroupEffect { group: 0, index: 1 });
        engine.drain();
        ctl.apply(Intent::RemoveNode { node: bus_filter }).unwrap();
        let (commands, _) = engine.drain();
        assert_eq!(commands, ["RemoveFreeModTarget mod=2 target=0", "RemoveGroupEffect group=0 index=1 keep"]);
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        assert_eq!(commands.len(), 2, "{commands:?}");
        assert!(commands[0].starts_with("InsertGroupEffect group=0 index=1 plugin=Filter mix="), "{commands:?}");
        assert!(commands[1].starts_with(&format!("InsertFreeModTarget mod=2 target=0 aim=Param {{ plugin: {bus_filter:?}, ")), "{commands:?}");
    }

    // -- audio cables (P3c of docs/patch-graph.md §11.6) --

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioOut }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioIn }
    }

    fn draw(ctl: &mut Controller, from: Endpoint, to: Endpoint, gain: f32) -> EdgeId {
        ctl.apply(Intent::AddCable { from, to, gain: Some(gain) }).unwrap();
        ctl.model.cables.iter().map(|cable| cable.id).max().unwrap()
    }

    #[test]
    fn an_audio_cable_that_was_drawn_is_taken_out_by_undo_and_put_back_by_redo_under_the_id_and_at_the_level_it_had() {
        let (engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        sent(&engine);
        let id = draw(&mut ctl, out(n.lane[1]), into(n.fx[0]), 0.25);
        assert_eq!(ctl.last_made().edges, [id]);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("draw an audio cable"), None));
        let drawn = format!("AddCable id={} from=Instrument({:?}) to=Effect({:?}) gain=0.250", id.0, n.lane[1], n.fx[0]);
        assert_eq!(sent(&engine), std::slice::from_ref(&drawn));

        let undone = ctl.undo().unwrap();
        assert!(undone.restructured);
        assert!(ctl.model.cables.is_empty());
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", id.0)]);
        assert_eq!((undo_label(&ctl), redo_label(&ctl).as_deref()), (None, Some("draw an audio cable")));

        let redone = ctl.redo().unwrap();
        assert!(redone.restructured);
        assert_eq!(ctl.model.cables.iter().map(|cable| (cable.id, cable.gain)).collect::<Vec<_>>(), [(id, 0.25)], "the cable that comes back has the id and the level it had");
        assert_eq!(sent(&engine), [drawn]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_audio_cable_that_was_taken_out_is_put_back_by_undo_as_it_was_and_taken_out_again_by_redo() {
        let (engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let first = draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), 0.5);
        let second = draw(&mut ctl, out(n.fx[2]), into(n.bus), 2.0);
        let third = draw(&mut ctl, out(n.lane[1]), into(n.fx[0]), 1.0);
        let before = ctl.model.cables.clone();
        sent(&engine);
        ctl.apply(Intent::RemoveEdge { edge: second }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove an audio cable"));
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", second.0)]);
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [first, third]);
        let undone = ctl.undo().unwrap();
        assert!(undone.restructured);
        assert_eq!(ctl.model.cables, before, "the cable is back among the cables, as it was");
        assert_eq!(sent(&engine), [format!("AddCable id={} from=Effect({:?}) to=Effect({:?}) gain=2.000", second.0, n.fx[2], n.bus)]);
        ctl.redo().unwrap();
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [first, third]);
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", second.0)]);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.cables, before);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_level_of_a_cable_is_a_step_that_a_drag_makes_one_of_and_undo_gives_the_level_it_had() {
        let (engine, mut ctl, clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let edge = draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), 0.5);
        let other = draw(&mut ctl, out(n.lane[1]), into(NodeId::MASTER), 0.5);
        sent(&engine);
        let level = |ctl: &Controller, edge| ctl.model.audio_cable(edge).unwrap().gain;
        // A drag: three changes a moment apart are one step, which puts back what the first found.
        for gain in [0.6, 0.7, 0.8] {
            ctl.apply(Intent::SetCableGain { edge, gain }).unwrap();
            clock.advance(ms(100));
        }
        assert_eq!(undo_label(&ctl).as_deref(), Some("set the level of an audio cable"));
        ctl.undo().unwrap();
        assert_eq!(level(&ctl, edge), 0.5);
        assert_eq!(sent(&engine).last().unwrap(), &format!("SetCableGain id={} gain=0.500", edge.0));
        assert_eq!(undo_label(&ctl).as_deref(), Some("draw an audio cable"), "that was the one step");
        ctl.redo().unwrap();
        assert_eq!(level(&ctl, edge), 0.8, "and redo does the last of the changes");
        // The level of another cable is a step of its own, and so is a change a long while after.
        ctl.apply(Intent::SetCableGain { edge: other, gain: 1.5 }).unwrap();
        clock.advance(ms(100));
        ctl.apply(Intent::SetCableGain { edge, gain: 0.9 }).unwrap();
        clock.advance(SAME_STEP + ms(1));
        ctl.apply(Intent::SetCableGain { edge, gain: 1.0 }).unwrap();
        ctl.undo().unwrap();
        assert_eq!((level(&ctl, edge), level(&ctl, other)), (0.9, 1.5));
        ctl.undo().unwrap();
        assert_eq!((level(&ctl, edge), level(&ctl, other)), (0.8, 1.5));
        ctl.undo().unwrap();
        assert_eq!((level(&ctl, edge), level(&ctl, other)), (0.8, 0.5));
    }

    #[test]
    fn a_set_level_that_changes_nothing_or_is_refused_leaves_no_step() {
        let (_engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let edge = draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), 0.5);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        ctl.apply(Intent::SetCableGain { edge, gain: f32::NAN }).unwrap_err();
        ctl.apply(Intent::SetCableGain { edge: EdgeId(999), gain: 0.5 }).unwrap_err();
        assert_eq!(undo_label(&ctl).as_deref(), Some("lock the piano to its scale"));
        // (The plan is no step either: a cable that is not there has no level to put back, whichever cable there is.)
        assert!(matches!(ctl.plan_history(&Intent::SetCableGain { edge: EdgeId(999), gain: 0.5 }), Plan::Nothing));
        assert!(matches!(ctl.plan_history(&Intent::SetCableGain { edge, gain: 0.7 }), Plan::Step(_)));
    }

    /// Cables that make a wiring of every kind of end in `SENDS`, with the levels they are drawn at.
    fn draw_the_sends(ctl: &mut Controller) -> Vec<EdgeId> {
        let n = Sends::of(&ctl.model);
        [
            (out(n.lane[0]), into(NodeId::MASTER), 0.5),
            (out(n.lane[1]), into(n.fx[0]), 1.0),
            (out(n.fx[0]), into(n.group), 0.25),
            (out(n.fx[1]), into(n.fx[2]), 2.0),
            (out(n.group), into(n.fx[2]), 0.75),
            (out(n.bus), into(NodeId::MASTER), 1.5),
        ]
        .into_iter()
        .map(|(from, to, gain)| draw(ctl, from, to, gain))
        .collect()
    }

    #[test]
    fn what_goes_with_a_node_that_has_audio_cables_comes_back_with_them_by_undo_in_the_model_the_picture_and_on_the_audio_thread() {
        // Each of what can be taken out, in a controller of its own: the lanes, the group, the effects of the lanes and the bus effect.
        let nodes = |ctl: &Controller| -> Vec<(String, NodeId)> {
            let n = Sends::of(&ctl.model);
            vec![
                ("lane 0".to_string(), n.lane[0]),
                ("lane 1".to_string(), n.lane[1]),
                ("the group".to_string(), n.group),
                ("lane 0's filter".to_string(), n.fx[0]),
                ("lane 0's delay".to_string(), n.fx[1]),
                ("lane 1's drive".to_string(), n.fx[2]),
                ("the group's reverb".to_string(), n.bus),
            ]
        };
        let count = nodes(&controller_over_an_audio_thread(SENDS).0).len();
        for at in 0..count {
            let (mut ctl, mut graph) = controller_over_an_audio_thread(SENDS);
            draw_the_sends(&mut ctl);
            graph.drain_commands();
            let (what, node) = nodes(&ctl).swap_remove(at);
            let before = everything_as_it_is(&ctl, &graph);
            let cables_before = ctl.model.cables.clone();
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            graph.drain_commands();
            let taken = everything_as_it_is(&ctl, &graph);
            assert!(ctl.model.cables.len() < cables_before.len(), "{what}: cables went with it");
            assert_eq!(graph.audio_cables(), testutil::model_audio_cables(&ctl.model), "{what}: taken out");
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_eq!(ctl.model.cables, cables_before, "{what}: the cables are back, as they were");
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("{what} put back"));
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &taken, &format!("{what} taken out again"));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_eq!(ctl.model.cables, cables_before, "{what}: and back again");
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("{what} put back again"));
            ctl.model.check_ids().unwrap();
            // The audio thread can make a plan of it, and the cables are heard at what they were (a loop would assert).
            let mut block = vec![vec![0.0f32; 64]; 2];
            graph.process(&[], &mut block).unwrap();
        }
    }

    #[test]
    fn an_effect_is_taken_out_after_the_audio_cables_at_it_and_put_back_before_them() {
        let (engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let into_filter = draw(&mut ctl, out(n.lane[1]), into(n.fx[0]), 1.0);
        let from_filter = draw(&mut ctl, out(n.fx[0]), into(NodeId::MASTER), 0.5);
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: n.fx[0] }).unwrap();
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", into_filter.0), format!("RemoveCable id={}", from_filter.0), "RemoveEffect inst=0 index=0 keep".to_string()]);
        ctl.undo().unwrap();
        let commands = sent(&engine);
        // The effect goes in first and then the cables, the last that went first.
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert!(commands[0].starts_with("InsertEffect inst=0 index=0 plugin=Filter mix="), "{commands:?}");
        assert_eq!(commands[1], format!("AddCable id={} from=Effect({:?}) to=Master gain=0.500", from_filter.0, n.fx[0]));
        assert_eq!(commands[2], format!("AddCable id={} from=Instrument({:?}) to=Effect({:?}) gain=1.000", into_filter.0, n.lane[1], n.fx[0]));
    }

    #[test]
    fn an_audio_cable_whose_id_is_in_use_is_refused_with_the_reason_and_the_step_is_dropped() {
        let (_engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let id = draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), 1.0);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        ctl.apply(Intent::RemoveEdge { edge: id }).unwrap();
        // Something draws a cable under that id without the history.
        ctl.model.insert_audio_cable(crate::app::model::AudioCable { id, from: CableFrom::Instrument(n.lane[1]), to: CableTo::Master, gain: 1.0 });
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("cable {} is there already", id.0)));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None));
    }

    #[test]
    fn a_cable_that_is_drawn_again_by_redo_where_its_id_is_in_use_is_refused() {
        let (_engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let id = draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), 1.0);
        ctl.undo().unwrap();
        ctl.model.insert_audio_cable(crate::app::model::AudioCable { id, from: CableFrom::Instrument(n.lane[1]), to: CableTo::Master, gain: 1.0 });
        assert_eq!(ctl.redo().unwrap_err(), IntentError::Invalid(format!("cable {} is there already", id.0)));
    }

    #[test]
    fn an_audio_cable_that_was_taken_out_by_the_controller_and_not_by_an_intent_leaves_nothing_to_put_back_and_forgets_the_history() {
        let (engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        let edge = draw(&mut ctl, out(n.lane[0]), into(NodeId::MASTER), 0.5);
        assert!(undo_label(&ctl).is_some(), "there is something to forget");
        sent(&engine);
        assert!(ctl.remove_edge(edge));
        assert!(ctl.journal.is_none(), "nothing is written down unless an intent is being carried out");
        assert_eq!(sent(&engine), [format!("RemoveCable id={}", edge.0)]);
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None });
        // One that is not there changes nothing, the history included.
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert!(!ctl.remove_edge(edge));
        assert_eq!(undo_label(&ctl).as_deref(), Some("lock the piano to its scale"));
    }

    #[test]
    fn a_cable_that_the_wiring_cannot_take_when_its_removal_is_undone_is_not_put_back_and_the_rest_of_the_undo_is_done() {
        let (engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        // The delay sends into the drive, and the cable is taken out.
        let cable = draw(&mut ctl, out(n.fx[1]), into(n.fx[2]), 1.0);
        ctl.apply(Intent::RemoveEdge { edge: cable }).unwrap();
        // Something draws a cable from the drive back to the filter, which is before the delay, without the history: the first cable would now close the circle.
        let back = EdgeId(900);
        ctl.model.insert_audio_cable(crate::app::model::AudioCable { id: back, from: CableFrom::Effect(n.fx[2]), to: CableTo::Effect(n.fx[0]), gain: 1.0 });
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.cables.iter().map(|cable| cable.id).collect::<Vec<_>>(), [back], "the cable that cannot be had is not put back");
        assert!(sent(&engine).is_empty(), "and the audio thread is told nothing of it");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("draw an audio cable"), Some("remove an audio cable")), "the step is undone all the same");
    }

    #[test]
    fn a_cable_of_modulation_whose_id_an_audio_cable_has_is_refused_when_its_removal_is_undone_with_the_reason_and_the_step_is_dropped() {
        let (_engine, mut ctl, clock) = controller_over(load(CABLES).model);
        set_detune(&mut ctl, 1.0);
        clock.advance(Duration::from_secs(5));
        let volume = cable_edge(&ctl, 0, 1);
        ctl.apply(Intent::RemoveEdge { edge: volume }).unwrap();
        // Something draws an audio cable under that id without the history.
        let lane = ctl.model.instruments[0].node;
        ctl.model.insert_audio_cable(crate::app::model::AudioCable { id: volume, from: CableFrom::Instrument(lane), to: CableTo::Master, gain: 1.0 });
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("cable {} is there already", volume.0)));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("set detune"), None), "the step is gone, the one before it is the next");
    }

    #[test]
    fn a_group_made_of_lanes_and_a_lane_put_in_a_group_are_steps_that_the_cables_do_not_disturb() {
        let (_engine, mut ctl, _clock) = controller_over(load(SENDS).model);
        let n = Sends::of(&ctl.model);
        draw(&mut ctl, out(n.lane[1]), into(n.fx[0]), 1.0);
        ctl.apply(Intent::SetLaneGroup { node: n.lane[1], group: Some(n.group) }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("put a lane in a group"));
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments[1].group, None);
        assert_eq!(ctl.model.cables.len(), 1, "the cable is where it was");
    }

    #[test]
    fn a_plugin_that_the_audio_thread_handed_back_is_put_back_as_it_is_and_one_that_it_did_not_is_loaded_again_with_what_was_set() {
        // Handed back: the very plugin, though the model could not load another (the id it has is of nothing that loads).
        let (mut ctl, mut graph) = controller_over_an_audio_thread(EFFECTS);
        ctl.model.instruments[0].effects[1].id = "builtin:nothing".to_string();
        let before = graph.chain_dump();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        graph.drain_commands();
        ctl.undo().unwrap();
        graph.drain_commands();
        assert_eq!(graph.chain_dump(), before, "the same plugin, in the same place");

        // Not handed back (nobody runs the audio thread): the plugin is loaded again, with the values the model has, which are what the user set.
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 0, index: 1 }, "damping");
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.8 }).unwrap();
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        engine.drain();
        ctl.undo().unwrap();
        let loaded = engine.cmd_rx.try_iter().find_map(|command| match command {
            crate::plugin::chain::GraphCommand::InsertEffect { effect, .. } => Some(effect),
            _ => None,
        });
        let mut loaded = loaded.expect("the plugin goes in again");
        assert_eq!(loaded.get_parameter(damping), Some(0.8), "with what the user had set");
    }

    #[test]
    fn a_plugin_that_a_running_audio_thread_hands_back_across_threads_is_put_back_at_once_and_not_loaded_again() {
        use crate::audio::{AudioEngine, NULL_DEVICE};
        use crate::plugin::chain::AudioGraph;
        // A real audio thread (the null backend renders on a timer) and a plugin that cannot be loaded again: undo has the plugin if the audio thread handed it
        // back, and fails after the wait (five seconds here) if it did not.
        let (mut handles, engine) = testutil::stub_engine_of(4096);
        handles.recovery_wait = Duration::from_secs(5);
        let model = testutil::load_onto(EFFECTS, &handles.cmd_tx);
        let graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut audio = AudioEngine::build(graph, engine.midi_rx.clone(), Some(NULL_DEVICE), false, 48_000, 256).unwrap();
        let mut ctl = Controller::new(model, handles, None);
        audio.play().unwrap();
        ctl.model.instruments[0].effects[1].id = "builtin:nothing".to_string();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        let started = Instant::now();
        let undone = ctl.undo();
        let waited = started.elapsed();
        audio.stop();
        undone.expect("the plugin came back from the audio thread");
        assert!(waited < Duration::from_secs(2), "{waited:?}: it did not wait for what was there");
        assert!(ctl.model.locate(reverb).is_some());
    }

    #[test]
    fn a_plugin_that_cannot_be_loaded_again_refuses_the_undo_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (_engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        ctl.model.instruments[0].effects[1].id = "builtin:nothing".to_string();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid("Reverb could not be loaded".to_string()));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step is gone, the one before it is next");
    }

    #[test]
    fn a_plugin_that_is_handed_back_with_outputs_that_a_chain_has_no_place_for_refuses_the_undo_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let reverb = node(&ctl, NodePath::Effect { inst: 1, index: 0 });
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        // The audio thread, by hand: what it hands back where the reverb was has one audio output.
        for command in engine.cmd_rx.try_iter() {
            if let crate::plugin::chain::GraphCommand::RemoveEffect { keep: Some(keep), .. } = command {
                keep.send(Box::new(testutil::MonoEffect)).unwrap();
            }
        }
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid("Reverb has 1 audio output, and an effect here needs 2".to_string()));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None));
    }

    #[test]
    fn a_plugin_that_is_loaded_again_has_its_preset_and_over_it_what_the_user_set() {
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        let reverb = node(&ctl, NodePath::Effect { inst: 1, index: 0 });
        ctl.apply(Intent::LoadPreset { node: reverb, preset: "cathedral".to_string() }).unwrap();
        let (damping, preset_damping) = param(&ctl, NodePath::Effect { inst: 1, index: 0 }, "damping");
        let (size, preset_size) = param(&ctl, NodePath::Effect { inst: 1, index: 0 }, "size");
        ctl.apply(Intent::SetParam { node: reverb, param: damping, value: 0.1 }).unwrap();
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        engine.drain();
        ctl.undo().unwrap();
        let loaded = engine.cmd_rx.try_iter().find_map(|command| match command {
            crate::plugin::chain::GraphCommand::InsertEffect { effect, .. } => Some(effect),
            _ => None,
        });
        let mut loaded = loaded.expect("the plugin goes in again");
        // The user's value is over the preset's, which is another, and what the user left alone is the preset's.
        assert_ne!(preset_damping, 0.1);
        assert_eq!(loaded.get_parameter(damping), Some(0.1));
        assert_eq!(loaded.get_parameter(size), Some(preset_size));
        assert_eq!(ctl.model.instruments[1].effects[0].current_preset.as_deref(), Some("Cathedral"));
    }

    #[test]
    fn an_effect_put_back_past_the_end_of_its_chain_goes_in_last_and_the_audio_thread_is_told_the_place_it_went_to() {
        use crate::app::controller::ChainAt;
        use crate::app::journal::{Inverse, Journal, Kept};
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        let (mut plugin, mut slot) = ctl.load_plugin("builtin:reverb").unwrap();
        slot.node = NodeId(9_999);
        // The plugin in hand has a state that the slot does not say: it is the one that goes in.
        let (param, default) = (slot.params[0].index, slot.params[0].value);
        assert_ne!(default, 0.3);
        plugin.set_parameter(param, 0.3).unwrap();
        engine.drain();
        // (A journal that a session which changed without the history can make: the chain is shorter than it was.)
        let mut journal = Journal::default();
        journal.push(Inverse::Effect { at: ChainAt::Lane(1), index: 99, slot, kept: Kept::InHand(plugin) });
        ctl.rewind(journal).unwrap();
        let chain: Vec<_> = ctl.model.instruments[1].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(chain, ["Reverb", "Reverb"]);
        let mut commands: Vec<_> = engine.cmd_rx.try_iter().collect();
        let [crate::plugin::chain::GraphCommand::InsertEffect { inst, index, effect, .. }] = &mut commands[..] else {
            panic!("one plugin goes in, and nothing else is sent");
        };
        assert_eq!((*inst, *index), (1, 1));
        assert_eq!(effect.get_parameter(param), Some(0.3), "the plugin that was in hand, and not one that is loaded again");
    }

    #[test]
    fn what_an_add_that_is_undone_took_out_is_not_held_on_to() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(EFFECTS);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let lane = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::AddEffect { parent: lane, plugin: "builtin:filter".to_string() }).unwrap();
        graph.drain_commands();
        ctl.undo().unwrap();
        // (The effect that was added is out of the lane again, and the audio thread has it for the channel that was made for it; nothing of the controller's keeps that
        // channel, since redo adds a plugin again and no step puts this one back.)
        assert_eq!(ctl.model.instruments[1].effects.len(), 1);
        assert_eq!(ctl.taken_last.plugins_kept(), 0);
    }

    #[test]
    fn an_effect_that_is_there_already_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (_engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let reverb = node(&ctl, NodePath::Effect { inst: 1, index: 0 });
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        // Something puts an effect there without the history: one with that node (the one that went, loaded as the TUI's tests do).
        let (_plugin, mut spare) = ctl.load_plugin("builtin:reverb").unwrap();
        spare.node = reverb;
        ctl.model.insert_effect_at(1, 0, spare).unwrap();
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("node {} is there already", reverb.0)));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None));
    }

    #[test]
    fn an_effect_that_was_taken_out_by_the_controller_and_not_by_an_intent_is_handed_to_no_channel() {
        let (engine, mut ctl, _clock) = controller_over(load(EFFECTS).model);
        engine.drain();
        assert!(ctl.remove_effect(1, 0));
        assert!(ctl.remove_bus_effect(0, 0));
        assert_eq!(engine.drain().0, ["RemoveEffect inst=1 index=0", "RemoveGroupEffect group=0 index=0"]);
    }

    /// A lane with ten reverbs.
    fn ten_effects() -> String {
        let mut session = "[[instrument]]\nplugin = \"builtin:osc\"\n".to_string();
        for _ in 0..10 {
            session.push_str("[[instrument.effect]]\nplugin = \"builtin:reverb\"\n");
        }
        session
    }

    #[test]
    fn the_plugins_of_the_newest_removals_are_kept_and_the_one_of_an_older_removal_is_loaded_again() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(&ten_effects());
        // The plugins have a state that the model does not know, as a plugin has apart from its parameters: one that comes back from the audio thread has it, one
        // that is loaded again has not.
        let state = |i: usize| 0.1 + 0.01 * i as f32;
        for i in 0..10 {
            graph.poke_effect(0, i, 0, state(i));
        }
        // The last nine are taken out, one after the other (the last first), each a step of its own, and put back.
        for node in effect_nodes_of(&ctl)[1..].iter().rev() {
            ctl.apply(Intent::RemoveNode { node: *node }).unwrap();
            graph.drain_commands();
        }
        assert_eq!(graph.chain_dump()[0].len(), 1);
        for _ in 0..9 {
            ctl.undo().unwrap();
            graph.drain_commands();
        }
        let kept: Vec<bool> = (0..10).map(|i| (graph.peek_effect(0, i, 0).unwrap() - state(i)).abs() < 1e-6).collect();
        // The one that never went, and the newest eight that did, are the plugins they were; the oldest removal let go of its plugin when the ninth was made.
        assert_eq!(kept, [true, true, true, true, true, true, true, true, true, false]);

        // And again by redo, which takes each plugin out anew (every effect has a state of its own now): the newest eight are kept again, and the oldest is let go of
        // when the ninth is done again.
        let state_again = |i: usize| 0.5 + 0.01 * i as f32;
        for i in 0..10 {
            graph.poke_effect(0, i, 0, state_again(i));
        }
        for _ in 0..9 {
            ctl.redo().unwrap();
            graph.drain_commands();
        }
        assert_eq!(graph.chain_dump()[0].len(), 1);
        for _ in 0..9 {
            ctl.undo().unwrap();
            graph.drain_commands();
        }
        let kept: Vec<bool> = (0..10).map(|i| (graph.peek_effect(0, i, 0).unwrap() - state_again(i)).abs() < 1e-6).collect();
        assert_eq!(kept, [true, true, true, true, true, true, true, true, true, false]);
    }

    #[test]
    fn what_a_removal_holds_is_dropped_after_the_devices_and_before_what_the_runtime_is_followed_by_when_the_controller_shuts_down() {
        use crate::app::controller::{ChainAt, DeviceWatcher};
        use crate::app::journal::{Inverse, Journal, Kept};
        use crate::app::subscribers::EventSink;
        type Order = Arc<Mutex<Vec<&'static str>>>;
        /// Says that it is dropped, and in which order.
        struct Says(Order, &'static str);
        impl Drop for Says {
            fn drop(&mut self) {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).push(self.1);
            }
        }
        struct Watcher {
            _says: Says,
        }
        impl DeviceWatcher for Watcher {
            fn poll_new_devices(&mut self) -> crate::midi::DeviceChanges {
                crate::midi::DeviceChanges::default()
            }

            fn sources_of(&self, _: &str) -> crate::source::SourceSet {
                crate::source::SourceSet::NONE
            }

            fn open_names(&self) -> Vec<String> {
                Vec::new()
            }
        }
        struct Sink {
            _says: Says,
        }
        impl EventSink for Sink {
            fn applied(&mut self, _: &Applied) {}
        }
        let order: Order = Arc::new(Mutex::new(Vec::new()));
        let (_engine, mut ctl, _clock) = controller();
        ctl.watch_devices(Box::new(Watcher { _says: Says(order.clone(), "devices closed") }));
        // (The controller's fields are dropped in the order they are declared in, the plugin runtime before the fields after it, of which this is one: a plugin has to be
        // dropped while the libraries it came from are there, so what a removal holds is let go of before any of them.)
        ctl.subscribe(Box::new(Sink { _says: Says(order.clone(), "subscribers dropped") }));
        // A removal that holds a plugin, as an intent that took an effect out would have it.
        let removal = |ctl: &mut Controller, said: &'static str| {
            let mut journal = Journal::default();
            let (_plugin, reverb) = ctl.load_plugin("builtin:reverb").unwrap();
            let order = order.clone();
            let probe = crate::app::testutil::DropProbe::new(move |_| order.lock().unwrap_or_else(|e| e.into_inner()).push(said));
            journal.push(Inverse::Effect { at: ChainAt::Lane(0), index: 0, slot: reverb, kept: Kept::InHand(Box::new(probe)) });
            journal
        };
        // One is a step of the history, and the one that was carried out last is held until the next intent.
        let in_a_step = removal(&mut ctl, "the plugin of a step dropped");
        ctl.history.record(Step::removed("remove Reverb", Intent::Undo, in_a_step, Instant::now()));
        ctl.taken_last = removal(&mut ctl, "the plugin of the last removal dropped");
        ctl.shutdown();
        let order = order.lock().unwrap().clone();
        assert_eq!(order.first(), Some(&"devices closed"), "{order:?}");
        assert_eq!(order.last(), Some(&"subscribers dropped"), "{order:?}");
        let mut plugins = order[1..order.len() - 1].to_vec();
        plugins.sort_unstable();
        assert_eq!(plugins, ["the plugin of a step dropped", "the plugin of the last removal dropped"], "{order:?}");
    }

    // -- a lane that is taken out is put back --

    /// A group of two lanes (the first and the third) with a reverb on its bus, whose modulator has cables to the first member's detune, to the second member's filter and its
    /// modulator's rate, and to the bus; the first lane has a filter and a modulator that drives it and its own volume; the second, in no group, a reverb, a filter and an
    /// envelope that drives the filter; the third a filter and a modulator; and a fourth lane has no instrument, a reverb and a modulator that drives it.
    /// Two groups, one muted and one soloed, and lanes that are muted, soloed, both and neither, in the groups and out of them.
    const MUTED_AND_SOLOED: &str = r#"
[[group]]
name = "Pad"
mute = true
[[group]]
solo = true

[[instrument]]
plugin = "builtin:osc"
group = 0
mute = true
[[instrument]]
plugin = "builtin:osc"
group = 1
solo = true
[[instrument]]
plugin = "builtin:osc"
mute = true
solo = true
[[instrument]]
plugin = "builtin:osc"
"#;

    const LANES: &str = r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1
[[group.modulator.target]]
member = 1
slot = 1
param = "cutoff"
depth = 0.2
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.3
[[group.modulator.target]]
member = 1
mod_rate = 0
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
group = 0
volume = 0.5
range = "C2-C6"
transpose = 3
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5
[[instrument.modulator.target]]
param = "volume"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
mix = 0.25
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "envelope"
attack = 0.1
decay = 0.2
sustain = 0.3
release = 0.4
[[instrument.modulator.target]]
slot = 2
param = "resonance"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 3.0

[[instrument]]
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 4.0
[[instrument.modulator.target]]
slot = 1
param = "damping"
depth = 0.6
"#;

    /// Every lane of `session`, one at a time (each in a controller of its own, after a step of another kind): taken out by an intent, put back by undo, taken out again by
    /// redo and put back again. Each time the model, the picture a client is given, the file and the audio thread are as they were, and the plugins that are back are the
    /// very ones that went (the audio thread is run after each, as it runs on its own: it is what hands the lane back). How many were put back, and how many were not.
    fn every_lane_is_put_back(name: &str, session: &str) -> (usize, usize) {
        let count = controller_over_an_audio_thread(session).0.model.instruments.len();
        let (mut put_back, mut forgotten) = (0, 0);
        for n in 0..count {
            let (mut ctl, mut graph) = controller_over_an_audio_thread(session);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let lane = ctl.model.instruments[n].node;
            let before = everything_as_it_is(&ctl, &graph);
            if count == 1 {
                // (A session keeps its last lane.)
                assert!(ctl.apply(Intent::RemoveNode { node: lane }).is_err());
                continue;
            }
            ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
            graph.drain_commands();
            let taken = everything_as_it_is(&ctl, &graph);
            assert_ne!(taken.0 .0 .0, before.0 .0 .0, "lane {n} of {name}: it is gone from the picture");
            assert_ne!(taken.2, before.2, "lane {n} of {name}: it is gone from the audio thread");
            if ctl.history_state() == (HistoryState { undo: None, redo: None }) {
                assert!(ctl.undo().is_err(), "lane {n}: nothing to undo");
                forgotten += 1;
                continue;
            }
            assert!(undo_label(&ctl).is_some_and(|label| label.starts_with("remove ")), "lane {n}: {:?}", undo_label(&ctl));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("lane {n} of {name} put back"));
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &taken, &format!("lane {n} of {name} taken out again"));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("lane {n} of {name} put back again"));
            ctl.model.check_ids().unwrap();
            put_back += 1;
        }
        (put_back, forgotten)
    }

    #[test]
    fn every_lane_of_every_session_is_put_back_by_undo_as_the_very_lane_in_the_model_the_picture_the_file_and_on_the_audio_thread() {
        let sessions = [
            ("MUTED_AND_SOLOED", MUTED_AND_SOLOED),
            ("LANES", LANES),
            ("SESSION", SESSION),
            ("EFFECTS", EFFECTS),
            ("LONG_CHAINS", testutil::LONG_CHAINS),
            ("CHAINS", testutil::CHAINS),
            ("GROUPS", testutil::GROUPS),
            ("LANES_IN_GROUPS", testutil::LANES_IN_GROUPS),
            ("CROSS_MOD_RACKS", testutil::CROSS_MOD_RACKS),
            ("SOURCES", testutil::SOURCES),
            ("FREE_MODULATORS", testutil::FREE_MODULATORS),
        ];
        let mut total = 0;
        for (name, session) in sessions {
            let (put_back, forgotten) = every_lane_is_put_back(name, session);
            assert_eq!(forgotten, 0, "{name}");
            total += put_back;
        }
        assert!(total > 20, "{total} lanes is not enough to say much");
    }

    #[test]
    fn a_lane_is_a_step_in_the_words_of_its_instrument_or_of_what_it_holds_when_it_has_none() {
        let (_engine, mut ctl, _clock) = controller_over(load(LANES).model);
        let (first, holder) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Instrument(3)));
        // (The last lane has no instrument and a reverb, and is called by the reverb that it holds: step 87.)
        assert_eq!(ctl.model.instruments[3].instrument.as_ref().map(|plugin| plugin.name.as_str()), None);
        ctl.apply(Intent::RemoveNode { node: holder }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Reverb"));
        ctl.apply(Intent::RemoveNode { node: first }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove the lane of Oscillator"));
        // With the reverb taken out first, there is nothing in it to call it by.
        let (_engine, mut ctl, _clock) = controller_over(load(LANES).model);
        let (reverb, holder) = (node(&ctl, NodePath::Effect { inst: 3, index: 0 }), node(&ctl, NodePath::Instrument(3)));
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        ctl.apply(Intent::RemoveNode { node: holder }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove an empty lane"));
    }

    #[test]
    fn a_lane_that_has_cables_of_notes_is_taken_out_with_them_and_undo_puts_them_back_under_their_ids_and_tells_the_lane_what_it_hears() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let cables = |ctl: &Controller| ctl.model.event_cables.iter().map(|cable| (cable.id.0, cable.from, cable.to)).collect::<Vec<_>>();
        let before = cables(&ctl);
        let lane = node(&ctl, NodePath::Instrument(1));
        engine.drain();
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        // The two cables that ended at the lane went with it (and the cable of the lane that stays is where it was); what the others hear is not changed.
        assert_eq!(cables(&ctl), [before[0]]);
        assert_eq!(engine.drain().0, ["RemoveInstrument inst=1 keep"]);
        // Undo: the lane, then its cables, under the ids they had; the lane that is back is told what it hears (which it had when it went, and is not told otherwise).
        ctl.undo().unwrap();
        assert_eq!(cables(&ctl), before);
        let (commands, _) = engine.drain();
        assert!(commands.contains(&"InsertInstrument index=1 plugins=1".to_string()), "{commands:?}");
        assert!(commands.contains(&"SetLaneSources inst=1 sources=[]".to_string()), "{commands:?}");
        ctl.model.check_ids().unwrap();
        // Redo takes them out again, and undo again puts them back (the audio thread, by hand, hands the lane back).
        ctl.redo().unwrap();
        assert_eq!(cables(&ctl), [before[0]]);
        engine.drain();
        ctl.undo().unwrap();
        assert_eq!(cables(&ctl), before);
    }

    #[test]
    fn a_cable_of_notes_whose_source_is_gone_when_its_lane_is_put_back_is_left_out_and_the_rest_of_the_undo_is_done() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let lane = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        engine.drain();
        // The first device, which the lane had a cable from, is taken out while the lane is out (the history undoes what was done after it first, so only a change that is made behind its back can: the model is changed by hand).
        let gone = ctl.model.sources[1].node;
        ctl.model.sources.retain(|source| source.node != gone);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.instruments.len(), 3, "the lane is back");
        let (commands, _) = engine.drain();
        assert!(commands.contains(&"InsertInstrument index=1 plugins=1".to_string()), "{commands:?}");
        assert_eq!(
            ctl.model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(),
            [0, 2],
            "the keys' cable, and the cable from the device that is still there: the one from the device that is gone is left out"
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_notes_that_is_there_already_refuses_the_undo_that_would_put_it_back_and_nothing_changes() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let lane = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        engine.drain();
        // (Another front-end has drawn a cable under the id of one that went: ids are not handed out twice, so only a change made behind the controller's back can, and the model is changed by hand.)
        let (keys, other) = (ctl.model.sources[0].node, ctl.model.instruments[1].node);
        ctl.model.insert_event_cable(crate::app::model::EventCable { id: EdgeId(1), from: keys, to: other });
        let error = ctl.undo().unwrap_err();
        assert!(matches!(&error, IntentError::Invalid(why) if why.contains("is there already")), "{error:?}");
        assert_eq!(ctl.model.instruments.len(), 2, "the lane is not put back");
    }

    // -- sources of notes and the cables from them, in the history (P4b of docs/patch-graph.md §11.6) --

    fn notes(keys: NodeId, lane: NodeId) -> Intent {
        Intent::AddCable { from: Endpoint { node: keys, port: Port::EventsOut }, to: Endpoint { node: lane, port: Port::EventsIn }, gain: None }
    }

    fn cable_ids(ctl: &Controller) -> Vec<u32> {
        ctl.model.event_cables.iter().map(|cable| cable.id.0).collect()
    }

    #[test]
    fn a_source_that_was_added_is_taken_out_by_undo_and_made_again_by_redo_under_its_node() {
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        let (before, saved) = (picture(&ctl), written(&ctl));
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        ctl.apply(Intent::AddSource { source: SourceInfo::Device { name: " Seaboard ".to_string() } }).unwrap();
        let (keys, seaboard) = (ctl.model.sources[0].node, ctl.model.sources[1].node);
        let (after, saved_after) = (picture(&ctl), written(&ctl));
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("add Seaboard"), None), "in the words of the name, without the spaces round it");
        sent(&engine);

        ctl.undo().unwrap();
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("add Keys"), Some("add Seaboard")));
        ctl.undo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (before, saved), "both are out, and the session is what it was");
        assert_eq!((undo_label(&ctl), redo_label(&ctl).as_deref()), (None, Some("add Keys")));

        ctl.redo().unwrap();
        ctl.redo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (after, saved_after));
        assert_eq!((ctl.model.sources[0].node, ctl.model.sources[1].node), (keys, seaboard), "under the nodes they had");
        assert_eq!(sent(&engine), Vec::<String>::new(), "the audio thread has nothing of them");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_cable_of_notes_that_was_drawn_is_taken_out_by_undo_and_drawn_again_by_redo_under_its_id_and_the_lane_is_told_what_it_hears() {
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        ctl.apply(Intent::AddSource { source: SourceInfo::Keys }).unwrap();
        let (before, saved) = (picture(&ctl), written(&ctl));
        let keys = ctl.model.sources[0].node;
        sent(&engine);
        ctl.apply(notes(keys, lane(&ctl))).unwrap();
        let id = ctl.model.event_cables[0].id;
        let (after, saved_after) = (picture(&ctl), written(&ctl));
        assert_eq!(undo_label(&ctl).as_deref(), Some("draw a cable of notes"));
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=[0]"]);

        ctl.undo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (before, saved));
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=all"], "the lane hears every source again");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("add Keys"), Some("draw a cable of notes")));

        ctl.redo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (after, saved_after));
        assert_eq!(ctl.model.event_cables[0].id, id, "under the id it had");
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=[0]"]);
    }

    #[test]
    fn a_cable_of_notes_that_was_taken_out_is_put_back_by_undo_where_it_was_and_taken_out_again_by_redo() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let (before, saved) = (picture(&ctl), written(&ctl));
        sent(&engine);
        ctl.apply(Intent::RemoveEdge { edge: EdgeId(0) }).unwrap();
        let (after, saved_after) = (picture(&ctl), written(&ctl));
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("remove a cable of notes"), None));
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=all"], "the lane that had only that cable hears every source");

        ctl.undo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (before, saved));
        assert_eq!(cable_ids(&ctl), [0, 1, 2]);
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=[0]"]);

        ctl.redo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (after, saved_after));
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=all"]);
    }

    #[test]
    fn a_source_that_has_cables_is_taken_out_with_them_and_undo_puts_it_back_at_its_place_and_the_cables_under_their_ids() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let (before, saved) = (picture(&ctl), written(&ctl));
        let launchkey = node(&ctl, NodePath::Source(1));
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: launchkey }).unwrap();
        let (after, saved_after) = (picture(&ctl), written(&ctl));
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("remove Launchkey Mini MK4 37"), None));
        assert_eq!(cable_ids(&ctl), [0, 2], "its cable went with it");

        ctl.undo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (before, saved), "between the keys and the other device, with its cable back");
        assert_eq!(ctl.model.locate(launchkey), Some(NodePath::Source(1)));
        assert_eq!(cable_ids(&ctl), [0, 1, 2]);

        ctl.redo().unwrap();
        assert_eq!((picture(&ctl), written(&ctl)), (after, saved_after));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn the_lane_that_heard_only_a_source_hears_every_source_while_it_is_out_and_the_source_again_when_it_is_back() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let keys = node(&ctl, NodePath::Source(0));
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: keys }).unwrap();
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=all"]);
        ctl.undo().unwrap();
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=[0]"]);
        ctl.redo().unwrap();
        assert_eq!(sent(&engine), ["SetLaneSources inst=0 sources=all"]);
    }

    #[test]
    fn a_source_that_the_session_has_already_refuses_the_undo_that_would_put_it_back_and_nothing_changes_and_the_step_is_dropped() {
        // (The history undoes what was done after a step first, so only a change that is made behind its back can leave a source of the kind there: the model is changed by hand.)
        for (place, there, why) in [
            (0, SourceKind::Keys, "the keys are a source already"),
            (2, SourceKind::Device("Seaboard".to_string()), "Seaboard is a source already"),
        ] {
            let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
            let gone = node(&ctl, NodePath::Source(place));
            ctl.apply(Intent::RemoveNode { node: gone }).unwrap();
            engine.drain();
            let other = ctl.model.alloc_node();
            ctl.model.sources.push(SourceNode { node: other, kind: there });
            let (before, cables) = (picture(&ctl), cable_ids(&ctl));
            assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(why.to_string()));
            assert_eq!((picture(&ctl), cable_ids(&ctl)), (before, cables), "nothing is put back: not the source, nor the cables that went with it");
            assert_eq!((undo_label(&ctl), redo_label(&ctl)), (None, None), "the step is gone");
        }
    }

    #[test]
    fn a_source_whose_node_is_there_already_refuses_the_undo_with_the_reason() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        let keys = node(&ctl, NodePath::Source(0));
        ctl.apply(Intent::RemoveNode { node: keys }).unwrap();
        engine.drain();
        // (Something has put a source under that node without the history: of another kind, so that it is the node that is found there.)
        ctl.model.sources.push(SourceNode { node: keys, kind: SourceKind::Device("Roland".to_string()) });
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("node {} is there already", keys.0)));
        assert_eq!(cable_ids(&ctl), [1, 2], "the keys' cable is not put back");
    }

    #[test]
    fn a_cable_of_notes_or_a_source_that_was_taken_out_by_the_controller_and_not_by_an_intent_leaves_nothing_to_put_back_and_forgets_the_history() {
        let (_engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert!(undo_label(&ctl).is_some(), "there is something to forget");
        assert!(ctl.remove_edge(EdgeId(0)));
        assert!(ctl.journal.is_none(), "nothing is written down unless an intent is being carried out");
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "a cable of notes");
        ctl.apply(Intent::SetLocked { locked: false }).unwrap();
        assert!(undo_label(&ctl).is_some(), "there is something to forget");
        assert!(ctl.remove_source(1));
        assert!(ctl.journal.is_none());
        assert_eq!(ctl.history_state(), HistoryState { undo: None, redo: None }, "a source");
        assert_eq!(cable_ids(&ctl), [2], "with the cable that started at it");
        // One that is not there changes nothing, the history included.
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        assert!(!ctl.remove_source(9));
        assert!(!ctl.remove_edge(EdgeId(1)), "the cable went with its source");
        assert_eq!(undo_label(&ctl).as_deref(), Some("lock the piano to its scale"));
    }

    #[test]
    fn a_cable_of_notes_whose_lane_is_gone_when_it_is_put_back_is_left_out_and_the_rest_of_the_undo_is_done() {
        let (engine, mut ctl, _clock) = controller_over(load(testutil::SOURCES).model);
        ctl.apply(Intent::RemoveEdge { edge: EdgeId(0) }).unwrap();
        engine.drain();
        // (The lane that it ended at is taken out behind the controller's back: the model is changed by hand.)
        ctl.model.remove_lane(0).unwrap();
        ctl.undo().unwrap();
        assert_eq!(cable_ids(&ctl), [1, 2], "the cable is left out, and the cables of the lanes that are there are as they were");
        assert_eq!(redo_label(&ctl).as_deref(), Some("remove a cable of notes"), "the undo is done all the same");
    }

    #[test]
    fn a_lane_that_was_not_handed_back_refuses_the_undo_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (_engine, mut ctl, _clock) = controller_over(load(LANES).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let second = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        // (Nobody runs the audio thread, and the engine's commands have not been read: the channel the lane was to be handed to has nothing, and nobody to wait for.)
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid("the lane of Oscillator was not handed back by the audio thread".to_string()));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step is gone, the one before it is next");
    }

    #[test]
    fn a_lane_whose_effect_is_loaded_again_with_other_outputs_than_the_chain_has_refuses_the_undo_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        // (What the model says the second lane's filter was loaded from is a plugin that loads as an effect with one audio output.)
        ctl.model.instruments[1].effects[1].id = "builtin:mono-effect".to_string();
        let second = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        // The audio thread (by hand) hands back a lane that has no plugins of its own, so undo loads them again.
        engine.drain();
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid("Filter has 1 audio output, and an effect here needs 2".to_string()));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step is gone, the one before it is next");
    }

    #[test]
    fn a_lane_whose_node_is_there_already_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let second = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        engine.drain();
        // Something puts a lane there without the history: one with that node.
        let lane = ctl.add_lane();
        ctl.model.instruments[lane].node = second;
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("node {} is there already", second.0)));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None));
    }

    #[test]
    fn a_lane_with_an_effect_that_is_there_already_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        // (A lane has no modulator of its own to be put back: its modulators stay when it goes.)
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let lane = node(&ctl, NodePath::Instrument(0));
        let filter = ctl.model.instruments[0].effects[0].node;
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        engine.drain();
        // Something puts one there without the history: the id that the lane had of it is the id of another's.
        let expected = {
            let (_plugin, mut spare) = ctl.load_plugin("builtin:filter").unwrap();
            spare.node = filter;
            ctl.model.insert_effect_at(0, 0, spare).unwrap();
            format!("node {} is there already", filter.0)
        };
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(expected));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None));
    }

    #[test]
    fn a_lane_put_back_past_the_end_goes_in_last_and_the_audio_thread_is_told_the_place_it_went_to() {
        use crate::app::journal::{Inverse, Journal, Kept};
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        // The last lane, which has no instrument and a reverb, goes out by the model alone, and comes back in a journal of the kind that a session which changed without the
        // history can make: its place is past the end of the lanes.
        let lane = ctl.model.remove_lane(3).unwrap();
        let node_of_the_lane = lane.node;
        let mut journal = Journal::default();
        journal.push(Inverse::Lane { index: 99, lane, kept: Kept::InHand(crate::plugin::chain::KeptLane::empty()) });
        engine.drain();
        ctl.rewind(journal).unwrap();
        assert_eq!(ctl.model.instruments.last().map(|lane| lane.node), Some(node_of_the_lane));
        let (commands, _) = engine.drain();
        assert_eq!(commands[0], "InsertInstrument index=3 plugins=1", "{commands:?}");
    }

    #[test]
    fn a_lane_that_a_running_audio_thread_hands_back_across_threads_is_put_back_at_once_with_its_plugins_and_they_are_not_loaded_again() {
        use crate::audio::{AudioEngine, NULL_DEVICE};
        use crate::plugin::chain::AudioGraph;
        // A real audio thread (the null backend renders on a timer), and plugins that cannot be loaded again: undo has the lane, whole, only if the audio thread handed it
        // over the channel, across threads; if it did not it fails after the wait (five seconds here).
        let (mut handles, engine) = testutil::stub_engine_of(4096);
        handles.recovery_wait = Duration::from_secs(5);
        let model = testutil::load_onto(LANES, &handles.cmd_tx);
        let graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut audio = AudioEngine::build(graph, engine.midi_rx.clone(), Some(NULL_DEVICE), false, 48_000, 256).unwrap();
        let mut ctl = Controller::new(model, handles, None);
        audio.play().unwrap();
        for plugin in ["builtin:nothing", "builtin:nothing"] {
            let lane = &mut ctl.model.instruments[1];
            let (instrument, effect) = (lane.instrument.as_mut().unwrap(), &mut lane.effects[0]);
            instrument.id = plugin.to_string();
            effect.id = plugin.to_string();
        }
        let second = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        let started = Instant::now();
        let undone = ctl.undo();
        let waited = started.elapsed();
        audio.stop();
        undone.expect("the lane came back from the audio thread");
        assert!(waited < Duration::from_secs(2), "{waited:?}: it did not wait for what was there");
        assert!(ctl.model.locate(second).is_some());
    }

    #[test]
    fn a_lane_that_was_taken_out_by_the_controller_and_not_by_an_intent_is_handed_to_no_channel() {
        let (engine, mut ctl, _clock) = controller_over(load(LANES).model);
        engine.drain();
        assert!(ctl.remove_lane(3));
        assert_eq!(engine.drain().0, ["RemoveFreeModTarget mod=3 target=0", "RemoveInstrument inst=3"]);
    }

    /// A session of `count` lanes that have two reverbs each (so that each has three plugins) and a lane to keep, which the removals leave.
    fn lanes_of_three_plugins(count: usize) -> String {
        let mut session = String::new();
        for _ in 0..count {
            session.push_str("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n");
        }
        session
    }

    #[test]
    fn the_plugins_of_the_newest_lane_removals_are_kept_and_the_ones_of_an_older_lane_are_loaded_again_with_what_the_user_set() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(&lanes_of_three_plugins(5));
        // The plugins have a state that the model does not know: one that comes back with its lane has it, one that is loaded again has not. (The effects' first parameter.)
        let state = |lane: usize, effect: usize| 0.1 + 0.01 * lane as f32 + 0.1 * effect as f32;
        for lane in 0..5 {
            for effect in 0..2 {
                graph.poke_effect(lane, effect, 0, state(lane, effect));
            }
        }
        // The user set the second effect of the last lane through the controller, so that the model has it.
        let (damping, _) = param(&ctl, NodePath::Effect { inst: 4, index: 1 }, "damping");
        let last_effect = node(&ctl, NodePath::Effect { inst: 4, index: 1 });
        ctl.apply(Intent::SetParam { node: last_effect, param: damping, value: 0.8 }).unwrap();
        // The last four lanes are taken out, the last first: three plugins each, so that the third removal makes nine, which is more than eight, and the oldest lets go of
        // its plugins; and the fourth lets go of the second's.
        for lane in (1..5).rev() {
            let node = ctl.model.instruments[lane].node;
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            graph.drain_commands();
        }
        assert_eq!(graph.lanes().len(), 1);
        for _ in 0..4 {
            ctl.undo().unwrap();
            graph.drain_commands();
        }
        assert_eq!(graph.lanes().len(), 5);
        let kept: Vec<bool> = (0..5).map(|lane| (graph.peek_effect(lane, 0, 0).unwrap() - state(lane, 0)).abs() < 1e-6).collect();
        // The one that never went, and the newest two removals (lanes 1 and 2), are the plugins they were; lanes 3 and 4 are loaded again.
        assert_eq!(kept, [true, true, true, false, false]);
        // A lane that is loaded again has the values the user set.
        assert_eq!(graph.peek_effect(4, 1, damping), Some(0.8));
    }

    // -- a group that is taken out is put back --

    /// Three groups and four lanes: "Pad" has a reverb and a filter on its bus, and a modulator with cables to the first bus effect, to the first member's detune and to the second member's
    /// filter; the second group has a filter and an envelope with a cable to its filter and one to its member's volume; the third has nothing but a modulator. Lanes 0 and 2 are in "Pad", lane 1
    /// is in the second group (and has a filter), and lane 3 is in none.
    const GROUPS_AMONG_LANES: &str = r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:reverb"
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.1
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.2
[[group.modulator.target]]
member = 1
slot = 1
param = "cutoff"
depth = 0.3

[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.modulator]]
type = "envelope"
attack = 0.1
decay = 0.2
sustain = 0.3
release = 0.4
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
member = 0
param = "volume"
depth = 0.5

[[group]]
[[group.modulator]]
rate = 2.0

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument]]
plugin = "builtin:osc"
group = 1
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument]]
plugin = "builtin:osc"
"#;

    /// The nodes of the groups of the session, in the order the model lists them.
    fn group_nodes_of(ctl: &Controller) -> Vec<NodeId> {
        ctl.model.groups.iter().map(|group| group.node).collect()
    }

    /// Every group of `session`, one at a time (each in a controller of its own, after a step of another kind): taken out by an intent, put back by undo, taken out again by redo
    /// and put back again. Each time the model, the picture a client is given, the file and the audio thread are as they were, and the plugins that are back are the very ones that went
    /// (the audio thread is run after each, as it runs on its own: it is what hands the group back). How many were put back, and how many were not.
    fn every_group_is_put_back(name: &str, session: &str) -> (usize, usize) {
        let count = controller_over_an_audio_thread(session).0.model.groups.len();
        let (mut put_back, mut forgotten) = (0, 0);
        for n in 0..count {
            let (mut ctl, mut graph) = controller_over_an_audio_thread(session);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let group = group_nodes_of(&ctl)[n];
            let before = everything_as_it_is(&ctl, &graph);
            ctl.apply(Intent::RemoveNode { node: group }).unwrap();
            graph.drain_commands();
            let taken = everything_as_it_is(&ctl, &graph);
            assert_ne!(taken.0 .0 .0, before.0 .0 .0, "group {n} of {name}: it is gone from the picture");
            assert_ne!(taken.2, before.2, "group {n} of {name}: it is gone from the audio thread");
            if ctl.history_state() == (HistoryState { undo: None, redo: None }) {
                assert!(ctl.undo().is_err(), "group {n}: nothing to undo");
                forgotten += 1;
                continue;
            }
            assert!(undo_label(&ctl).is_some_and(|label| label.starts_with("remove ")), "group {n}: {:?}", undo_label(&ctl));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("group {n} of {name} put back"));
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &taken, &format!("group {n} of {name} taken out again"));
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("group {n} of {name} put back again"));
            ctl.model.check_ids().unwrap();
            put_back += 1;
        }
        (put_back, forgotten)
    }

    #[test]
    fn every_group_of_every_session_is_put_back_by_undo_as_the_very_group_in_the_model_the_picture_the_file_and_on_the_audio_thread() {
        let sessions = [
            ("MUTED_AND_SOLOED", MUTED_AND_SOLOED),
            ("GROUPS_AMONG_LANES", GROUPS_AMONG_LANES),
            ("LANES", LANES),
            ("SESSION", SESSION),
            ("EFFECTS", EFFECTS),
            ("LONG_CHAINS", testutil::LONG_CHAINS),
            ("CHAINS", testutil::CHAINS),
            ("GROUPS", testutil::GROUPS),
            ("LANES_IN_GROUPS", testutil::LANES_IN_GROUPS),
            ("CROSS_MOD_RACKS", testutil::CROSS_MOD_RACKS),
            ("FREE_MODULATORS", testutil::FREE_MODULATORS),
        ];
        let mut total = 0;
        for (name, session) in sessions {
            let (put_back, forgotten) = every_group_is_put_back(name, session);
            assert_eq!(forgotten, 0, "{name}");
            total += put_back;
        }
        assert!(total > 10, "{total} groups is not enough to say much");
    }

    #[test]
    fn a_group_is_a_step_in_the_words_of_its_name_or_of_the_place_it_is_called_by() {
        let (_engine, mut ctl, _clock) = controller_over(load(GROUPS_AMONG_LANES).model);
        let (pad, second) = (node(&ctl, NodePath::Group(0)), node(&ctl, NodePath::Group(1)));
        ctl.apply(Intent::RemoveNode { node: second }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Group 2"));
        ctl.apply(Intent::RemoveNode { node: pad }).unwrap();
        assert_eq!(undo_label(&ctl).as_deref(), Some("remove Pad"));
    }

    #[test]
    fn a_group_that_was_not_handed_back_refuses_the_undo_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        // (The first group is called by its name, the second by the place it has, which is the place it is taken out of.)
        for (index, words) in [(0, "Pad"), (1, "Group 2")] {
            let (_engine, mut ctl, _clock) = controller_over(load(GROUPS_AMONG_LANES).model);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let group = node(&ctl, NodePath::Group(index));
            ctl.apply(Intent::RemoveNode { node: group }).unwrap();
            // (Nobody runs the audio thread, and the engine's commands have not been read: the channel the group was to be handed to has nothing, and nobody to wait for.)
            let before = picture(&ctl);
            assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("{words} was not handed back by the audio thread")));
            assert_eq!(picture(&ctl), before);
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step is gone, the one before it is next");
        }
    }

    #[test]
    fn a_group_whose_bus_effect_is_loaded_again_with_other_outputs_than_the_chain_has_refuses_the_undo_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (engine, mut ctl, _clock) = controller_over(load(GROUPS_AMONG_LANES).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        // (What the model says the filter on the first group's bus was loaded from is a plugin that loads as an effect with one audio output.)
        ctl.model.groups[0].effects[1].id = "builtin:mono-effect".to_string();
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RemoveNode { node: group }).unwrap();
        // The audio thread (by hand) hands back a group that has no plugins of its own, so undo loads them again.
        engine.drain();
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid("Filter has 1 audio output, and an effect here needs 2".to_string()));
        assert_eq!(picture(&ctl), before);
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step is gone, the one before it is next");
    }

    #[test]
    fn a_group_with_a_node_or_a_member_that_is_not_as_it_was_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        for what in ["the group", "an effect", "a member that is gone", "a member that is in a group"] {
            let (engine, mut ctl, _clock) = controller_over(load(GROUPS_AMONG_LANES).model);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let pad = node(&ctl, NodePath::Group(0));
            let filter = ctl.model.groups[0].effects[1].node;
            let member = ctl.model.instruments[2].node;
            ctl.apply(Intent::RemoveNode { node: pad }).unwrap();
            engine.drain();
            // Something changes the session without the history.
            let expected = match what {
                "the group" => {
                    ctl.model.groups[0].node = pad;
                    format!("node {} is there already", pad.0)
                }
                "an effect" => {
                    let (_plugin, mut spare) = ctl.load_plugin("builtin:filter").unwrap();
                    spare.node = filter;
                    ctl.model.groups[0].effects.push(spare);
                    format!("node {} is there already", filter.0)
                }
                "a member that is gone" => {
                    ctl.model.instruments.remove(2);
                    format!("node {} is not there to be put in the group", member.0)
                }
                _ => {
                    ctl.model.instruments[2].group = Some(0);
                    format!("node {} is in a group already", member.0)
                }
            };
            let before = picture(&ctl);
            assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(expected), "{what}");
            assert_eq!(picture(&ctl), before, "{what}");
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "{what}");
        }
    }

    #[test]
    fn a_group_that_was_taken_out_by_the_controller_and_not_by_an_intent_is_handed_to_no_channel() {
        let (engine, mut ctl, _clock) = controller_over(load(GROUPS_AMONG_LANES).model);
        engine.drain();
        assert!(ctl.remove_group(2));
        assert_eq!(engine.drain().0.last().map(String::as_str), Some("RemoveGroup group=2"));
    }

    #[test]
    fn a_group_put_back_past_the_end_goes_in_last_with_its_members_and_the_audio_thread_is_told_the_place_it_went_to() {
        use crate::app::journal::{Inverse, Journal, Kept};
        let (engine, mut ctl, _clock) = controller_over(load(GROUPS_AMONG_LANES).model);
        // The last group, which has no members, a modulator and nothing else, goes out by the model alone and comes back in a journal of the kind that a session which changed without the
        // history can make: its place is past the end of the groups, and the lane that it names is the last.
        let group = ctl.model.remove_group(2).unwrap();
        let node_of_the_group = group.node;
        let member = ctl.model.instruments[3].node;
        let mut journal = Journal::default();
        journal.push(Inverse::Group { index: 99, group, members: vec![member], kept: Kept::InHand(crate::plugin::chain::KeptGroup::empty()) });
        engine.drain();
        ctl.rewind(journal).unwrap();
        assert_eq!(ctl.model.groups.last().map(|group| group.node), Some(node_of_the_group));
        assert_eq!(ctl.model.instruments[3].group, Some(2));
        let (commands, _) = engine.drain();
        assert_eq!(commands[0], "InsertGroup index=2 plugins=0 members=[3]", "{commands:?}");
    }

    #[test]
    fn a_group_that_a_running_audio_thread_hands_back_across_threads_is_put_back_at_once_with_its_plugins_and_they_are_not_loaded_again() {
        use crate::audio::{AudioEngine, NULL_DEVICE};
        use crate::plugin::chain::AudioGraph;
        // A real audio thread (the null backend renders on a timer), and plugins that cannot be loaded again: undo has the group, whole, only if the audio thread handed it over the
        // channel, across threads; if it did not it fails after the wait (five seconds here).
        let (mut handles, engine) = testutil::stub_engine_of(4096);
        handles.recovery_wait = Duration::from_secs(5);
        let model = testutil::load_onto(GROUPS_AMONG_LANES, &handles.cmd_tx);
        let graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut audio = AudioEngine::build(graph, engine.midi_rx.clone(), Some(NULL_DEVICE), false, 48_000, 256).unwrap();
        let mut ctl = Controller::new(model, handles, None);
        audio.play().unwrap();
        for effect in &mut ctl.model.groups[0].effects {
            effect.id = "builtin:nothing".to_string();
        }
        let pad = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RemoveNode { node: pad }).unwrap();
        let started = Instant::now();
        let undone = ctl.undo();
        let waited = started.elapsed();
        audio.stop();
        undone.expect("the group came back from the audio thread");
        assert!(waited < Duration::from_secs(2), "{waited:?}: it did not wait for what was there");
        assert!(ctl.model.locate(pad).is_some());
    }

    /// A session of `count` groups that have two reverbs each on their buses and a lane in none, which the removals leave.
    fn groups_of_two_plugins(count: usize) -> String {
        let mut session = String::new();
        for _ in 0..count {
            session.push_str("[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\n");
        }
        session.push_str("[[instrument]]\nplugin = \"builtin:osc\"\n");
        session
    }

    #[test]
    fn the_plugins_of_the_newest_group_removals_are_kept_and_the_ones_of_an_older_group_are_loaded_again_with_what_the_user_set() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(&groups_of_two_plugins(6));
        // The plugins have a state that the model does not know: one that comes back with its group has it, one that is loaded again has not. (The bus effects' first parameter.)
        // A group's bus effects are not reached by the test accessors of the lanes, so the state is the parameter that the model knows, set through the controller and then changed
        // behind its back: `damping` of the first bus effect, to 0.3 (the model says it is something else).
        let (damping, _) = param(&ctl, NodePath::GroupEffect { group: 0, index: 0 }, "damping");
        for group in 0..6 {
            graph.poke_bus_effect(group, 0, damping, 0.3);
        }
        // The user set the first bus effect of the last group through the controller, so that the model has it.
        let last_effect = node(&ctl, NodePath::GroupEffect { group: 5, index: 0 });
        ctl.apply(Intent::SetParam { node: last_effect, param: damping, value: 0.8 }).unwrap();
        // Six groups of two plugins are taken out, the last first, so that the fifth removal makes ten, which is more than eight, and the oldest lets go of its plugins.
        for group in (0..6).rev() {
            let node = ctl.model.groups[group].node;
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            graph.drain_commands();
        }
        assert_eq!(graph.groups().len(), 0);
        for _ in 0..6 {
            ctl.undo().unwrap();
            graph.drain_commands();
        }
        assert_eq!(graph.groups().len(), 6);
        let kept: Vec<bool> = (0..6).map(|group| graph.peek_bus_effect(group, 0, damping) == Some(0.3)).collect();
        // The newest four removals (groups 0 to 3) are the plugins they were, and the two oldest (groups 4 and 5) were let go of and are loaded again.
        assert_eq!(kept, [true, true, true, true, false, false]);
        // A group that is loaded again has the values the user set.
        assert_eq!(graph.peek_bus_effect(5, 0, damping), Some(0.8));
    }

    #[test]
    fn a_step_counts_the_plugins_of_the_instrument_the_effect_the_lane_and_the_group_it_is_to_be_handed_and_none_once_it_has_let_go_of_them() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(GROUPS_AMONG_LANES);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        // An instrument (lane 3's, one plugin), an effect (the filter of lane 2, one plugin), a group (two plugins on its bus) and a lane (an instrument and a filter: lane 1, which is in the
        // second group, now the first).
        let lane3 = node(&ctl, NodePath::Instrument(3));
        ctl.apply(Intent::ReplaceInstrument { node: lane3, plugin: "builtin:osc".to_string() }).unwrap();
        graph.drain_commands();
        let effect = node(&ctl, NodePath::Effect { inst: 2, index: 0 });
        ctl.apply(Intent::RemoveNode { node: effect }).unwrap();
        graph.drain_commands();
        let group = node(&ctl, NodePath::Group(0));
        ctl.apply(Intent::RemoveNode { node: group }).unwrap();
        graph.drain_commands();
        let lane = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::RemoveNode { node: lane }).unwrap();
        graph.drain_commands();
        let kept = |ctl: &Controller| -> Vec<Option<usize>> {
            ctl.history.undo.iter().map(|step| if let Taken::Held(journal) = &step.taken { Some(journal.plugins_kept()) } else { None }).collect()
        };
        assert_eq!(kept(&ctl), [Some(1), Some(1), Some(2), Some(2)]);
        for step in &mut ctl.history.undo {
            let Taken::Held(journal) = &mut step.taken else { panic!("a removal holds what it took out") };
            journal.let_go_of_plugins();
        }
        assert_eq!(kept(&ctl), [Some(0), Some(0), Some(0), Some(0)], "what was let go of is not kept, and does not count against what is");
    }

    // -- an instrument that is replaced is put back --

    /// Three lanes and a group: the first lane, in the group "Pad", has an oscillator with parameters set, a note remap with its pitch bend range, a range, a transposition, a volume, a
    /// filter and a modulator whose cable drives its volume, while the group's modulator drives its detune; the second has an oscillator with a waveform set; the third has no instrument,
    /// and a reverb.
    const INSTRUMENTS: &str = r#"
[[group]]
name = "Pad"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
range = "C2-C6"
transpose = 3
volume = 0.5
pitch_bend_range = 4
[instrument.params]
detune = 3.0
sustain = 0.5
[instrument.remap]
"G#4" = { note = "G4", detune = 1.0 }
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
param = "volume"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
[instrument.params]
waveform = 2.0

[[instrument]]
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    /// What the audio thread has of its lanes and groups, with nothing that tells one plugin from another: the facts of each, and which lanes have a note remapper.
    fn lane_facts(graph: &crate::plugin::chain::AudioGraph) -> String {
        format!("{:?}\n{:?}\n{:?}", graph.lanes(), graph.remappers(), graph.groups())
    }

    #[test]
    fn every_instrument_that_is_replaced_by_an_intent_is_put_back_by_undo_as_the_very_instrument_with_its_cables_and_its_note_remap() {
        let lanes = controller_over_an_audio_thread(INSTRUMENTS).0.model.instruments.len();
        assert_eq!(lanes, 3);
        for n in 0..lanes {
            // (Each in a controller of its own, after a step of another kind, which the replacement leaves in the history.)
            let (mut ctl, mut graph) = controller_over_an_audio_thread(INSTRUMENTS);
            ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let lane = node(&ctl, NodePath::Instrument(n));
            let before = everything_as_it_is(&ctl, &graph);
            ctl.apply(Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() }).unwrap();
            graph.drain_commands();
            let taken = everything_as_it_is(&ctl, &graph);
            let taken_facts = lane_facts(&graph);
            assert_ne!(taken.0 .1, before.0 .1, "lane {n}: the file has another instrument");
            assert_ne!(taken.2, before.2, "lane {n}: the audio thread has another instrument");
            let words = if n == 2 { "put Oscillator in a lane" } else { "change the instrument to Oscillator" };
            assert_eq!(undo_label(&ctl).as_deref(), Some(words), "lane {n}");
            // The instrument that was in the lane is back, as it was: the very plugin, the lane's note remap, the cables that drove it and what they were centred on.
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("lane {n} put back"));
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("lock the piano to its scale"), Some(words)), "lane {n}");
            ctl.model.check_ids().unwrap();
            // Redone, it is another instrument that goes in (loaded from the catalog again): everything else is as it was after the replacement.
            ctl.redo().unwrap();
            graph.drain_commands();
            let redone = everything_as_it_is(&ctl, &graph);
            assert_same_modulators(&redone.0, &taken.0, &format!("lane {n} replaced again"));
            assert_eq!(redone.1, taken.1, "lane {n} replaced again: the chains");
            assert_eq!(lane_facts(&graph), taken_facts, "lane {n} replaced again: the lanes");
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("lane {n} put back again"));
        }
    }

    #[test]
    fn an_instrument_is_replaced_after_the_cables_that_drove_it_have_gone_and_put_back_before_them_and_when_it_was_not_handed_back_it_is_loaded_again() {
        let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let lane0 = node(&ctl, NodePath::Instrument(0));
        let before = (written(&ctl), picture(&ctl));
        engine.drain();
        ctl.apply(Intent::ReplaceInstrument { node: lane0, plugin: "builtin:osc".to_string() }).unwrap();
        let (commands, _) = engine.drain();
        // The cables that drove the old instrument go first (the lane's own, then its group's), and then the swap, which asks for the old instrument to be handed to a channel, and has
        // no note remap for the new one.
        assert_eq!(commands, ["RemoveFreeModTarget mod=0 target=0", "RemoveFreeModTarget mod=1 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]);
        assert_ne!(written(&ctl), before.0);
        // (Nobody runs the audio thread, and nobody answers the channel: undo loads the oscillator again, with the values that the model has.) The swap is the first to be put back, the cables after it,
        // the last that went first.
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert_eq!(commands[0], "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=true");
        assert!(commands[1].starts_with(&format!("InsertFreeModTarget mod=1 target=0 aim=Param {{ plugin: {lane0:?}, ")), "{commands:?}");
        assert!(commands[2].starts_with(&format!("InsertFreeModTarget mod=0 target=0 aim=Param {{ plugin: {lane0:?}, ")), "{commands:?}");
        assert_eq!((written(&ctl), picture(&ctl)), before, "the model has what it had, the values the user set among it");
    }

    #[test]
    fn every_instrument_that_is_taken_out_of_its_lane_by_an_intent_is_put_back_by_undo_as_the_very_instrument_with_its_cables_and_its_note_remap_and_taken_out_again_by_redo() {
        // (The third lane has no instrument: it is refused, below. Each is in a controller of its own, after a step of another kind, which taking the instrument out leaves in the history.)
        for n in 0..2 {
            let (mut ctl, mut graph) = controller_over_an_audio_thread(INSTRUMENTS);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let lane = node(&ctl, NodePath::Instrument(n));
            let before = everything_as_it_is(&ctl, &graph);
            ctl.apply(Intent::ClearInstrument { node: lane }).unwrap();
            graph.drain_commands();
            let taken = everything_as_it_is(&ctl, &graph);
            let taken_facts = lane_facts(&graph);
            assert!(ctl.model.instruments[n].instrument.is_none(), "lane {n}: the lane has none now");
            assert_ne!(taken.0 .1, before.0 .1, "lane {n}: the file has no instrument in it");
            assert_ne!(taken.2, before.2, "lane {n}: the audio thread has no instrument in it");
            assert_eq!(undo_label(&ctl).as_deref(), Some("take Oscillator out of its lane"), "lane {n}");
            // The instrument that was in the lane is back, as it was: the very plugin, the lane's note remap, the cables that drove it and what they were centred on.
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("lane {n} put back"));
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl).as_deref()), (Some("lock the piano to its scale"), Some("take Oscillator out of its lane")), "lane {n}");
            ctl.model.check_ids().unwrap();
            // Redone, it is taken out again: everything is as it was after the first time.
            ctl.redo().unwrap();
            graph.drain_commands();
            let redone = everything_as_it_is(&ctl, &graph);
            assert_same_modulators(&redone.0, &taken.0, &format!("lane {n} taken out again"));
            assert_eq!(redone.1, taken.1, "lane {n} taken out again: the chains");
            assert_eq!(lane_facts(&graph), taken_facts, "lane {n} taken out again: the lanes");
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_everything(&everything_as_it_is(&ctl, &graph), &before, &format!("lane {n} put back again"));
        }
    }

    #[test]
    fn the_instrument_that_is_taken_out_goes_with_the_cables_that_drove_it_first_and_is_put_back_before_them_and_when_it_was_not_handed_back_it_is_loaded_again() {
        let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
        let lane0 = node(&ctl, NodePath::Instrument(0));
        let before = (written(&ctl), picture(&ctl));
        engine.drain();
        ctl.apply(Intent::ClearInstrument { node: lane0 }).unwrap();
        let (commands, _) = engine.drain();
        // The cables that drove the instrument go first (the lane's own, then its group's), and then the instrument, which is asked to be handed to a channel.
        assert_eq!(commands, ["RemoveFreeModTarget mod=0 target=0", "RemoveFreeModTarget mod=1 target=0", "ClearInstrument inst=0 keep"]);
        assert_ne!(written(&ctl), before.0);
        // (Nobody runs the audio thread, and nobody answers the channel: undo loads the oscillator again, with the values that the model has.) The swap is the first to be put back, the cables
        // after it, the last that went first.
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        assert_eq!(commands.len(), 3, "{commands:?}");
        assert_eq!(commands[0], "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=true");
        assert!(commands[1].starts_with(&format!("InsertFreeModTarget mod=1 target=0 aim=Param {{ plugin: {lane0:?}, ")), "{commands:?}");
        assert!(commands[2].starts_with(&format!("InsertFreeModTarget mod=0 target=0 aim=Param {{ plugin: {lane0:?}, ")), "{commands:?}");
        assert_eq!((written(&ctl), picture(&ctl)), before, "the model has what it had, the values the user set among it");
    }

    #[test]
    fn taking_the_instrument_out_of_a_lane_that_has_none_and_of_what_is_no_lane_is_refused_and_leaves_no_step() {
        let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let (empty, filter) = (node(&ctl, NodePath::Instrument(2)), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        let (group, ghost) = (node(&ctl, NodePath::Group(0)), NodeId(9_999));
        engine.drain();
        let before = picture(&ctl);
        for (node, why) in [(empty, "the lane has no instrument to take out"), (filter, "an instrument is taken out of a lane"), (group, "an instrument is taken out of a lane")] {
            assert_eq!(ctl.apply(Intent::ClearInstrument { node }).unwrap_err(), IntentError::Invalid(why.to_string()), "{why}");
        }
        assert_eq!(ctl.apply(Intent::ClearInstrument { node: ghost }).unwrap_err(), IntentError::NotFound(ghost));
        assert_eq!(picture(&ctl), before);
        assert!(engine.drain().0.is_empty(), "nothing was sent");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step before is the last one");
    }

    #[test]
    fn an_instrument_that_is_taken_out_of_a_lane_whose_lane_is_gone_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let lane = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::ClearInstrument { node: lane }).unwrap();
        engine.drain();
        // Something takes the lane out without the history.
        ctl.model.instruments.remove(1);
        let before = picture(&ctl);
        assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("node {} is not there to have its instrument put back", lane.0)));
        assert_eq!(picture(&ctl), before);
        assert!(engine.drain().0.is_empty(), "nothing was sent");
        assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "the step is gone, the one before it is next");
    }

    #[test]
    fn a_lane_that_had_no_instrument_is_emptied_again_by_undo_and_filled_by_redo() {
        let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        let empty = node(&ctl, NodePath::Instrument(2));
        engine.drain();
        ctl.apply(Intent::ReplaceInstrument { node: empty, plugin: "builtin:osc".to_string() }).unwrap();
        // (Nothing is handed back from an empty lane: no channel is asked for.)
        assert_eq!(engine.drain().0, ["SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false"]);
        assert!(ctl.model.instruments[2].instrument.is_some());
        ctl.undo().unwrap();
        assert_eq!(engine.drain().0, ["ClearInstrument inst=2"]);
        assert!(ctl.model.instruments[2].instrument.is_none());
        ctl.redo().unwrap();
        assert_eq!(engine.drain().0, ["SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false"]);
        assert!(ctl.model.instruments[2].instrument.is_some());
    }

    #[test]
    fn an_instrument_whose_lane_is_gone_or_is_no_lane_any_more_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        for what in ["the lane is gone", "its node is another kind of node"] {
            let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
            ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let lane = node(&ctl, NodePath::Instrument(1));
            ctl.apply(Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() }).unwrap();
            engine.drain();
            // Something takes the lane out without the history, and, the second time, something else has its id (the reverb of the lane that was after it).
            ctl.model.instruments.remove(1);
            if what != "the lane is gone" {
                ctl.model.instruments[1].effects[0].node = lane;
            }
            let before = picture(&ctl);
            assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("node {} is not there to have its instrument put back", lane.0)), "{what}");
            assert_eq!(picture(&ctl), before, "{what}");
            assert!(engine.drain().0.is_empty(), "{what}: nothing was sent");
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "{what}: the step is gone, the one before it is next");
        }
    }

    #[test]
    fn an_instrument_that_was_handed_back_and_has_no_audio_output_is_refused_with_the_reason_and_nothing_changes() {
        use crate::app::journal::{Inverse, Journal, Kept, Replaced};
        let (engine, mut ctl, _clock) = controller_over(load(INSTRUMENTS).model);
        let lane = node(&ctl, NodePath::Instrument(1));
        // (A journal that a session which changed without the history can make: what is in hand is not what the lane had, and a lane takes none that has nothing to hear.)
        let (_plugin, slot) = ctl.load_plugin("builtin:osc").unwrap();
        let mut journal = Journal::default();
        journal.push(Inverse::Instrument {
            lane,
            before: Some(Replaced { slot, remap: Default::default(), pitch_bend_range: 2.0, kept: Kept::InHand(Box::new(testutil::FakeInstrument(0))) }),
        });
        engine.drain();
        let before = picture(&ctl);
        assert_eq!(ctl.rewind(journal).unwrap_err(), IntentError::Invalid("FakeInstrument has no audio outputs".to_string()));
        assert_eq!(picture(&ctl), before);
        assert!(engine.drain().0.is_empty(), "nothing was sent");
    }

    // -- a preset that is loaded is put back --

    /// Two lanes with a reverb each, and a reverb on the bus of the group that the first is in. The first has a preset loaded and a value set over it, and three cables drive it: two of its
    /// lane's modulator and one of its group's; the second has no preset, a value set, and nothing drives it.
    const PRESETS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
slot = 1
param = "decay"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
preset = "Cathedral"
[instrument.effect.params]
damping = 0.3
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "damping"
depth = 0.2
[[instrument.modulator.target]]
slot = 1
param = "width"
depth = 0.4

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
[instrument.effect.params]
size = 0.5
"#;

    /// The values of the ten parameters of the reverb in the first place of lane `inst`'s chain, as the plugin on the audio thread says them.
    fn live_reverb(graph: &mut crate::plugin::chain::AudioGraph, inst: usize) -> Vec<Option<f32>> {
        (0..10).map(|param| graph.plugin_parameter(inst, 1, param)).collect()
    }

    #[test]
    fn every_preset_loaded_into_every_reverb_is_put_back_by_undo_with_the_preset_the_values_and_the_centres_of_the_cables_it_had() {
        let presets = controller_over_an_audio_thread(PRESETS).0.model.instruments[0].effects[0].presets.clone();
        assert_eq!(presets.len(), 5);
        for lane in 0..2 {
            for preset in &presets {
                // (Each in a controller of its own, after a step of another kind, which the load leaves in the history.)
                let (mut ctl, mut graph) = controller_over_an_audio_thread(PRESETS);
                ctl.apply(Intent::SetLocked { locked: true }).unwrap();
                let reverb = node(&ctl, NodePath::Effect { inst: lane, index: 0 });
                let (slot_of, name) = (|ctl: &Controller| format!("{:?}", ctl.model.instruments[lane].effects[0]), format!("lane {lane}, {}", preset.name));
                let before = (everything_as_it_is(&ctl, &graph), slot_of(&ctl), written(&ctl), live_reverb(&mut graph, lane));
                ctl.apply(Intent::LoadPreset { node: reverb, preset: preset.id.clone() }).unwrap();
                graph.drain_commands();
                let taken = (everything_as_it_is(&ctl, &graph), slot_of(&ctl), written(&ctl), live_reverb(&mut graph, lane));
                assert_ne!(taken.1, before.1, "{name}: the model has the preset");
                assert_ne!(taken.3, before.3, "{name}: the plugin has the preset");
                let words = format!("load preset {}", preset.name);
                assert_eq!(undo_label(&ctl), Some(words.clone()), "{name}");
                ctl.undo().unwrap();
                graph.drain_commands();
                assert_same_everything(&everything_as_it_is(&ctl, &graph), &before.0, &format!("{name} put back"));
                assert_eq!(slot_of(&ctl), before.1, "{name}: the plugin's slot in the model, its values and what they are compared with, and its preset");
                assert_eq!(written(&ctl), before.2, "{name}: the file");
                assert_eq!(live_reverb(&mut graph, lane), before.3, "{name}: the values on the audio thread");
                assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), Some(words.clone())), "{name}");
                ctl.redo().unwrap();
                graph.drain_commands();
                assert_same_everything(&everything_as_it_is(&ctl, &graph), &taken.0, &format!("{name} loaded again"));
                assert_eq!((slot_of(&ctl), written(&ctl), live_reverb(&mut graph, lane)), (taken.1.clone(), taken.2.clone(), taken.3.clone()), "{name}: loaded again");
                ctl.undo().unwrap();
                graph.drain_commands();
                assert_same_everything(&everything_as_it_is(&ctl, &graph), &before.0, &format!("{name} put back again"));
                assert_eq!((slot_of(&ctl), live_reverb(&mut graph, lane)), (before.1.clone(), before.3.clone()), "{name}: put back again");
            }
        }
    }

    #[test]
    fn undoing_a_preset_loads_the_preset_the_plugin_had_again_and_gives_back_each_value_that_the_preset_changed_or_the_user_had_set() {
        let (engine, mut ctl, _clock) = controller_over(load(PRESETS).model);
        let (first, second) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::Effect { inst: 1, index: 0 }));
        engine.drain();
        // The first reverb has Cathedral, and a damping of 0.3 that the user set over it. Another preset is loaded over both: the audio thread loads it.
        ctl.apply(Intent::LoadPreset { node: first, preset: "plate".to_string() }).unwrap();
        assert_eq!(engine.drain().0, ["LoadPreset inst=0 slot=1 id=plate"]);
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        // Undo has the audio thread load Cathedral again, and then gives each of the ten values back that Plate changed (all of them), the user's damping among them, the first parameters first.
        assert_eq!(commands.len(), 11, "{commands:?}");
        assert_eq!(commands[0], "LoadPreset inst=0 slot=1 id=cathedral");
        assert_eq!(commands[3], "SetParameter inst=0 slot=1 param=2 value=0.300", "the damping the user set, and not the 0.55 that Cathedral has");
        assert_eq!(commands[1], "SetParameter inst=0 slot=1 param=0 value=0.960");
        assert_eq!(commands[10], "SetParameter inst=0 slot=1 param=9 value=0.800");
        // The second has no preset: none is loaded again, and the values go back one by one: the level, which the preset has as the plugin does, is not among them.
        ctl.apply(Intent::LoadPreset { node: second, preset: "room".to_string() }).unwrap();
        assert_eq!(engine.drain().0, ["LoadPreset inst=1 slot=1 id=room"]);
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        assert_eq!(commands.len(), 9, "{commands:?}");
        assert!(commands.iter().all(|command| command.starts_with("SetParameter inst=1 slot=1 param=")), "{commands:?}");
        assert!(commands.contains(&"SetParameter inst=1 slot=1 param=1 value=0.500".to_string()), "the size the user set: {commands:?}");
        assert!(!commands.iter().any(|command| command.contains("param=9")), "{commands:?}");
    }

    #[test]
    fn a_preset_that_the_plugin_had_by_a_name_it_has_no_preset_of_is_not_loaded_again_and_the_values_go_back() {
        let (engine, mut ctl, _clock) = controller_over(load(PRESETS).model);
        let reverb = node(&ctl, NodePath::Effect { inst: 1, index: 0 });
        // (A session file can say a preset that the plugin does not have, and it is the name the model keeps.)
        ctl.model.instruments[1].effects[0].current_preset = Some("Gone".to_string());
        let before = format!("{:?}", ctl.model.instruments[1].effects[0]);
        engine.drain();
        ctl.apply(Intent::LoadPreset { node: reverb, preset: "room".to_string() }).unwrap();
        assert_eq!(ctl.model.instruments[1].effects[0].current_preset.as_deref(), Some("Small Room"));
        engine.drain();
        ctl.undo().unwrap();
        let (commands, _) = engine.drain();
        assert!(commands.iter().all(|command| command.starts_with("SetParameter")), "no preset is loaded again: {commands:?}");
        assert_eq!(format!("{:?}", ctl.model.instruments[1].effects[0]), before);
    }

    #[test]
    fn a_preset_whose_plugin_is_gone_or_is_no_plugin_any_more_is_refused_with_the_reason_and_nothing_changes_and_the_step_is_dropped() {
        for what in ["the effect is gone", "its node is a group's", "its node is a bus effect's", "its node is a lane that has no instrument"] {
            let (engine, mut ctl, _clock) = controller_over(load(PRESETS).model);
            ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let reverb = node(&ctl, NodePath::Effect { inst: 1, index: 0 });
            ctl.apply(Intent::LoadPreset { node: reverb, preset: "room".to_string() }).unwrap();
            engine.drain();
            // Something takes the effect out without the history, and then something else has its id: the group of the first lane, or the reverb of its bus, or the second lane, which has
            // no instrument any more.
            ctl.model.instruments[1].effects.remove(0);
            match what {
                "its node is a group's" => ctl.model.groups[0].node = reverb,
                "its node is a bus effect's" => ctl.model.groups[0].effects[0].node = reverb,
                "its node is a lane that has no instrument" => {
                    ctl.model.instruments[1].node = reverb;
                    ctl.model.instruments[1].instrument = None;
                }
                _ => {}
            }
            let before = picture(&ctl);
            assert_eq!(ctl.undo().unwrap_err(), IntentError::Invalid(format!("node {} is not there to have its preset put back", reverb.0)), "{what}");
            assert_eq!(picture(&ctl), before, "{what}");
            assert!(engine.drain().0.is_empty(), "{what}: nothing was sent");
            assert_eq!((undo_label(&ctl).as_deref(), redo_label(&ctl)), (Some("lock the piano to its scale"), None), "{what}: the step is gone, the one before it is next");
        }
    }

    #[test]
    fn a_preset_that_is_refused_leaves_no_step_and_the_history_as_it_was() {
        let (_engine, mut ctl, _clock) = controller_over(load(PRESETS).model);
        ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let (reverb, oscillator, group) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Group(0)));
        let bus = node(&ctl, NodePath::GroupEffect { group: 0, index: 0 });
        let before = ctl.history_state();
        for (what, intent) in [
            ("an id the plugin lacks", Intent::LoadPreset { node: reverb, preset: "no-such".to_string() }),
            ("a name, not an id", Intent::LoadPreset { node: reverb, preset: "Cathedral".to_string() }),
            ("an instrument without presets", Intent::LoadPreset { node: oscillator, preset: "room".to_string() }),
            ("a bus effect, which has presets and does not load them", Intent::LoadPreset { node: bus, preset: "room".to_string() }),
            ("a group", Intent::LoadPreset { node: group, preset: "room".to_string() }),
            ("a node that is not there", Intent::LoadPreset { node: NodeId(424242), preset: "room".to_string() }),
        ] {
            // (The plan is no step either: what is refused is not looked at, but the plan says what it would be.)
            assert!(matches!(ctl.plan_history(&intent), Plan::Nothing), "{what}");
            assert!(ctl.apply(intent).is_err(), "{what}");
            assert_eq!(ctl.history_state(), before, "{what}");
        }
    }

    /// A session of `count` lanes that have an oscillator each.
    fn lanes_of_oscillators(count: usize) -> String {
        "[[instrument]]\nplugin = \"builtin:osc\"\n".repeat(count)
    }

    #[test]
    fn the_instruments_of_the_newest_replacements_are_kept_and_the_ones_of_an_older_replacement_are_loaded_again_with_what_the_user_set() {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(&lanes_of_oscillators(10));
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        // The instruments have a state that the model does not know: one that comes back has it, one that is loaded again has not. (The detune, set on the plugin itself.)
        let (detune, _) = param(&ctl, NodePath::Instrument(0), "detune");
        for lane in 0..10 {
            graph.poke_instrument(lane, detune, 0.3);
        }
        // Ten instruments are replaced, one plugin each: the history keeps eight, so the first two let go of theirs.
        for lane in 0..10 {
            let node = ctl.model.instruments[lane].node;
            ctl.apply(Intent::ReplaceInstrument { node, plugin: "builtin:osc".to_string() }).unwrap();
            graph.drain_commands();
        }
        for lane in 0..10 {
            assert_ne!(graph.plugin_parameter(lane, 0, detune), Some(0.3), "lane {lane}: another oscillator is in it");
        }
        for _ in 0..10 {
            ctl.undo().unwrap();
            graph.drain_commands();
        }
        let kept: Vec<bool> = (0..10).map(|lane| graph.plugin_parameter(lane, 0, detune) == Some(0.3)).collect();
        // The newest eight are the very instruments they were, and the two oldest (lanes 0 and 1) were loaded again, with the values of the model.
        assert_eq!(kept, [false, false, true, true, true, true, true, true, true, true]);
        assert_eq!(graph.plugin_parameter(0, 0, detune), Some(0.0));
    }

    #[test]
    fn an_instrument_that_a_running_audio_thread_hands_back_across_threads_is_put_back_at_once_and_is_not_loaded_again() {
        use crate::audio::{AudioEngine, NULL_DEVICE};
        use crate::plugin::chain::AudioGraph;
        // A real audio thread (the null backend renders on a timer), and an instrument that cannot be loaded again: undo has it only if the audio thread handed it over the channel,
        // across threads; if it did not it fails after the wait (five seconds here).
        let (mut handles, engine) = testutil::stub_engine_of(4096);
        handles.recovery_wait = Duration::from_secs(5);
        let model = testutil::load_onto(INSTRUMENTS, &handles.cmd_tx);
        let graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let mut audio = AudioEngine::build(graph, engine.midi_rx.clone(), Some(NULL_DEVICE), false, 48_000, 256).unwrap();
        let mut ctl = Controller::new(model, handles, None);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        audio.play().unwrap();
        ctl.model.instruments[1].instrument.as_mut().unwrap().id = "builtin:nothing".to_string();
        let lane = node(&ctl, NodePath::Instrument(1));
        ctl.apply(Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() }).unwrap();
        let started = Instant::now();
        let undone = ctl.undo();
        let waited = started.elapsed();
        audio.stop();
        undone.expect("the instrument came back from the audio thread");
        assert!(waited < Duration::from_secs(2), "{waited:?}: it did not wait for what was there");
        assert_eq!(ctl.model.instruments[1].instrument.as_ref().unwrap().id, "builtin:nothing");
    }

    /// A step of a script: an intent, or time going by (in milliseconds).
    enum Op {
        Do(Intent),
        Wait(u64),
    }

    /// The session of `SESSION` and what the controller says to scripts of intents and waits, as the web client's tests read it
    /// (`web/src/client/fixtures/history.json`): they pin the mock engine's history to the controller's, op by op. Each op is
    /// answered by the events it made (stamped, in the order they came), whether it changed the structure, what Undo and Redo would
    /// do after it, or the controller's words for a refusal. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_history_comes_to_is_current() {
        let (_engine, base, _clock) = controller();
        let (lane0, lane1) = (node(&base, NodePath::Instrument(0)), node(&base, NodePath::Instrument(1)));
        let (reverb, bus) = (node(&base, NodePath::Effect { inst: 0, index: 0 }), node(&base, NodePath::GroupEffect { group: 0, index: 0 }));
        let (g0, g1) = (node(&base, NodePath::Group(0)), node(&base, NodePath::Group(1)));
        let (lfo, envelope) = (base.model.free_modulators[0].node, base.model.free_modulators[1].node);
        let other_lfo = hearing_lane(&base.model, 1)[0].node;
        let edge = base.model.free_modulators[0].targets[0].edge;
        let other_edge = base.model.free_modulators[1].targets[0].edge;
        let (detune, _) = param(&base, NodePath::Instrument(0), "detune");
        let (volume, _) = param(&base, NodePath::Instrument(0), "volume");
        let (pan, _) = param(&base, NodePath::Instrument(0), "pan");
        // The same index in three nodes: the knob of one is not the knob of another.
        let (filter_fx, filter_bus) = (node(&base, NodePath::Effect { inst: 0, index: 1 }), bus);
        let (resonance, _) = param(&base, NodePath::Effect { inst: 0, index: 1 }, "resonance");
        let (bus_resonance, _) = param(&base, NodePath::GroupEffect { group: 0, index: 0 }, "resonance");
        assert_eq!((detune, resonance, bus_resonance), (1, 1, 1));
        let ghost = NodeId(9_999);
        // The ids that are handed out go on from where the session's end: what an add makes first has the next one.
        let first_node = base.model.nodes().map(|(id, _)| id.0).max().unwrap() + 1;
        let first_edge = base.model.free_modulators.iter().flat_map(|modulator| &modulator.targets).map(|target| target.edge.0).max().unwrap() + 1;
        let fresh = |n: u32| NodeId(first_node + n);
        let fresh_edge = |n: u32| EdgeId(first_edge + n);

        let set = |param: u32, value: f32| Op::Do(Intent::SetParam { node: lane0, param, value });
        let gain = |node: NodeId, gain: Gain, value: f32| Op::Do(Intent::SetGain { node, gain, value });
        let field = |node: NodeId, field: ModulatorField, value: f32| Op::Do(Intent::SetModulatorField { node, field, value });
        let name = |node: NodeId, name: &str| Op::Do(Intent::RenameGroup { node, name: name.to_string() });
        let undo = || Op::Do(Intent::Undo);
        let redo = || Op::Do(Intent::Redo);
        // A change is a step of its own when a second has gone by since the last of its kind.
        let pause = || Op::Wait(5000);
        let n_of = |n: usize, op: fn() -> Op| (0..n).map(|_| op()).collect::<Vec<Op>>();
        let add_effect = |parent: NodeId, plugin: &str| Op::Do(Intent::AddEffect { parent, plugin: plugin.to_string() });
        let add_instrument = |plugin: &str| Op::Do(Intent::AddInstrument { plugin: plugin.to_string() });
        let add_modulator = |parent: NodeId, kind: ModulatorKind| Op::Do(Intent::AddFreeModulator { kind, hears: Some(parent) });
        let add_group = |members: Vec<NodeId>| Op::Do(Intent::AddGroup { members });
        let cable = |modulator: NodeId, node: NodeId, index: u32| Op::Do(Intent::AddModEdge { modulator, to: Endpoint { node, port: Port::Param { index } } });
        let put_in = |node: NodeId, group: Option<NodeId>| Op::Do(Intent::SetLaneGroup { node, group });
        let route = |node: NodeId, route: Route| Op::Do(Intent::SetRoute { node, route });
        let feed = |node: NodeId, feed: Feed| Op::Do(Intent::SetFeed { node, feed });

        let mut scripts: Vec<(&str, Vec<Op>)> = vec![
            ("a parameter that was set is put back and set again", vec![set(detune, 3.0), undo(), redo(), undo(), undo(), redo(), redo()]),
            (
                "a drag is one step that goes back to before it",
                vec![set(detune, 1.0), Op::Wait(16), set(detune, 2.0), Op::Wait(16), set(detune, 3.0), Op::Wait(16), set(detune, 4.0), undo(), undo(), redo()],
            ),
            ("a pause of more than a second makes a step of its own", vec![set(detune, 1.0), Op::Wait(1500), set(detune, 2.0), undo(), undo(), undo()]),
            ("a pause of exactly a second does not", vec![set(detune, 1.0), Op::Wait(1000), set(detune, 2.0), undo(), undo()]),
            ("another thing is a step of its own, and so is going back to the first", vec![set(detune, 1.0), set(volume, 0.5), set(detune, 2.0), undo(), undo(), undo()]),
            ("a change after an undo ends what could be redone", vec![set(detune, 1.0), pause(), undo(), set(volume, 0.5), redo(), undo(), undo()]),
            ("an intent that is refused changes nothing in the history", vec![set(detune, 1.0), set(9_999, 1.0), gain(g0, Gain::Mix, 0.5), field(lane0, ModulatorField::Rate, 1.0), Op::Do(Intent::RemoveNode { node: ghost }), undo()]),
            ("with nothing done there is nothing to undo or redo", vec![undo(), redo()]),
            ("what cannot be undone empties the history, what could be redone too", vec![set(detune, 1.0), pause(), set(volume, 0.5), undo(), Op::Do(Intent::AddGroup { members: vec![lane0] }), undo(), redo()]),
            ("making a group of a lane that is in another is one of them", vec![set(detune, 1.0), Op::Do(Intent::AddGroup { members: vec![lane0] }), undo()]),
            (
                "a note length to follow the tempo by is a step in the words of the setting, put back and set again, and an envelope has none",
                vec![field(lfo, ModulatorField::Sync, 11.0), pause(), field(lfo, ModulatorField::Sync, 14.0), field(envelope, ModulatorField::Sync, 3.0), undo(), undo(), redo(), redo()],
            ),
            (
                "a tempo is put back, a drag of it is one step, and one that is held to the tempos there can be says what it is",
                vec![
                    Op::Do(Intent::SetBpm { bpm: 90.0 }),
                    Op::Wait(16),
                    Op::Do(Intent::SetBpm { bpm: 100.0 }),
                    Op::Wait(16),
                    Op::Do(Intent::SetBpm { bpm: 140.0 }),
                    pause(),
                    Op::Do(Intent::SetBpm { bpm: 5.0 }),
                    pause(),
                    Op::Do(Intent::SetBpm { bpm: 1000.0 }),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                    redo(),
                    redo(),
                ],
            ),
            (
                "what is done to a pattern is no step: undo goes back past it, and it forgets nothing",
                vec![
                    set(detune, 1.0),
                    pause(),
                    Op::Do(Intent::SetPatternField { node: lane0, field: PatternField::Length, value: 8.0 }),
                    Op::Do(Intent::SetRecording { node: lane0, recording: true }),
                    Op::Do(Intent::SetRecording { node: lane0, recording: false }),
                    Op::Do(Intent::SetPatternField { node: lane0, field: PatternField::Enabled, value: 1.0 }),
                    Op::Do(Intent::ClearPattern { node: lane0 }),
                    Op::Do(Intent::ClearPattern { node: lane0 }),
                    Op::Do(Intent::SetRecording { node: g0, recording: true }),
                    undo(),
                    undo(),
                    redo(),
                ],
            ),
        ];
        let mut gains = vec![gain(lane0, Gain::Volume, 2.5), pause(), gain(reverb, Gain::Mix, 0.125), pause(), gain(bus, Gain::Mix, 0.75), pause(), gain(g0, Gain::GroupVolume, 0.5), pause(), gain(g0, Gain::GroupPan, -0.5)];
        gains.extend(n_of(5, undo));
        gains.extend(n_of(5, redo));
        scripts.push(("the gains are put back each by its own words", gains));
        let mut modulators = vec![
            field(lfo, ModulatorField::Rate, 9.0),
            pause(),
            field(lfo, ModulatorField::Waveform, 1.0),
            pause(),
            field(lfo, ModulatorField::Waveform, 2.0),
            pause(),
            field(envelope, ModulatorField::Attack, 1.5),
            pause(),
            field(envelope, ModulatorField::Sustain, 0.9),
            pause(),
            Op::Do(Intent::SetEdgeDepth { edge, depth: -0.75 }),
            Op::Do(Intent::SetEdgeDepth { edge, depth: -0.5 }),
        ];
        modulators.extend(n_of(6, undo));
        modulators.extend(n_of(6, redo));
        scripts.push(("the settings of modulators and the depth of a cable are put back", modulators));
        let mut keys = vec![
            Op::Do(Intent::SetRange { node: lane0, range: Some(NoteRange { low: 36, high: 60 }) }),
            pause(),
            Op::Do(Intent::SetRange { node: lane0, range: None }),
            pause(),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: Some(NodeUi { x: 10.0, y: 20.0 }) }),
            Op::Do(Intent::SetNodeUi { node: lane1, ui: Some(NodeUi { x: 30.0, y: 40.0 }) }),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: None }),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: Some(NodeUi { x: 50.0, y: 60.0 }) }),
            Op::Do(Intent::SetNodeUi { node: lane0, ui: Some(NodeUi { x: 70.0, y: 80.0 }) }),
            pause(),
            Op::Do(Intent::SetPinned { node: lane0, params: vec![detune] }),
        ];
        keys.extend(n_of(8, undo));
        keys.extend(n_of(8, redo));
        scripts.push(("the keys, the places of nodes and the knobs are put back", keys));
        let mut names = vec![name(g0, "Strings"), name(g1, "Keys"), name(g1, ""), name(g0, "  ")];
        names.extend(n_of(4, undo));
        names.extend(n_of(4, redo));
        scripts.push(("the name of a group is put back, and a group that had none has its default again", names));
        let mut flags = vec![
            Op::Do(Intent::SetMute { node: lane0, muted: true }),
            Op::Do(Intent::SetSolo { node: lane0, solo: true }),
            Op::Do(Intent::SetMute { node: g0, muted: true }),
            Op::Do(Intent::SetSolo { node: g1, solo: true }),
            // (What changes nothing keeps no step, a node that is neither a lane nor a group is refused, and the master is not found.)
            Op::Do(Intent::SetMute { node: g0, muted: true }),
            Op::Do(Intent::SetMute { node: reverb, muted: true }),
            Op::Do(Intent::SetSolo { node: NodeId::MASTER, solo: true }),
            Op::Do(Intent::SetMute { node: lane0, muted: false }),
            Op::Do(Intent::SetSolo { node: g1, solo: false }),
        ];
        flags.extend(n_of(6, undo));
        flags.extend(n_of(6, redo));
        scripts.push(("a mute and a solo are steps in the words of the lane or the group, and what changes nothing is none", flags));
        let mut piano = vec![
            Op::Do(Intent::SetScale { scale: "F# dorian".to_string() }),
            pause(),
            Op::Do(Intent::SetScale { scale: "not a scale".to_string() }),
            Op::Do(Intent::SetLocked { locked: true }),
            pause(),
            Op::Do(Intent::SetLocked { locked: false }),
        ];
        piano.extend(n_of(3, undo));
        piano.extend(n_of(3, redo));
        scripts.push(("the scale and the lock of the piano are put back", piano));
        scripts.push(("an effect that was moved is put back, and the structure is said to have changed", vec![Op::Do(Intent::MoveEffect { node: reverb, index: 1 }), undo(), redo(), undo()]));

        // Each thing is a step of its own, though no time goes by: what a change is of is the node it is for and what it is for.
        let mut knobs = vec![
            set(detune, 1.0),
            Op::Do(Intent::SetParam { node: filter_fx, param: resonance, value: 2.0 }),
            Op::Do(Intent::SetParam { node: filter_bus, param: bus_resonance, value: 3.0 }),
            Op::Do(Intent::SetParam { node: filter_bus, param: bus_resonance, value: 4.0 }),
        ];
        knobs.extend(n_of(3, undo));
        knobs.extend(n_of(3, redo));
        scripts.push(("the same knob of another node is a step of its own, though no time has gone by", knobs));
        let mut gains_together = vec![
            gain(lane0, Gain::Volume, 2.0),
            gain(lane1, Gain::Volume, 3.0),
            gain(reverb, Gain::Mix, 0.25),
            gain(bus, Gain::Mix, 0.75),
            gain(g0, Gain::GroupVolume, 0.5),
            gain(g0, Gain::GroupPan, 0.5),
            gain(g0, Gain::GroupPan, 0.25),
        ];
        gains_together.extend(n_of(6, undo));
        gains_together.extend(n_of(6, redo));
        scripts.push(("a gain is a step for each node and each kind of gain, and the same one again is the same step", gains_together));
        let mut master = vec![
            Op::Do(Intent::SetLimiter { on: false }),
            gain(NodeId::MASTER, Gain::LimiterCeiling, -6.0),
            gain(NodeId::MASTER, Gain::MasterVolume, 0.5),
            // (The same gain again is the same step, and a volume is held to its range.)
            gain(NodeId::MASTER, Gain::MasterVolume, 9.0),
            // (A limiter that is as it is asked to be keeps no step.)
            Op::Do(Intent::SetLimiter { on: false }),
            Op::Do(Intent::SetLimiter { on: true }),
            // (A gain that is not the master's is refused for it, and the master's for every other node.)
            gain(NodeId::MASTER, Gain::Volume, 0.5),
            gain(lane0, Gain::MasterVolume, 0.5),
            gain(g0, Gain::LimiterCeiling, -3.0),
        ];
        master.extend(n_of(5, undo));
        master.extend(n_of(5, redo));
        scripts.push(("the limiter, the master's volume and the limiter's ceiling are steps in the words of what they do, and a gain that is not the master's is refused for it", master));
        let mut settings = vec![
            field(lfo, ModulatorField::Rate, 5.0),
            field(other_lfo, ModulatorField::Rate, 6.0),
            field(lfo, ModulatorField::Waveform, 2.0),
            field(envelope, ModulatorField::Attack, 1.0),
            field(envelope, ModulatorField::Decay, 1.0),
            field(envelope, ModulatorField::Attack, 2.0),
            field(envelope, ModulatorField::Attack, 3.0),
        ];
        settings.extend(n_of(6, undo));
        settings.extend(n_of(6, redo));
        scripts.push(("a setting of a modulator is a step for each modulator and each setting, and the same one again is the same step", settings));
        let mut cables = vec![
            Op::Do(Intent::SetEdgeDepth { edge, depth: -0.25 }),
            Op::Do(Intent::SetEdgeDepth { edge: other_edge, depth: 0.75 }),
            Op::Do(Intent::SetEdgeDepth { edge, depth: 0.125 }),
            Op::Do(Intent::SetEdgeDepth { edge, depth: 0.5 }),
        ];
        cables.extend(n_of(3, undo));
        cables.extend(n_of(3, redo));
        scripts.push(("the depth of a cable is a step for each cable, and the same one again is the same step", cables));
        // What is added is taken out by undo and put back by redo under the id it had, so that what was done to it goes on naming it.
        scripts.push(("an effect that is added is taken out by undo and put back by redo, and so is what was done to it", vec![
            add_effect(lane0, "builtin:reverb"),
            gain(fresh(0), Gain::Mix, 0.25),
            undo(),
            undo(),
            undo(),
            redo(),
            redo(),
            redo(),
            undo(),
        ]));
        scripts.push(("an effect on the bus of a group is added and taken out the same way", vec![add_effect(g0, "builtin:filter"), undo(), redo(), undo()]));
        scripts.push((
            "a modulator and its cables are taken out one at a time and put back under their ids",
            vec![
                add_modulator(lane0, ModulatorKind::Lfo),
                cable(fresh(0), filter_fx, resonance),
                Op::Do(Intent::SetEdgeDepth { edge: fresh_edge(0), depth: -0.25 }),
                add_modulator(lane0, ModulatorKind::Envelope),
                Op::Do(Intent::AddModEdge { modulator: fresh(1), to: Endpoint { node: fresh(0), port: Port::Rate } }),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a MIDI control, and a modulator of a group with cables to its bus and to a member",
            vec![
                add_modulator(lane1, ModulatorKind::MidiLearn),
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(1), bus, bus_resonance),
                cable(fresh(1), lane0, detune),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a macro is added with a cable, turned, taken out and put back at the value it had",
            vec![
                add_modulator(lane0, ModulatorKind::Macro),
                cable(fresh(0), lane0, detune),
                field(fresh(0), ModulatorField::Value, 0.75),
                field(fresh(0), ModulatorField::Value, 0.5),
                Op::Do(Intent::RemoveNode { node: fresh(0) }),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        let steps = |node: NodeId, edits: &[(usize, f32)]| {
            let mut values = ModSourceSlot::staircase();
            for &(step, value) in edits {
                values[step] = value;
            }
            Op::Do(Intent::SetSteps { node, values: values.to_vec() })
        };
        scripts.push((
            "a step sequencer is added with a cable, a stroke across its steps is one step, its settings are changed, and it is taken out and put back as it was",
            {
                let mut script = vec![
                    add_modulator(lane0, ModulatorKind::Steps),
                    cable(fresh(0), lane0, detune),
                    // A stroke: one bar after the other, in a moment.
                    steps(fresh(0), &[(0, 0.9)]),
                    steps(fresh(0), &[(0, 0.9), (1, 0.8)]),
                    steps(fresh(0), &[(0, 0.9), (1, 0.8), (2, 0.7)]),
                    pause(),
                    // Another, a while after: a step of its own.
                    steps(fresh(0), &[(0, 0.9), (1, 0.8), (2, 0.7), (15, -3.0)]),
                    field(fresh(0), ModulatorField::Length, 5.0),
                    field(fresh(0), ModulatorField::Glide, 0.5),
                    field(fresh(0), ModulatorField::Retrigger, 0.0),
                    field(fresh(0), ModulatorField::Rate, 3.0),
                    Op::Do(Intent::RemoveNode { node: fresh(0) }),
                ];
                // (Nine steps: the modulator, its cable, the stroke, the other steps, the four settings, and its removal.)
                script.extend(n_of(9, undo));
                script.extend(n_of(9, redo));
                script
            },
        ));
        scripts.push((
            "a velocity and a key are added with cables, one is taken out, and each goes back and comes again",
            vec![
                add_modulator(lane0, ModulatorKind::Velocity),
                cable(fresh(0), lane0, detune),
                add_modulator(lane0, ModulatorKind::Key),
                cable(fresh(1), lane0, volume),
                Op::Do(Intent::RemoveNode { node: fresh(0) }),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        // A modulator that is in no lane and no group (the session's own rack): a node with no parent that the graph lists after the lanes', and whose cables go to plugins of any lane or bus.
        scripts.push((
            "a modulator that is in no lane is added with cables to plugins of two lanes and of a bus, and to the settings of another like it, and each of them goes back under its id",
            {
                let mut script = vec![
                    Op::Do(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }),
                    cable(fresh(0), lane0, detune),
                    cable(fresh(0), lane1, detune),
                    cable(fresh(0), bus, bus_resonance),
                    cable(fresh(0), filter_fx, resonance),
                    Op::Do(Intent::SetEdgeDepth { edge: fresh_edge(1), depth: -0.25 }),
                    Op::Do(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }),
                    Op::Do(Intent::AddModEdge { modulator: fresh(1), to: Endpoint { node: fresh(0), port: Port::Rate } }),
                    Op::Do(Intent::RemoveNode { node: fresh(0) }),
                ];
                // (Nine steps: the LFO, its four cables, the depth of one, the envelope, its cable to the LFO's rate, and the LFO's removal with all its cables.)
                script.extend(n_of(9, undo));
                script.extend(n_of(9, redo));
                script
            },
        ));
        scripts.push((
            "modulators that are in no lane are refused what a rack cannot reach, and a modulator made for the rate of one is another that is in no lane",
            {
                let mut script = vec![
                    Op::Do(Intent::AddFreeModulator { kind: ModulatorKind::Steps, hears: None }),
                    Op::Do(Intent::AddFreeModulator { kind: ModulatorKind::MidiLearn, hears: None }),
                    cable(fresh(0), lane1, volume),
                    // Refused, and no step, no id and no slot for any: its own settings, the settings of a modulator of a lane (and a lane's of its), a group (which has no plugin) and a cable that is there.
                    Op::Do(Intent::AddModEdge { modulator: fresh(0), to: Endpoint { node: fresh(0), port: Port::Rate } }),
                    Op::Do(Intent::AddModEdge { modulator: fresh(0), to: Endpoint { node: lfo, port: Port::Rate } }),
                    Op::Do(Intent::AddModEdge { modulator: lfo, to: Endpoint { node: fresh(0), port: Port::Rate } }),
                    Op::Do(Intent::AddModEdge { modulator: fresh(0), to: Endpoint { node: g0, port: Port::Param { index: 0 } } }),
                    cable(fresh(0), lane1, volume),
                    // The rate of a modulator that is in no lane is driven by one that is in no lane.
                    Op::Do(Intent::Modulate { to: Endpoint { node: fresh(0), port: Port::Rate }, kind: ModulatorKind::Lfo, depth: None }),
                ];
                // (Four steps and then one that finds nothing: the steps, the control, the cable and the modulator made for the rate.)
                script.extend(n_of(5, undo));
                script.extend(n_of(4, redo));
                script
            },
        ));
        scripts.push((
            "what a modulator that is in no lane drives goes with the plugin when the plugin, its lane or its group is taken out, and comes back with it",
            {
                let mut script = vec![
                    Op::Do(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }),
                    cable(fresh(0), filter_fx, resonance),
                    cable(fresh(0), bus, bus_resonance),
                    cable(fresh(0), lane1, detune),
                    Op::Do(Intent::RemoveNode { node: filter_fx }),
                    Op::Do(Intent::RemoveNode { node: lane1 }),
                    Op::Do(Intent::RemoveNode { node: g0 }),
                ];
                // (Seven steps: the LFO, its three cables, and the three removals, each with the free cable that went with it.)
                script.extend(n_of(7, undo));
                script.extend(n_of(7, redo));
                script
            },
        ));
        scripts.push((
            "a modulator that is in no lane is made to hear a lane, a group and every note, the lane that it hears is taken out and put back, and what is refused or is as it is asked to be is no step",
            {
                let ear = |node: NodeId, ear: Option<NodeId>| Op::Do(Intent::SetModulatorEar { node, ear });
                let mut script = vec![
                    Op::Do(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: None }),
                    ear(fresh(0), Some(lane1)),
                    ear(fresh(0), Some(g0)),
                    // As it is: applied, and no step.
                    ear(fresh(0), Some(g0)),
                    ear(fresh(0), None),
                    ear(fresh(0), Some(lane1)),
                    Op::Do(Intent::RemoveNode { node: lane1 }),
                    // As it is, too: the lane is gone and the modulator hears every note, which is no step and does not forget the lane (Undo of the removal gives the ear back).
                    ear(fresh(0), None),
                    // Refused, and no step: a lane has no ear (only a modulator has), a plugin is not a lane or a group, and a node that is not there.
                    ear(lane0, Some(lane0)),
                    ear(fresh(0), Some(reverb)),
                    ear(fresh(0), Some(ghost)),
                    ear(ghost, None),
                ];
                // (Six steps: the envelope, four ears, and the lane's removal.)
                script.extend(n_of(6, undo));
                script.extend(n_of(6, redo));
                script
            },
        ));
        scripts.push((
            "an instrument is added and taken out as a lane, and put back with its id",
            vec![
                add_instrument("builtin:osc"),
                Op::Do(Intent::SetNodeUi { node: fresh(0), ui: Some(NodeUi { x: 5.0, y: 6.0 }) }),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        // The audio input (`builtin:input`, step 99) starts bare, as an effect that stands by itself does: its lane's way out is cut, so that it is heard only through the cables that are drawn from it, and a
        // cable from it to the mix is the way out given back (`link_or_cable`: a step in the words of the link). Undo takes the lane out and redo makes it again bare, under its id.
        scripts.push((
            "an audio input is added in a lane whose way out is cut, the cable from it to the mix gives the way out back, and undo and redo keep both",
            vec![
                add_instrument("builtin:input"),
                Op::Do(Intent::AddCable { from: Endpoint { node: fresh(0), port: Port::AudioOut }, to: Endpoint { node: NodeId::MASTER, port: Port::AudioIn }, gain: None }),
                Op::Do(Intent::AddCable { from: Endpoint { node: fresh(0), port: Port::AudioOut }, to: Endpoint { node: reverb, port: Port::AudioIn }, gain: Some(0.5) }),
                Op::Do(Intent::SetRoute { node: fresh(0), route: Route::None }),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                undo(),
            ],
        ));
        // An effect that stands by itself is a lane with the effect in it (the lane's node, and then the effect's, are the next two ids) and no way out (step 88: a bare module): undo takes the lane out
        // with it, and what is done to the lane, a mute and a way out given, is in the words of what it holds, the effect, or a chain of effects when there is more than one.
        scripts.push((
            "an effect that stands by itself is a lane that undo takes out with the effect in it and redo puts back, and the lane is called by what it holds",
            {
                let mut script = vec![
                    Op::Do(Intent::AddFreeEffect { plugin: "builtin:reverb".to_string() }),
                    gain(fresh(1), Gain::Mix, 0.25),
                    Op::Do(Intent::SetMute { node: fresh(0), muted: true }),
                    route(fresh(0), Route::Out),
                    add_effect(fresh(0), "builtin:filter"),
                    Op::Do(Intent::SetSolo { node: fresh(0), solo: true }),
                ];
                script.extend(n_of(6, undo));
                script.extend(n_of(6, redo));
                script
            },
        ));
        scripts.push((
            "an effect that stands by itself is refused for what an effect in a chain is, and a refusal leaves no step and uses no id",
            vec![
                Op::Do(Intent::AddFreeEffect { plugin: "builtin:osc".to_string() }),
                Op::Do(Intent::AddFreeEffect { plugin: "clap:com.example.nothing".to_string() }),
                Op::Do(Intent::AddFreeEffect { plugin: "builtin:filter".to_string() }),
                undo(),
                undo(),
                redo(),
            ],
        ));
        // A math effect has a second input (step 95): a cable drawn to it is a step of its own, at the port it was drawn to, which undo takes out and redo draws again under its id; the lane that holds the effect is
        // taken out with the cables at both its inputs (each written down) and put back with them.
        scripts.push((
            "a cable to the second input of a math effect is a step that undo takes out and redo draws again, and the effect is taken out with the cables at its two inputs and put back with them",
            {
                let math = fresh(1);
                let from = |node: NodeId| Endpoint { node, port: Port::AudioOut };
                let mut script = vec![
                    Op::Do(Intent::AddFreeEffect { plugin: "builtin:math".to_string() }),
                    Op::Do(Intent::AddCable { from: from(lane0), to: Endpoint { node: math, port: Port::AudioInSide }, gain: None }),
                    Op::Do(Intent::AddCable { from: from(lane1), to: Endpoint { node: math, port: Port::AudioIn }, gain: Some(0.5) }),
                    Op::Do(Intent::SetCableGain { edge: fresh_edge(0), gain: 0.25 }),
                    Op::Do(Intent::RemoveEdge { edge: fresh_edge(1) }),
                    Op::Do(Intent::RemoveNode { node: fresh(0) }),
                ];
                script.extend(n_of(6, undo));
                script.extend(n_of(6, redo));
                script
            },
        ));
        scripts.push((
            "a group is added with a lane that was in none, and taken out with the lane in none again",
            vec![add_group(vec![lane1]), name(fresh(0), "Keys"), undo(), undo(), redo(), redo(), add_group(vec![]), undo(), undo(), undo()],
        ));
        scripts.push((
            "a lane is put in a group, taken out of it, and moved from one to another, and each goes back",
            vec![
                put_in(lane1, Some(g1)),
                put_in(lane1, None),
                put_in(lane1, Some(g0)),
                put_in(lane0, Some(g1)),
                put_in(lane0, Some(g1)),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a lane that leaves a group whose modulator drives it cannot be put back, though one that is not driven can",
            vec![
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), lane0, detune),
                cable(fresh(0), bus, bus_resonance),
                put_in(lane1, Some(g0)),
                undo(),
                redo(),
                put_in(lane1, None),
                undo(),
                put_in(lane0, Some(g1)),
                undo(),
                redo(),
            ],
        ));
        scripts.push((
            "a lane that leaves a group that drives it for none is as much a barrier",
            vec![add_modulator(g0, ModulatorKind::Lfo), cable(fresh(0), lane0, detune), put_in(lane0, None), undo(), redo()],
        ));
        scripts.push((
            "a group made of a lane that is in another is a barrier, whatever else is in the list",
            vec![set(detune, 1.0), pause(), set(detune, 2.0), add_group(vec![lane1, lane0]), undo(), set(detune, 3.0), add_group(vec![lane0, lane1]), undo()],
        ));
        // What is taken out is put back by undo as it was, under its ids, and taken out again by redo.
        let remove_cable = |edge: EdgeId| Op::Do(Intent::RemoveEdge { edge });
        let drive_depth = |modulator: NodeId, node: NodeId, edge: EdgeId| Op::Do(Intent::AddModEdge { modulator, to: Endpoint { node, port: Port::Depth { edge } } });
        scripts.push((
            "a cable that is taken out is put back by undo and taken out again by redo, and what was done before it is still to be undone",
            vec![set(detune, 1.0), pause(), remove_cable(edge), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push((
            "the cables that drive the depth of a cable that is taken out go with it and come back with it",
            vec![drive_depth(envelope, lfo, edge), remove_cable(edge), undo(), redo(), undo(), undo(), redo(), redo(), redo()],
        ));
        scripts.push((
            "a cable of a group's modulator, and one that drives the depth of a member's, are put back each where it was",
            vec![
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), bus, bus_resonance),
                cable(fresh(0), lane0, pan),
                drive_depth(fresh(0), lfo, edge),
                remove_cable(fresh_edge(1)),
                undo(),
                remove_cable(edge),
                undo(),
                redo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        // A modulator goes with its own cables and the cables that address it, and comes back with them, each where it was and under the ids it had.
        let remove_node = |node: NodeId| Op::Do(Intent::RemoveNode { node });
        scripts.push((
            "a modulator that is taken out is put back by undo with its cable and taken out again by redo, and what was done before it is still to be undone",
            vec![set(detune, 1.0), pause(), remove_node(lfo), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push((
            "the cables that drive a modulator's settings go with it and come back with it",
            vec![
                Op::Do(Intent::AddModEdge { modulator: envelope, to: Endpoint { node: lfo, port: Port::Rate } }),
                remove_node(lfo),
                undo(),
                redo(),
                undo(),
                undo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a modulator that is not the last of its rack goes and comes back with the cables of the ones after it told their places",
            vec![
                add_modulator(lane0, ModulatorKind::Lfo),
                Op::Do(Intent::AddModEdge { modulator: fresh(0), to: Endpoint { node: envelope, port: Port::Attack } }),
                remove_node(lfo),
                undo(),
                remove_node(envelope),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "a modulator of a group, with cables to its bus and to a member, and a member's that the group drives, are put back each by its own rack",
            vec![
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), bus, bus_resonance),
                // (Not the detune, which the lane's LFO drives: of two cables that drive one parameter the layout of the telemetry names the first.)
                cable(fresh(0), lane0, pan),
                Op::Do(Intent::AddModEdge { modulator: fresh(0), to: Endpoint { node: lfo, port: Port::Rate } }),
                remove_node(lfo),
                undo(),
                remove_node(fresh(0)),
                undo(),
                redo(),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        // An effect goes with the cables that drive it and comes back with them, in its place in its chain, at its mix and under its id.
        let (cutoff, _) = param(&base, NodePath::Effect { inst: 0, index: 1 }, "cutoff");
        scripts.push((
            "an effect that is taken out is put back by undo in its place and taken out again by redo, and what was done before it is still to be undone",
            vec![gain(reverb, Gain::Mix, 0.125), pause(), remove_node(reverb), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push((
            "the cables that drive an effect, and the one that drives the depth of one of them, go with it and come back with it",
            vec![
                cable(envelope, filter_fx, resonance),
                cable(envelope, filter_fx, cutoff),
                drive_depth(lfo, envelope, fresh_edge(0)),
                remove_node(filter_fx),
                undo(),
                redo(),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "the first effect of a chain whose later effects have cables goes and comes back, and the cables of a group to a member's effect with it",
            vec![
                cable(envelope, filter_fx, resonance),
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), filter_fx, cutoff),
                remove_node(reverb),
                undo(),
                remove_node(filter_fx),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "an effect on the bus of a group, with a cable from the group's modulator, goes and comes back",
            vec![add_modulator(g0, ModulatorKind::Lfo), cable(fresh(0), bus, bus_resonance), remove_node(bus), undo(), redo(), undo(), undo(), undo()],
        ));
        // A lane goes with its own modulators and cables, its plugins and its pattern, after the cables of its group to it, and comes back with them: at its place among the lanes
        // and in its group, under its id, and what is done after it is put back first.
        scripts.push((
            "a lane that is taken out is put back by undo in its place and taken out again by redo, and what was done before it is still to be undone",
            vec![gain(reverb, Gain::Mix, 0.125), pause(), remove_node(lane1), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push((
            "a member of a group goes with the cables of its group to it and comes back with them",
            vec![add_modulator(g0, ModulatorKind::Lfo), cable(fresh(0), lane0, detune), remove_node(lane0), undo(), redo(), undo(), undo(), undo()],
        ));
        scripts.push((
            "the first of two members goes, and the cables of its group to the one after it are told its place and told it again when the first is back",
            vec![
                put_in(lane1, Some(g0)),
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), lane0, detune),
                cable(fresh(0), lane1, detune),
                remove_node(lane0),
                undo(),
                redo(),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "the last lane of a session is not taken out, and the history is as it was",
            vec![remove_node(lane0), remove_node(lane1), undo(), undo(), redo()],
        ));
        // What an add makes when it is redone has slots of the telemetry that are new: a removal that is redone after it puts back the session as it was before that redo, and not as it
        // was before the removal the first time.
        scripts.push((
            "a lane that an add made is taken out, and when the add has been undone and redone it is put back as the redo made it",
            vec![add_instrument("builtin:osc"), remove_node(fresh(0)), undo(), undo(), redo(), redo(), undo(), undo()],
        ));
        // A group goes with its bus effects, its modulators and their cables, and its members are in none; it comes back with them, at its place among the groups, its members put in
        // it again, under its id, and what was done after it is put back first.
        scripts.push((
            "a group that is taken out is put back by undo with its members, its bus effect and its cables, and taken out again by redo",
            vec![
                add_modulator(g0, ModulatorKind::Lfo),
                cable(fresh(0), lane0, detune),
                cable(fresh(0), bus, bus_resonance),
                remove_node(g0),
                undo(),
                redo(),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        scripts.push(("a group that has nothing in it is taken out and put back", vec![remove_node(g1), undo(), redo(), undo()]));
        scripts.push((
            "the group before another goes, and the lane of the one after it is told which group it is in, and again when the first is back",
            vec![put_in(lane1, Some(g1)), remove_node(g0), undo(), redo(), undo(), undo(), undo()],
        ));
        scripts.push((
            "a MIDI control with a cable is taken out and put back in the words of its own",
            vec![add_modulator(lane1, ModulatorKind::MidiLearn), cable(fresh(0), lane1, detune), remove_node(fresh(0)), undo(), redo(), undo(), undo(), undo()],
        ));
        scripts.push((
            "a change after an undo of a removal ends what could be redone",
            vec![remove_cable(edge), undo(), Op::Do(Intent::SetLocked { locked: true }), redo(), undo(), undo()],
        ));
        scripts.push((
            "a cable that is not there is refused and leaves the history as it was",
            vec![set(detune, 1.0), remove_cable(EdgeId(9_999)), remove_cable(fresh_edge(40)), undo(), undo()],
        ));
        scripts.push((
            "an add that is refused leaves the history as it was",
            vec![
                set(detune, 1.0),
                add_effect(lane0, "builtin:nothing"),
                add_effect(ghost, "builtin:reverb"),
                add_effect(lfo, "builtin:reverb"),
                add_effect(lane0, "builtin:osc"),
                add_instrument("builtin:nothing"),
                add_instrument("builtin:reverb"),
                add_modulator(ghost, ModulatorKind::Lfo),
                add_modulator(reverb, ModulatorKind::Lfo),
                cable(ghost, lane0, detune),
                cable(lfo, ghost, detune),
                cable(lfo, lane0, 9_999),
                cable(lfo, lane0, detune),
                cable(lfo, lane1, detune),
                add_group(vec![ghost]),
                add_group(vec![lane1, reverb]),
                put_in(ghost, None),
                put_in(lane0, Some(ghost)),
                put_in(lane0, Some(lane1)),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "what is added after an undo is a new node, and what could have been redone is gone",
            vec![add_effect(lane0, "builtin:reverb"), undo(), add_effect(lane0, "builtin:filter"), gain(fresh(1), Gain::Mix, 0.5), redo(), undo(), undo(), undo()],
        ));
        // A knob is modulated in one step: the modulator that is made for it and its cable are made together, and undo takes both out, and redo puts both back under their ids.
        let modulate = |node: NodeId, port: Port, kind: ModulatorKind, depth: Option<f32>| Op::Do(Intent::Modulate { to: Endpoint { node, port }, kind, depth });
        let param_of = |index: u32| Port::Param { index };
        scripts.push((
            "a parameter that is modulated has a new modulator and a cable to it in one step, and undo takes both out and redo puts both back",
            vec![
                modulate(filter_fx, param_of(resonance), ModulatorKind::Lfo, None),
                modulate(filter_fx, param_of(cutoff), ModulatorKind::Envelope, Some(-0.25)),
                // (The detune of the first lane is driven by its LFO: the layout of the telemetry names the value once, whichever cables drive it.)
                modulate(lane0, param_of(detune), ModulatorKind::Lfo, Some(0.75)),
                modulate(lane1, param_of(detune), ModulatorKind::MidiLearn, Some(1.0)),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                redo(),
                undo(),
            ],
        ));
        scripts.push((
            "a velocity and a key are made for knobs, at half depth when none is asked, and a setting of a modulator can be driven by a key",
            vec![
                modulate(lane0, param_of(volume), ModulatorKind::Velocity, None),
                modulate(lane0, param_of(pan), ModulatorKind::Key, Some(-0.5)),
                modulate(lfo, Port::Rate, ModulatorKind::Key, None),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                undo(),
            ],
        ));
        scripts.push((
            "a bus effect's parameter is modulated from a modulator of the group, and the depth that is asked for is clamped as a depth that is set is",
            vec![
                modulate(bus, param_of(bus_resonance), ModulatorKind::Lfo, Some(5.0)),
                modulate(bus, param_of(bus_resonance), ModulatorKind::Envelope, Some(-5.0)),
                undo(),
                undo(),
                redo(),
                redo(),
                undo(),
            ],
        ));
        scripts.push((
            "a setting of a modulator, and the depth of a cable, are modulated from a new modulator of the same rack",
            vec![
                modulate(lfo, Port::Rate, ModulatorKind::Lfo, None),
                modulate(envelope, Port::Attack, ModulatorKind::Envelope, Some(0.125)),
                modulate(lfo, Port::Depth { edge }, ModulatorKind::MidiLearn, Some(1.0)),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
                undo(),
            ],
        ));
        scripts.push((
            "a modulator that a modulate made is taken out whole by a removal, and put back by undo with its cable, and what was done before is still to be undone",
            vec![set(detune, 1.0), pause(), modulate(filter_fx, param_of(resonance), ModulatorKind::Lfo, None), remove_node(fresh(0)), undo(), redo(), undo(), undo(), undo()],
        ));
        scripts.push((
            "a modulate that is refused makes nothing and leaves the history as it was: a node that is not there, a group, a port that no cable ends at, and what a cable cannot end at",
            vec![
                set(detune, 1.0),
                modulate(ghost, param_of(detune), ModulatorKind::Lfo, None),
                modulate(NodeId::MASTER, param_of(detune), ModulatorKind::Lfo, None),
                modulate(g0, param_of(detune), ModulatorKind::Lfo, None),
                modulate(g0, Port::ModOut, ModulatorKind::Lfo, None),
                modulate(lane0, Port::ModOut, ModulatorKind::Lfo, None),
                modulate(lane0, Port::AudioIn, ModulatorKind::Lfo, None),
                modulate(lane0, param_of(9_999), ModulatorKind::Lfo, None),
                modulate(lfo, param_of(detune), ModulatorKind::Lfo, None),
                modulate(reverb, Port::Rate, ModulatorKind::Lfo, None),
                modulate(lfo, Port::Attack, ModulatorKind::Lfo, None),
                modulate(envelope, Port::Rate, ModulatorKind::Lfo, None),
                modulate(lfo, Port::Depth { edge: EdgeId(9_999) }, ModulatorKind::Lfo, None),
                undo(),
                undo(),
            ],
        ));
        // The instrument of a lane goes with the cables that drove it, and comes back with them; a lane that had none is emptied again.
        let replace = |node: NodeId, plugin: &str| Op::Do(Intent::ReplaceInstrument { node, plugin: plugin.to_string() });
        scripts.push((
            "an instrument that is replaced is put back by undo with the cables that drove it and replaced again by redo, and what was done before it is still to be undone",
            vec![set(detune, 1.0), pause(), set(volume, 0.5), pause(), replace(lane0, "builtin:osc"), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push(("the instrument of a lane that nothing drives is put back too", vec![replace(lane1, "builtin:osc"), undo(), redo(), undo()]));
        scripts.push((
            "the instrument of a lane that an add made is changed, and both are put back",
            vec![add_instrument("builtin:osc"), replace(fresh(0), "builtin:osc"), undo(), undo(), redo(), redo(), undo(), undo()],
        ));
        scripts.push((
            "an instrument that is refused changes nothing in the history",
            vec![set(detune, 1.0), replace(lane0, "builtin:reverb"), replace(reverb, "builtin:osc"), replace(ghost, "builtin:osc"), replace(lane0, "builtin:nothing"), undo()],
        ));
        // The instrument of a lane that is taken out leaves the lane, with the rest of what it has: it goes with the cables that drove it, and comes back with them.
        let clear = |node: NodeId| Op::Do(Intent::ClearInstrument { node });
        scripts.push((
            "an instrument that is taken out of its lane is put back by undo with the cables that drove it and taken out again by redo, and what was done before it is still to be undone",
            vec![set(detune, 1.0), pause(), set(volume, 0.5), pause(), clear(lane0), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push(("the instrument of a lane that nothing drives is taken out and put back too", vec![clear(lane1), undo(), redo(), undo()]));
        scripts.push((
            "the instrument of a lane that an add made is taken out, and both are put back",
            vec![add_instrument("builtin:osc"), clear(fresh(0)), undo(), undo(), redo(), redo(), undo(), undo()],
        ));
        scripts.push((
            "a lane that has no instrument, an effect, a group and a node that is not there are refused when an instrument is taken out of them, and change nothing in the history",
            vec![set(detune, 1.0), clear(lane1), clear(lane1), clear(reverb), clear(g0), clear(ghost), undo(), undo()],
        ));
        scripts.push((
            "an instrument that is put in a lane that its own was taken out of is taken out again by undo, and the first is put back by the one after it",
            vec![clear(lane1), replace(lane1, "builtin:osc"), undo(), undo(), redo(), redo()],
        ));
        // The volume of a lane, the mix of an effect and the volume and the pan of a group are what the host keeps for them, and a modulator can drive each as it drives a plugin's parameter (step 97): the cable is a step in the
        // words of a cable, a modulator made for the value ("modulate volume") is one step with its cable, and a node that has not the value refuses it.
        let to_port = |modulator: NodeId, node: NodeId, port: Port| Op::Do(Intent::AddModEdge { modulator, to: Endpoint { node, port } });
        let modulate_port = |node: NodeId, port: Port| Op::Do(Intent::Modulate { to: Endpoint { node, port }, kind: ModulatorKind::Lfo, depth: None });
        scripts.push((
            "a cable to the volume of a lane, the mix of an effect and of a bus effect and the volume and the pan of a group is put back and made again, and a node that has not the value refuses it",
            vec![
                to_port(lfo, lane0, Port::Volume),
                to_port(lfo, reverb, Port::Mix),
                to_port(lfo, bus, Port::Mix),
                to_port(lfo, g0, Port::Volume),
                to_port(lfo, g0, Port::Pan),
                to_port(lfo, lane0, Port::Pan),
                to_port(lfo, reverb, Port::Volume),
                to_port(lfo, g0, Port::Mix),
                to_port(lfo, lane0, Port::Volume),
                to_port(lfo, ghost, Port::Mix),
                gain(reverb, Gain::Mix, 0.7),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                undo(),
                undo(),
                undo(),
                undo(),
                redo(),
            ],
        ));
        scripts.push((
            "a modulator that is made for the volume of a lane, the mix of an effect or the pan of a group hears the lane or the group, and is one step with its cable",
            vec![
                modulate_port(lane0, Port::Volume),
                modulate_port(reverb, Port::Mix),
                modulate_port(bus, Port::Mix),
                modulate_port(g0, Port::Pan),
                modulate_port(g0, Port::Volume),
                modulate_port(lane0, Port::Pan),
                modulate_port(g1, Port::Mix),
                undo(),
                undo(),
                redo(),
                redo(),
                undo(),
                undo(),
                undo(),
                undo(),
                undo(),
            ],
        ));
        scripts.push((
            "what has a cable to its volume, its mix or its pan is taken out with the cable and put back with it by undo, and replacing an instrument leaves the cable to the lane's volume",
            vec![
                to_port(lfo, lane0, Port::Volume),
                to_port(lfo, reverb, Port::Mix),
                to_port(lfo, bus, Port::Mix),
                to_port(lfo, g0, Port::Pan),
                Op::Do(Intent::RemoveNode { node: reverb }),
                undo(),
                Op::Do(Intent::RemoveNode { node: bus }),
                undo(),
                Op::Do(Intent::RemoveNode { node: g0 }),
                undo(),
                Op::Do(Intent::RemoveNode { node: lane0 }),
                undo(),
                replace(lane0, "builtin:osc"),
                undo(),
                redo(),
                clear(lane0),
                undo(),
                redo(),
                undo(),
            ],
        ));
        // A preset that is loaded overwrites the values of the plugin and the preset it had: undo gives them back.
        let (damping, _) = param(&base, NodePath::Effect { inst: 0, index: 0 }, "damping");
        let load_preset = |node: NodeId, preset: &str| Op::Do(Intent::LoadPreset { node, preset: preset.to_string() });
        scripts.push((
            "a preset that is loaded is put back by undo with the values the plugin had and loaded again by redo, and what was done before it is still to be undone",
            vec![gain(reverb, Gain::Mix, 0.125), pause(), load_preset(reverb, "plate"), undo(), undo(), redo(), redo(), undo(), redo(), undo()],
        ));
        scripts.push((
            "a preset over another goes back to the first, and a value that was set over it is put back with it",
            vec![
                load_preset(reverb, "room"),
                Op::Do(Intent::SetParam { node: reverb, param: damping, value: 0.9 }),
                load_preset(reverb, "cathedral"),
                undo(),
                undo(),
                undo(),
                redo(),
                redo(),
                redo(),
            ],
        ));
        scripts.push((
            "a preset that is refused leaves the history as it was: an id the plugin lacks, an instrument that has none, a bus effect, a group and a node that is not there",
            vec![
                set(detune, 1.0),
                load_preset(reverb, "no-such"),
                load_preset(reverb, "Cathedral"),
                load_preset(lane0, "plate"),
                load_preset(bus, "plate"),
                load_preset(g0, "plate"),
                load_preset(ghost, "plate"),
                undo(),
            ],
        ));
        let barriers = [
            // A group made of a lane that is in another: the lane leaves it, and the cables of the group to the lane would go.
            Intent::AddGroup { members: vec![lane0] },
        ];
        let mut forgotten = Vec::new();
        for barrier in barriers {
            forgotten.extend([set(detune, 1.0), pause(), set(detune, 2.0), undo(), Op::Do(barrier), undo(), redo()]);
        }
        scripts.push(("each of what cannot be undone yet empties the history, and what could be redone", forgotten));

        // A session of its own for a script that needs what the base one has not: cables that drive each other's depths, which only a file can make (a cable
        // is made to drive a cable that is there already). The controller is not asked to put back what the first of them took out.
        let (_engine, cycle, _clock) = controller_over(load(testutil::CABLES_THAT_DRIVE_EACH_OTHER).model);
        let (cycle_lane, cycle_edge) = (node(&cycle, NodePath::Instrument(0)), cycle.model.free_modulators[0].targets[0].edge);
        let (cycle_detune, _) = param(&cycle, NodePath::Instrument(0), "detune");
        let cycle_set = |value: f32| Op::Do(Intent::SetParam { node: cycle_lane, param: cycle_detune, value });
        let cycle_modulator = cycle.model.free_modulators[0].node;
        // And one for the instruments: a lane with a note remap, and the cables of its group and its own to its instrument, and a lane that has none.
        let (_engine, instruments, _clock) = controller_over(load(INSTRUMENTS).model);
        let (member_lane, empty_lane) = (node(&instruments, NodePath::Instrument(0)), node(&instruments, NodePath::Instrument(2)));
        // And two for the audio cables: `SENDS`, which has none, and `PATCHED`, which has six that make some moves go round (the ids go on from the last cable there is).
        let (_engine, sends, _clock) = controller_over(load(SENDS).model);
        let s = Sends::of(&sends.model);
        let (_engine, patched, _clock) = controller_over(load(testutil::PATCHED).model);
        let (p_lane, p_fx, p_group) = (
            [node(&patched, NodePath::Instrument(0)), node(&patched, NodePath::Instrument(1)), node(&patched, NodePath::Instrument(2))],
            [node(&patched, NodePath::Effect { inst: 0, index: 0 }), node(&patched, NodePath::Effect { inst: 0, index: 1 }), node(&patched, NodePath::Effect { inst: 1, index: 0 })],
            node(&patched, NodePath::Group(0)),
        );
        let p_bus = [node(&patched, NodePath::GroupEffect { group: 0, index: 0 }), node(&patched, NodePath::GroupEffect { group: 0, index: 1 }), node(&patched, NodePath::GroupEffect { group: 0, index: 2 })];
        let p_cables: Vec<EdgeId> = patched.model.cables.iter().map(|cable| cable.id).collect();
        assert_eq!(p_cables.len(), 6);
        let draw_cable = |from: NodeId, to: NodeId, gain: Option<f32>| Op::Do(Intent::AddCable { from: Endpoint { node: from, port: Port::AudioOut }, to: Endpoint { node: to, port: Port::AudioIn }, gain });
        let level = |edge: EdgeId, gain: f32| Op::Do(Intent::SetCableGain { edge, gain });
        let take_out = |edge: EdgeId| Op::Do(Intent::RemoveEdge { edge });
        let lock = || Op::Do(Intent::SetLocked { locked: true });
        let mix = NodeId::MASTER;
        let (c0, c1) = (EdgeId(0), EdgeId(1));
        // And two for sources of notes: `NOTES`, which has the keys, a device and a cable of notes to its second lane, and `SENDS`, which has none (the ids go on from the last node there is, and the cables start at 0).
        let (_engine, noted, _clock) = controller_over(load(testutil::NOTES).model);
        let (n_keys, n_device) = (noted.model.sources[0].node, noted.model.sources[1].node);
        let n_lane = [node(&noted, NodePath::Instrument(0)), node(&noted, NodePath::Instrument(1)), node(&noted, NodePath::Instrument(2))];
        let (n_cable, n_audio) = (noted.model.event_cables[0].id, noted.model.cables[0].id);
        let first_source = sends.model.nodes().map(|(id, _)| id.0).max().unwrap() + 1;
        let (keys, seaboard) = (NodeId(first_source), NodeId(first_source + 1));
        let add_source = |source: SourceInfo| Op::Do(Intent::AddSource { source });
        let device = |name: &str| SourceInfo::Device { name: name.to_string() };
        let notes = |from: NodeId, to: NodeId| Op::Do(Intent::AddCable { from: Endpoint { node: from, port: Port::EventsOut }, to: Endpoint { node: to, port: Port::EventsIn }, gain: None });
        let place = |node: NodeId, x: f32, y: f32| Op::Do(Intent::SetNodeUi { node, ui: Some(NodeUi { x, y }) });
        let own_sessions: Vec<(&str, &str, Vec<Op>)> = vec![
            (
                "an audio cable that is drawn is taken out by undo and drawn again by redo under its id and at its level, and what was done before it is still to be undone",
                SENDS,
                vec![lock(), draw_cable(s.lane[1], s.fx[0], Some(0.5)), draw_cable(s.fx[2], mix, None), draw_cable(s.group, s.fx[2], Some(9.0)), undo(), undo(), undo(), undo(), redo(), redo(), redo(), redo(), undo()],
            ),
            (
                "the level of an audio cable is a step that a drag makes one of, put back to the level it found, and a level that is held says what it was held to",
                SENDS,
                vec![
                    draw_cable(s.lane[1], s.fx[0], Some(0.5)),
                    level(c0, 0.6),
                    Op::Wait(16),
                    level(c0, 0.7),
                    Op::Wait(16),
                    level(c0, 0.8),
                    pause(),
                    level(c0, 9.0),
                    pause(),
                    level(c0, -1.0),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                    redo(),
                    redo(),
                    redo(),
                ],
            ),
            (
                "an audio cable that is taken out is put back by undo as it was, at its place among the others, and taken out again by redo",
                SENDS,
                vec![
                    lock(),
                    draw_cable(s.lane[1], s.fx[0], Some(0.5)),
                    draw_cable(s.fx[2], mix, Some(0.25)),
                    draw_cable(s.group, s.fx[2], Some(2.0)),
                    take_out(c1),
                    undo(),
                    redo(),
                    take_out(c0),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                ],
            ),
            (
                "an audio cable that is refused is no step, and the history is as it was: a loop, a cable between the same two again, ends that are not, a mix with an output and a lane with an input",
                SENDS,
                vec![
                    lock(),
                    draw_cable(s.lane[1], s.fx[0], None),
                    draw_cable(s.fx[1], s.fx[0], None),
                    draw_cable(s.fx[0], s.fx[0], None),
                    draw_cable(s.lane[1], s.fx[0], Some(2.0)),
                    draw_cable(s.lane[0], s.lane[1], None),
                    draw_cable(mix, s.fx[0], None),
                    draw_cable(NodeId(9_999), s.fx[0], None),
                    draw_cable(s.lane[0], NodeId(9_999), None),
                    Op::Do(Intent::AddCable { from: Endpoint { node: s.lane[0], port: Port::AudioIn }, to: Endpoint { node: s.fx[0], port: Port::AudioIn }, gain: None }),
                    Op::Do(Intent::AddCable { from: Endpoint { node: s.lane[0], port: Port::AudioOut }, to: Endpoint { node: s.fx[0], port: Port::ModOut }, gain: None }),
                    level(EdgeId(9_999), 0.5),
                    take_out(EdgeId(9_999)),
                    Op::Do(Intent::SetEdgeDepth { edge: c0, depth: 0.5 }),
                    undo(),
                    undo(),
                    undo(),
                ],
            ),
            (
                "the audio cables at an effect that is taken out come back with it, in the order they had, by undo, and go again by redo",
                SENDS,
                vec![
                    lock(),
                    draw_cable(s.lane[1], s.fx[0], Some(0.5)),
                    draw_cable(s.fx[0], mix, Some(0.25)),
                    draw_cable(s.group, s.fx[2], None),
                    Op::Do(Intent::RemoveNode { node: s.fx[0] }),
                    undo(),
                    redo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                ],
            ),
            (
                "the audio cables at a lane, a group and the effect of a bus that are taken out come back with them by undo, each time, whole",
                SENDS,
                vec![
                    draw_cable(s.lane[1], s.fx[0], Some(0.5)),
                    draw_cable(s.fx[1], s.group, Some(1.5)),
                    draw_cable(s.group, s.fx[2], None),
                    draw_cable(s.bus, mix, Some(0.75)),
                    Op::Do(Intent::RemoveNode { node: s.lane[0] }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: s.group }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: s.bus }),
                    undo(),
                    redo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                ],
            ),
            (
                "an audio cable of the session's own is put back by undo with its level and its place among the others, and the effects that have some come back with them",
                testutil::PATCHED,
                vec![
                    take_out(p_cables[2]),
                    undo(),
                    redo(),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: p_fx[1] }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: p_bus[0] }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: p_lane[0] }),
                    undo(),
                    redo(),
                    undo(),
                ],
            ),
            (
                "a lane that is put in a group, and an effect that is moved, where the audio cables there are would make it go round, are refused and leave the history as it was; the others are steps",
                testutil::PATCHED,
                vec![
                    lock(),
                    Op::Do(Intent::SetLaneGroup { node: p_lane[1], group: Some(p_group) }),
                    Op::Do(Intent::SetLaneGroup { node: p_lane[2], group: Some(p_group) }),
                    Op::Do(Intent::MoveEffect { node: p_fx[0], index: 1 }),
                    Op::Do(Intent::MoveEffect { node: p_fx[1], index: 0 }),
                    Op::Do(Intent::MoveEffect { node: p_bus[0], index: 2 }),
                    Op::Do(Intent::MoveEffect { node: p_bus[2], index: 0 }),
                    Op::Do(Intent::MoveEffect { node: p_bus[1], index: 2 }),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                    redo(),
                    redo(),
                ],
            ),
            (
                "the way out of a lane and of a group that is cut is put back by undo and cut again by redo; one that is as it is asked to be, and one of what is neither a lane nor a group, is no step; and a way out that is given back where the audio cables there are would make it go round is refused",
                SENDS,
                vec![
                    lock(),
                    route(s.lane[1], Route::None),
                    route(s.group, Route::None),
                    route(s.lane[1], Route::None),
                    route(s.lane[0], Route::Out),
                    route(s.fx[0], Route::None),
                    route(s.bus, Route::None),
                    route(NodeId::MASTER, Route::None),
                    route(NodeId(9_999), Route::None),
                    draw_cable(s.group, s.fx[0], None),
                    route(s.lane[0], Route::None),
                    draw_cable(s.group, s.fx[0], Some(0.5)),
                    route(s.lane[0], Route::Out),
                    undo(),
                    undo(),
                    route(s.lane[0], Route::Out),
                    undo(),
                    undo(),
                    redo(),
                    redo(),
                    redo(),
                ],
            ),
            (
                "the link into an effect that is cut is put back by undo and cut again by redo, in a lane's chain and on a bus; one that is as it is asked to be, and one into what is no effect, is no step; a link that is given back where the audio cables there are would make it go round is refused; and an effect that is cut is taken out, moved and put back with its link still cut",
                SENDS,
                vec![
                    lock(),
                    feed(s.fx[0], Feed::None),
                    feed(s.bus, Feed::None),
                    feed(s.fx[0], Feed::None),
                    feed(s.fx[2], Feed::Chain),
                    feed(s.lane[0], Feed::None),
                    feed(s.group, Feed::None),
                    feed(NodeId::MASTER, Feed::None),
                    feed(NodeId(9_999), Feed::None),
                    // Lane 0's delay sent back to its filter is a loop while the filter feeds the delay: refused; with the link into the delay cut it is drawn, and the link is not given back.
                    draw_cable(s.fx[1], s.fx[0], None),
                    feed(s.fx[1], Feed::None),
                    draw_cable(s.fx[1], s.fx[0], Some(0.5)),
                    feed(s.fx[1], Feed::Chain),
                    Op::Do(Intent::RemoveNode { node: s.fx[0] }),
                    undo(),
                    Op::Do(Intent::MoveEffect { node: s.fx[0], index: 1 }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: s.bus }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: s.lane[0] }),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                    redo(),
                    redo(),
                    redo(),
                ],
            ),
            (
                "the keys and a device are sources that undo takes out and redo puts back under the ids they had, a cable of notes is a step of its own words that goes the same way, and what is refused is no step",
                SENDS,
                vec![
                    lock(),
                    add_source(SourceInfo::Keys),
                    add_source(device(" Seaboard ")),
                    notes(keys, s.lane[0]),
                    notes(seaboard, s.lane[0]),
                    notes(keys, s.lane[1]),
                    add_source(SourceInfo::Keys),
                    add_source(device("Seaboard")),
                    add_source(device("")),
                    notes(keys, s.lane[0]),
                    notes(s.lane[0], s.lane[1]),
                    notes(keys, s.fx[0]),
                    notes(NodeId(9_999), s.lane[1]),
                    notes(keys, NodeId(9_999)),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                    redo(),
                    redo(),
                    redo(),
                    redo(),
                    redo(),
                    undo(),
                ],
            ),
            (
                "a cable of notes that is taken out is put back by undo as it was, at its place among the others, and a source that is taken out comes back with its cables, at its place among the sources",
                SENDS,
                vec![
                    lock(),
                    add_source(SourceInfo::Keys),
                    add_source(device("Seaboard")),
                    notes(keys, s.lane[0]),
                    notes(seaboard, s.lane[1]),
                    notes(seaboard, s.lane[0]),
                    notes(keys, s.lane[1]),
                    Op::Do(Intent::RemoveEdge { edge: EdgeId(1) }),
                    undo(),
                    redo(),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: keys }),
                    undo(),
                    redo(),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: seaboard }),
                    undo(),
                    Op::Do(Intent::RemoveEdge { edge: EdgeId(9_999) }),
                    Op::Do(Intent::RemoveNode { node: NodeId(9_999) }),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                    undo(),
                ],
            ),
            (
                "a lane that has cables of notes comes back with them, and a source that is placed is put back with its place",
                SENDS,
                vec![
                    lock(),
                    add_source(SourceInfo::Keys),
                    notes(keys, s.lane[0]),
                    notes(keys, s.lane[1]),
                    place(keys, 10.0, 20.0),
                    place(keys, 30.0, 40.0),
                    Op::Do(Intent::RemoveNode { node: s.lane[0] }),
                    undo(),
                    redo(),
                    undo(),
                    undo(),
                    undo(),
                    redo(),
                ],
            ),
            (
                "the session's own sources and cables of notes are put back by undo: a cable, the keys with the cable that starts at it, a device that has none, and a lane that has a cable of notes and an audio cable",
                testutil::NOTES,
                vec![
                    Op::Do(Intent::RemoveEdge { edge: n_cable }),
                    undo(),
                    redo(),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: n_keys }),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: n_device }),
                    undo(),
                    redo(),
                    undo(),
                    Op::Do(Intent::RemoveNode { node: n_lane[1] }),
                    undo(),
                    redo(),
                    undo(),
                    Op::Do(Intent::RemoveEdge { edge: n_audio }),
                    undo(),
                    notes(n_device, n_lane[0]),
                    notes(n_keys, n_lane[0]),
                    notes(n_device, n_lane[2]),
                    undo(),
                    undo(),
                    undo(),
                ],
            ),
            (
                "cables that drive each other's depths are not put back, and what came before them is forgotten",
                testutil::CABLES_THAT_DRIVE_EACH_OTHER,
                vec![cycle_set(1.0), pause(), Op::Do(Intent::RemoveEdge { edge: cycle_edge }), undo(), redo()],
            ),
            (
                "a modulator whose cables drive the depth of another's cable and are driven by it is not put back either",
                testutil::CABLES_THAT_DRIVE_EACH_OTHER,
                vec![cycle_set(1.0), pause(), Op::Do(Intent::RemoveNode { node: cycle_modulator }), undo(), redo()],
            ),
            (
                "an instrument that the cables of its group and of its lane drive is put back with them, and a lane that had none is emptied again",
                INSTRUMENTS,
                vec![replace(member_lane, "builtin:osc"), undo(), redo(), undo(), replace(empty_lane, "builtin:osc"), undo(), redo(), undo()],
            ),
        ];

        let mut written = Vec::new();
        let sessions: Vec<(&str, Option<&str>, &Vec<Op>)> = scripts
            .iter()
            .map(|(what, ops)| (*what, None, ops))
            .chain(own_sessions.iter().map(|(what, session, ops)| (*what, Some(*session), ops)))
            .collect();
        for (what, own_session, ops) in sessions {
            let (engine, mut ctl, clock) = match own_session {
                Some(session) => controller_over(load(session).model),
                None => controller(),
            };
            ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
            let own_snapshot = own_session.map(|_| {
                let mut snapshot = wire(&ctl.snapshot());
                snapshot["telemetry"]["rev"] = serde_json::json!(1);
                snapshot
            });
            ctl.subscribe(Box::new(Page));
            let mut rows = Vec::new();
            for op in ops {
                match op {
                    Op::Wait(ms) => {
                        clock.advance(Duration::from_millis(*ms));
                        rows.push(serde_json::json!({ "wait": ms }));
                    }
                    Op::Do(intent) => {
                        let state = ctl.apply(intent.clone());
                        // (There is no audio thread to hand back the lane that a removal took out; reading the engine's commands answers them, as an audio thread would.)
                        engine.drain();
                        let history = ctl.history_state();
                        let history = serde_json::json!({ "undo": history.undo, "redo": history.redo });
                        rows.push(match state {
                            Err(error) => serde_json::json!({ "intent": wire(intent), "error": error.to_string(), "history": history }),
                            Ok(applied) => {
                                let mut row = serde_json::json!({ "intent": wire(intent), "events": wire(&applied.stamped()), "restructured": applied.restructured, "history": history });
                                // What the session comes to, when the structure changed: the nodes and the cables, with their ids, and the layout of the telemetry.
                                if applied.restructured {
                                    row["shape"] = testutil::shape_of(&ctl.snapshot());
                                }
                                row
                            }
                        });
                    }
                }
            }
            let mut script = serde_json::json!({ "name": what, "rows": rows });
            if let Some(snapshot) = own_snapshot {
                script["snapshot"] = snapshot;
            }
            written.push(script);
        }
        let mut snapshot = wire(&base.snapshot());
        // The layout's fingerprint comes from the hasher, which the standard library does not promise to keep the same.
        snapshot["telemetry"]["rev"] = serde_json::json!(1);
        let fixture = serde_json::json!({ "snapshot": snapshot, "scripts": written });
        let fresh = serde_json::to_string_pretty(&fixture).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/history.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a history comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    // -- The host-side values that cables drive (step 97) --

    /// `GAINS` over an audio thread, with an LFO that has a cable to each value that a cable can drive in it: the volume of the lane that has an instrument, the mix of its filter, the mix of the
    /// delay of the lane that has none, the mix of the group's reverb and its volume and pan. Each node, in the order of the cables.
    fn gains_over_an_audio_thread() -> (Controller, crate::plugin::chain::AudioGraph, [NodeId; 6]) {
        let (mut ctl, mut graph) = controller_over_an_audio_thread(testutil::GAINS);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: None }).unwrap();
        let lfo = ctl.model.free_modulators[0].node;
        let nodes = [
            ctl.model.instruments[0].node,
            ctl.model.instruments[0].effects[0].node,
            ctl.model.instruments[1].effects[0].node,
            ctl.model.groups[0].effects[0].node,
            ctl.model.groups[0].node,
            ctl.model.groups[0].node,
        ];
        for (node, port) in nodes.into_iter().zip([Port::Volume, Port::Mix, Port::Mix, Port::Mix, Port::Volume, Port::Pan]) {
            ctl.apply(Intent::AddModEdge { modulator: lfo, to: Endpoint { node, port } }).unwrap();
        }
        graph.drain_commands();
        (ctl, graph, nodes)
    }

    #[test]
    fn the_cables_to_a_host_side_value_go_with_what_has_it_and_undo_puts_them_back_in_the_model_the_file_and_the_audio_thread() {
        let (mut ctl, mut graph, nodes) = gains_over_an_audio_thread();
        let before = modulators_as_they_are(&ctl, &graph);
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 6);
        assert!(before.1.contains("gain = \"mix\"") && before.1.contains("gain = \"volume\"") && before.1.contains("gain = \"pan\""), "the file says which value each cable drives:\n{}", before.1);
        // What each node takes out with it: a filter its mix, a lane its volume and the mix of its effect (and the empty lane the mix of its delay), a bus effect its mix, and a group its
        // volume, its pan and the mix of its bus effects.
        for (node, goes) in [(nodes[1], 1), (nodes[3], 1), (ctl.model.instruments[1].node, 1), (nodes[0], 2), (nodes[4], 3)] {
            ctl.apply(Intent::RemoveNode { node }).unwrap();
            graph.drain_commands();
            assert_eq!(ctl.model.free_modulators[0].targets.len(), 6 - goes, "{node:?}");
            ctl.model.check_ids().unwrap();
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &before, &format!("{node:?} taken out and put back"));
            ctl.redo().unwrap();
            graph.drain_commands();
            assert_eq!(ctl.model.free_modulators[0].targets.len(), 6 - goes, "{node:?} again");
            ctl.undo().unwrap();
            graph.drain_commands();
            assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &before, &format!("{node:?} put back again"));
        }
    }

    #[test]
    fn replacing_the_instrument_of_a_lane_leaves_the_cable_to_its_volume_and_taking_it_out_takes_the_cable_and_undo_gives_it_back() {
        let (mut ctl, mut graph, nodes) = gains_over_an_audio_thread();
        let before = modulators_as_they_are(&ctl, &graph);
        let lane = nodes[0];
        // Another instrument in the lane is the lane's volume all the same.
        ctl.apply(Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() }).unwrap();
        graph.drain_commands();
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 6);
        ctl.undo().unwrap();
        graph.drain_commands();
        assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &before, "the instrument replaced and put back");
        // With no instrument a lane has no volume to drive.
        ctl.apply(Intent::ClearInstrument { node: lane }).unwrap();
        graph.drain_commands();
        let aims: Vec<Driven> = ctl.model.free_modulators[0].targets.iter().map(|t| t.aim).collect();
        assert!(!aims.contains(&Driven::Gain { node: lane, gain: crate::app::model::ModGain::Volume }), "{aims:?}");
        assert_eq!(aims.len(), 5);
        ctl.undo().unwrap();
        graph.drain_commands();
        assert_same_modulators(&modulators_as_they_are(&ctl, &graph), &before, "the instrument taken out and put back");
    }
}
