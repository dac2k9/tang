//! The controller: the one owner of a running session.
//!
//! Whichever thread runs a front-end (the TUI loop, the `play` loop, later a
//! headless server) owns a `Controller` and calls `poll` every iteration. It is
//! a plain struct, no threads inside; see `docs/web-ui.md` §3.1. It holds
//!
//! - **the session**: the model of what the loader put into the engine, the
//!   file it came from, whether it has changed since, and the tempo;
//! - **the engine's handles** (`EngineHandles`): the command channel to the
//!   audio thread, the plugin runtime, the notification receivers and the state
//!   shared with the note senders;
//! - **the plugin catalog**, scanned on a background thread (`app::catalog`);
//!
//! and it does the housekeeping every front-end used to do for itself, or
//! forget:
//!
//! - **Notifications.** What the audio thread reports (a pattern recording that
//!   has finished, a control a MIDI-learn modulator has captured) is folded
//!   into the model, and what the catalog scan finds into the catalog.
//!   `poll` returns what changed, so that a front-end redraws only what it
//!   shows.
//! - **Reaping.** When the audio thread swaps or removes a plugin it hands the
//!   old one back over a channel so that it is dropped here, on the owner
//!   thread. Dropping a plugin can take milliseconds and free memory, which the
//!   audio thread must not do. If nobody reads the channel it fills up (16
//!   slots), and from then on the audio thread drops them itself.
//! - **Device hot-plug.** New MIDI inputs are looked for once a second.
//!
//! The fields are public while the front-ends still edit the model themselves;
//! they close as those edits become intents (`docs/web-ui.md` step 6).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::app::catalog::Catalog;
use crate::app::history::History;
use crate::app::journal::{Inverse, Journal, Kept, Replaced};
use crate::app::intent::{Applied, IntentError};
use crate::app::load::fresh_slot;
use crate::app::module;
use crate::app::model::{
    AudioCable, CABLE_GAIN_MAX, Driven, EdgeId, GainTarget, GroupNode, InstrumentNode, Made, ModGain, ModSetting, ModSourceSlot, ModTargetSlot, ModulatorSlot, NodeId, NodePath, ParamTarget,
    PatternState, PluginSlot, SessionModel, default_effect_mix, mod_source_slot_to_graph, modulator_slot_commands,
};
use crate::app::save::to_save;
use crate::app::subscribers::{EventSink, SubscriberId, Subscribers};
use crate::audio::MidiEvent;
use crate::held_notes::HeldNotes;
use crate::midi::DeviceChanges;
use crate::midi_monitor::MidiMonitor;
use crate::note_input::NoteInput;
use crate::piano_filter::{PianoFilter, PianoMode};
use crate::source::SourceSet;
use crate::plugin::chain::{
    CHAIN_CHANNELS, CableFrom, CableProblem, CableTo, GraphCommand, MidiLearnedNotification, ModTarget, ModulatorLocation, PatternNotification,
};
use crate::plugin::{self, Plugin, Runtime};
use crate::session::{self, PianoConfig, Route};
use crate::telemetry::Telemetry;

/// How often `poll` looks for newly connected devices.
const DEVICE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What a `poll` changed, for the front-end to redraw from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// The model changed: a recording finished or a control was captured.
    pub model_changed: bool,
    /// Plugins joined the catalog.
    pub catalog_changed: bool,
}

/// Something that notices devices being plugged in and unplugged: the MIDI manager.
pub trait DeviceWatcher {
    /// Open whatever has been connected since the last call, and close what has gone: the devices that came and the devices that went, by their sources.
    fn poll_new_devices(&mut self) -> DeviceChanges;

    /// The sources of the open devices that the device called `name` in a session stands for: none while it is not plugged in.
    fn sources_of(&self, name: &str) -> SourceSet;

    /// The names of the devices that are open, in the order they were opened: what a client can make a source of. (With the `web` feature or in tests only: it is the web interface that tells clients.)
    #[cfg(any(test, feature = "web"))]
    fn open_names(&self) -> Vec<String>;
}

impl DeviceWatcher for crate::midi::MidiManager {
    fn poll_new_devices(&mut self) -> DeviceChanges {
        crate::midi::MidiManager::poll_new_devices(self)
    }

    fn sources_of(&self, name: &str) -> SourceSet {
        crate::midi::MidiManager::sources_of(self, name)
    }

    #[cfg(any(test, feature = "web"))]
    fn open_names(&self) -> Vec<String> {
        crate::midi::MidiManager::open_names(self)
    }
}

/// The channels to and from the running engine, and the state it shares with
/// the note senders: everything a `Controller` needs besides the session.
pub struct EngineHandles {
    /// Commands for the audio thread.
    pub cmd_tx: Sender<GraphCommand>,
    /// Notes for the audio thread, from the MIDI devices and the virtual piano.
    pub midi_tx: Sender<MidiEvent>,
    /// Plugins the audio thread has taken out of the graph.
    pub return_rx: Receiver<Box<dyn Plugin>>,
    /// Pattern recordings the audio thread has finished.
    pub pattern_rx: Receiver<PatternNotification>,
    /// Controls the audio thread has captured for a MIDI-learn modulator.
    pub learn_rx: Receiver<MidiLearnedNotification>,
    /// What plugins are loaded with (the shared LV2 world).
    pub runtime: Runtime,
    pub sample_rate: f32,
    pub max_block_size: usize,
    /// The notes currently held, written by every note source.
    pub held: Arc<HeldNotes>,
    /// The Piano tab's scale and Highlight / Locked mode, read by every note
    /// source.
    pub piano_filter: Arc<PianoFilter>,
    /// Incoming MIDI, tallied for `--debug`.
    pub midi_monitor: Arc<MidiMonitor>,
    /// What the audio thread publishes about itself: meters, for now.
    pub telemetry: Arc<Telemetry>,
    /// How long an undo waits for the audio thread to hand back a plugin that it was asked to keep (a block or two of audio) before it loads the plugin
    /// again.
    pub recovery_wait: Duration,
}

/// The command that tells the audio thread what the output mix is (step 102): its volume, whether its limiter is in the sound, the limiter's ceiling and the sample rate its look-ahead is made for.
pub(super) fn master_command(master: &crate::app::model::MasterNode, sample_rate: f32) -> GraphCommand {
    GraphCommand::SetMaster { volume: master.volume, limiter: master.limiter, ceiling: master.ceiling, sample_rate }
}

/// The command that gives the audio thread the rate of the LFO at `index` of the rack at `at`.
pub(super) fn rate_command(at: ModulatorLocation, index: usize, rate: f32) -> GraphCommand {
    GraphCommand::SetModulatorRate { location: at, mod_index: index, rate }
}

/// The command that gives the audio thread the stages of the envelope at `index` of the rack at `at`:
/// attack, decay, sustain and release.
pub(super) fn envelope_command(at: ModulatorLocation, index: usize, (attack, decay, sustain, release): (f32, f32, f32, f32)) -> GraphCommand {
    GraphCommand::SetModulatorEnvelopeParam { location: at, mod_index: index, attack, decay, sustain, release }
}

/// The command that gives the audio thread a macro of `value` in the place of the macro at `index` of the rack at `at`: a macro has nothing in the engine but its value,
/// so a new one is all that a change of the value is.
pub(super) fn macro_command(at: ModulatorLocation, index: usize, value: f32) -> GraphCommand {
    let source = crate::plugin::chain::ModSource::Macro { value };
    GraphCommand::SetModulatorSource { location: at, mod_index: index, source }
}

/// The command that gives the audio thread what the step sequencer at `index` of the rack at `at` goes through, how many of them are in use, how it glides and
/// whether a note starts it again: all of it, whichever of them was changed, since the model has it all and the command is the whole of it. Its rate is another command,
/// an LFO's (`rate_command`), and where it is in the pattern stays.
pub(super) fn steps_command(at: ModulatorLocation, index: usize, values: &[f32; crate::plugin::chain::MAX_STEPS], length: usize, glide: f32, retrigger: bool) -> GraphCommand {
    GraphCommand::SetModulatorSteps { location: at, index, values: *values, length, glide, retrigger }
}

/// The command that gives the audio thread what the LFO at `index` of the rack at `at` has of its key: whether a note starts it again at the start of its wave and the
/// seconds it takes to come in after a note. Both, whichever of them was changed, since the model has both; where it is in its wave stays.
pub(super) fn key_command(at: ModulatorLocation, index: usize, retrigger: bool, fade: f32) -> GraphCommand {
    GraphCommand::SetModulatorKey { location: at, index, retrigger, fade }
}

/// The command that gives the audio thread how many beats of the tempo a cycle of the LFO, or a step of the step sequencer, at `index` of the rack at `at` is (0 for none: it
/// goes at its own rate, which the audio thread has kept). Where it is stays.
pub(super) fn sync_command(at: ModulatorLocation, index: usize, beats: f32) -> GraphCommand {
    GraphCommand::SetModulatorSync { location: at, index, beats }
}

/// The command that gives the audio thread the depth of cable `target_index` of the modulator at `mod_index` of the rack at `at`.
pub(super) fn depth_command(at: ModulatorLocation, mod_index: usize, target_index: usize, depth: f32) -> GraphCommand {
    GraphCommand::SetModTargetDepth { location: at, mod_index, target_index, depth }
}

/// A modulation target to bind (`Controller::add_target`): the parameter it
/// drives, or the modulator field, and how strongly.
pub struct NewTarget {
    /// What it drives, by the node and the port.
    pub aim: Driven,
    pub param_name: String,
    pub depth: f32,
    /// The user's set value of the parameter, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

impl NewTarget {
    /// A cable to the host-side value `target` (`SessionModel::gain_targets`), pulling `depth`.
    pub fn to_gain(target: &GainTarget, depth: f32) -> NewTarget {
        let (param_min, param_max) = target.gain.range();
        NewTarget { aim: target.aim(), param_name: target.gain.name().to_string(), depth, base_value: target.value, param_min, param_max }
    }

    /// A cable to the plugin parameter `target` (`SessionModel::param_targets`), pulling `depth`.
    pub fn to_param(target: &ParamTarget, depth: f32) -> NewTarget {
        NewTarget {
            aim: target.aim(),
            param_name: target.param_name.clone(),
            depth,
            base_value: target.base_value,
            param_min: target.param_min,
            param_max: target.param_max,
        }
    }
}

/// How the session is written to a file: over what is there (a save), or only where nothing is, so that the write itself refuses a path that is taken
/// (a save as another name: `session::save_new`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileMode {
    Replace,
    CreateNew,
}

/// A chain of effects: a lane's, or a group's bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChainAt {
    Lane(usize),
    Bus(usize),
}

/// An instrument has to have an audio output, or there is nothing of it to hear (the first two are what is).
pub(super) fn has_audio_output(instrument: &dyn Plugin) -> Result<(), IntentError> {
    if instrument.audio_output_count() == 0 {
        return Err(IntentError::Invalid(format!("{} has no audio outputs", instrument.name())));
    }
    Ok(())
}

/// An effect has as many audio outputs as the chains have channels (`CHAIN_CHANNELS`), or there is no place for it: the audio thread turns one that has not
/// away (it logs why and hands the plugin back), so a model that kept it would list a plugin that is not in the audio path. The one rule of the selector,
/// of undo and of the loader; `name` is what the refusal calls the plugin.
pub(super) fn outputs_fit_the_chain(name: &str, outputs: usize) -> Result<(), IntentError> {
    if outputs != CHAIN_CHANNELS {
        let s = if outputs == 1 { "" } else { "s" };
        return Err(IntentError::Invalid(format!("{name} has {outputs} audio output{s}, and an effect here needs {CHAIN_CHANNELS}")));
    }
    Ok(())
}

pub struct Controller {
    /// The instrument lanes and the submix groups.
    pub model: SessionModel,
    /// Where `Ctrl+S` writes.
    pub session_path: Option<PathBuf>,
    /// The session differs from what was last saved.
    pub dirty: bool,
    /// How many intents have been applied (`Controller::apply`).
    pub rev: u64,
    /// The installed plugins, for the selector popups. Empty until
    /// `catalog.scan()` is called: sessions that never open a selector (`play`)
    /// do not pay for it.
    pub catalog: Catalog,
    /// The directory of the library of modules (`app::module`): a file for each chain of effects that is saved. None for a controller that has no library (a test's), which lists none and
    /// saves none; the program's is `~/.config/tang/modules`, or where `TANG_MODULES_DIR` says.
    pub modules_dir: Option<PathBuf>,

    pub cmd_tx: Sender<GraphCommand>,
    pub midi_tx: Sender<MidiEvent>,
    pub pattern_rx: Receiver<PatternNotification>,
    pub learn_rx: Receiver<MidiLearnedNotification>,
    pub runtime: Runtime,
    pub sample_rate: f32,
    pub max_block_size: usize,
    pub held: Arc<HeldNotes>,
    pub piano_filter: Arc<PianoFilter>,
    pub midi_monitor: Arc<MidiMonitor>,
    pub telemetry: Arc<Telemetry>,
    /// How long an undo waits for the audio thread to hand back a plugin (`EngineHandles::recovery_wait`).
    pub(super) recovery_wait: Duration,

    /// Plugins the audio thread has taken out of the graph.
    return_rx: Receiver<Box<dyn Plugin>>,
    pub(super) devices: Option<Box<dyn DeviceWatcher>>,
    last_device_poll: Instant,
    /// What the audio thread was last told that each lane hears (`app::sources`), by the lane's node: a lane that is not here has not been told, and hears every source until it is.
    pub(super) sent_sources: Vec<(NodeId, SourceSet)>,
    /// Who hears what `apply` does.
    subscribers: Subscribers,
    /// What can be undone and redone (`app::history`).
    pub(super) history: History,
    /// The ids the intent that was carried out last made: what an add that is undone and done again has to be given again.
    pub(super) made_last: Made,
    /// What the intent that is being carried out has taken out so far (`app::journal`); none when no intent is.
    pub(super) journal: Option<Journal>,
    /// What the intent that was carried out last took out: what a removal that is undone has to put back.
    pub(super) taken_last: Journal,
    /// What time it is, for changes that go on to be told from those that are made again (a test turns it).
    pub(super) clock: Box<dyn Fn() -> Instant + Send>,
    /// Which nodes are watched, and by whom, and the ring each has (`app::probes`).
    pub(super) probes: super::probes::Probes,
    /// Which plugin's window the change that is being applied came from (step 112; `app::windows`), so that the window is not told back the value it has just made, in the middle of a drag.
    pub(super) editing_from: Option<crate::plugin::editors::EditorId>,
    /// Whether this machine can show a plugin's window, and why it cannot if it cannot (`window::available`; a test gives its own).
    pub window_check: fn() -> Result<(), String>,
}

impl Controller {
    /// A controller for a freshly loaded session: nothing has changed yet.
    pub fn new(model: SessionModel, engine: EngineHandles, session_path: Option<PathBuf>) -> Self {
        Self::starting_at(model, engine, session_path, Instant::now())
    }

    fn starting_at(
        model: SessionModel,
        engine: EngineHandles,
        session_path: Option<PathBuf>,
        now: Instant,
    ) -> Self {
        // (A lane that was just loaded hears every source: that is what the audio thread has until it is told another.)
        let sent_sources = model.instruments.iter().map(|lane| (lane.node, SourceSet::ALL)).collect();
        let EngineHandles {
            cmd_tx,
            midi_tx,
            return_rx,
            pattern_rx,
            learn_rx,
            runtime,
            sample_rate,
            max_block_size,
            held,
            piano_filter,
            midi_monitor,
            telemetry,
            recovery_wait,
        } = engine;
        let mut controller = Controller {
            rev: 0,
            catalog: Catalog::default(),
            modules_dir: None,
            model,
            session_path,
            dirty: false,
            cmd_tx,
            midi_tx,
            pattern_rx,
            learn_rx,
            runtime,
            sample_rate,
            max_block_size,
            held,
            piano_filter,
            midi_monitor,
            telemetry,
            recovery_wait,
            return_rx,
            devices: None,
            last_device_poll: now,
            sent_sources,
            subscribers: Subscribers::default(),
            history: History::default(),
            made_last: Made::default(),
            journal: None,
            taken_last: Journal::default(),
            clock: Box::new(Instant::now),
            probes: super::probes::Probes::default(),
            editing_from: None,
            window_check: crate::window::available,
        };
        // (A lane that cables of notes end at hears the sources they start at: the audio thread is told, as it is when a device is plugged in.)
        controller.sync_lane_sources();
        controller
    }

    /// The ids the intent that was carried out last made.
    #[cfg(test)]
    pub fn last_made(&self) -> &Made {
        &self.made_last
    }

    /// Load `source` (a plugin path or id) with the model slot that describes it,
    /// every parameter at the value the plugin reports, in a node of its own. The
    /// plugin is for the audio thread; None, after logging why, if it does not load.
    /// (An instrument takes its lane's node instead, in `set_instrument`. Only the tests
    /// load a plugin like this: the front-ends ask for one by an intent, which loads
    /// from the catalog.)
    #[cfg(test)]
    pub fn load_plugin(&mut self, source: &str) -> Option<(Box<dyn Plugin>, PluginSlot)> {
        let mut loaded = match plugin::load(source, self.sample_rate, self.max_block_size, &self.runtime) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to load plugin '{source}': {e}");
                return None;
            }
        };
        let slot = fresh_slot(loaded.as_mut(), source, self.model.alloc_node());
        Some((loaded, slot))
    }

    /// Make `loaded`/`slot` the instrument plugin of lane `inst`, on the audio
    /// thread and in the model. The plugin sits in the lane's node.
    pub fn set_instrument(&mut self, inst: usize, loaded: Box<dyn Plugin>, slot: PluginSlot) {
        // (A call that no intent asks for is no step, and what was set on the plugin that goes is addressed by its parameters' places, which are the new plugin's
        // now: what could be undone cannot be. When an intent is carried out, the instrument that goes is written down for the history, `install_instrument`.)
        self.forget_the_history_unless_an_intent_asks();
        self.install_instrument(inst, loaded, slot);
    }

    /// `set_instrument`, leaving the history as it is: for a lane that has just been made, which nothing in the history names, and for the intent that replaces
    /// an instrument, which has its step. When an intent is carried out, the instrument that goes is handed by the audio thread to a channel of its own, and written
    /// down with the model's plugin and the lane's note remap (`Inverse::Instrument`), which go with it: undo swaps it back in. A lane that had none is written down
    /// as that, for undo to empty it again.
    fn install_instrument(&mut self, inst: usize, loaded: Box<dyn Plugin>, mut slot: PluginSlot) {
        let inst_buf = (0..loaded.audio_output_count()).map(|_| Vec::new()).collect();
        let had_one = self.model.instruments.get(inst).is_some_and(|lane| lane.instrument.is_some());
        let (keep, kept) = if had_one { self.channel_for() } else { (None, Kept::Nothing) };
        let _ = self.cmd_tx.send(GraphCommand::SwapInstrument {
            inst,
            instrument: loaded,
            inst_buf,
            remapper: None,
            keep,
        });
        if let Some(lane) = self.model.instruments.get_mut(inst) {
            slot.node = lane.node;
            let node = lane.node;
            let before = lane.instrument.replace(slot).map(|old| Replaced { slot: old, remap: std::mem::take(&mut lane.remap), pitch_bend_range: lane.pitch_bend_range, kept });
            lane.forget_remap();
            self.took_out(Inverse::Instrument { lane: node, before });
        }
    }

    /// Add an empty instrument lane at the end, full range and in no group: the
    /// audio thread gets it with a meter, the model a node of its own for it.
    /// Returns its index. (The lane makes no sound until an instrument is put in.)
    pub fn add_lane(&mut self) -> usize {
        let inst = self.model.instruments.len();
        let node = self.model.alloc_node();
        let _ = self.cmd_tx.send(GraphCommand::AddInstrument { range: None, node });
        let meter = self.model.slots.meter();
        if let Some(slots) = meter {
            let _ = self.cmd_tx.send(GraphCommand::SetLaneMeter { inst, slots });
        }
        self.model.instruments.push(InstrumentNode {
            node,
            range: None,
            transpose: 0,
            volume: 1.0,
            muted: false,
            solo: false,
            route: Route::Out,
            instrument: None,
            effects: Vec::new(),
            pattern: None,
            pitch_bend_range: 2.0,
            remap: Default::default(),
            group: None,
            meter,
        });
        inst
    }

    /// Add an empty submix group at the end, with a meter: the audio thread gets
    /// it and the model a node of its own for it. Returns its index.
    pub fn add_group(&mut self) -> usize {
        let group = self.model.groups.len();
        let node = self.model.alloc_node();
        let _ = self.cmd_tx.send(GraphCommand::AddGroup { node });
        let meter = self.model.slots.meter();
        if let Some(slots) = meter {
            let _ = self.cmd_tx.send(GraphCommand::SetGroupMeter { group, slots });
        }
        self.model.groups.push(GroupNode {
            node,
            name: None,
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            route: Route::Out,
            effects: Vec::new(),
            meter,
        });
        group
    }

    /// Add a modulator with `source` at the end of the rack at `at`: the audio thread gets it, the model a node of its own for it.
    /// Returns its index in the rack, or `None`, sending nothing, if there is no
    /// such rack.
    #[cfg(test)]
    pub fn add_modulator(&mut self, at: ModulatorLocation, source: ModSourceSlot) -> Option<usize> {
        self.add_modulator_hearing(at, source, None)
    }

    /// `add_modulator`, for a modulator that hears the lane or the group `ear` (the node of one: the caller has looked) and not every note (the audio thread is
    /// told it with the modulator, as the loader tells it, so that Undo and Redo give it back).
    pub fn add_modulator_hearing(&mut self, at: ModulatorLocation, source: ModSourceSlot, ear: Option<NodeId>) -> Option<usize> {
        let index = self.model.modulators_mut(at)?.len();
        let graph_source = mod_source_slot_to_graph(&source);
        let node = self.model.alloc_node();
        let _ = self.cmd_tx.send(GraphCommand::InsertModulator { location: at, index, source: graph_source, node });
        // Its output is published from now on, and where it is if it is a step sequencer.
        let (output, position) = ModulatorSlot::alloc_slots(&mut self.model.slots, &source);
        for command in modulator_slot_commands(at, index, output, position, ear) {
            let _ = self.cmd_tx.send(command);
        }
        self.model.modulators_mut(at)?.push(ModulatorSlot { node, source, targets: Vec::new(), output, position, ear });
        Some(index)
    }

    /// Put the lanes at `a` and `b` in each other's places, each with everything it has: in the model, and
    /// on the audio thread, which is told to do the same (`SessionModel::swap_lanes`). No intent asks for this, so
    /// the history is forgotten (`forget_the_history_unless_an_intent_asks`). False, sending nothing and keeping
    /// the history, if either is not there or they are one.
    pub fn swap_lanes(&mut self, a: usize, b: usize) -> bool {
        if !self.model.swap_lanes(a, b) {
            return false;
        }
        // (A step that took something out names its lane by its place: it would put it back in whatever lane is there now, with cables addressing the
        // plugins and modulators of the lane it was in.)
        self.forget_the_history_unless_an_intent_asks();
        let _ = self.cmd_tx.send(GraphCommand::SwapInstruments { inst_a: a, inst_b: b });
        true
    }

    /// Take the instrument plugin out of lane `inst`, which is left with what else it has: its effects, its modulators, its volume and range. False, sending nothing, if there is no
    /// such lane or it has no plugin. The intent `ClearInstrument` asks for it, and then the plugin that goes is handed by the audio thread to a channel of its own and written down
    /// with the model's slot and the lane's note remap, which go with it (`Inverse::Instrument`, as when an instrument is replaced): undo swaps it back in, after the cables that drove it.
    /// A call that no intent asks for (the tests') forgets the history instead, since what was set on the plugin is addressed by its parameters' places, which another plugin in the lane
    /// would have.
    pub fn clear_instrument(&mut self, inst: usize) -> bool {
        let Some(lane) = self.model.instruments.get(inst) else { return false };
        if lane.instrument.is_none() {
            return false;
        }
        let lane_node = lane.node;
        self.forget_the_history_unless_an_intent_asks();
        // The cables that drive the plugin's parameters go first: there is nothing for them to drive. So do those that drive the lane's volume, which a lane that has no instrument has not.
        for edge in self.model.cables_into_plugin(lane_node).into_iter().chain(self.model.cables_into_gains_of(lane_node)) {
            // (One that an earlier one took with it is not there any more.)
            self.take_out(edge);
        }
        let (keep, kept) = self.channel_for();
        let _ = self.cmd_tx.send(GraphCommand::ClearInstrument { inst, keep });
        if let Some(lane) = self.model.instruments.get_mut(inst) {
            let node = lane.node;
            let before = lane.instrument.take().map(|old| Replaced { slot: old, remap: std::mem::take(&mut lane.remap), pitch_bend_range: lane.pitch_bend_range, kept });
            lane.forget_remap();
            self.took_out(Inverse::Instrument { lane: node, before });
        }
        true
    }

    /// Put the instrument `source` in lane `inst`, replacing the one it has (an empty lane is filled): loaded here, from the
    /// catalog only, with the lane's node. The cables that drove the old plugin's parameters go first, since the new one has
    /// parameters of its own; the lane keeps what else it has (effects, modulators, range, volume, group), but not a note
    /// remap, which was for the old plugin (the audio thread drops it with the plugin). Refused, with nothing changed and
    /// nothing sent, if there is no such lane; if `source` is not a plugin the catalog names, or is not an instrument, or does
    /// not load, or has no audio outputs.
    pub fn put_instrument(&mut self, inst: usize, source: &str) -> Result<(), IntentError> {
        let loaded = self.load_from_catalog(source, true)?;
        self.place_instrument(inst, loaded, source)
    }

    /// Add a lane at the end with the instrument `source` in it, as `put_instrument` loads it: the lane is full range and in no
    /// group, and has a meter, and its way out is cut if the plugin says so (`Plugin::starts_bare`: the audio input). Its node. Refused, with no lane made, for what `put_instrument` refuses.
    pub fn add_instrument_lane(&mut self, source: &str) -> Result<NodeId, IntentError> {
        let loaded = self.load_from_catalog(source, true)?;
        self.place_instrument_in_new_lane(loaded, source)
    }

    /// Add a lane with `loaded`, the plugin `source`, in it (`add_instrument_lane` says how, and when not).
    fn place_instrument_in_new_lane(&mut self, mut loaded: Box<dyn Plugin>, source: &str) -> Result<NodeId, IntentError> {
        has_audio_output(loaded.as_ref())?;
        let inst = self.add_lane();
        if loaded.starts_bare() {
            // (Cut before the instrument is in, as for an effect that stands by itself: a way out that was there for a moment would be heard. An audio input is a microphone, and it is heard where
            // cables are drawn from it: through the speakers that it listens to, by itself, it would howl.)
            self.model.instruments[inst].route = Route::None;
            let _ = self.cmd_tx.send(GraphCommand::SetLaneRoute { inst, route: Route::None });
        }
        let node = self.model.instruments[inst].node;
        let slot = fresh_slot(loaded.as_mut(), source, node);
        self.install_instrument(inst, loaded, slot);
        Ok(node)
    }

    /// Put `loaded`, the plugin `source`, in lane `inst` (`put_instrument` says how, and when not).
    fn place_instrument(&mut self, inst: usize, mut loaded: Box<dyn Plugin>, source: &str) -> Result<(), IntentError> {
        let Some(lane) = self.model.instruments.get(inst) else { return Err(IntentError::Invalid(format!("there is no lane {inst}"))) };
        has_audio_output(loaded.as_ref())?;
        let node = lane.node;
        if lane.instrument.is_some() {
            for edge in self.model.cables_into_plugin(node) {
                // (One that an earlier one took with it is not there any more.)
                self.take_out(edge);
            }
        }
        let slot = fresh_slot(loaded.as_mut(), source, node);
        self.set_instrument(inst, loaded, slot);
        Ok(())
    }

    /// Put the plugin `source` at the end of lane `inst`'s chain as an effect: loaded here, in a node of its own,
    /// at the mix that suits it (`default_effect_mix`), on the audio thread and in the model. It comes after all
    /// that is there, so no cable that drives a plugin of the chain is moved. Refused, with nothing changed and
    /// nothing sent, if there is no such lane; if `source` is not a plugin the catalog names (a front-end chooses
    /// from the catalog, and an id or a path of its own is not loaded); if it is an instrument; if it does not
    /// load; or if its outputs are not the chain's channels, which the audio thread would turn away (leaving the
    /// model with an effect that is not there).
    pub fn add_effect(&mut self, inst: usize, source: &str) -> Result<NodeId, IntentError> {
        let loaded = self.load_effect(source)?;
        self.insert_effect(ChainAt::Lane(inst), loaded, source)
    }

    /// `add_effect`, to the end of group `group`'s bus.
    pub fn add_bus_effect(&mut self, group: usize, source: &str) -> Result<NodeId, IntentError> {
        let loaded = self.load_effect(source)?;
        self.insert_effect(ChainAt::Bus(group), loaded, source)
    }

    /// Read the library of modules again and have the catalog list what is in it (`app::module`). True if it listed other modules than it did, which the clients are told again. A controller that
    /// has no library lists none.
    pub fn refresh_modules(&mut self) -> bool {
        let Some(dir) = &self.modules_dir else { return false };
        let modules = module::list(dir);
        self.catalog.set_modules(modules)
    }

    /// Add the plugin `source` as an effect that stands by itself (step 87): a lane of its own at the end, with no instrument, full range and in no group,
    /// with the effect as the whole of its chain (`add_lane` and then `add_effect`). The node of the effect, which is the second that is made, the lane's the first.
    /// It is a bare module (step 88): the lane's way out is cut, so that the effect sends where cables are drawn from it and nowhere before. Refused, with nothing
    /// changed, as `add_effect` refuses, and with no lane made and no id used.
    pub fn add_free_effect(&mut self, source: &str) -> Result<NodeId, IntentError> {
        let loaded = self.load_effect(source)?;
        self.add_free_effect_loaded(loaded, source)
    }

    /// `add_free_effect` with the plugin loaded. What the chain cannot take is turned away before the lane is made, so that a plugin that is refused leaves
    /// neither a lane nor a gap in the ids (`insert_effect` looks again: the check is the same).
    pub(super) fn add_free_effect_loaded(&mut self, loaded: Box<dyn Plugin>, source: &str) -> Result<NodeId, IntentError> {
        outputs_fit_the_chain(loaded.name(), loaded.audio_output_count())?;
        let inst = self.add_bare_lane(None);
        self.insert_effect(ChainAt::Lane(inst), loaded, source)
    }

    /// Add a lane at the end whose way out is cut, with `instrument` (the plugin, loaded, and the id it was loaded from) in it if there is one: a lane that stands by itself, as a module's are
    /// (`load_module`). The way out is cut before anything is in the lane, as the loader cuts what a file cuts, before the cables: a way out that was there for a moment would be one that is heard.
    /// Returns the lane's place; the caller has checked what the instrument needs (`has_audio_output`).
    pub(super) fn add_bare_lane(&mut self, instrument: Option<(Box<dyn Plugin>, &str)>) -> usize {
        let inst = self.add_lane();
        self.model.instruments[inst].route = Route::None;
        let _ = self.cmd_tx.send(GraphCommand::SetLaneRoute { inst, route: Route::None });
        if let Some((mut loaded, source)) = instrument {
            let node = self.model.instruments[inst].node;
            let slot = fresh_slot(loaded.as_mut(), source, node);
            self.install_instrument(inst, loaded, slot);
        }
        inst
    }

    /// The plugin `source` loaded to be an effect. The catalog has to name it and it must not be an instrument.
    pub(super) fn load_effect(&mut self, source: &str) -> Result<Box<dyn Plugin>, IntentError> {
        self.load_from_catalog(source, false)
    }

    /// The plugin `source` loaded to be an instrument. The catalog has to name it and it must be one.
    pub(super) fn load_instrument(&mut self, source: &str) -> Result<Box<dyn Plugin>, IntentError> {
        self.load_from_catalog(source, true)
    }

    /// The plugin `source`, loaded, if the catalog names it and it is an instrument or not as `instrument` says. (Why it did
    /// not load goes to the log: it can name where the plugin is on this machine.)
    fn load_from_catalog(&mut self, source: &str, instrument: bool) -> Result<Box<dyn Plugin>, IntentError> {
        // (A module is listed with the plugins and is not one: it is a chain of effects, which goes in a lane of its own.)
        if module::name_of(source).is_some() {
            return Err(IntentError::Invalid("a module is added as an effect that stands by itself, in a lane of its own: it is not a plugin that goes in a chain".to_string()));
        }
        let Some(entry) = self.catalog.plugins().iter().find(|p| p.id == source) else {
            return Err(IntentError::Invalid(format!("{source} is not a plugin that is installed")));
        };
        if entry.is_instrument != instrument {
            let (is, isnt) = if instrument { ("an effect", "an instrument") } else { ("an instrument", "an effect") };
            return Err(IntentError::Invalid(format!("{} is {is}, not {isnt}", entry.name)));
        }
        let name = entry.name.clone();
        plugin::load(source, self.sample_rate, self.max_block_size, &self.runtime).map_err(|e| {
            log::error!("Failed to load plugin '{source}': {e}");
            IntentError::Invalid(format!("{name} could not be loaded"))
        })
    }

    /// Put `loaded`, the plugin `source`, at the end of the chain `at` (`add_effect` says how, and when not).
    pub(super) fn insert_effect(&mut self, at: ChainAt, mut loaded: Box<dyn Plugin>, source: &str) -> Result<NodeId, IntentError> {
        let index = match at {
            ChainAt::Lane(inst) => self.model.instruments.get(inst).map(|lane| lane.effects.len()),
            ChainAt::Bus(group) => self.model.groups.get(group).map(|g| g.effects.len()),
        };
        let Some(index) = index else { return Err(IntentError::Invalid("there is no such chain".to_string())) };
        outputs_fit_the_chain(loaded.name(), loaded.audio_output_count())?;
        // (The node is made last: a plugin that is turned away leaves no gap in the ids.)
        let mut slot = fresh_slot(loaded.as_mut(), source, self.model.alloc_node());
        slot.mix = default_effect_mix(source);
        let (node, mix) = (slot.node, f64::from(slot.mix));
        match at {
            ChainAt::Lane(inst) => {
                let _ = self.cmd_tx.send(GraphCommand::InsertEffect { inst, index, effect: loaded, mix, node });
                self.model.instruments[inst].effects.push(slot);
            }
            ChainAt::Bus(group) => {
                let _ = self.cmd_tx.send(GraphCommand::InsertGroupEffect { group, index, effect: loaded, mix, node });
                self.model.groups[group].effects.push(slot);
            }
        }
        Ok(node)
    }

    /// Take the effect at `index` out of lane `inst`'s chain: the audio thread hands the plugin back, and
    /// the model drops it. The cables that drive its parameters, a lane's and a group's, go first, each by
    /// `take_out` with the cables that drive the depths of those (nothing is left pointing at a cable that is
    /// not there); what they were driving goes with the plugin, so nothing is given back. The cables that
    /// drive the plugins after it are told their new slots. False, sending nothing, if there is no such effect.
    pub fn remove_effect(&mut self, inst: usize, index: usize) -> bool {
        let Some(effect) = self.model.instruments.get(inst).and_then(|lane| lane.effects.get(index)) else { return false };
        let effect = effect.node;
        self.forget_the_history_unless_an_intent_asks();
        for edge in self.model.cables_into_plugin(effect).into_iter().chain(self.model.cables_into_gains_of(effect)) {
            // (One that an earlier one took with it is not there any more.)
            self.take_out(edge);
        }
        self.take_out_audio_cables_at(&[effect]);
        // The very plugin is kept for the history, when an intent is carried out: the audio thread hands it to a channel of its own.
        let (keep, kept) = self.channel_for();
        let _ = self.cmd_tx.send(GraphCommand::RemoveEffect { inst, index, keep });
        if let Some(slot) = self.model.remove_effect(inst, index) {
            self.took_out(Inverse::Effect { at: ChainAt::Lane(inst), index, slot, kept });
        }
        true
    }

    /// Move the effect at `from` of lane `inst`'s chain to `to` (the effects between move one place the other way): the audio
    /// thread reorders its chain, and the model does the same (`SessionModel::move_effect`), the cables of the lane's modulators
    /// and of its group's following their plugins to the new slots. False, sending nothing, if either place is not in the chain,
    /// or they are the same.
    pub fn move_effect(&mut self, inst: usize, from: usize, to: usize) -> bool {
        let Some(effect) = self.model.instruments.get(inst).and_then(|lane| lane.effects.get(from)) else { return false };
        // (With the cables there are, the audio may go round when the chain is in another order.)
        if self.model.loop_if_effect_goes_to(effect.node, to).is_some() || !self.model.move_effect(inst, from, to) {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        let _ = self.cmd_tx.send(GraphCommand::ReorderEffect { inst, from, to });
        true
    }

    /// Move the effect at `from` of group `group`'s bus to `to` (the effects between move one place the other way), as
    /// `move_effect` does in a lane's chain: the audio thread reorders the bus, and the model does the same
    /// (`SessionModel::move_bus_effect`), the group's cables to the bus effects following their plugins to the new
    /// places. False, sending nothing, if either place is not on the bus, or they are the same.
    pub fn move_bus_effect(&mut self, group: usize, from: usize, to: usize) -> bool {
        let Some(effect) = self.model.groups.get(group).and_then(|bus| bus.effects.get(from)) else { return false };
        if self.model.loop_if_effect_goes_to(effect.node, to).is_some() || !self.model.move_bus_effect(group, from, to) {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        let _ = self.cmd_tx.send(GraphCommand::ReorderGroupEffect { group, from, to });
        true
    }

    /// Take the effect at `index` out of group `group`'s bus: as `remove_effect` does for a lane's chain, the
    /// group's cables to it go first (with the cables that drive their depths), and the group's cables to the
    /// bus effects after it are told their new places. False, sending nothing, if there is no such effect.
    pub fn remove_bus_effect(&mut self, group: usize, index: usize) -> bool {
        let Some(effect) = self.model.groups.get(group).and_then(|g| g.effects.get(index)) else { return false };
        let effect = effect.node;
        self.forget_the_history_unless_an_intent_asks();
        for edge in self.model.cables_into_plugin(effect).into_iter().chain(self.model.cables_into_gains_of(effect)) {
            // (One that an earlier one took with it is not there any more.)
            self.take_out(edge);
        }
        self.take_out_audio_cables_at(&[effect]);
        let (keep, kept) = self.channel_for();
        let _ = self.cmd_tx.send(GraphCommand::RemoveGroupEffect { group, index, keep });
        if let Some(slot) = self.model.remove_bus_effect(group, index) {
            self.took_out(Inverse::Effect { at: ChainAt::Bus(group), index, slot, kept });
        }
        true
    }

    /// Take lane `inst` out, with everything it has: its instrument, effects and pattern. The audio
    /// thread hands its plugins back. The cables of the modulators to its plugins go first, each by `take_out` with
    /// the cables that drive their depths (the modulators stay, hearing every note if they heard the lane). False,
    /// sending nothing, if there is no such lane, or it is the only one: a session keeps a lane.
    pub fn remove_lane(&mut self, inst: usize) -> bool {
        if inst >= self.model.instruments.len() || self.model.instruments.len() < 2 {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        // The audio cables at its instrument and at each of its effects go with it, and so do the cables of notes that end at it. So do the cables of the modulators to its
        // plugins (a modulator stays: it is not the lane's, and the cables are written down by themselves).
        let nodes = self.model.audio_nodes_of_lane(inst);
        for edge in self.model.free_cables_into(&nodes) {
            // (One that an earlier one took with it is not there any more.)
            self.take_out(edge);
        }
        self.take_out_audio_cables_at(&nodes);
        self.take_out_event_cables_at(&[self.model.instruments[inst].node]);
        // The whole lane is kept for the history, when an intent is carried out: the audio thread hands it to a channel of its own.
        let (keep, kept) = self.channel_for();
        let _ = self.cmd_tx.send(GraphCommand::RemoveInstrument { inst, keep });
        let node = self.model.instruments[inst].node;
        if let Some(lane) = self.model.remove_lane(inst) {
            // (The module that it came with, if any, loses a member before the lane is written down, so that undo puts the lane back first and the module after it.)
            self.leave_module(node);
            self.took_out(Inverse::Lane { index: inst, lane, kept });
        }
        true
    }

    /// `member`, a lane or a modulator that is going out of the session, leaves the module that came with it (step 108): the module has one member less, and is gone if it has none. Written down for the history, as
    /// the module was, when an intent is carried out: what undo puts back is the module as it was.
    fn leave_module(&mut self, member: NodeId) {
        if let Some((index, module)) = self.model.leave_module(member) {
            self.took_out(Inverse::Module { index, module });
        }
    }

    /// Take module `index` out, with everything that came with it (step 108: what a front-end draws as one box and takes out as one): its modulators, and then its lanes with their plugins, each as `remove_modulator` and
    /// `remove_lane` take them out (the last made first, as undo of the add takes them out). The record of the module goes first, so that it comes back last, with its members back in. False, sending nothing and changing
    /// nothing, if there is no such module, or if the lanes in it are all the lanes the session has: a session keeps a lane.
    pub fn remove_module(&mut self, index: usize) -> bool {
        let Some(members) = self.model.modules.get(index).map(|module| module.members.clone()) else { return false };
        let lanes = members.iter().filter(|node| matches!(self.model.locate(**node), Some(NodePath::Instrument(_)))).count();
        if lanes >= self.model.instruments.len() {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        let module = self.model.modules.remove(index);
        self.took_out(Inverse::Module { index, module });
        for node in members.into_iter().rev() {
            match self.model.locate(node) {
                Some(NodePath::Modulator { at, index }) => {
                    self.remove_modulator(at, index);
                }
                Some(NodePath::Instrument(inst)) => {
                    self.remove_lane(inst);
                }
                _ => {}
            }
        }
        true
    }

    /// Take group `group` out, with its bus effects: the audio thread hands the effects back
    /// and lets its members go (they are in no group, and the lanes of later groups have a group index one less).
    /// The cables of the modulators to its bus effects go first, each by `take_out` with the cables that drive their
    /// depths (the modulators stay, hearing every note if they heard the group: nothing is given back, since what the
    /// cables drove goes with the bus). False, sending nothing, if there is no such group.
    pub fn remove_group(&mut self, group: usize) -> bool {
        if group >= self.model.groups.len() {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        // The lanes that are in it, by node: they are in none once it has gone, and are put in it again when it comes back.
        let members: Vec<NodeId> = self.model.instruments.iter().filter(|lane| lane.group == Some(group)).map(|lane| lane.node).collect();
        // The audio cables at its bus, its output and each of its bus effects go with it, and so do the cables of the modulators to its bus effects (nothing is given back:
        // what they drove goes with the bus).
        let nodes = self.model.audio_nodes_of_group(group);
        for edge in self.model.free_cables_into(&nodes) {
            // (One that an earlier one took with it is not there any more.)
            self.take_out(edge);
        }
        self.take_out_audio_cables_at(&nodes);
        // The whole group is kept for the history, when an intent is carried out: the audio thread hands it to a channel of its own.
        let (keep, kept) = self.channel_for();
        let _ = self.cmd_tx.send(GraphCommand::RemoveGroup { group, keep });
        if let Some(node) = self.model.remove_group(group) {
            self.took_out(Inverse::Group { index: group, group: node, members, kept });
        }
        true
    }

    /// Put lane `inst` in group `group`, or in none: the audio thread moves it (`SetLaneGroup`), and the model does the
    /// same (`SessionModel::set_lane_group`). A lane put in the group it is in has nothing changed. False, sending nothing, if there
    /// is no such lane or no such group, or if the audio would go round by the cables there are with the lane in that group.
    pub fn set_lane_group(&mut self, inst: usize, group: Option<usize>) -> bool {
        if inst >= self.model.instruments.len() {
            return false;
        }
        if group.is_some_and(|g| g >= self.model.groups.len()) {
            return false;
        }
        // (The audio cables are by node, so they stay where they are: but with the lane in another group the audio may go round by them.)
        if self.model.loop_if_lane_goes_to(inst, group).is_some() {
            return false;
        }
        if !self.model.set_lane_group(inst, group) {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        let _ = self.cmd_tx.send(GraphCommand::SetLaneGroup { inst, group });
        true
    }

    /// Take modulator `index` out of the rack at `at`: the audio
    /// thread drops it and the model with it, and its own cables with it. The cables of the
    /// other modulators that drove its rate, its stages or the depth of its cables go first, each by `take_out`, so
    /// that what drove the depth of any of these goes as well and nothing is left pointing at a
    /// cable that is not there. The cables that drove later modulators of the rack move down an
    /// index with them. What its own cables drove has the value the user set again. When an
    /// intent is being carried out the modulator is written down for the history as it goes, with
    /// what each of its cables was centred on (`Inverse::Modulator`). False, sending nothing, if
    /// there is no such modulator.
    pub fn remove_modulator(&mut self, at: ModulatorLocation, index: usize) -> bool {
        if self.model.modulators_mut(at).is_none_or(|rack| index >= rack.len()) {
            return false;
        }
        self.forget_the_history_unless_an_intent_asks();
        for edge in self.model.cables_into(at, index) {
            // (One that an earlier one took with it is not there any more. They drive the modulator's own
            // rate, stages and cable depths, and the cables that drive the depths of those go with them, so
            // there is nothing left of what they drove to give anything back to.)
            self.take_out(edge);
        }
        // What its own cables drive, told before they go and given back when it has gone, and what each is centred on (what the user has set what it
        // drives to), which is what puts it back as it was.
        let (mut driven, mut bases): (Vec<Driven>, Vec<f32>) = (Vec::new(), Vec::new());
        if let Some(modulator) = self.model.modulators(at).and_then(|rack| rack.get(index)) {
            for target in &modulator.targets {
                driven.push(target.aim);
                bases.push(self.model.centre_of(target.aim).unwrap_or(target.param_min));
            }
        }
        let _ = self.cmd_tx.send(GraphCommand::RemoveModulator { location: at, index });
        // The model does what the audio thread does: the cables of the modulators after it are told the places they have now, and a lane's that is a member
        // of a group tells the group's cables to its modulators.
        if let Some(modulator) = self.model.remove_modulator_at(at, index) {
            // (The module that it came with, if any, loses a member before the modulator is written down, so that undo puts the modulator back first and the module after it.)
            self.leave_module(modulator.node);
            self.took_out(Inverse::Modulator { at, index, modulator, bases });
        }
        self.give_back(driven);
        true
    }

    /// Take the cable `edge` out: of the model, and of the audio thread, which is sent the same
    /// removals in the same order. The cables that drive the depth of a cable that goes are
    /// taken out first and go with it (`SessionModel::remove_edge`). What the cables that go
    /// were driving has the value the user set again, unless another cable drives it
    /// (`give_back`). False, sending nothing, if there is no such cable.
    pub fn remove_edge(&mut self, edge: EdgeId) -> bool {
        // An audio cable drives nothing: it goes, and nothing is given back.
        if self.model.audio_cable(edge).is_some() {
            self.forget_the_history_unless_an_intent_asks();
            return self.take_out_audio_cable(edge);
        }
        // Nor does a cable of notes: it goes, and the lane that it ended at hears what its other cables bring, or every source (`sync_lane_sources`).
        if self.model.event_cable(edge).is_some() {
            self.forget_the_history_unless_an_intent_asks();
            return self.take_out_event_cable(edge);
        }
        let Some(driven) = self.take_out(edge) else { return false };
        self.forget_the_history_unless_an_intent_asks();
        self.give_back(driven);
        true
    }

    /// `remove_edge` without giving anything back: what the cable was driving, for the caller to give
    /// back when it has done what else it has to take out. (The cables that go with it, because they
    /// drive the depth of cables that go, drive nothing that stays.) `None`, sending nothing, if there is
    /// no such cable.
    fn take_out(&mut self, edge: EdgeId) -> Option<Vec<Driven>> {
        let driven = self.model.driven_by_edge(edge).into_iter().collect();
        let removals = self.model.remove_edge(edge)?;
        for removed in removals {
            let (at, mod_index, target_index) = (removed.at, removed.mod_index, removed.target_index);
            let _ = self.cmd_tx.send(GraphCommand::RemoveModTarget { location: at, mod_index, target_index });
            self.took_out(if removed.still_driven { Inverse::NoWayBack } else { Inverse::Cable(removed) });
        }
        Some(driven)
    }

    /// Draw an audio cable from `from` to `to` at `gain` (held to nothing and `CABLE_GAIN_MAX`), in the model and on the audio thread, under an id of the series that the
    /// modulation cables have: the cable's. Refused, with nothing changed and no id used, for a level that is not a number, a cable between the two ends that is there already,
    /// and a cable that the wiring cannot take (`SessionModel::cable_problem`: an end that is not there, or a loop: the wiring is compiled on the side, as the audio thread
    /// does it, so that what is refused here is what it would refuse).
    pub fn add_audio_cable(&mut self, from: CableFrom, to: CableTo, gain: f32) -> Result<EdgeId, IntentError> {
        if !gain.is_finite() {
            return Err(IntentError::Invalid(format!("{gain} is not a value for the level of a cable")));
        }
        if self.model.cables.iter().any(|cable| (cable.from, cable.to) == (from, to)) {
            return Err(IntentError::Invalid("there is a cable between those two already".to_string()));
        }
        if let Some(problem) = self.model.cable_problem(from, to) {
            return Err(match problem {
                CableProblem::NoStart => IntentError::Invalid("what the cable starts at is not there".to_string()),
                CableProblem::NoEnd => IntentError::Invalid("what the cable ends at is not there".to_string()),
                CableProblem::Loop(reason) => {
                    log::debug!("a cable that would make the audio go round: {reason}");
                    IntentError::Invalid("that cable would make the audio go round: it would feed what it comes from".to_string())
                }
            });
        }
        let gain = gain.clamp(0.0, CABLE_GAIN_MAX);
        let id = self.model.alloc_edge();
        self.model.insert_audio_cable(AudioCable { id, from, to, gain });
        let _ = self.cmd_tx.send(GraphCommand::AddCable { id, from, to, gain });
        Ok(id)
    }

    /// Take the audio cable `id` out of the model and off the audio thread (which lets it go down to nothing over a buffer), and write it down for the history, as what
    /// puts it back. False, sending nothing, if there is no such cable.
    fn take_out_audio_cable(&mut self, id: EdgeId) -> bool {
        let Some((_, cable)) = self.model.remove_audio_cable(id) else { return false };
        let _ = self.cmd_tx.send(GraphCommand::RemoveCable { id });
        self.took_out(Inverse::AudioCable(cable));
        true
    }

    /// Take out the audio cables that start or end at any of `nodes`: what goes with the lane, the group or the effect that has them, before it goes.
    fn take_out_audio_cables_at(&mut self, nodes: &[NodeId]) {
        for id in self.model.audio_cables_at(nodes) {
            self.take_out_audio_cable(id);
        }
    }

    /// Take the event cable `id` out of the model, and write it down for the history, as what puts it back. (The audio thread has no cable of notes: what a lane hears is the set of sources that
    /// the controller tells it, `app::sources`.) False if there is no such cable.
    pub(super) fn take_out_event_cable(&mut self, id: EdgeId) -> bool {
        let Some(cable) = self.model.remove_event_cable(id) else { return false };
        self.took_out(Inverse::EventCable(cable));
        true
    }

    /// Take out the cables of notes that start or end at any of `nodes`: what goes with the lane that they end at, or the source that they start at, before it goes.
    pub(super) fn take_out_event_cables_at(&mut self, nodes: &[NodeId]) {
        for id in self.model.event_cables_at(nodes) {
            self.take_out_event_cable(id);
        }
    }

    /// Give what the user set back to what cables that are gone were driving. A cable writes into a
    /// parameter, a modulator's rate or stage, or the depth of a cable, on every buffer, and nothing
    /// writes the user's value back when it is taken out: what it wrote last would stay for ever. The
    /// values are the model's, which are the user's; what another cable still drives is left to that
    /// cable, and what is not there any more is given nothing. (`driven` is named by what survives the
    /// removals: nodes and edges, not places in a rack.)
    fn give_back(&mut self, driven: Vec<Driven>) {
        let mut done: Vec<Driven> = Vec::new();
        for driven in driven {
            if done.contains(&driven) || self.model.is_driven(driven) {
                continue;
            }
            done.push(driven);
            if let Some(command) = self.value_command(driven) {
                let _ = self.cmd_tx.send(command);
            }
        }
    }

    /// The command that gives the audio thread the model's value of `driven`; none if it is not there.
    fn value_command(&self, driven: Driven) -> Option<GraphCommand> {
        let value_of = |plugin: &PluginSlot, index: u32| plugin.params.iter().find(|p| p.index == index).map(|p| p.value);
        match driven {
            Driven::Param { plugin, index } => match self.model.locate(plugin)? {
                NodePath::Instrument(inst) => {
                    let value = value_of(self.model.instruments.get(inst)?.instrument.as_ref()?, index)?;
                    Some(GraphCommand::SetParameter { inst, slot: 0, param_index: index, value })
                }
                NodePath::Effect { inst, index: effect } => {
                    let value = value_of(self.model.instruments.get(inst)?.effects.get(effect)?, index)?;
                    Some(GraphCommand::SetParameter { inst, slot: effect + 1, param_index: index, value })
                }
                NodePath::GroupEffect { group, index: effect } => {
                    let value = value_of(self.model.groups.get(group)?.effects.get(effect)?, index)?;
                    Some(GraphCommand::SetGroupParameter { group, index: effect, param_index: index, value })
                }
                NodePath::Group(_) | NodePath::Modulator { .. } | NodePath::Source(_) | NodePath::Module(_) => None,
            },
            Driven::Setting { modulator, field } => {
                let NodePath::Modulator { at, index } = self.model.locate(modulator)? else { return None };
                match (&self.model.modulators(at)?.get(index)?.source, field) {
                    (ModSourceSlot::Lfo { rate, .. }, ModSetting::Rate) => Some(rate_command(at, index, *rate)),
                    (
                        ModSourceSlot::Envelope { attack, decay, sustain, release },
                        ModSetting::Attack | ModSetting::Decay | ModSetting::Sustain | ModSetting::Release,
                    ) => Some(envelope_command(at, index, (*attack, *decay, *sustain, *release))),
                    _ => None,
                }
            }
            Driven::Depth(edge) => {
                let (at, mod_index, target_index) = self.model.locate_edge(edge)?;
                let depth = self.model.modulators(at)?.get(mod_index)?.targets.get(target_index)?.depth;
                Some(depth_command(at, mod_index, target_index, depth))
            }
            Driven::Gain { node, gain } => {
                let value = self.model.gain_of(node, gain)?;
                match (self.model.locate(node)?, gain) {
                    (NodePath::Instrument(inst), ModGain::Volume) => Some(GraphCommand::SetVolume { inst, value }),
                    (NodePath::Effect { inst, index }, ModGain::Mix) => Some(GraphCommand::SetMix { inst, slot: index + 1, value }),
                    (NodePath::GroupEffect { group, index }, ModGain::Mix) => Some(GraphCommand::SetGroupMix { group, index, value }),
                    (NodePath::Group(group), ModGain::Volume) => Some(GraphCommand::SetGroupVolume { group, value }),
                    (NodePath::Group(group), ModGain::Pan) => Some(GraphCommand::SetGroupPan { group, value }),
                    _ => None,
                }
            }
        }
    }

    /// Bind `new` as a target of modulator `index` in the rack at `at`, on the audio
    /// thread and in the model. False, sending nothing, if there is no such
    /// modulator.
    pub fn add_target(&mut self, at: ModulatorLocation, index: usize, new: NewTarget) -> bool {
        if self.model.modulators_mut(at).and_then(|rack| rack.get(index)).is_none() {
            return false;
        }
        // (A cable that drives nothing that is in reach of the rack is not drawn.)
        if self.model.kind_of(at, index, new.aim).is_none() {
            return false;
        }
        // A target publishes the value that what it drives is set to: a plugin's parameter, or a setting of a modulator.
        let effective = self.model.slots.alloc();
        let edge = self.model.alloc_edge();
        let target = ModTarget {
            edge,
            aim: new.aim,
            depth: new.depth,
            base_value: new.base_value,
            param_min: new.param_min,
            param_max: new.param_max,
            effective,
        };
        let Some(modulator) = self.model.modulators_mut(at).and_then(|rack| rack.get_mut(index)) else {
            return false;
        };
        modulator.targets.push(ModTargetSlot {
            edge,
            param_name: new.param_name,
            aim: new.aim,
            depth: new.depth,
            param_min: new.param_min,
            param_max: new.param_max,
            effective,
        });
        let _ = self.cmd_tx.send(GraphCommand::AddModTarget { location: at, mod_index: index, target });
        true
    }

    /// Tell `sink` about every intent applied from now on (see `EventSink`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime subscribes"))]
    pub fn subscribe(&mut self, sink: Box<dyn EventSink>) -> SubscriberId {
        // The first to subscribe starts from the session as it is now.
        #[cfg(any(test, feature = "web"))]
        if self.subscribers.is_empty() {
            self.subscribers.start_from(self.snapshot());
        }
        self.subscribers.add(sink)
    }

    /// End a subscription. False if there is none by that id (any more).
    #[cfg_attr(not(test), expect(dead_code, reason = "the web runtime subscribes, later in step 10"))]
    pub fn unsubscribe(&mut self, id: SubscriberId) -> bool {
        self.subscribers.remove(id)
    }

    /// Tell the subscribers about an intent that has been applied.
    pub(super) fn publish(&mut self, applied: &Applied) {
        self.subscribers.publish(applied);
    }

    /// Tell the subscribers about changes that no intent made, if there are any. A
    /// front-end that edits the model directly (the TUI, for most of what it does)
    /// calls this when it is done, and `poll` does when the audio thread has changed
    /// the model. What the subscribers have been told is compared with the session as
    /// it is now, and if it differs they are given a snapshot to start again from, at
    /// a revision of its own: a change that a snapshot does not show is no change.
    /// True if they were told.
    #[cfg(any(test, feature = "web"))]
    pub fn resync(&mut self) -> bool {
        if self.subscribers.is_empty() {
            return false;
        }
        let mut now = self.snapshot();
        if !self.subscribers.differ_from(&now) {
            return false;
        }
        self.rev += 1;
        now.rev = self.rev;
        self.subscribers.resync(now);
        true
    }

    /// A handle on the note path (the Locked mode, the held notes, the channel to
    /// the audio thread) for a note source to keep and call from its own thread.
    pub fn note_input(&self) -> NoteInput {
        NoteInput::new(self.midi_tx.clone(), self.held.clone(), self.piano_filter.clone())
    }

    /// The name of the file the session is saved to (not its path), if it has one.
    pub fn session_file_name(&self) -> Option<String> {
        self.session_path.as_ref().and_then(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned())
    }

    /// Save the session to the file it came from.
    pub fn save(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.session_path.clone() else {
            anyhow::bail!("No session path — cannot save");
        };
        self.save_to(&path, FileMode::Replace)
    }

    /// Where `name` is written when the session is saved as it (`Intent::SaveAs`): the file that `session::plain_file_name` makes of it, beside the session's
    /// file (where the program was started, if it has none). Refused, in the rule's words, if `name` is not a plain file name.
    pub(super) fn save_as_path(&self, name: &str) -> Result<PathBuf, IntentError> {
        let file = crate::session::plain_file_name(name).map_err(IntentError::Invalid)?;
        let dir = self.session_path.as_deref().and_then(Path::parent).unwrap_or(Path::new("."));
        Ok(dir.join(file))
    }

    /// Write the session, with the Piano tab's scale and mode, to `path`, as `mode` says. Does
    /// not touch `session_path`: callers decide what a failed save means for it.
    pub(super) fn save_to(&mut self, path: &Path, mode: FileMode) -> anyhow::Result<()> {
        // (What is in the plugins that a person has looked into is read first, so that the file has it as it is now.)
        self.keep_the_states();
        let (instruments, groups, cables, sources) = to_save(&self.model);
        let piano = PianoConfig {
            scale: Some(self.piano_filter.scale().short()),
            locked: matches!(self.piano_filter.mode(), PianoMode::Locked),
        };
        match mode {
            FileMode::Replace => session::save(path, &instruments, &groups, &cables, &sources, Some(&piano), self.model.master_ui(), self.model.bpm_to_save())?,
            FileMode::CreateNew => session::save_new(path, &instruments, &groups, &cables, &sources, Some(&piano), self.model.master_ui(), self.model.bpm_to_save())?,
        }
        self.dirty = false;
        log::info!("Session saved to {}", path.display());
        Ok(())
    }

    /// The session as the text of a session file, as `save_to` would write it, for a host that keeps it in a project of its own (Tang as a plugin, step 113). What is in the plugins that a person has looked into is read first,
    /// as for a save.
    pub fn session_text(&mut self) -> anyhow::Result<String> {
        self.keep_the_states();
        let (instruments, groups, cables, sources) = to_save(&self.model);
        let piano = PianoConfig {
            scale: Some(self.piano_filter.scale().short()),
            locked: matches!(self.piano_filter.mode(), PianoMode::Locked),
        };
        session::to_toml(&instruments, &groups, &cables, &sources, Some(&piano), self.model.master_ui(), self.model.bpm_to_save())
    }

    /// Have `poll` look for newly connected devices through `watcher`.
    pub fn watch_devices(&mut self, watcher: Box<dyn DeviceWatcher>) {
        self.devices = Some(watcher);
        // (The devices that are open are known now: what a lane that is wired to one hears is not what it was.)
        self.sync_lane_sources();
    }

    /// The names of the MIDI inputs that are open, in the order they were opened: what a client can make a source of (`AddSource`). None while no one watches the devices (a program that
    /// has no MIDI subsystem, or that has not looked yet). (With the `web` feature or in tests only: it is the web interface that tells clients.)
    #[cfg(any(test, feature = "web"))]
    pub fn inputs(&self) -> Vec<String> {
        self.devices.as_ref().map_or_else(Vec::new, |devices| devices.open_names())
    }

    /// Housekeeping, to be called on every iteration of the front-end's loop.
    pub fn poll(&mut self) -> Polled {
        self.poll_at(Instant::now())
    }

    pub(super) fn poll_at(&mut self, now: Instant) -> Polled {
        self.reap();
        // The plugins' own windows (step 112): what a knob in one of them did is applied as the change of any front-end is, and a window whose plugin has left the session is closed.
        self.apply_window_edits();
        self.close_windows_of_plugins_that_are_gone();
        // (What a person did in a window that was closed is kept: the session is unsaved if it was something.)
        let state_changed = self.keep_the_states_of_closed_windows();
        if now.duration_since(self.last_device_poll) >= DEVICE_POLL_INTERVAL {
            self.last_device_poll = now;
            if let Some(changes) = self.devices.as_mut().map(|devices| devices.poll_new_devices()) {
                self.devices_changed(changes);
            }
        }
        let catalog_changed = self.catalog.poll();
        let model_changed = self.fold_in_notifications() || state_changed;
        // What the audio thread reported is a change no intent made.
        #[cfg(any(test, feature = "web"))]
        if model_changed {
            self.resync();
        }
        // (And so is what the terminal does to the lanes without one: where the nodes that are watched are is looked at again.)
        self.reconcile_probes();
        Polled { catalog_changed, model_changed }
    }

    /// Devices came and went. What a device that has gone was holding would be held for ever, so every lane is told to let go of what it has sounding from it (a lane that does not hear
    /// the device has nothing of it to let go of).
    fn devices_changed(&mut self, changes: DeviceChanges) {
        for source in changes.gone {
            let _ = self.cmd_tx.send(GraphCommand::ReleaseSource { source });
        }
        // What a lane that is wired to a device hears is the ports that are open for it: a device that came is heard, and one that went is not (and a lane that hears what it was told
        // is told nothing, so that this is done at every look, whether or not a device came or went).
        self.sync_lane_sources();
    }

    /// Fold what the audio thread has reported into the model. True if the
    /// model changed.
    fn fold_in_notifications(&mut self) -> bool {
        let mut changed = false;
        while let Ok(recording) = self.pattern_rx.try_recv() {
            changed |= self.finish_recording(recording);
        }
        while let Ok(capture) = self.learn_rx.try_recv() {
            changed |= self.bind_capture(capture);
        }
        changed
    }

    /// A pattern recording has finished: it becomes the lane's pattern, at the
    /// tempo it was recorded at. One for a lane that is gone changes nothing.
    fn finish_recording(&mut self, recording: PatternNotification) -> bool {
        let Some(lane) = self.model.instruments.get_mut(recording.inst) else {
            return false;
        };
        // Re-recording replaces the events but keeps the transpose mode.
        let in_key = lane.pattern.as_ref().is_some_and(|p| p.in_key);
        lane.pattern = Some(PatternState {
            bpm: self.model.bpm,
            length_beats: recording.length_beats,
            looping: recording.looping,
            base_note: recording.base_note,
            events: recording.events,
            enabled: recording.enabled,
            recording: false,
            in_key,
        });
        self.dirty = true;
        true
    }

    /// A MIDI-learn modulator has captured a control: bind it. One for a
    /// modulator that is gone, or no longer listening, changes nothing.
    fn bind_capture(&mut self, capture: MidiLearnedNotification) -> bool {
        let Some(modulator) = self
            .model
            .modulators_mut(capture.location)
            .and_then(|rack| rack.get_mut(capture.mod_index))
        else {
            return false;
        };
        let ModSourceSlot::MidiLearn { source, learning } = &mut modulator.source else {
            return false;
        };
        *source = Some(capture.source);
        *learning = false;
        self.dirty = true;
        true
    }

    /// Drop the plugins the audio thread has handed back.
    fn reap(&self) {
        while self.return_rx.try_recv().is_ok() {}
    }

    /// Close the devices, then drop what is left in the return channel, and the
    /// plugin runtime after that. Call it once the audio stream has stopped, so
    /// that nothing is still calling into a plugin.
    pub fn shutdown(mut self) {
        self.devices = None;
        // What the history holds of plugins goes now, before the runtime does (a field, dropped when `self` is, which is before the history is): the libraries
        // that a plugin came from have to be there when it is dropped.
        self.history.clear();
        self.taken_last = Journal::default();
        self.reap();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::thread;

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

    use crate::app::testutil::{self, DropProbe, StubEngine, load, model_groups, model_lanes, recording};
    use crate::plugin::chain::{AudioGraph, CableFrom, CableTo, GraphCommand, MidiBindSource, ModTargetKind, ModulatorLocation};

    fn controller() -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(SessionModel::default(), handles, None))
    }

    #[test]
    fn poll_drops_returned_plugins_on_the_calling_thread() {
        let (engine, mut ctl) = controller();
        let (probe, dropped_on) = DropProbe::recording();
        // The "audio thread" hands a plugin back and goes away.
        let tx = engine.return_tx.clone();
        let audio_thread = thread::spawn(move || tx.send(Box::new(probe) as Box<dyn Plugin>).unwrap());
        audio_thread.join().unwrap();
        let before = *dropped_on.lock().unwrap();
        assert!(before.is_none(), "nothing is dropped until someone reaps");

        ctl.poll();
        let after = *dropped_on.lock().unwrap();
        assert_eq!(after, Some(thread::current().id()));
    }

    #[test]
    fn poll_reaps_what_the_audio_graph_hands_back() {
        // The real producer: swapping an instrument returns the old one.
        let (engine, mut ctl) = controller();
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let swap = |plugin: Box<dyn Plugin>| GraphCommand::SwapInstrument {
            inst: 0,
            instrument: plugin,
            inst_buf: vec![Vec::new(), Vec::new()],
            remapper: None,
            keep: None,
        };
        ctl.cmd_tx.send(GraphCommand::AddInstrument { range: None, node: crate::plugin::chain::test_node() }).unwrap();
        let osc = |n: usize| (0..n).map(|_| crate::plugin::load("builtin:osc", 48_000.0, 256, &Default::default()).unwrap());
        for plugin in osc(3) {
            ctl.cmd_tx.send(swap(plugin)).unwrap();
        }
        graph.drain_commands();
        assert_eq!(engine.return_tx.len(), 2, "the second and third swap each hand one back");

        ctl.poll();
        assert_eq!(engine.return_tx.len(), 0);
    }

    #[test]
    fn devices_are_polled_once_a_second() {
        struct Watcher(Rc<Cell<usize>>);
        impl DeviceWatcher for Watcher {
            fn poll_new_devices(&mut self) -> DeviceChanges {
                self.0.set(self.0.get() + 1);
                DeviceChanges::default()
            }

            fn sources_of(&self, _: &str) -> SourceSet {
                SourceSet::NONE
            }

            fn open_names(&self) -> Vec<String> {
                Vec::new()
            }
        }
        let t0 = Instant::now();
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::starting_at(SessionModel::default(), handles, None, t0);
        let polls = Rc::new(Cell::new(0));
        ctl.watch_devices(Box::new(Watcher(polls.clone())));

        let at = |ms: u64| t0 + Duration::from_millis(ms);
        for (ms, expected) in [(100, 0), (999, 0), (1000, 1), (1500, 1), (1999, 1), (2000, 2), (5000, 3)] {
            ctl.poll_at(at(ms));
            assert_eq!(polls.get(), expected, "after {ms} ms");
        }
    }

    /// A watcher that says what it is told, one look at a time, and nothing after that.
    struct Scripted(std::collections::VecDeque<DeviceChanges>);

    impl DeviceWatcher for Scripted {
        fn poll_new_devices(&mut self) -> DeviceChanges {
            self.0.pop_front().unwrap_or_default()
        }

        fn sources_of(&self, _: &str) -> SourceSet {
            SourceSet::NONE
        }

        fn open_names(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn a_device_that_has_gone_is_let_go_of_by_every_lane_and_a_look_that_found_a_device_come_or_nothing_sends_nothing() {
        use crate::source::Source;
        let t0 = Instant::now();
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::starting_at(SessionModel::default(), handles, None, t0);
        ctl.watch_devices(Box::new(Scripted(
            [
                DeviceChanges { came: vec![Source::device(0)], gone: vec![] },
                DeviceChanges::default(),
                DeviceChanges { came: vec![], gone: vec![Source::device(1), Source::device(0)] },
            ]
            .into(),
        )));

        let at = |seconds: u64| t0 + Duration::from_secs(seconds);
        ctl.poll_at(at(1));
        ctl.poll_at(at(2));
        assert_eq!(engine.drain().0, Vec::<String>::new(), "a device that came changes no lane, and a look that found nothing neither");
        ctl.poll_at(at(3));
        assert_eq!(engine.drain().0, ["ReleaseSource source=2", "ReleaseSource source=1"], "each device that has gone, in the order it is told");
    }

    #[test]
    fn polling_without_a_device_watcher_is_fine() {
        let (_engine, mut ctl) = controller();
        ctl.poll_at(Instant::now() + Duration::from_secs(10));
    }

    #[test]
    fn shutdown_closes_the_devices_before_it_drops_what_was_handed_back() {
        struct Watcher(Arc<Mutex<Vec<&'static str>>>);
        impl DeviceWatcher for Watcher {
            fn poll_new_devices(&mut self) -> DeviceChanges {
                DeviceChanges::default()
            }

            fn sources_of(&self, _: &str) -> SourceSet {
                SourceSet::NONE
            }

            fn open_names(&self) -> Vec<String> {
                Vec::new()
            }
        }
        impl Drop for Watcher {
            fn drop(&mut self) {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).push("devices closed");
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let (engine, mut ctl) = controller();
        ctl.watch_devices(Box::new(Watcher(log.clone())));
        let probe_log = log.clone();
        let probe = DropProbe::new(move |_| probe_log.lock().unwrap_or_else(|e| e.into_inner()).push("plugin dropped"));
        engine.return_tx.send(Box::new(probe)).unwrap();

        ctl.shutdown();
        let order = log.lock().unwrap().clone();
        assert_eq!(order, ["devices closed", "plugin dropped"]);
    }

    #[test]
    fn a_new_controller_has_nothing_to_save() {
        let (handles, _engine) = testutil::stub_engine();
        let ctl = Controller::new(load(testutil::GROUPS).model, handles, Some(PathBuf::from("song.toml")));
        assert!(!ctl.dirty);
        assert_eq!(ctl.session_path, Some(PathBuf::from("song.toml")));
        assert!(!ctl.model.instruments.is_empty(), "the model is the one it was given");
    }

    fn controller_for(session: &str) -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        (engine, Controller::new(load(session).model, handles, None))
    }

    // -- Subscribers: who hears what `apply` did --

    use crate::app::intent::{Applied, Event, Gain, Intent, IntentError, ModulatorKind};

    /// A sink that keeps everything it hears, and a handle on what it has heard.
    fn recorder() -> (Box<dyn EventSink>, Arc<Mutex<Vec<Applied>>>) {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = heard.clone();
        (Box::new(move |applied: &Applied| sink.lock().unwrap().push(applied.clone())), heard)
    }

    fn volume(ctl: &Controller, value: f32) -> Intent {
        Intent::SetGain { node: ctl.model.instruments[0].node, gain: Gain::Volume, value }
    }

    #[test]
    fn a_subscriber_hears_each_applied_intent_with_its_revision_and_events() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (sink, heard) = recorder();
        ctl.subscribe(sink);
        let lane = ctl.model.instruments[0].node;
        let first = ctl.apply(volume(&ctl, 0.5)).unwrap();
        let second = ctl.apply(volume(&ctl, 0.25)).unwrap();
        assert_eq!(*heard.lock().unwrap(), [first.clone(), second]);
        assert_eq!(first.events, [Event::GainSet { node: lane, gain: Gain::Volume, value: 0.5 }]);
        assert_eq!(first.rev, 1);
    }

    #[test]
    fn a_save_is_published_as_the_event_that_says_so_and_one_that_fails_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        ctl.session_path = Some(dir.path().join("song.toml"));
        let (sink, heard) = recorder();
        ctl.subscribe(sink);
        ctl.apply(volume(&ctl, 0.5)).unwrap();
        let saved = ctl.apply(Intent::Save).unwrap();
        assert_eq!(saved.events, [Event::Saved { file: Some("song.toml".to_string()) }]);
        assert_eq!(heard.lock().unwrap().last(), Some(&saved), "a subscriber is told");
        ctl.session_path = None;
        assert!(ctl.apply(Intent::Save).is_err());
        assert_eq!(heard.lock().unwrap().len(), 2, "a save that failed is told to nobody");
    }

    #[test]
    fn a_save_as_is_published_with_the_name_the_session_has_after_it_and_one_that_is_refused_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        ctl.session_path = Some(dir.path().join("song.toml"));
        let (sink, heard) = recorder();
        ctl.subscribe(sink);
        ctl.apply(volume(&ctl, 0.5)).unwrap();
        let saved = ctl.apply(Intent::SaveAs { name: "copy".to_string() }).unwrap();
        assert_eq!(saved.events, [Event::Saved { file: Some("copy.toml".to_string()) }]);
        assert_eq!(heard.lock().unwrap().last(), Some(&saved), "a subscriber is told");
        assert!(ctl.apply(Intent::SaveAs { name: "../copy".to_string() }).is_err());
        assert_eq!(heard.lock().unwrap().len(), 2, "a refused save as is told to nobody");
    }

    #[test]
    fn an_intent_that_fails_is_not_published() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (sink, heard) = recorder();
        ctl.subscribe(sink);
        let ghost = crate::app::model::NodeId(9_999);
        let result = ctl.apply(Intent::SetGain { node: ghost, gain: Gain::Volume, value: 0.5 });
        assert_eq!(result, Err(IntentError::NotFound(ghost)));
        assert!(heard.lock().unwrap().is_empty());
    }

    #[test]
    fn every_subscriber_hears_it_in_the_order_they_subscribed() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let order = Arc::new(Mutex::new(Vec::new()));
        for name in ["first", "second", "third"] {
            let order = order.clone();
            ctl.subscribe(Box::new(move |applied: &Applied| order.lock().unwrap().push((name, applied.rev))));
        }
        ctl.apply(volume(&ctl, 0.5)).unwrap();
        assert_eq!(*order.lock().unwrap(), [("first", 1), ("second", 1), ("third", 1)]);
    }

    #[test]
    fn a_subscriber_hears_only_what_comes_after_it_subscribed_and_before_it_leaves() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        ctl.apply(volume(&ctl, 0.9)).unwrap();
        let (sink, heard) = recorder();
        let id = ctl.subscribe(sink);
        ctl.apply(volume(&ctl, 0.5)).unwrap();
        assert!(ctl.unsubscribe(id));
        ctl.apply(volume(&ctl, 0.25)).unwrap();
        let heard = heard.lock().unwrap();
        assert_eq!(heard.iter().map(|a| a.rev).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn unsubscribing_twice_or_an_id_nobody_has_does_nothing() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (sink, _heard) = recorder();
        let (other, other_heard) = recorder();
        let id = ctl.subscribe(sink);
        ctl.subscribe(other);
        assert!(ctl.unsubscribe(id));
        assert!(!ctl.unsubscribe(id), "already gone");
        ctl.apply(volume(&ctl, 0.5)).unwrap();
        assert_eq!(other_heard.lock().unwrap().len(), 1, "the other one is still there");
    }

    #[test]
    fn subscriber_ids_are_not_reused() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (a, _ha) = recorder();
        let (b, _hb) = recorder();
        let first = ctl.subscribe(a);
        ctl.unsubscribe(first);
        let second = ctl.subscribe(b);
        assert_ne!(first, second);
        assert!(!ctl.unsubscribe(first), "the old id does not reach the new subscriber");
    }

    const TWO_LANES: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n";

    // -- Subscribers: what they have been told --

    use crate::app::snapshot::Snapshot;

    /// What a mirror was told.
    #[derive(Clone, Debug, PartialEq)]
    enum Heard {
        /// An intent, by the revision it made.
        Applied(u64),
        /// A snapshot, by its revision.
        Resynced(u64),
    }

    #[derive(Default)]
    struct MirrorState {
        /// What the mirror knows of the session; None once an event did not fit it.
        copy: Option<Snapshot>,
        heard: Vec<Heard>,
    }

    /// A subscriber that keeps a copy of the session the way a client does: it starts
    /// from a snapshot, applies the events it hears, and takes a snapshot it is told
    /// of as its new copy.
    #[derive(Clone, Default)]
    struct Mirror(Arc<Mutex<MirrorState>>);

    impl Mirror {
        /// Subscribe to `ctl`, from the snapshot it has now.
        fn join(ctl: &mut Controller) -> Mirror {
            let mirror = Mirror::default();
            mirror.0.lock().unwrap().copy = Some(ctl.snapshot());
            ctl.subscribe(Box::new(mirror.clone()));
            mirror
        }

        fn copy(&self) -> Snapshot {
            self.0.lock().unwrap().copy.clone().expect("the copy fits what it has been told")
        }

        fn heard(&self) -> Vec<Heard> {
            self.0.lock().unwrap().heard.clone()
        }
    }

    impl EventSink for Mirror {
        fn applied(&mut self, applied: &Applied) {
            let mut state = self.0.lock().unwrap();
            state.heard.push(Heard::Applied(applied.rev));
            if let Some(copy) = &mut state.copy {
                if copy.apply(applied).is_err() {
                    state.copy = None;
                }
            }
        }

        fn resync(&mut self, snapshot: &Snapshot) {
            let mut state = self.0.lock().unwrap();
            state.heard.push(Heard::Resynced(snapshot.rev));
            state.copy = Some(snapshot.clone());
        }
    }

    /// A lane with an oscillator and a reverb (which has presets), and a lane without an instrument.
    const WITH_REVERB: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
"#;

    #[test]
    fn a_change_no_intent_made_is_told_to_every_subscriber_as_a_snapshot_of_the_next_revision() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let first = Mirror::join(&mut ctl);
        let second = Mirror::join(&mut ctl);
        let (follower, followed) = recorder();
        ctl.subscribe(follower);

        // What the TUI's `b` does: the model is edited, and no intent is applied.
        ctl.model.bpm = 90.0;
        assert!(ctl.resync());

        assert_eq!(ctl.rev, 1, "a revision of its own");
        for mirror in [&first, &second] {
            assert_eq!(mirror.heard(), [Heard::Resynced(1)]);
            assert_eq!((mirror.copy().rev, mirror.copy().meta.bpm), (1, 90.0));
            assert_eq!(mirror.copy(), ctl.snapshot());
        }
        assert!(followed.lock().unwrap().is_empty(), "a sink that follows intents hears of no intent, and need not know of snapshots");
    }

    #[test]
    fn a_save_that_a_front_end_makes_without_an_intent_is_told_as_a_snapshot_that_says_nothing_is_unsaved() {
        let dir = tempfile::tempdir().unwrap();
        let (_engine, mut ctl) = controller_for(WITH_REVERB);
        ctl.session_path = Some(dir.path().join("song.toml"));
        let mirror = Mirror::join(&mut ctl);
        let osc = ctl.model.instruments[0].node;
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 2.0 }).unwrap();
        assert!(mirror.copy().meta.dirty);

        // What the TUI's Ctrl+S does: the file is written, and no intent is applied.
        ctl.save().unwrap();
        assert!(ctl.resync(), "the subscribers are told, as a snapshot");
        assert_eq!(mirror.heard().last(), Some(&Heard::Resynced(2)));
        assert!(!mirror.copy().meta.dirty);
        assert_eq!(mirror.copy(), ctl.snapshot());
    }

    #[test]
    fn a_module_that_is_saved_is_told_as_an_event_that_neither_marks_the_session_nor_cleans_it_and_leaves_nothing_for_a_resync() {
        let (library, sessions) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_engine, mut ctl) = controller_for(WITH_REVERB);
        ctl.modules_dir = Some(library.path().to_path_buf());
        ctl.session_path = Some(sessions.path().join("song.toml"));
        let mirror = Mirror::join(&mut ctl);
        let osc = ctl.model.instruments[0].node;

        // A session that has been changed is still unsaved after a module is saved from it: the session's own file is not what was written.
        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 2.0 }).unwrap();
        assert!(mirror.copy().meta.dirty);
        let saved = ctl.apply(Intent::SaveModule { node: osc, name: "Room".to_string() }).unwrap();
        assert_eq!(saved.events, [Event::ModuleSaved { name: "Room".to_string() }]);
        assert!(mirror.copy().meta.dirty, "the module is a file of its own: the session is as unsaved as it was");
        assert_eq!(mirror.heard().last(), Some(&Heard::Applied(saved.rev)));
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert!(!ctl.resync(), "what a mirror keeps is what the session is: nothing to tell again");

        // And a session that has been saved is as saved after it.
        ctl.save().unwrap();
        assert!(ctl.resync());
        assert!(!mirror.copy().meta.dirty);
        ctl.apply(Intent::SaveModule { node: osc, name: "Room".to_string() }).unwrap();
        assert!(!mirror.copy().meta.dirty, "a module that is saved is no change of the session");
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert!(!ctl.resync());
        assert!(!ctl.dirty);
    }

    #[test]
    fn what_an_intent_changed_is_not_told_again_as_a_snapshot() {
        let (_engine, mut ctl) = controller_for(WITH_REVERB);
        let mirror = Mirror::join(&mut ctl);
        let osc = ctl.model.instruments[0].node;
        let reverb = ctl.model.instruments[0].effects[0].node;
        let detune = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.name == "detune").unwrap().index;
        for intent in [
            Intent::SetParam { node: osc, param: detune, value: 3.0 },
            Intent::SetGain { node: osc, gain: Gain::Volume, value: 2.0 },
            Intent::SetGain { node: reverb, gain: Gain::Mix, value: 0.25 },
            Intent::LoadPreset { node: reverb, preset: "cathedral".to_string() },
        ] {
            ctl.apply(intent).unwrap();
            assert!(!ctl.resync(), "the subscribers heard it");
        }
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Applied(2), Heard::Applied(3), Heard::Applied(4)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert_eq!(ctl.rev, 4, "no revision was spent on telling them again");
    }

    #[test]
    fn a_change_made_after_an_intent_is_told_with_the_revision_after_it() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let mirror = Mirror::join(&mut ctl);
        ctl.apply(volume(&ctl, 0.5)).unwrap();
        ctl.model.bpm = 90.0;
        assert!(ctl.resync());
        ctl.apply(volume(&ctl, 0.25)).unwrap();
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2), Heard::Applied(3)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert_eq!((mirror.copy().rev, mirror.copy().meta.bpm), (3, 90.0));
    }

    #[test]
    fn nobody_is_told_while_nobody_listens_and_a_subscriber_starts_from_the_session_as_it_is() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        ctl.model.bpm = 80.0;
        assert!(!ctl.resync(), "there is nobody to tell");
        assert_eq!(ctl.rev, 0, "and no revision is spent on it");

        let mirror = Mirror::join(&mut ctl);
        assert_eq!(mirror.copy().meta.bpm, 80.0);
        assert!(!ctl.resync(), "it has what the session has");
        ctl.model.bpm = 100.0;
        assert!(ctl.resync());
        assert_eq!(mirror.copy().meta.bpm, 100.0);
    }

    #[test]
    fn a_number_that_is_not_a_number_is_told_once_and_not_by_every_resync() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let mirror = Mirror::join(&mut ctl);
        // (No popup or file should let one in, but a plugin may report one, and NaN is not
        // equal to itself: a snapshot that holds one differs from every other, itself too.)
        ctl.model.bpm = f32::NAN;
        assert!(ctl.resync(), "it is a change");
        for _ in 0..5 {
            assert!(!ctl.resync(), "and it has been told");
        }
        assert_eq!(ctl.rev, 1, "no revision is spent on telling it again");
        assert_eq!(mirror.heard(), [Heard::Resynced(1)]);

        // The rest of the session is still watched while it is there.
        ctl.model.instruments[1].volume = 3.0;
        assert!(ctl.resync());
        assert_eq!(mirror.heard(), [Heard::Resynced(1), Heard::Resynced(2)]);
        assert!(!ctl.resync());
        // And a number that is one again is a change.
        ctl.model.bpm = 100.0;
        assert!(ctl.resync());
        assert_eq!(mirror.copy().meta.bpm, 100.0);
        assert!(!ctl.resync());
    }

    #[test]
    fn the_last_subscriber_to_leave_takes_what_was_told_with_it() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (sink, _heard) = recorder();
        let id = ctl.subscribe(sink);
        assert!(ctl.unsubscribe(id));
        ctl.model.bpm = 70.0;
        assert!(!ctl.resync(), "nobody is left");

        let mirror = Mirror::join(&mut ctl);
        assert!(!ctl.resync(), "the new one starts from the session as it is, not as it was when the old one left");
        assert_eq!(mirror.copy().meta.bpm, 70.0);
    }

    #[test]
    fn subscribers_share_what_they_have_been_told_so_a_late_one_may_be_told_again() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let early = Mirror::join(&mut ctl);
        ctl.model.bpm = 90.0; // not told yet
        let late = Mirror::join(&mut ctl);
        assert_eq!(late.copy().meta.bpm, 90.0, "it started from the session as it is");
        assert_eq!(early.copy().meta.bpm, 120.0);
        assert!(ctl.resync(), "the early one is behind");
        assert_eq!(early.copy(), ctl.snapshot());
        assert_eq!(late.copy(), ctl.snapshot());
    }

    #[test]
    fn an_intent_about_what_the_subscribers_have_not_heard_of_is_followed_by_a_snapshot_for_them_to_take() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let mirror = Mirror::join(&mut ctl);
        let lane = ctl.add_lane(); // behind the subscribers' backs
        let node = ctl.model.instruments[lane].node;
        ctl.apply(Intent::SetGain { node, gain: Gain::Volume, value: 0.5 }).unwrap();
        // The event names a node the copy has not got, so the copy is lost.
        assert!(mirror.0.lock().unwrap().copy.is_none());

        assert!(ctl.resync(), "they are told again, from the start");
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert!(!ctl.resync(), "and that is the end of it");
    }

    #[test]
    fn the_subscribers_end_up_where_the_session_is_whatever_was_done_to_it() {
        let (_engine, mut ctl) = controller_for(WITH_REVERB);
        let mirror = Mirror::join(&mut ctl);
        let osc = ctl.model.instruments[0].node;
        let reverb = ctl.model.instruments[0].effects[0].node;

        ctl.apply(Intent::SetGain { node: osc, gain: Gain::Volume, value: 0.5 }).unwrap();
        assert_eq!(mirror.copy(), ctl.snapshot());

        // A lane is added, and the volume of another edited, without intents.
        ctl.add_lane();
        ctl.model.instruments[1].volume = 3.0;
        ctl.apply(Intent::SetGain { node: reverb, gain: Gain::Mix, value: 0.75 }).unwrap();
        assert_ne!(mirror.copy(), ctl.snapshot(), "until they are told, they have the old structure");
        assert!(ctl.resync());
        assert_eq!(mirror.copy(), ctl.snapshot());

        // The new lane takes an intent, and the piano's scale changes under the TUI's hands.
        let added = ctl.model.instruments[2].node;
        ctl.apply(Intent::SetGain { node: added, gain: Gain::Volume, value: 1.5 }).unwrap();
        ctl.piano_filter.set_scale(crate::scale::ScaleSetting::parse("F# dorian").unwrap());
        assert!(ctl.resync());
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert_eq!(mirror.copy().meta.piano.scale, "F# dorian");

        // Saved: the flag that says so changes with no intent.
        ctl.dirty = false;
        assert!(ctl.resync());
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert!(!mirror.copy().meta.dirty);
        assert!(!ctl.resync());
    }

    #[test]
    fn a_change_of_structure_is_told_to_the_subscribers_as_a_snapshot_right_after_the_intent() {
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        let cables = |ctl: &Controller| ctl.snapshot().graph.edges.iter().filter(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })).count();
        assert_eq!(cables(&ctl), 1);
        let mirror = Mirror::join(&mut ctl);
        let edge = ctl.model.free_modulators[0].targets[0].edge;

        let applied = ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        // The intent is told (there is nothing in it for a client to apply), and the snapshot follows, a revision on.
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(ctl.rev, 2, "the snapshot has a revision of its own");
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert_eq!(cables(&ctl), 0);
        assert!(mirror.copy().meta.dirty);
        assert!(!ctl.resync(), "and there is nothing left to tell");
    }

    #[test]
    fn a_modulator_added_by_an_intent_is_in_the_snapshot_the_subscribers_are_given() {
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        let modulators = |snapshot: &Snapshot| {
            snapshot.graph.nodes.iter().filter(|n| matches!(n.kind, crate::app::graph::NodeKind::Modulator { .. })).map(|n| n.id).collect::<Vec<_>>()
        };
        let mirror = Mirror::join(&mut ctl);
        let before = modulators(&mirror.copy());
        let lane = ctl.model.instruments[0].node;

        let applied = ctl.apply(Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: Some(lane) }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        let after = modulators(&mirror.copy());
        assert_eq!(after.len(), before.len() + 1);
        let added = ctl.model.free_modulators.last().unwrap().node;
        assert!(after.contains(&added) && !before.contains(&added), "the one that was added, and a node of its own");
        let node = mirror.copy().graph.nodes.into_iter().find(|n| n.id == added).unwrap();
        assert_eq!(node.parent, None, "in no lane");
        assert!(matches!(node.kind, crate::app::graph::NodeKind::Modulator { source: crate::app::graph::ModulatorSource::Envelope { .. }, hears } if hears == Some(lane)), "and it hears the lane it was added for");
    }

    #[test]
    fn a_cable_drawn_by_an_intent_is_in_the_snapshot_the_subscribers_are_given_with_the_slot_of_the_value_it_drives() {
        use crate::app::graph::{Endpoint, Port};
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        let cables = |snapshot: &Snapshot| snapshot.graph.edges.iter().filter(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })).count();
        let mirror = Mirror::join(&mut ctl);
        let modulator = ctl.model.free_modulators[0].node;
        let filter = ctl.model.instruments[0].effects[0].node;
        let param = ctl.model.instruments[0].effects[0].params[0].index;
        assert_eq!(cables(&mirror.copy()), 0);

        let to = Endpoint { node: filter, port: Port::Param { index: param } };
        let applied = ctl.apply(Intent::AddModEdge { modulator, to }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        let told = mirror.copy();
        assert_eq!(cables(&told), 1);
        let edge = told.graph.edges.iter().find(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })).unwrap();
        assert_eq!((edge.from, edge.to), (Endpoint { node: modulator, port: Port::ModOut }, to));
        assert!(matches!(edge.kind, crate::app::graph::EdgeKind::Modulation { amount, .. } if amount == 0.5));
        // The value the cable drives is published from now on, and the layout says where.
        let slot = ctl.model.free_modulators[0].targets[0].effective.expect("a parameter's value has a slot").index();
        let value = told.telemetry.modulation.iter().find(|e| e.kind == crate::app::layout::SlotKind::Effective && e.node == filter && e.port == Port::Param { index: param });
        assert_eq!(value.map(|e| e.slot), Some(slot));
        assert!(mirror.copy().meta.dirty);
    }

    #[test]
    fn an_audio_cable_drawn_by_an_intent_is_in_the_snapshot_the_subscribers_are_given_and_its_level_is_told_as_an_event() {
        use crate::app::graph::{EdgeKind, Endpoint, Port};
        let (_engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        let mirror = Mirror::join(&mut ctl);
        let (from, to) = (Endpoint { node: n.lane[1], port: Port::AudioOut }, Endpoint { node: n.fx[0], port: Port::AudioIn });
        let applied = ctl.apply(Intent::AddCable { from, to, gain: Some(0.5) }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        let told = mirror.copy();
        let edge = told.graph.edges.iter().find(|e| matches!(e.kind, EdgeKind::Cable { .. })).expect("the cable is in the picture");
        assert_eq!((edge.from, edge.to), (from, to));
        let EdgeKind::Cable { id, gain } = edge.kind else { unreachable!() };
        assert_eq!(gain, 0.5);
        assert!(told.meta.dirty);
        // Its level is an event that the copy applies, and no snapshot follows it.
        let applied = ctl.apply(Intent::SetCableGain { edge: id, gain: 1.5 }).unwrap();
        assert_eq!(applied, Applied::of(3, vec![Event::CableGainSet { edge: id, gain: 1.5 }]));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2), Heard::Applied(3)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        // Taking it out is a change of structure, and the cable is gone from the picture.
        ctl.apply(Intent::RemoveEdge { edge: id }).unwrap();
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert!(!mirror.copy().graph.edges.iter().any(|e| matches!(e.kind, EdgeKind::Cable { .. })));
    }

    #[test]
    fn the_level_of_one_of_two_audio_cables_is_applied_by_a_copy_to_that_cable_and_to_no_other() {
        use crate::app::graph::{EdgeKind, Endpoint, Port};
        let (_engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        let mirror = Mirror::join(&mut ctl);
        let master = Endpoint { node: NodeId::MASTER, port: Port::AudioIn };
        for lane in n.lane {
            ctl.apply(Intent::AddCable { from: Endpoint { node: lane, port: Port::AudioOut }, to: master, gain: Some(0.5) }).unwrap();
        }
        let (first, second) = (ctl.model.cables[0].id, ctl.model.cables[1].id);
        ctl.apply(Intent::SetCableGain { edge: second, gain: 1.5 }).unwrap();
        assert_eq!(mirror.copy(), ctl.snapshot());
        let levels: Vec<_> = mirror.copy().graph.edges.iter().filter_map(|e| if let EdgeKind::Cable { id, gain } = e.kind { Some((id, gain)) } else { None }).collect();
        assert_eq!(levels, [(first, 0.5), (second, 1.5)]);
    }

    #[test]
    fn a_cable_to_a_modulators_setting_drawn_by_an_intent_is_in_the_snapshot_the_subscribers_are_given_ending_at_the_port_of_the_setting() {
        use crate::app::graph::{Endpoint, Port};
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        let mirror = Mirror::join(&mut ctl);
        // The group's LFO drives the rate of the LFO of the member that has one.
        let (group_lfo, lane_lfo) = (hearing_group(&ctl.model, 0)[0].node, ctl.model.free_modulators[0].node);

        let to = Endpoint { node: lane_lfo, port: Port::Rate };
        let applied = ctl.apply(Intent::AddModEdge { modulator: group_lfo, to }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        let told = mirror.copy();
        let cables: Vec<_> = told.graph.edges.iter().filter(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })).collect();
        assert_eq!(cables.len(), 1);
        assert_eq!((cables[0].from, cables[0].to), (Endpoint { node: group_lfo, port: Port::ModOut }, to));
        assert!(matches!(cables[0].kind, crate::app::graph::EdgeKind::Modulation { amount, .. } if amount == 0.5));
        // What it drives is a setting of a modulator: the value that setting has is published at its port (f132).
        let value = told.telemetry.modulation.iter().find(|e| e.kind == crate::app::layout::SlotKind::Effective && e.node == lane_lfo);
        assert_eq!(value.map(|e| e.port), Some(Port::Rate), "a cable to a modulator's own field has an effective-value slot, named at the port it ends at");
        assert!(mirror.copy().meta.dirty);
    }

    #[test]
    fn a_modulator_taken_out_by_an_intent_is_gone_from_the_snapshot_the_subscribers_are_given() {
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        let edge_count = |snapshot: &Snapshot| snapshot.graph.edges.iter().filter(|e| matches!(e.kind, crate::app::graph::EdgeKind::Modulation { .. })).count();
        let mirror = Mirror::join(&mut ctl);
        let node = ctl.model.free_modulators[0].node;
        assert!(mirror.copy().graph.nodes.iter().any(|n| n.id == node));
        assert_eq!(edge_count(&mirror.copy()), 1);

        let applied = ctl.apply(Intent::RemoveNode { node }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        assert!(!mirror.copy().graph.nodes.iter().any(|n| n.id == node), "the node is gone");
        assert_eq!(edge_count(&mirror.copy()), 0, "and so is the cable it drove");
        assert!(!ctl.resync(), "and there is nothing left to tell");
    }

    #[test]
    fn an_effect_moved_by_an_intent_is_in_its_new_place_in_the_snapshot_the_subscribers_are_given_with_the_audio_path_through_it_and_the_cables_on_the_same_plugins() {
        use crate::app::graph::{Edge, EdgeKind, Endpoint, NodeKind, Port};
        let (_engine, mut ctl) = controller_for(testutil::CHAINS);
        let mirror = Mirror::join(&mut ctl);
        let before = mirror.copy();
        let (lane, group) = (ctl.model.instruments[0].node, ctl.model.groups[0].node);
        let (filter, reverb) = (ctl.model.instruments[0].effects[0].node, ctl.model.instruments[0].effects[1].node);
        // The nodes of a lane's chain, in the order the graph lists them, and the links of its audio path.
        let chain_of = |snapshot: &Snapshot, parent: crate::app::model::NodeId| -> Vec<crate::app::model::NodeId> {
            snapshot.graph.nodes.iter().filter(|n| n.parent == Some(parent) && matches!(n.kind, NodeKind::Effect { .. })).map(|n| n.id).collect()
        };
        let links_from = |snapshot: &Snapshot, nodes: &[crate::app::model::NodeId]| -> Vec<(crate::app::model::NodeId, crate::app::model::NodeId, bool)> {
            snapshot
                .graph
                .edges
                .iter()
                .filter(|e| !matches!(e.kind, EdgeKind::Modulation { .. }) && nodes.contains(&e.from.node))
                .map(|e| (e.from.node, e.to.node, matches!(e.kind, EdgeKind::Member)))
                .collect()
        };
        assert_eq!(chain_of(&before, lane), [filter, reverb]);
        assert_eq!(links_from(&before, &[lane, filter, reverb]), [(lane, filter, false), (filter, reverb, false), (reverb, group, true)]);

        let applied = ctl.apply(Intent::MoveEffect { node: filter, index: 1 }).unwrap();
        assert_eq!(applied, Applied::restructured(1));
        assert_eq!(mirror.heard(), [Heard::Applied(1), Heard::Resynced(2)]);
        assert_eq!(mirror.copy(), ctl.snapshot());
        let told = mirror.copy();
        // The graph lists the reverb first now, and the audio path goes lane, reverb, filter, and out to the group, the last link being the member's.
        assert_eq!(chain_of(&told, lane), [reverb, filter]);
        assert_eq!(links_from(&told, &[lane, filter, reverb]), [(lane, reverb, false), (reverb, filter, false), (filter, group, true)]);
        // The links are where they were in the list of edges: the audio path first, one chain after the other.
        let positions = |snapshot: &Snapshot| -> Vec<usize> { snapshot.graph.edges.iter().enumerate().filter(|(_, e)| !matches!(e.kind, EdgeKind::Modulation { .. })).map(|(i, _)| i).collect() };
        assert_eq!(positions(&told), positions(&before));
        // The cables are the same cables, on the same plugins: what each drives is the node and the parameter it was.
        let cables = |snapshot: &Snapshot| -> Vec<Edge> { snapshot.graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).cloned().collect() };
        assert_eq!(cables(&told), cables(&before));
        assert!(cables(&told).iter().any(|e| e.to == Endpoint { node: filter, port: Port::Param { index: 0 } }));
        // The slots the audio thread publishes are the same, for the same nodes: a move changes no layout.
        assert_eq!(told.telemetry, before.telemetry);
        assert!(told.meta.dirty);
        assert!(!ctl.resync(), "and there is nothing left to tell");
    }

    #[test]
    fn nobody_is_told_of_a_change_of_structure_while_nobody_listens() {
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5));
        let edge = ctl.model.free_modulators[0].targets[0].edge;
        ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        assert_eq!(ctl.rev, 1, "no snapshot is made for nobody");
    }

    #[test]
    fn poll_tells_the_subscribers_what_the_audio_thread_reported() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        let mirror = Mirror::join(&mut ctl);
        engine.pattern_tx.send(recording(1)).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert_eq!(mirror.heard(), [Heard::Resynced(1)]);
        assert!(mirror.copy().meta.dirty, "the recording is part of the session");
        assert_eq!(mirror.copy(), ctl.snapshot());
    }

    #[test]
    fn poll_with_nothing_to_report_tells_nobody_anything() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let mirror = Mirror::join(&mut ctl);
        ctl.model.bpm = 90.0; // a change nobody has told them of: `poll` is not what finds it
        assert_eq!(ctl.poll(), Polled::default());
        assert!(mirror.heard().is_empty());
    }

    const CHANGED_MODEL: Polled = Polled { model_changed: true, catalog_changed: false };

    #[test]
    fn polling_with_nothing_to_report_changes_nothing() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        assert_eq!(ctl.poll(), Polled::default());
        assert!(!ctl.dirty);
    }

    #[test]
    fn a_finished_recording_becomes_the_lanes_pattern() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        // It carries the tempo the session has when the recording ends, and the
        // player's loop setting.
        ctl.model.bpm = 95.0;
        engine.pattern_tx.send(PatternNotification { looping: false, ..recording(1) }).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);

        let pattern = ctl.model.instruments[1].pattern.as_ref().expect("lane 1 has a pattern now");
        assert_eq!((pattern.bpm, pattern.length_beats, pattern.looping, pattern.enabled), (95.0, 2.0, false, true));
        assert_eq!((pattern.base_note, pattern.in_key, pattern.recording), (Some(60), false, false));
        assert_eq!(pattern.events, [(0, 0x90, 60, 100), (24_000, 0x80, 60, 0)]);
        assert!(ctl.model.instruments[0].pattern.is_none());
        assert!(ctl.dirty, "the recording is part of the session");
        assert_eq!(ctl.poll(), Polled::default(), "it is reported once");
    }

    #[test]
    fn re_recording_replaces_the_events_but_keeps_the_transpose_mode() {
        let (engine, mut ctl) = controller_for(
            r#"
[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = false
looping = false
transpose = "in_key"
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 12000
status = "off"
note = "C4"
velocity = 0
"#,
        );
        engine.pattern_tx.send(recording(0)).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);

        let pattern = ctl.model.instruments[0].pattern.as_ref().unwrap();
        assert!(pattern.in_key, "the transpose mode is the user's, not the recording's");
        assert_eq!((pattern.length_beats, pattern.looping, pattern.enabled), (2.0, true, true));
        assert_eq!(pattern.events, [(0, 0x90, 60, 100), (24_000, 0x80, 60, 0)]);
    }

    #[test]
    fn a_recording_with_no_notes_leaves_a_disabled_pattern() {
        // The user pressed `r` and played nothing: the player keeps an empty,
        // disabled pattern, and so does the model.
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine
            .pattern_tx
            .send(PatternNotification { base_note: None, enabled: false, events: vec![], ..recording(1) })
            .unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);

        let pattern = ctl.model.instruments[1].pattern.as_ref().expect("the lane still gets a pattern");
        assert!(!pattern.enabled);
        assert!(pattern.events.is_empty());
        assert_eq!(pattern.base_note, None);
    }

    #[test]
    fn a_recording_for_a_lane_that_is_gone_changes_nothing() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine.pattern_tx.send(recording(7)).unwrap();
        assert_eq!(ctl.poll(), Polled::default());
        assert!(ctl.model.instruments.iter().all(|lane| lane.pattern.is_none()));
        assert!(!ctl.dirty, "nothing changed");
    }

    #[test]
    fn recordings_are_folded_in_in_the_order_they_were_sent() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine.pattern_tx.send(PatternNotification { length_beats: 2.0, ..recording(0) }).unwrap();
        engine.pattern_tx.send(PatternNotification { length_beats: 3.0, ..recording(0) }).unwrap();
        engine.pattern_tx.send(recording(1)).unwrap();
        engine.pattern_tx.send(recording(7)).unwrap(); // changes nothing, and must not hide the rest
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert_eq!(ctl.model.instruments[0].pattern.as_ref().unwrap().length_beats, 3.0, "the later one wins");
        assert!(ctl.model.instruments[1].pattern.is_some(), "all of them are drained in one poll");
    }

    /// Lane 0 has an armed MIDI-learn modulator (index 0) and an LFO (index 1);
    /// its group has an armed one too.
    const ARMED: &str = r#"
[[group]]
[[group.modulator]]
type = "midi"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
type = "midi"
[[instrument.modulator]]
type = "lfo"
"#;

    fn capture(location: ModulatorLocation, mod_index: usize, source: MidiBindSource) -> MidiLearnedNotification {
        MidiLearnedNotification { location, mod_index, source }
    }

    #[test]
    fn a_captured_control_binds_the_armed_lane_modulator() {
        for source in [MidiBindSource::Cc(74), MidiBindSource::PitchBend, MidiBindSource::Aftertouch] {
            let (engine, mut ctl) = controller_for(ARMED);
            assert!(matches!(ctl.model.free_modulators[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }));
            engine.learn_tx.send(capture(ModulatorLocation::Free, 0, source)).unwrap();
            assert_eq!(ctl.poll(), CHANGED_MODEL, "{source:?}");

            let bound = &ctl.model.free_modulators[0].source;
            assert!(
                matches!(bound, ModSourceSlot::MidiLearn { source: Some(s), learning: false } if *s == source),
                "{source:?}: {bound:?}"
            );
            assert!(ctl.dirty, "{source:?}: the binding is part of the saved session");
            assert!(matches!(hearing_group(&ctl.model, 0)[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }), "the group's is untouched");
        }
    }

    #[test]
    fn a_capture_with_no_modulator_to_take_it_changes_nothing() {
        let (engine, mut ctl) = controller_for(ARMED);
        for (location, mod_index) in [
            (ModulatorLocation::Free, 1), // the LFO is not listening
            (ModulatorLocation::Free, 9), // no such modulator
        ] {
            engine.learn_tx.send(capture(location, mod_index, MidiBindSource::PitchBend)).unwrap();
        }
        assert_eq!(ctl.poll(), Polled::default());
        assert!(!ctl.dirty, "nothing changed");
        assert!(matches!(ctl.model.free_modulators[1].source, ModSourceSlot::Lfo { .. }));
        assert!(matches!(ctl.model.free_modulators[0].source, ModSourceSlot::MidiLearn { source: None, learning: true }));
    }

    #[test]
    fn captures_are_folded_in_in_the_order_they_were_sent() {
        let (engine, mut ctl) = controller_for(ARMED);
        // The model mirrors the audio thread, which may learn a modulator again:
        // the last word on lane 0's modulator is Cc(2). The group's comes after.
        engine.learn_tx.send(capture(ModulatorLocation::Free, 0, MidiBindSource::Cc(1))).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Free, 0, MidiBindSource::Cc(2))).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Free, 2, MidiBindSource::Cc(3))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert!(matches!(
            ctl.model.free_modulators[0].source,
            ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(2)), learning: false }
        ));
        assert!(
            matches!(
                ctl.model.free_modulators[2].source,
                ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(3)), learning: false }
            ),
            "all of them are drained in one poll"
        );
    }

    #[test]
    fn a_recording_and_a_capture_in_one_poll_are_both_reported() {
        let (engine, mut ctl) = controller_for(ARMED);
        engine.pattern_tx.send(recording(0)).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Free, 0, MidiBindSource::Cc(7))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL);
        assert!(ctl.model.instruments[0].pattern.is_some());
        assert!(matches!(ctl.model.free_modulators[0].source, ModSourceSlot::MidiLearn { source: Some(_), .. }));

        // Either one alone is enough, whichever order they are drained in.
        engine.pattern_tx.send(recording(0)).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Free, 0, MidiBindSource::Cc(7))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL, "the capture that finds nothing does not undo the recording");
        engine.pattern_tx.send(recording(7)).unwrap();
        engine.learn_tx.send(capture(ModulatorLocation::Free, 0, MidiBindSource::Cc(8))).unwrap();
        assert_eq!(ctl.poll(), CHANGED_MODEL, "the recording that finds nothing does not undo the capture");
    }

    fn controller_with_file(path: &Path) -> Controller {
        let (handles, _engine) = testutil::stub_engine();
        Controller::new(load(testutil::GROUPS).model, handles, Some(path.to_path_buf()))
    }

    #[test]
    fn save_writes_the_session_and_the_piano_settings_to_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&path);
        ctl.piano_filter.set_scale(crate::scale::ScaleSetting::parse("D dorian").unwrap());
        ctl.piano_filter.set_mode(PianoMode::Locked);
        ctl.dirty = true;
        ctl.save().unwrap();

        assert!(!ctl.dirty);
        let text = std::fs::read_to_string(&path).unwrap();
        let piano = session::parse(&text).unwrap().piano.expect("the piano settings are saved");
        assert_eq!((piano.scale.as_deref(), piano.locked), (Some("D dorian"), true));
        testutil::assert_same_model("the saved file", &ctl.model, &load(&text).model);
    }

    #[test]
    fn save_writes_the_tempo_and_a_load_gives_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&path);
        ctl.model.bpm = 87.5;
        ctl.save().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(session::parse(&text).unwrap().bpm, Some(87.5));
        assert_eq!(load(&text).model.bpm, 87.5, "(a session of no patterns would be at 120 without it)");
        testutil::assert_same_model("the saved file", &ctl.model, &load(&text).model);
    }

    #[test]
    fn save_writes_no_tempo_for_a_session_that_has_not_set_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let mut ctl = controller_with_file(&path);
        ctl.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("bpm"), "{text}");
        assert_eq!(session::parse(&text).unwrap().bpm, None);
    }

    #[test]
    fn save_writes_where_the_nodes_are_drawn_the_master_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.toml");
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(testutil::PLACED).model, handles, Some(path.clone()));
        ctl.save().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let config = session::parse(&text).unwrap();
        assert_eq!(config.master_ui, Some(session::NodeUi { x: 1200.0, y: 300.5 }), "{text}");
        assert_eq!(config.instruments[0].effects[0].ui, Some(session::NodeUi { x: 376.0, y: 0.25 }), "{text}");
        assert_eq!(config.instruments[1].ui, None, "{text}");
        testutil::assert_same_model("the saved file", &ctl.model, &load(&text).model);
    }

    #[test]
    fn save_needs_a_file_and_keeps_the_mark_when_it_fails() {
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(SessionModel::default(), handles, None);
        ctl.dirty = true;
        assert!(ctl.save().is_err());
        assert!(ctl.dirty);

        // A file where the directory should be: the write fails.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blocker"), "").unwrap();
        let mut ctl = controller_with_file(&dir.path().join("blocker").join("song.toml"));
        ctl.dirty = true;
        assert!(ctl.save().is_err());
        assert!(ctl.dirty, "a failed save has not saved anything");
    }

    #[test]
    fn load_plugin_gives_the_plugin_and_the_slot_that_describes_it() {
        let (mut handles, _engine) = testutil::stub_engine();
        handles.sample_rate = 44_100.0;
        let mut ctl = Controller::new(SessionModel::default(), handles, None);
        let (plugin, slot) = ctl.load_plugin("builtin:osc").expect("a built-in loads");
        assert_eq!(plugin.sample_rate(), 44_100.0, "it is loaded at the session's sample rate");
        assert_eq!((slot.name.as_str(), slot.id.as_str(), slot.format.as_str()), ("Oscillator", "builtin:osc", "Built-in"));
        assert!(slot.is_instrument);
        assert_eq!(slot.params.len(), plugin.parameters().len());
    }

    #[test]
    fn each_loaded_plugin_gets_a_node_of_its_own() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (_, first) = ctl.load_plugin("builtin:filter").unwrap();
        let (_, second) = ctl.load_plugin("builtin:filter").unwrap();
        assert_ne!(first.node, second.node);
        assert!(ctl.model.nodes().all(|(id, _)| id != first.node && id != second.node), "nor does any node in the session have them");
    }

    #[test]
    fn a_plugin_that_does_not_load_is_none() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        assert!(ctl.load_plugin("builtin:no-such-plugin").is_none());
        assert!(ctl.load_plugin("./no-such-plugin.clap").is_none());
    }

    #[test]
    fn set_instrument_swaps_it_into_the_audio_thread_and_the_model() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        let (plugin, slot) = ctl.load_plugin("builtin:filter").unwrap();
        ctl.set_instrument(1, plugin, slot);

        assert_eq!(ctl.model.instruments[1].instrument.as_ref().unwrap().name, "Filter");
        assert_eq!(ctl.model.instruments[0].instrument.as_ref().unwrap().name, "Oscillator", "the other lane is untouched");
        let lane = &ctl.model.instruments[1];
        assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node, "the plugin sits in the lane's node");
        ctl.model.check_ids().unwrap();
        let sent: Vec<String> = testutil::described(engine.cmd_rx.try_iter());
        assert_eq!(sent, ["SwapInstrument inst=1 plugin=Filter bufs=2 remap=false"]);
    }

    #[test]
    fn set_instrument_for_a_lane_that_is_gone_changes_no_lane() {
        let (_engine, mut ctl) = controller_for(TWO_LANES);
        let (plugin, slot) = ctl.load_plugin("builtin:filter").unwrap();
        ctl.set_instrument(9, plugin, slot);
        assert!(ctl.model.instruments.iter().all(|lane| lane.instrument.as_ref().unwrap().name == "Oscillator"));
    }

    #[test]
    fn plugins_the_scan_finds_are_reported_once() {
        let (handles, _engine) = testutil::stub_engine();
        let mut ctl = Controller::new(SessionModel::default(), handles, None);
        let (scan, batches) = crossbeam_channel::unbounded();
        ctl.catalog.follow(batches);
        let found = |name: &str| crate::plugin::PluginInfo {
            name: name.to_string(),
            id: format!("builtin:{name}"),
            is_instrument: false,
            param_count: 0,
            preset_count: 0,
            path: String::new(),
            scan_ms: 0,
        };
        scan.send(vec![found("Delay")]).unwrap();
        assert_eq!(ctl.poll(), Polled { model_changed: false, catalog_changed: true });
        assert_eq!(ctl.catalog.plugins().len(), 1);
        assert_eq!(ctl.poll(), Polled::default());
        assert!(!ctl.dirty, "the catalog is not part of the session");
    }

    // -- modulators and their targets --

    /// A group (one member) with a lane LFO and a group LFO, and a bare lane.
    const WITH_MODULATORS: &str = r#"
[[group]]
[[group.modulator]]
rate = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0

[[instrument]]
plugin = "builtin:osc"
"#;

    fn sent(engine: &StubEngine) -> Vec<String> {
        testutil::described(engine.cmd_rx.try_iter())
    }

    /// `sent`, and with it the node that each modulator in it was given (`describe` leaves it out), in the order they were sent.
    fn sent_with_modulator_nodes(engine: &StubEngine) -> (Vec<String>, Vec<NodeId>) {
        let commands: Vec<GraphCommand> = engine.cmd_rx.try_iter().collect();
        let nodes = commands
            .iter()
            .filter_map(|command| match command {
                GraphCommand::InsertModulator { node, .. } => Some(*node),
                _ => None,
            })
            .collect();
        (testutil::described(commands), nodes)
    }

    fn lfo_source() -> ModSourceSlot {
        ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 1.0, retrigger: false, fade: 0.0, sync: 0 }
    }

    /// A cable to the cutoff of the first effect of lane 0, which is the filter of the scenes that these tests bind targets in.
    fn cutoff_target(ctl: &Controller, depth: f32) -> NewTarget {
        NewTarget {
            aim: Driven::Param { plugin: ctl.model.instruments[0].effects[0].node, index: 0 },
            param_name: "cutoff".to_string(),
            depth,
            base_value: 0.35,
            param_min: 0.0,
            param_max: 1.0,
        }
    }

    #[test]
    fn add_modulator_puts_one_at_the_end_of_the_rack_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let before = ctl.model.free_modulators.len();
        assert_eq!(ctl.add_modulator(ModulatorLocation::Free, lfo_source()), Some(before));
        let envelope = ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 };
        assert_eq!(ctl.add_modulator(ModulatorLocation::Free, envelope), Some(before + 1));
        let learn = ModSourceSlot::MidiLearn { source: None, learning: true };
        assert_eq!(ctl.add_modulator(ModulatorLocation::Free, learn), Some(before + 2));
        let (told, nodes) = sent_with_modulator_nodes(&engine);
        assert_eq!(
            told,
            [
                format!("InsertFreeModulator index={before} source=Lfo {{ waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }}"),
                format!("InsertFreeModulator index={} source=Envelope {{ attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }}", before + 1),
                format!("InsertFreeModulator index={} source=MidiLearn {{ source: None, value: 0.0, learning: true }}", before + 2),
            ]
        );
        let rack = &ctl.model.free_modulators;
        assert_eq!(nodes, [rack[before].node, rack[before + 1].node, rack[before + 2].node], "the audio thread names each modulator by the node that the model gave it");
        assert_eq!(rack.len(), before + 3);
        assert!(matches!(rack[before].source, ModSourceSlot::Lfo { rate, .. } if rate == 1.0));
        assert!(rack[before].targets.is_empty());
        assert_ne!(rack[before].node, rack[before + 1].node, "a node of its own each");
        assert!(rack[..before].iter().all(|m| m.node != rack[before].node));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_target_binds_a_parameter_to_a_lane_modulator() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        assert_eq!(
            sent(&engine),
            [testutil::told(&ctl.model, "AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 1, param_index: 0 } depth=0.500 base=0.350 range=[0.000, 1.000]")]
        );
        let target = &ctl.model.free_modulators[0].targets[0];
        assert_eq!((target.param_name.as_str(), target.depth), ("cutoff", 0.5));
        assert_eq!((target.param_min, target.param_max), (0.0, 1.0));
        let filter = ctl.model.instruments[0].effects[0].node;
        assert_eq!(ctl.model.kind_of_target(ModulatorLocation::Free, 0, 0), Some(ModTargetKind::FreeParam { plugin: filter, param_index: 0 }));
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, -0.25)));
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 2, "after the first");
    }

    #[test]
    fn each_target_the_controller_binds_gets_an_edge_id_of_its_own() {
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        let before: Vec<_> = ctl.model.free_modulators.iter().flat_map(|m| &m.targets).map(|t| t.edge).collect();
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.25)));
        let targets = &ctl.model.free_modulators[0].targets;
        let (a, b) = (targets[targets.len() - 2].edge, targets[targets.len() - 1].edge);
        assert_ne!(a, b);
        assert!(!before.contains(&a) && !before.contains(&b), "and not one a target already has");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_target_to_a_modulator_that_is_not_there_does_nothing() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let slots_before = ctl.model.slots.high_water();
        assert!(!ctl.add_target(ModulatorLocation::Free, 5, cutoff_target(&ctl, 0.5)));
        assert!(!ctl.add_target(ModulatorLocation::Free, 99, cutoff_target(&ctl, 0.5)));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.slots.high_water(), slots_before, "no slot is used up for nothing");
        assert!(ctl.model.free_modulators.iter().all(|m| m.targets.is_empty()));
    }

    #[test]
    fn modulators_and_targets_do_not_mark_the_session_changed() {
        // The front-end says when an edit is a change: these only do the work.
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        ctl.add_modulator(ModulatorLocation::Free, lfo_source());
        ctl.add_target(ModulatorLocation::Free, 1, cutoff_target(&ctl, 0.5));
        assert!(!ctl.dirty);
    }

    /// What each modulator of the rack at `at` drives, as kinds: the places that the audio thread's racks name it by.
    fn driven(model: &SessionModel, at: ModulatorLocation) -> Vec<Vec<ModTargetKind>> {
        let rack = model.modulators(at).expect("the rack is there");
        rack.iter()
            .enumerate()
            .map(|(m, modulator)| (0..modulator.targets.len()).map(|t| model.kind_of_target(at, m, t).expect("a cable drives something that is in reach")).collect())
            .collect()
    }

    #[test]
    fn remove_modulator_takes_one_out_of_the_rack_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        let before = ctl.model.free_modulators.len();
        assert_eq!(ctl.add_modulator(ModulatorLocation::Free, lfo_source()), Some(before));
        let rack_nodes = |ctl: &Controller| ctl.model.free_modulators.iter().map(|m| m.node).collect::<Vec<_>>();
        let nodes = rack_nodes(&ctl);
        sent(&engine);

        // The first goes, and then the one that was second is the first and goes in its turn: the audio thread is told the place each had at the time.
        assert!(ctl.remove_modulator(ModulatorLocation::Free, 0));
        assert_eq!(sent(&engine), ["RemoveFreeModulator index=0"]);
        assert_eq!(rack_nodes(&ctl), nodes[1..], "the others stay, as they were");
        ctl.model.check_ids().unwrap();
        assert!(ctl.model.locate(nodes[0]).is_none(), "its node is gone");

        let last = ctl.model.free_modulators.len() - 1;
        assert!(ctl.remove_modulator(ModulatorLocation::Free, last));
        assert_eq!(sent(&engine), [format!("RemoveFreeModulator index={last}")]);
        assert_eq!(rack_nodes(&ctl), nodes[1..nodes.len() - 1]);
        assert!(ctl.model.locate(*nodes.last().unwrap()).is_none());
    }

    #[test]
    fn remove_edge_sends_the_audio_thread_each_removal_in_order_the_depth_cables_first() {
        let (engine, mut ctl) = controller_for(testutil::CROSS_MOD);
        sent(&engine);
        // The member's first modulator's one cable: the member's third modulator's cable and the group's drive its depth.
        let detune = ctl.model.free_modulators[0].targets[0].edge;
        assert!(ctl.remove_edge(detune));
        // (And the detune that the cable drove is given the value the user set.)
        assert_eq!(
            sent(&engine),
            [
                "RemoveFreeModTarget mod=4 target=6",
                "RemoveFreeModTarget mod=2 target=1",
                "RemoveFreeModTarget mod=0 target=0",
                "SetParameter inst=0 slot=0 param=1 value=0.000"
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn remove_edge_for_a_cable_that_is_not_there_does_nothing() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        assert!(!ctl.remove_edge(EdgeId(77)));
        assert!(sent(&engine).is_empty());
    }

    /// Every modulator's cables as the model has them, in the order `AudioGraph::cables` has them.
    fn model_cables(model: &SessionModel) -> crate::plugin::chain::RackCables {
        testutil::model_racks(model)
    }

    /// A controller over `session`, and a real audio graph that has been sent what loading it takes.
    fn controller_on_a_real_graph(session: &str) -> (crate::plugin::chain::AudioGraph, Controller) {
        let (handles, engine) = testutil::stub_engine_of(4096);
        let model = testutil::load_onto(session, &handles.cmd_tx);
        let mut graph = crate::plugin::chain::AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        let ctl = Controller::new(model, handles, None);
        graph.drain_commands();
        (graph, ctl)
    }

    const SCENES_OF_CABLES: [&str; 3] = [testutil::CROSS_MOD, testutil::CABLES_THAT_DRIVE_CABLES, testutil::CABLES_THAT_DRIVE_EACH_OTHER];

    /// The value the cables that drive parameter `param` of the plugin `plugin` (the lane's own, and its group's) are centred on, on the audio thread.
    fn bases_of_the_cables_on(graph: &crate::plugin::chain::AudioGraph, plugin: NodeId, param: u32) -> Vec<f32> {
        graph
            .cable_bases()
            .into_iter()
            .filter(|(_, _, _, aim, _)| matches!(aim, Driven::Param { plugin: p, index } if *p == plugin && *index == param))
            .map(|(.., base)| base)
            .collect()
    }

    #[test]
    fn a_preset_that_is_loaded_is_what_the_cables_that_drive_its_plugin_are_centred_on_on_the_audio_thread() {
        let (mut graph, mut ctl) = controller_on_a_real_graph(testutil::CHAINS);
        // Lane 0's reverb (slot 2) has a cable of the lane's own LFO on its damping, and one of the group's.
        let reverb = ctl.model.instruments[0].effects[1].node;
        let damping = ctl.model.instruments[0].effects[1].params.iter().find(|p| p.name == "damping").expect("a damping").index;
        let cutoff = ctl.model.instruments[0].effects[0].params.iter().find(|p| p.name == "cutoff").expect("a cutoff").index;
        let filter = ctl.model.instruments[0].effects[0].node;
        let before = graph.plugin_parameter(0, 2, damping).expect("the reverb has it");
        assert_eq!(bases_of_the_cables_on(&graph, reverb, damping), [before, before], "centred on what it is, as loaded");
        let filter_before = bases_of_the_cables_on(&graph, filter, cutoff);
        assert!(!filter_before.is_empty());

        // A preset that has another damping than the reverb has (some of them do).
        let presets: Vec<String> = ctl.model.instruments[0].effects[1].presets.iter().map(|p| p.id.clone()).collect();
        let mut changed = None;
        for preset in &presets {
            ctl.apply(crate::app::intent::Intent::LoadPreset { node: reverb, preset: preset.clone() }).unwrap();
            graph.drain_commands();
            let after = graph.plugin_parameter(0, 2, damping).unwrap();
            if (after - before).abs() > 1e-6 {
                changed = Some(after);
                // The cables that drive the parameter are centred on the value the plugin has now (modulation is applied around it, and
                // would otherwise put the old one back at the next buffer, undoing the preset for what is driven).
                assert_eq!(bases_of_the_cables_on(&graph, reverb, damping), [after, after], "preset {preset}");
                break;
            }
        }
        assert!(changed.is_some(), "one of the reverb's presets has a damping of its own");
        // What belongs to another plugin of the lane is as it was.
        assert_eq!(bases_of_the_cables_on(&graph, filter, cutoff), filter_before);
    }

    #[test]
    fn a_preset_that_is_loaded_leaves_the_cables_of_another_member_of_the_group_where_they_are_centred() {
        // Two members of a group, each with a reverb, and a cable of the group's LFO on the damping of each.
        let (mut graph, mut ctl) = controller_on_a_real_graph(
            r#"
[[group]]
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
member = 0
slot = 1
param = "damping"
depth = 0.2
[[group.modulator.target]]
member = 1
slot = 1
param = "damping"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
"#,
        );
        let damping = ctl.model.instruments[0].effects[0].params.iter().find(|p| p.name == "damping").expect("a damping").index;
        // The reverbs of the two members, which the group's cables name by their nodes.
        let reverbs = [ctl.model.instruments[0].effects[0].node, ctl.model.instruments[1].effects[0].node];
        let bases = |graph: &crate::plugin::chain::AudioGraph| -> Vec<(NodeId, f32)> {
            graph
                .cable_bases()
                .into_iter()
                .filter_map(|(_, _, _, aim, base)| match aim {
                    Driven::Param { plugin, index } if index == damping => Some((plugin, base)),
                    _ => None,
                })
                .collect()
        };
        let before = graph.plugin_parameter(0, 1, damping).unwrap();
        assert_eq!(bases(&graph), [(reverbs[0], before), (reverbs[1], before)]);
        let reverb = reverbs[0];
        for preset in ctl.model.instruments[0].effects[0].presets.iter().map(|p| p.id.clone()).collect::<Vec<_>>() {
            ctl.apply(crate::app::intent::Intent::LoadPreset { node: reverb, preset }).unwrap();
            graph.drain_commands();
            let after = graph.plugin_parameter(0, 1, damping).unwrap();
            if (after - before).abs() > 1e-6 {
                // The first member's cable follows its plugin; the second member's reverb was not touched, and its cable is as it was.
                assert_eq!(bases(&graph), [(reverbs[0], after), (reverbs[1], before)]);
                assert_eq!(graph.plugin_parameter(1, 1, damping), Some(before));
                return;
            }
        }
        panic!("none of the reverb's presets has a damping of its own");
    }

    #[test]
    fn taking_cables_out_leaves_the_model_and_the_audio_thread_with_the_same_cables() {
        for session in SCENES_OF_CABLES {
            // From the front each time, and from the back each time: the later cables move or do not.
            for from_the_back in [false, true] {
                let (mut graph, mut ctl) = controller_on_a_real_graph(session);
                assert_eq!(graph.cables(), model_cables(&ctl.model), "as loaded");
                let mut removed = 0;
                loop {
                    let edges: Vec<EdgeId> = ctl.model.free_modulators.iter().flat_map(|m| m.targets.iter().map(|t| t.edge)).collect();
                    let Some(&edge) = (if from_the_back { edges.last() } else { edges.first() }) else { break };
                    assert!(ctl.remove_edge(edge));
                    graph.drain_commands();
                    removed += 1;
                    assert_eq!(graph.cables(), model_cables(&ctl.model), "after the {removed}th removal ({edge:?}, from the back: {from_the_back})");
                    ctl.model.check_ids().unwrap();
                }
                assert!(removed > 0);
            }
        }
    }

    /// The cables of the rack at `at` that drive nothing that is in reach.
    fn cables_driving_nothing(model: &SessionModel, at: ModulatorLocation) -> Vec<(usize, usize)> {
        let rack = model.modulators(at).expect("the rack is there");
        let mut found = Vec::new();
        for (m, modulator) in rack.iter().enumerate() {
            for t in 0..modulator.targets.len() {
                if model.kind_of_target(at, m, t).is_none() {
                    found.push((m, t));
                }
            }
        }
        found
    }

    #[test]
    fn taking_modulators_out_leaves_the_model_and_the_audio_thread_with_the_same_cables_and_none_driving_nothing() {
        for (scene, session) in SCENES_OF_CABLES.into_iter().enumerate() {
            for from_the_back in [false, true] {
                let (mut graph, mut ctl) = controller_on_a_real_graph(session);
                let mut removed = 0;
                loop {
                    let racks: Vec<(ModulatorLocation, usize)> = (0..ctl.model.free_modulators.len()).map(|m| (ModulatorLocation::Free, m)).collect();
                    let Some(&(at, index)) = (if from_the_back { racks.last() } else { racks.first() }) else { break };
                    assert!(ctl.remove_modulator(at, index));
                    graph.drain_commands();
                    removed += 1;
                    assert_eq!(graph.cables(), model_cables(&ctl.model), "after the {removed}th removal ({at:?} {index}, from the back: {from_the_back}, scene {scene})");
                    ctl.model.check_ids().unwrap();
                    assert_eq!(cables_driving_nothing(&ctl.model, ModulatorLocation::Free), [], "scene {scene}, after the {removed}th removal");
                }
                assert!(removed > 0);
            }
        }
    }

    #[test]
    fn remove_modulator_leaves_no_cable_driving_the_depth_of_a_cable_that_went_with_it() {
        let (engine, mut ctl) = controller_for(testutil::CABLES_THAT_DRIVE_CABLES);
        sent(&engine);
        // The first modulator drives three parameters; the second drives the depth of its first and third cable, and
        // the third the depth of the second's first. Take the first out: its cables go with it, and what drove them
        // goes first (and what drove the depth of that).
        assert!(ctl.remove_modulator(ModulatorLocation::Free, 0));
        let rack = &ctl.model.free_modulators;
        assert_eq!(driven(&ctl.model, ModulatorLocation::Free), [vec![], vec![]], "the two that are left drive nothing: what they drove is gone");
        assert!(rack.iter().flat_map(|m| &m.targets).all(|t| !matches!(t.aim, Driven::Depth(_))));
        ctl.model.check_ids().unwrap();
        assert_eq!(
            sent(&engine),
            [
                // The second modulator's first cable, after the third's that drove its depth; then its other.
                "RemoveFreeModTarget mod=2 target=0",
                "RemoveFreeModTarget mod=1 target=0",
                "RemoveFreeModTarget mod=1 target=0",
                "RemoveFreeModulator index=0",
                // Then what its own three cables drove has the value the user set: the detune, the volume and the pan.
                "SetParameter inst=0 slot=0 param=1 value=0.000",
                "SetParameter inst=0 slot=0 param=2 value=1.000",
                "SetParameter inst=0 slot=0 param=7 value=0.000",
            ]
        );
    }

    #[test]
    fn remove_modulator_takes_out_the_cables_of_others_that_drive_it_before_it_goes() {
        let (engine, mut ctl) = controller_for(testutil::CROSS_MOD);
        sent(&engine);
        assert!(ctl.remove_modulator(ModulatorLocation::Free, 0));
        // The lane's third modulator's cables to its rate and to the depth of its cable, and then the group's, one at a
        // time (each the one at the place it has when its turn comes).
        assert_eq!(
            sent(&engine),
            [
                "RemoveFreeModTarget mod=2 target=0",
                "RemoveFreeModTarget mod=2 target=0",
                "RemoveFreeModTarget mod=4 target=4",
                "RemoveFreeModTarget mod=4 target=5",
                "RemoveFreeModulator index=0",
                // And what its own cable drove, the detune, has the value the user set (the rate and the depth that the
                // cables drove were its own and its cable's: gone with it).
                "SetParameter inst=0 slot=0 param=1 value=0.000",
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    /// The names of the effects of each lane as the model has them, in the terms `AudioGraph::effect_names` uses.
    fn model_effect_names(model: &SessionModel) -> Vec<Vec<String>> {
        model.instruments.iter().map(|lane| lane.effects.iter().map(|effect| effect.name.clone()).collect()).collect()
    }

    /// The cables of the model that point at nothing: the plugin, the member, the modulator or the cable they
    /// name is not there.
    fn dangling(model: &SessionModel) -> Vec<EdgeId> {
        let at = ModulatorLocation::Free;
        model.free_modulators.iter().enumerate().flat_map(|(source, m)| m.targets.iter().filter(move |t| model.kind_of(at, source, t.aim).is_none()).map(|t| t.edge)).collect()
    }

    #[test]
    fn swap_lanes_swaps_them_in_the_model_and_tells_the_audio_thread_once() {
        let (engine, mut ctl) = controller_for(testutil::LANES_IN_GROUPS);
        sent(&engine);
        let (first, last) = (ctl.model.instruments[0].node, ctl.model.instruments[3].node);
        assert!(ctl.swap_lanes(0, 3));
        assert_eq!(sent(&engine), ["SwapInstruments a=0 b=3"]);
        assert_eq!((ctl.model.instruments[0].node, ctl.model.instruments[3].node), (last, first));
        // Lanes that are not there, or are one: nothing happens and nothing is sent.
        assert!(!ctl.swap_lanes(1, 1) && !ctl.swap_lanes(0, 4) && !ctl.swap_lanes(8, 2));
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn clear_instrument_empties_the_lanes_instrument_and_leaves_it_everything_else() {
        let (engine, mut ctl) = controller_for(testutil::LANES_IN_GROUPS);
        sent(&engine);
        assert!(ctl.clear_instrument(1));
        assert_eq!(sent(&engine), ["ClearInstrument inst=1"]);
        let lane = &ctl.model.instruments[1];
        assert!(lane.instrument.is_none());
        assert_eq!((lane.effects.len(), hearing_lane(&ctl.model, 1).len(), lane.volume, lane.transpose), (1, 2, 0.6, 7));
        // One that has none, and one that is not there: nothing is sent.
        assert!(!ctl.clear_instrument(1) && !ctl.clear_instrument(9));
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn lanes_that_swap_and_instruments_that_are_cleared_leave_the_model_and_the_audio_thread_the_same() {
        use crate::plugin::chain::AudioGraph;
        let same = |graph: &AudioGraph, ctl: &Controller, what: &str| {
            assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
            assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
            ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
        };
        for session in [testutil::LANES_IN_GROUPS, testutil::GROUPS, testutil::CROSS_MOD] {
            let lanes = load(session).model.instruments.len();
            // Every pair of lanes, one swap each, from a fresh start.
            for a in 0..lanes {
                for b in 0..lanes {
                    let (mut graph, mut ctl) = controller_on_a_real_graph(session);
                    same(&graph, &ctl, "as loaded");
                    assert_eq!(ctl.swap_lanes(a, b), a != b);
                    graph.drain_commands();
                    same(&graph, &ctl, &format!("after swapping {a} and {b}"));
                }
            }
            // Several in a row, each after the one before, with an instrument cleared in between.
            let (mut graph, mut ctl) = controller_on_a_real_graph(session);
            for (step, (a, b)) in [(0, lanes - 1), (1, 0), (lanes - 1, 1), (0, 1), (lanes - 1, 0)].into_iter().enumerate() {
                ctl.swap_lanes(a, b);
                if step == 2 {
                    ctl.clear_instrument(0);
                }
                graph.drain_commands();
                same(&graph, &ctl, &format!("after step {step} ({a} and {b}), scene of {lanes} lanes"));
            }
        }
    }

    /// The cables of `CHAINS` in the order `model_cables` lists them (lane racks, then group racks), as
    /// `(rack, modulator, [the place that `kind_of` says for each cable])`.
    fn chains_cables(model: &SessionModel) -> Vec<(ModulatorLocation, usize, Vec<ModTargetKind>)> {
        let at = ModulatorLocation::Free;
        (0..model.free_modulators.len()).map(|m| (at, m, testutil::kinds_at(model, at, m))).collect()
    }

    #[test]
    fn an_effect_that_is_not_there_is_not_taken_out() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        assert!(!ctl.remove_effect(0, 2) && !ctl.remove_effect(1, 0) && !ctl.remove_effect(9, 0));
        assert!(sent(&engine).is_empty());
        assert_eq!(model_effect_names(&ctl.model), [vec!["Filter", "Reverb"], vec![], vec!["Filter"]]);
    }

    #[test]
    fn effects_that_are_taken_out_leave_the_model_and_the_audio_thread_the_same() {
        // Each effect of the scene on its own, and then all of them, in both orders.
        let all = [(0, 0), (0, 1), (2, 0)];
        let mut sequences: Vec<Vec<(usize, usize)>> = all.iter().map(|&one| vec![one]).collect();
        sequences.push(vec![(0, 0), (0, 0), (2, 0)]); // (the reverb moves into the filter's place)
        sequences.push(vec![(2, 0), (0, 1), (0, 0)]);
        for sequence in sequences {
            let (mut graph, mut ctl) = controller_on_a_real_graph(testutil::CHAINS);
            for (inst, index) in sequence.iter().copied() {
                assert!(ctl.remove_effect(inst, index), "{sequence:?}: {inst}/{index}");
                graph.drain_commands();
                let what = format!("after {inst}/{index} of {sequence:?}");
                assert_eq!(graph.effect_names(), model_effect_names(&ctl.model), "the chains {what}");
                assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}");
                ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
            }
        }
    }

    #[test]
    fn an_effect_is_not_moved_to_where_it_is_or_out_of_its_chain_or_in_a_lane_that_is_not_there() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        for (inst, from, to) in [(0, 0, 0), (0, 1, 1), (0, 0, 2), (0, 2, 0), (0, 5, 5), (1, 0, 0), (1, 0, 1), (2, 0, 1), (9, 0, 1)] {
            assert!(!ctl.move_effect(inst, from, to), "{inst}: {from} to {to}");
        }
        assert!(sent(&engine).is_empty());
        assert_eq!(model_effect_names(&ctl.model), [vec!["Filter", "Reverb"], vec![], vec!["Filter"]]);
    }

    #[test]
    fn effects_that_are_moved_leave_the_model_and_the_audio_thread_the_same() {
        for sequence in [vec![(0, 1)], vec![(1, 0)], vec![(0, 1), (1, 0)], vec![(0, 1), (0, 1)]] {
            let (mut graph, mut ctl) = controller_on_a_real_graph(testutil::CHAINS);
            let mut moved = 0;
            for (from, to) in sequence.iter().copied() {
                // (A move that is not one, from where an effect is to where it is, is refused by both and is no step.)
                let did = ctl.move_effect(0, from, to);
                graph.drain_commands();
                if did {
                    moved += 1;
                }
                let what = format!("after {from} to {to} of {sequence:?}");
                assert_eq!(graph.effect_names(), model_effect_names(&ctl.model), "the chains {what}");
                assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}");
                ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
            }
            assert!(moved > 0, "{sequence:?} moved nothing");
        }
    }

    #[test]
    fn a_bus_effect_that_is_not_there_is_not_taken_out() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        assert!(!ctl.remove_bus_effect(0, 2) && !ctl.remove_bus_effect(1, 0));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.groups[0].effects.len(), 2);
    }

    #[test]
    fn bus_effects_that_are_taken_out_leave_the_model_and_the_audio_thread_the_same() {
        for sequence in [vec![0], vec![1], vec![0, 0], vec![1, 0]] {
            let (mut graph, mut ctl) = controller_on_a_real_graph(testutil::CHAINS);
            for index in sequence.iter().copied() {
                assert!(ctl.remove_bus_effect(0, index), "{sequence:?}: {index}");
                graph.drain_commands();
                let what = format!("after {index} of {sequence:?}");
                assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}");
                ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
            }
        }
    }

    #[test]
    fn a_bus_effect_is_not_moved_to_where_it_is_or_out_of_its_bus_or_in_a_group_that_is_not_there() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        for (group, from, to) in [(0, 0, 0), (0, 1, 1), (0, 0, 2), (0, 2, 0), (0, 5, 5), (1, 0, 1), (9, 0, 1)] {
            assert!(!ctl.move_bus_effect(group, from, to), "{group}: {from} to {to}");
        }
        assert!(sent(&engine).is_empty());
        let bus: Vec<_> = ctl.model.groups[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(bus, ["Filter", "Reverb"]);
    }

    #[test]
    fn bus_effects_that_are_moved_leave_the_model_and_the_audio_thread_the_same() {
        for sequence in [vec![(0, 1)], vec![(1, 0)], vec![(0, 1), (1, 0)], vec![(0, 1), (0, 1)]] {
            let (mut graph, mut ctl) = controller_on_a_real_graph(testutil::CHAINS);
            let mut moved = 0;
            for (from, to) in sequence.iter().copied() {
                // (A move that is not one, from where an effect is to where it is, is refused by both and is no step.)
                let did = ctl.move_bus_effect(0, from, to);
                graph.drain_commands();
                if did {
                    moved += 1;
                }
                let what = format!("after {from} to {to} of {sequence:?}");
                assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}");
                ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
            }
            assert!(moved > 0, "{sequence:?} moved nothing");
        }
    }

    /// Two groups, each with a filter and then a reverb on its bus, and a lane in each.
    const TWO_BUSES: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"

[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument]]
plugin = "builtin:osc"
group = 1
"#;

    #[test]
    fn a_bus_effect_is_moved_in_the_group_that_has_it_and_in_no_other() {
        let (engine, mut ctl) = controller_for(TWO_BUSES);
        sent(&engine);
        assert!(ctl.move_bus_effect(1, 0, 1));
        assert_eq!(sent(&engine), ["ReorderGroupEffect group=1 from=0 to=1"]);
        let buses: Vec<Vec<&str>> = ctl.model.groups.iter().map(|g| g.effects.iter().map(|e| e.name.as_str()).collect()).collect();
        assert_eq!(buses, [vec!["Filter", "Reverb"], vec!["Reverb", "Filter"]]);
        // The audio thread agrees: it moves the bus of the group it is told, and the other's is as it was.
        let (mut graph, mut ctl) = controller_on_a_real_graph(TWO_BUSES);
        assert!(ctl.move_bus_effect(1, 0, 1));
        graph.drain_commands();
        assert_eq!(graph.groups(), model_groups(&ctl.model));
        assert!(ctl.move_bus_effect(0, 1, 0));
        graph.drain_commands();
        assert_eq!(graph.groups(), model_groups(&ctl.model));
        assert_eq!(model_groups(&ctl.model).iter().map(|g| g.effects.clone()).collect::<Vec<_>>(), [vec!["Reverb", "Filter"], vec!["Reverb", "Filter"]]);
    }

    /// A controller over the stub engine whose catalog has the built-in plugins, as a scan finds them.
    fn controller_with_builtins(session: &str) -> (StubEngine, Controller) {
        let (engine, mut ctl) = controller_for(session);
        ctl.catalog.add(plugin::builtin::enumerate_plugins());
        (engine, ctl)
    }

    #[test]
    fn an_effect_is_added_at_the_end_of_a_lanes_chain_in_the_model_and_in_the_engine() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        // Lane 1 has no effects. A reverb goes in, half wet, in a node of its own ...
        let reverb = ctl.add_effect(1, "builtin:reverb").expect("a reverb is added");
        assert_eq!(sent(&engine), ["InsertEffect inst=1 index=0 plugin=Reverb mix=0.500"]);
        let slot = &ctl.model.instruments[1].effects[0];
        assert_eq!((slot.node, slot.name.as_str(), slot.id.as_str(), slot.mix), (reverb, "Reverb", "builtin:reverb", 0.5));
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::Effect { inst: 1, index: 0 }));
        // ... and a filter after it, fully wet (a half-wet filter barely filters).
        let filter = ctl.add_effect(1, "builtin:filter").expect("a filter is added");
        assert_eq!(sent(&engine), ["InsertEffect inst=1 index=1 plugin=Filter mix=1.000"]);
        let chain: Vec<_> = ctl.model.instruments[1].effects.iter().map(|e| (e.node, e.name.as_str(), e.mix)).collect();
        assert_eq!(chain, [(reverb, "Reverb", 0.5), (filter, "Filter", 1.0)]);
        assert_ne!(reverb, filter);
        assert_eq!(ctl.model.locate(filter), Some(NodePath::Effect { inst: 1, index: 1 }));
        // A lane that has effects gets it after them.
        ctl.add_effect(0, "builtin:filter").unwrap();
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=2 plugin=Filter mix=1.000"]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_effect_is_added_at_the_end_of_a_groups_bus_in_the_model_and_in_the_engine() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let reverb = ctl.add_bus_effect(0, "builtin:reverb").expect("a reverb is added");
        assert_eq!(sent(&engine), ["InsertGroupEffect group=0 index=2 plugin=Reverb mix=0.500"]);
        let slot = &ctl.model.groups[0].effects[2];
        assert_eq!((slot.node, slot.name.as_str(), slot.id.as_str(), slot.mix), (reverb, "Reverb", "builtin:reverb", 0.5));
        assert_eq!(ctl.model.locate(reverb), Some(NodePath::GroupEffect { group: 0, index: 2 }));
        let filter = ctl.add_bus_effect(0, "builtin:filter").unwrap();
        assert_eq!(sent(&engine), ["InsertGroupEffect group=0 index=3 plugin=Filter mix=1.000"]);
        assert_eq!(ctl.model.groups[0].effects[3].mix, 1.0);
        assert_ne!(reverb, filter);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn adding_an_effect_leaves_the_chains_and_the_cables_that_were_there_as_they_were() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let cables = chains_cables(&ctl.model);
        let before = model_effect_names(&ctl.model);
        ctl.add_effect(0, "builtin:reverb").unwrap();
        ctl.add_bus_effect(0, "builtin:filter").unwrap();
        // (Only the two insertions: no cable is moved, none taken out, no value is sent.)
        assert_eq!(sent(&engine), ["InsertEffect inst=0 index=2 plugin=Reverb mix=0.500", "InsertGroupEffect group=0 index=2 plugin=Filter mix=1.000"]);
        assert_eq!(chains_cables(&ctl.model), cables);
        assert_eq!(model_effect_names(&ctl.model)[1..], before[1..]);
        assert_eq!(model_effect_names(&ctl.model)[0], ["Filter", "Reverb", "Reverb"]);
        assert!(dangling(&ctl.model).is_empty());
    }

    #[test]
    fn effects_that_are_added_leave_the_model_and_the_audio_thread_the_same() {
        use crate::plugin::chain::AudioGraph;
        let same = |graph: &mut AudioGraph, ctl: &Controller, what: &str| {
            graph.drain_commands();
            assert_eq!(graph.effect_names(), model_effect_names(&ctl.model), "the chains {what}");
            assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
            assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
            assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
            assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}");
            ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
        };
        for session in [testutil::CHAINS, testutil::LANES_IN_GROUPS, testutil::GROUPS] {
            let (mut graph, mut ctl) = controller_on_a_real_graph(session);
            ctl.catalog.add(plugin::builtin::enumerate_plugins());
            let (lanes, groups) = (ctl.model.instruments.len(), ctl.model.groups.len());
            let (chains, buses) = (model_effect_names(&ctl.model), model_groups(&ctl.model));
            same(&mut graph, &ctl, "as loaded");
            // A reverb and a filter to the end of every lane and every bus, each checked as it goes in ...
            for source in ["builtin:reverb", "builtin:filter"] {
                for inst in 0..lanes {
                    ctl.add_effect(inst, source).unwrap();
                    same(&mut graph, &ctl, &format!("after {source} to lane {inst} of a session of {lanes} lanes"));
                }
                for group in 0..groups {
                    ctl.add_bus_effect(group, source).unwrap();
                    same(&mut graph, &ctl, &format!("after {source} to the bus of group {group} of a session of {groups} groups"));
                }
            }
            // ... and out again from the end, which leaves what the session had.
            for inst in 0..lanes {
                for _ in 0..2 {
                    let last = ctl.model.instruments[inst].effects.len() - 1;
                    assert!(ctl.remove_effect(inst, last));
                }
            }
            for group in 0..groups {
                for _ in 0..2 {
                    let last = ctl.model.groups[group].effects.len() - 1;
                    assert!(ctl.remove_bus_effect(group, last));
                }
            }
            same(&mut graph, &ctl, "after taking them out again");
            assert_eq!(model_effect_names(&ctl.model), chains);
            assert_eq!(model_groups(&ctl.model), buses);
        }
    }

    #[test]
    fn groups_that_are_added_leave_the_model_and_the_audio_thread_the_same() {
        use crate::plugin::chain::AudioGraph;
        let same = |graph: &mut AudioGraph, ctl: &Controller, what: &str| {
            graph.drain_commands();
            assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
            assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
            ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
        };
        for session in [testutil::CHAINS, testutil::LANES_IN_GROUPS, testutil::GROUPS] {
            let (mut graph, mut ctl) = controller_on_a_real_graph(session);
            same(&mut graph, &ctl, "as loaded");
            let before = ctl.model.groups.len();
            // One that the controller adds itself, empty ...
            assert_eq!(ctl.add_group(), before);
            same(&mut graph, &ctl, "after a group that is added");
            // ... and one that an intent adds, with the first lane in it.
            let lane = ctl.model.instruments[0].node;
            ctl.apply(Intent::AddGroup { members: vec![lane] }).unwrap();
            same(&mut graph, &ctl, "after a group that is added with a lane in it");
            let ids: Vec<NodeId> = ctl.model.groups.iter().map(|group| group.node).collect();
            assert_eq!(graph.groups().iter().map(|group| group.node).collect::<Vec<_>>(), ids, "the ids that the engine has are the model's, in the same order");
        }
    }

    #[test]
    fn an_effect_the_catalog_does_not_name_is_not_added() {
        let untouched = load(testutil::CHAINS).model;
        // No catalog at all, as in a session nobody has scanned for: even a built-in is not one that was named.
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        let error = ctl.add_effect(0, "builtin:filter").expect_err("not in the catalog").to_string();
        assert!(error.contains("builtin:filter"), "{error}");
        assert!(ctl.add_bus_effect(0, "builtin:filter").is_err());
        // A plugin by a path or by an id that is not in the catalog is not loaded, whether or not it is there to load.
        ctl.catalog.add(plugin::builtin::enumerate_plugins());
        for source in ["./examples/plugin.clap", "/usr/lib/lv2/some.lv2", "clap:com.example.nothing", "vst3:Nothing", "builtin:no-such-plugin"] {
            let error = ctl.add_effect(0, source).expect_err(source).to_string();
            assert!(error.contains(source), "{error}");
            assert!(ctl.add_bus_effect(0, source).is_err(), "{source}");
        }
        assert!(sent(&engine).is_empty(), "nothing was sent to the audio thread");
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn an_instrument_is_not_added_as_an_effect() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        let error = ctl.add_effect(0, "builtin:osc").expect_err("an instrument").to_string();
        assert!(error.contains("Oscillator") && error.contains("instrument"), "{error}");
        assert!(ctl.add_bus_effect(0, "builtin:osc").is_err());
        assert!(sent(&engine).is_empty());
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn a_plugin_that_the_catalog_names_and_that_does_not_load_is_not_added() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        // (An entry that is named by a path, so that what the loader says about it names that path.)
        ctl.catalog.add(vec![plugin::PluginInfo {
            name: "Vanished".into(),
            id: "./no-such-dir/Vanished.clap".into(),
            is_instrument: false,
            param_count: 3,
            preset_count: 0,
            path: "./no-such-dir/Vanished.clap".into(),
            scan_ms: 0,
        }]);
        let error = ctl.add_effect(0, "./no-such-dir/Vanished.clap").expect_err("it cannot be loaded").to_string();
        assert!(error.contains("Vanished") && error.contains("could not be loaded"), "{error}");
        assert!(!error.contains("no-such-dir"), "what the loader says can name where a plugin is on this machine, which is not for a client: {error}");
        assert!(ctl.add_bus_effect(0, "./no-such-dir/Vanished.clap").is_err());
        assert!(sent(&engine).is_empty());
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn an_effect_with_other_outputs_than_the_chains_have_is_turned_away_before_the_audio_thread_is_asked() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        let error = ctl.insert_effect(ChainAt::Lane(0), Box::new(testutil::MonoEffect), "test:mono").expect_err("one output").to_string();
        assert!(error.contains("MonoEffect") && error.contains("1 audio output"), "{error}");
        assert!(ctl.insert_effect(ChainAt::Bus(0), Box::new(testutil::MonoEffect), "test:mono").is_err());
        // (The audio thread would turn it away as well, and the model would be left with an effect that is not there.)
        assert!(sent(&engine).is_empty());
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn an_effect_that_stands_by_itself_is_a_lane_of_its_own_that_has_the_effect_for_its_chain_and_no_instrument() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let lanes = ctl.model.instruments.len();
        let effect = ctl.add_free_effect("builtin:filter").expect("a filter is added by itself");
        assert_eq!(ctl.model.instruments.len(), lanes + 1);
        let lane = ctl.model.instruments.last().unwrap();
        assert!(lane.instrument.is_none());
        assert_eq!((lane.range, lane.group, lane.volume, lane.transpose), (None, None, 1.0, 0));
        assert_eq!(lane.effects.iter().map(|e| (e.node, e.name.as_str(), e.mix)).collect::<Vec<_>>(), [(effect, "Filter", 1.0)]);
        // The lane's node is made first and the effect's next: what a redo gives back under the ids they had, in that order.
        assert_eq!(effect.0, lane.node.0 + 1);
        assert_eq!(lane.route, Route::None, "a bare module: it has no cable out, and sends where cables are drawn from it");
        assert_eq!(sent(&engine), ["AddInstrument range=None".to_string(), format!("SetLaneRoute inst={lanes} route=none"), format!("InsertEffect inst={lanes} index=0 plugin=Filter mix=1.000")]);
        ctl.model.check_ids().unwrap();
        // It goes at the end each time, in a lane of its own: two are two lanes, and not one chain.
        ctl.add_free_effect("builtin:reverb").expect("and a reverb");
        let chains: Vec<_> = ctl.model.instruments[lanes..].iter().map(|lane| lane.effects.len()).collect();
        assert_eq!(chains, [1, 1]);
    }

    #[test]
    fn an_effect_that_stands_by_itself_and_is_turned_away_leaves_no_lane_and_uses_no_id() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        let error = ctl.add_free_effect_loaded(Box::new(testutil::MonoEffect), "test:mono").expect_err("one output").to_string();
        assert!(error.contains("MonoEffect") && error.contains("1 audio output"), "{error}");
        for what in ["builtin:osc", "./examples/plugin.clap", "clap:com.example.nothing"] {
            assert!(ctl.add_free_effect(what).is_err(), "{what}");
        }
        assert!(sent(&engine).is_empty(), "no lane was made, on the audio thread or in the model");
        // (The ids are part of what is compared: a lane that was made and taken out again, or an id that was handed out for the lane before the plugin was looked at, would show as a next id that is not the one there was.)
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn a_chain_has_a_place_for_an_effect_with_as_many_outputs_as_it_has_channels_and_for_no_other() {
        assert_eq!(outputs_fit_the_chain("Reverb", CHAIN_CHANNELS), Ok(()));
        let refusal = |outputs: usize| outputs_fit_the_chain("Reverb", outputs).expect_err("not as many as the chain has").to_string();
        assert_eq!(refusal(1), "Reverb has 1 audio output, and an effect here needs 2");
        assert_eq!(refusal(0), "Reverb has 0 audio outputs, and an effect here needs 2");
        assert_eq!(refusal(6), "Reverb has 6 audio outputs, and an effect here needs 2");
    }

    #[test]
    fn a_lane_or_a_group_that_is_not_there_gets_no_effect() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        assert!(ctl.add_effect(3, "builtin:filter").is_err() && ctl.add_effect(9, "builtin:filter").is_err());
        assert!(ctl.add_bus_effect(1, "builtin:filter").is_err() && ctl.add_bus_effect(9, "builtin:filter").is_err());
        assert!(sent(&engine).is_empty());
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    // -- instruments --

    #[test]
    fn an_instrument_is_put_in_a_lane_in_the_model_and_in_the_engine_and_the_cables_to_the_old_one_go_first() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        // Lane 0's oscillator has a cable of its own lane's modulator to its detune (the third of that modulator's three)...
        ctl.put_instrument(0, "builtin:osc").expect("an oscillator is put in");
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=2", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false"]);
        // ... and lane 1's has the group's (the third of its modulator's six).
        ctl.put_instrument(1, "builtin:osc").unwrap();
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=3 target=2", "SwapInstrument inst=1 plugin=Oscillator bufs=2 remap=false"]);
        let lane = &ctl.model.instruments[0];
        let slot = lane.instrument.as_ref().expect("an instrument");
        assert_eq!((slot.node, slot.name.as_str(), slot.id.as_str()), (lane.node, "Oscillator", "builtin:osc"), "the plugin sits in the lane's node");
        assert!(slot.params.iter().all(|p| p.value == p.default), "the parameters are the new plugin's own");
        // The lane is what it was, but for the plugin; so are the others.
        assert_eq!((lane.volume, lane.group, lane.effects.len(), hearing_lane(&ctl.model, 0).len()), (1.0, Some(0), 2, 2));
        assert!(dangling(&ctl.model).is_empty(), "no cable points at the plugin that went");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_instrument_is_put_in_a_lane_that_has_none_and_keeps_what_else_the_lane_has() {
        // A lane that was emptied (by the TUI's `d`), with a filter and a volume of its own.
        let (engine, mut ctl) = controller_with_builtins("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.6\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n");
        assert!(ctl.clear_instrument(0));
        sent(&engine);
        ctl.put_instrument(0, "builtin:osc").unwrap();
        assert_eq!(sent(&engine), ["SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false"], "no cable to take out");
        let lane = &ctl.model.instruments[0];
        assert_eq!(lane.instrument.as_ref().map(|p| (p.node, p.name.as_str())), Some((lane.node, "Oscillator")));
        assert_eq!((lane.volume, lane.effects.len()), (0.6, 1), "the lane keeps what else it has");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lanes_note_remap_goes_with_the_instrument_it_was_for() {
        // The audio thread drops the remapper with the plugin, and the model has to say the same.
        let (engine, mut ctl) = controller_with_builtins(testutil::SETTINGS);
        assert_eq!(ctl.model.instruments[0].remap.len(), 1);
        assert_eq!(ctl.model.instruments[0].pitch_bend_range, 4.0);
        sent(&engine);
        ctl.put_instrument(0, "builtin:osc").unwrap();
        assert!(ctl.model.instruments[0].remap.is_empty());
        assert_eq!(ctl.model.instruments[0].pitch_bend_range, 2.0, "a plugin of its own has the usual range until it is told otherwise");
        let saved = crate::session::to_toml(&crate::app::save::to_save(&ctl.model).0, &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!saved.contains("remap"), "the file says what the audio thread has: {saved}");
    }

    #[test]
    fn clearing_a_lanes_instrument_takes_its_note_remap_with_it() {
        let (engine, mut ctl) = controller_with_builtins(testutil::SETTINGS);
        assert_eq!((ctl.model.instruments[0].remap.len(), ctl.model.instruments[0].pitch_bend_range), (1, 4.0));
        sent(&engine);
        assert!(ctl.clear_instrument(0));
        assert!(ctl.model.instruments[0].remap.is_empty(), "the audio thread drops the remapper with the plugin");
        assert_eq!(ctl.model.instruments[0].pitch_bend_range, 2.0);
        let saved = crate::session::to_toml(&crate::app::save::to_save(&ctl.model).0, &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!saved.contains("remap"), "the file says what the audio thread has: {saved}");
    }

    #[test]
    fn instruments_that_are_put_in_leave_the_model_and_the_audio_thread_the_same() {
        use crate::plugin::chain::AudioGraph;
        for session in [testutil::CHAINS, testutil::LANES_IN_GROUPS, testutil::CABLES_THAT_DRIVE_CABLES, testutil::GROUPS] {
            let (mut graph, mut ctl) = controller_on_a_real_graph(session);
            ctl.catalog.add(plugin::builtin::enumerate_plugins());
            let same = |graph: &mut AudioGraph, ctl: &Controller, what: &str| {
                graph.drain_commands();
                assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}");
                ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
            };
            for inst in 0..ctl.model.instruments.len() {
                ctl.put_instrument(inst, "builtin:osc").unwrap();
                same(&mut graph, &ctl, &format!("after an instrument in lane {inst} of {session:.40}"));
            }
            // And a lane of its own, which the audio thread has with its instrument.
            ctl.add_instrument_lane("builtin:osc").unwrap();
            same(&mut graph, &ctl, "after a lane with an instrument");
        }
    }

    #[test]
    fn a_lane_is_added_at_the_end_with_the_instrument_in_a_node_of_its_own() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let lanes = ctl.model.instruments.len();
        let node = ctl.add_instrument_lane("builtin:osc").expect("a lane is added");
        let lane = &ctl.model.instruments[lanes];
        assert_eq!(lane.node, node);
        assert_eq!((lane.range, lane.volume, lane.transpose, lane.group), (None, 1.0, 0, None));
        assert_eq!(lane.instrument.as_ref().map(|p| (p.node, p.name.as_str())), Some((node, "Oscillator")));
        assert!(lane.effects.is_empty() && lane.pattern.is_none());
        let meter = lane.meter.expect("a meter");
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands, ["AddInstrument range=None", format!("SwapInstrument inst={lanes} plugin=Oscillator bufs=2 remap=false").as_str()]);
        assert_eq!(telemetry, [format!("SetLaneMeter inst={lanes} peak={} clips={}", meter.peak.index(), meter.clips.index())]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn an_instrument_the_catalog_does_not_name_is_not_put_in_and_no_lane_is_made_for_it() {
        let untouched = load(testutil::CHAINS).model;
        // No catalog at all: even a built-in is not one that was named.
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        let error = ctl.put_instrument(0, "builtin:osc").expect_err("not in the catalog").to_string();
        assert!(error.contains("builtin:osc"), "{error}");
        assert!(ctl.add_instrument_lane("builtin:osc").is_err());
        ctl.catalog.add(plugin::builtin::enumerate_plugins());
        for source in ["./examples/plugin.clap", "/usr/lib/lv2/some.lv2", "clap:com.example.nothing", "vst3:Nothing", "builtin:no-such-plugin"] {
            let error = ctl.put_instrument(0, source).expect_err(source).to_string();
            assert!(error.contains(source), "{error}");
            assert!(ctl.add_instrument_lane(source).is_err(), "{source}");
        }
        // An effect is not an instrument.
        let error = ctl.put_instrument(0, "builtin:filter").expect_err("an effect").to_string();
        assert_eq!(error, "Filter is an effect, not an instrument");
        assert!(ctl.add_instrument_lane("builtin:reverb").is_err());
        assert!(sent(&engine).is_empty(), "nothing was sent to the audio thread: no lane, no cable taken out");
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn a_plugin_that_the_catalog_names_and_that_does_not_load_is_not_put_in_as_an_instrument() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        ctl.catalog.add(vec![plugin::PluginInfo {
            name: "Vanished".into(),
            id: "./no-such-dir/Vanished.clap".into(),
            is_instrument: true,
            param_count: 3,
            preset_count: 0,
            path: "./no-such-dir/Vanished.clap".into(),
            scan_ms: 0,
        }]);
        let error = ctl.put_instrument(0, "./no-such-dir/Vanished.clap").expect_err("it cannot be loaded").to_string();
        assert!(error.contains("Vanished") && error.contains("could not be loaded"), "{error}");
        assert!(!error.contains("no-such-dir"), "what the loader says can name where a plugin is on this machine: {error}");
        assert!(ctl.add_instrument_lane("./no-such-dir/Vanished.clap").is_err());
        assert!(sent(&engine).is_empty(), "the cables of the instrument that was there were not taken out for nothing");
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn an_instrument_with_no_audio_output_is_turned_away_before_a_cable_is_taken_out_or_a_lane_is_made() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        // Lane 0 has cables into its instrument, which are not taken out for a plugin that is then turned away.
        let error = ctl.place_instrument(0, Box::new(testutil::FakeInstrument(0)), "test:none").expect_err("no output").to_string();
        assert!(error.contains("FakeInstrument") && error.contains("no audio outputs"), "{error}");
        assert!(ctl.place_instrument_in_new_lane(Box::new(testutil::FakeInstrument(0)), "test:none").is_err());
        assert!(sent(&engine).is_empty());
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn an_instrument_with_one_audio_output_is_put_in_and_the_audio_thread_plays_it_on_the_left() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        ctl.place_instrument(0, Box::new(testutil::FakeInstrument(1)), "test:mono").expect("a mono instrument is put in");
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=2", "SwapInstrument inst=0 plugin=FakeInstrument bufs=1 remap=false"]);
        let lanes = ctl.model.instruments.len();
        ctl.place_instrument_in_new_lane(Box::new(testutil::FakeInstrument(1)), "test:mono").expect("and in a lane of its own");
        assert_eq!(ctl.model.instruments.len(), lanes + 1);
    }

    #[test]
    fn the_instrument_has_the_lanes_node_and_no_id_is_used_up_for_it() {
        let (_engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        let before = ctl.model.alloc_node();
        ctl.put_instrument(0, "builtin:osc").unwrap();
        assert_eq!(ctl.model.alloc_node().0, before.0 + 1, "a plugin put in a lane has the lane's node: none is made for it");
        let (before, lanes) = (ctl.model.alloc_node(), ctl.model.instruments.len());
        let node = ctl.add_instrument_lane("builtin:osc").unwrap();
        assert_eq!((node.0, ctl.model.instruments.len()), (before.0 + 1, lanes + 1), "the lane's node is the one id that a lane with an instrument takes");
        assert_eq!(ctl.model.alloc_node().0, before.0 + 2);
    }

    #[test]
    fn a_lane_that_is_not_there_gets_no_instrument() {
        let (engine, mut ctl) = controller_with_builtins(testutil::CHAINS);
        sent(&engine);
        let untouched = load(testutil::CHAINS).model;
        assert!(ctl.put_instrument(3, "builtin:osc").is_err() && ctl.put_instrument(9, "builtin:osc").is_err());
        assert!(sent(&engine).is_empty());
        testutil::assert_same_model("after the refusals", &ctl.model, &untouched);
    }

    #[test]
    fn clearing_a_lanes_instrument_takes_the_cables_to_it_out_first() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        assert!(ctl.clear_instrument(0));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=2", "ClearInstrument inst=0"]);
        assert!(ctl.clear_instrument(1));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=3 target=2", "ClearInstrument inst=1"]);
        assert!(dangling(&ctl.model).is_empty(), "no cable points at a plugin that is not there");
        ctl.model.check_ids().unwrap();
        // (A lane that has none has no cable to take out, and nothing to clear.)
        assert!(!ctl.clear_instrument(1));
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn a_lane_in_no_group_is_taken_out_with_nothing_but_itself_and_the_cables_to_its_plugins() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        assert!(ctl.remove_lane(2));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=2 target=0", "RemoveInstrument inst=2"]);
        assert_eq!(ctl.model.instruments.len(), 2);
        assert_eq!(hearing_group(&ctl.model, 0)[0].targets.len(), 6, "the group's cables are as they were");
    }

    #[test]
    fn a_lane_that_is_not_there_or_is_the_last_is_not_taken_out() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        assert!(!ctl.remove_lane(3) && !ctl.remove_lane(9));
        assert!(sent(&engine).is_empty());
        let (engine, mut ctl) = controller_for("[[instrument]]\nplugin = \"builtin:osc\"\n");
        sent(&engine);
        assert!(!ctl.remove_lane(0), "a session keeps a lane");
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.instruments.len(), 1);
    }

    #[test]
    fn lanes_that_are_taken_out_leave_the_model_and_the_audio_thread_the_same() {
        for session in [testutil::CHAINS, testutil::LANES_IN_GROUPS, testutil::GROUPS] {
            let lanes = load(session).model.instruments.len();
            // Each lane on its own, and then lanes one after another from the front and from the back.
            let mut sequences: Vec<Vec<usize>> = (0..lanes).map(|lane| vec![lane]).collect();
            sequences.push(vec![0; lanes.saturating_sub(1)]);
            sequences.push((1..lanes).rev().collect());
            for sequence in sequences {
                let (mut graph, mut ctl) = controller_on_a_real_graph(session);
                for inst in sequence.iter().copied() {
                    assert!(ctl.remove_lane(inst), "{sequence:?}: {inst} of {} lanes", ctl.model.instruments.len());
                    graph.drain_commands();
                    let what = format!("after {inst} of {sequence:?} in a session of {lanes} lanes");
                    assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                    assert_eq!(graph.effect_names(), model_effect_names(&ctl.model), "the chains {what}");
                    assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                    assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                    assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}: {:?}", dangling(&ctl.model));
                    ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
                }
            }
        }
    }

    #[test]
    fn a_group_that_is_not_there_is_not_taken_out() {
        let (engine, mut ctl) = controller_for(testutil::CHAINS);
        sent(&engine);
        assert!(!ctl.remove_group(1) && !ctl.remove_group(9));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.groups.len(), 1);
    }

    #[test]
    fn groups_that_are_taken_out_leave_the_model_and_the_audio_thread_the_same() {
        for session in [testutil::CHAINS, testutil::LANES_IN_GROUPS, testutil::GROUPS, testutil::CROSS_MOD] {
            let groups = load(session).model.groups.len();
            let mut sequences: Vec<Vec<usize>> = (0..groups).map(|group| vec![group]).collect();
            sequences.push(vec![0; groups]);
            sequences.push((0..groups).rev().collect());
            for sequence in sequences {
                let (mut graph, mut ctl) = controller_on_a_real_graph(session);
                for group in sequence.iter().copied() {
                    assert!(ctl.remove_group(group), "{sequence:?}: {group}");
                    graph.drain_commands();
                    let what = format!("after {group} of {sequence:?} in a session of {groups} groups");
                    assert_eq!(graph.lanes(), model_lanes(&ctl.model), "the lanes {what}");
                    assert_eq!(graph.groups(), model_groups(&ctl.model), "the groups {what}");
                    assert_eq!(graph.effect_names(), model_effect_names(&ctl.model), "the chains {what}");
                    assert_eq!(graph.cables(), model_cables(&ctl.model), "the cables {what}");
                    assert!(dangling(&ctl.model).is_empty(), "cables that point at nothing {what}: {:?}", dangling(&ctl.model));
                    ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
                }
            }
        }
    }

    // -- audio cables go with what they start or end at --

    /// The cables drawn in `SENDS` for the tests of what goes with a node, with the ids they get (0 to 5): the first lane's instrument to the master, lane 1's instrument to lane 0's
    /// filter, the filter to the group's bus, the delay to lane 1's drive, the group's output to the drive, and the bus effect to the master.
    fn the_sends_with_cables() -> (crate::plugin::chain::AudioGraph, Controller, testutil::Sends) {
        use crate::app::graph::{Endpoint, Port};
        let (mut graph, mut ctl) = controller_on_a_real_graph(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        let (out, into) = (|node| Endpoint { node, port: Port::AudioOut }, |node| Endpoint { node, port: Port::AudioIn });
        for (from, to) in [
            (out(n.lane[0]), into(NodeId::MASTER)),
            (out(n.lane[1]), into(n.fx[0])),
            (out(n.fx[0]), into(n.group)),
            (out(n.fx[1]), into(n.fx[2])),
            (out(n.group), into(n.fx[2])),
            (out(n.bus), into(NodeId::MASTER)),
        ] {
            ctl.apply(Intent::AddCable { from, to, gain: Some(0.5) }).unwrap();
        }
        graph.drain_commands();
        (graph, ctl, n)
    }

    /// The ids of the audio cables that the model has.
    fn model_cable_ids(ctl: &Controller) -> Vec<u32> {
        ctl.model.cables.iter().map(|cable| cable.id.0).collect()
    }

    #[test]
    fn the_cables_of_the_sends_scene_are_the_ones_the_tests_of_what_goes_with_a_node_count_on() {
        let (graph, ctl, n) = the_sends_with_cables();
        assert_eq!(model_cable_ids(&ctl), [0, 1, 2, 3, 4, 5]);
        assert_eq!(graph.audio_cables(), testutil::model_audio_cables(&ctl.model));
        assert_eq!(ctl.model.audio_cables_at(&[n.fx[0]]).iter().map(|id| id.0).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn an_effect_that_is_taken_out_takes_the_audio_cables_at_it_out_first() {
        let (engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        for (from, to) in [(CableFrom::Effect(n.fx[0]), CableTo::Master), (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])), (CableFrom::Instrument(n.lane[0]), CableTo::Master)] {
            ctl.add_audio_cable(from, to, 1.0).unwrap();
        }
        sent(&engine);
        // Lane 0's filter: the cable it sends and the cable it takes go (ids 0 and 1), and the cable of the lane's instrument stays.
        assert!(ctl.remove_effect(0, 0));
        assert_eq!(sent(&engine), ["RemoveCable id=0", "RemoveCable id=1", "RemoveEffect inst=0 index=0"]);
        assert_eq!(model_cable_ids(&ctl), [2]);
    }

    #[test]
    fn a_bus_effect_that_is_taken_out_takes_the_audio_cables_at_it_out_first() {
        let (engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        ctl.add_audio_cable(CableFrom::Effect(n.bus), CableTo::Master, 1.0).unwrap();
        ctl.add_audio_cable(CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus), 1.0).unwrap();
        ctl.add_audio_cable(CableFrom::Instrument(n.lane[1]), CableTo::Master, 1.0).unwrap();
        sent(&engine);
        assert!(ctl.remove_bus_effect(0, 0));
        assert_eq!(sent(&engine), ["RemoveCable id=0", "RemoveCable id=1", "RemoveGroupEffect group=0 index=0"]);
        assert_eq!(model_cable_ids(&ctl), [2]);
    }

    #[test]
    fn a_lane_that_is_taken_out_takes_the_audio_cables_at_its_instrument_and_at_each_of_its_effects() {
        let (mut graph, mut ctl, _n) = the_sends_with_cables();
        // Lane 0 and its filter and delay: its own instrument's cable (0), the cable into the filter (1), the filter's (2) and the delay's (3).
        assert!(ctl.remove_lane(0));
        assert_eq!(model_cable_ids(&ctl), [4, 5]);
        graph.drain_commands();
        assert_eq!(graph.audio_cables(), testutil::model_audio_cables(&ctl.model));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_group_that_is_taken_out_takes_the_audio_cables_at_its_bus_its_output_and_its_bus_effects() {
        let (mut graph, mut ctl, _n) = the_sends_with_cables();
        // The cable into the group's bus (2), the cable from its output (4) and the bus effect's (5).
        assert!(ctl.remove_group(0));
        assert_eq!(model_cable_ids(&ctl), [0, 1, 3]);
        graph.drain_commands();
        assert_eq!(graph.audio_cables(), testutil::model_audio_cables(&ctl.model));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn what_has_the_audio_cables_of_a_session_leaves_the_model_and_the_audio_thread_with_the_same_cables_whatever_is_taken_out() {
        // Every one of what can be taken out, on its own, each from the same start.
        let places = |ctl: &Controller| (ctl.model.instruments.len(), ctl.model.groups.len());
        let (lanes, groups) = places(&the_sends_with_cables().1);
        type Removal = Box<dyn Fn(&mut Controller) -> bool>;
        let mut removals: Vec<(String, Removal)> = Vec::new();
        for lane in 0..lanes {
            removals.push((format!("lane {lane}"), Box::new(move |ctl| ctl.remove_lane(lane))));
        }
        for group in 0..groups {
            removals.push((format!("group {group}"), Box::new(move |ctl| ctl.remove_group(group))));
        }
        removals.push(("lane 0's filter".to_string(), Box::new(|ctl| ctl.remove_effect(0, 0))));
        removals.push(("lane 0's delay".to_string(), Box::new(|ctl| ctl.remove_effect(0, 1))));
        removals.push(("lane 1's drive".to_string(), Box::new(|ctl| ctl.remove_effect(1, 0))));
        removals.push(("the group's reverb".to_string(), Box::new(|ctl| ctl.remove_bus_effect(0, 0))));
        for (what, remove) in &removals {
            let (mut graph, mut ctl, _n) = the_sends_with_cables();
            assert!(remove(&mut ctl), "{what}");
            graph.drain_commands();
            assert_eq!(graph.audio_cables(), testutil::model_audio_cables(&ctl.model), "taking out {what}");
            assert!(ctl.model.cables.len() < 6, "taking out {what} takes cables with it");
            ctl.model.check_ids().unwrap_or_else(|e| panic!("{what}: {e}"));
            // And what is left is a wiring that the audio thread could still make a plan of: a block is rendered (a loop would assert).
            let mut out = vec![vec![0.0f32; 64]; 2];
            graph.process(&[], &mut out).unwrap();
        }
    }

    #[test]
    fn an_audio_cable_that_is_taken_out_by_the_method_goes_and_one_that_is_not_there_does_not() {
        let (engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        let id = ctl.add_audio_cable(CableFrom::Instrument(n.lane[0]), CableTo::Master, 1.0).unwrap();
        sent(&engine);
        assert!(ctl.remove_edge(id));
        assert_eq!(sent(&engine), ["RemoveCable id=0"]);
        assert!(ctl.model.cables.is_empty());
        assert!(!ctl.remove_edge(id));
        assert!(sent(&engine).is_empty());
    }

    #[test]
    fn a_lane_is_not_put_in_a_group_by_the_method_when_the_audio_would_go_round() {
        let (engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        ctl.add_audio_cable(CableFrom::Group(n.group), CableTo::Effect(n.fx[2]), 1.0).unwrap();
        sent(&engine);
        assert!(!ctl.set_lane_group(1, Some(0)), "the group feeds the lane's drive");
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.instruments[1].group, None);
        // What is no loop is done.
        assert!(ctl.set_lane_group(0, None));
        assert!(!sent(&engine).is_empty());
    }

    #[test]
    fn an_effect_is_not_moved_by_the_method_when_the_audio_would_go_round() {
        let (engine, mut ctl) = controller_for(testutil::SENDS);
        let n = testutil::Sends::of(&ctl.model);
        ctl.add_audio_cable(CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[2]), 1.0).unwrap();
        ctl.add_audio_cable(CableFrom::Effect(n.fx[2]), CableTo::Effect(n.fx[1]), 1.0).unwrap();
        sent(&engine);
        assert!(!ctl.move_effect(0, 0, 1), "the delay would feed the filter, which feeds the drive, which feeds the delay");
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.instruments[0].effects[0].node, n.fx[0]);
        // The same on a bus, with a cable that goes out of the bus and one that comes back (the bus effects are the group's, so the loop is through a lane).
        let (engine, mut ctl) = controller_for(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:delay"

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:drive"
"#,
        );
        let (first, second, lane_fx) = (ctl.model.groups[0].effects[0].node, ctl.model.groups[0].effects[1].node, ctl.model.instruments[0].effects[0].node);
        ctl.add_audio_cable(CableFrom::Effect(first), CableTo::Effect(lane_fx), 1.0).unwrap();
        ctl.add_audio_cable(CableFrom::Effect(lane_fx), CableTo::Effect(second), 1.0).unwrap();
        sent(&engine);
        assert!(!ctl.move_bus_effect(0, 1, 0));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.groups[0].effects[0].node, first);
    }

    #[test]
    fn remove_modulator_that_is_not_there_does_nothing() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let before = ctl.model.free_modulators.len();
        assert!(!ctl.remove_modulator(ModulatorLocation::Free, before));
        assert!(!ctl.remove_modulator(ModulatorLocation::Free, 99));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.free_modulators.len(), before);
    }

    #[test]
    fn add_modulator_gives_the_modulator_an_output_slot_and_tells_the_audio_thread_right_after() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        let before = ctl.model.free_modulators.len();
        ctl.add_modulator(ModulatorLocation::Free, lfo_source());
        ctl.add_modulator(ModulatorLocation::Free, lfo_source());
        let (first, second) = (ctl.model.free_modulators[before].output.unwrap().index(), ctl.model.free_modulators[before + 1].output.unwrap().index());
        assert_ne!(first, second);
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands.len(), 2, "the slot commands are not among the rest: {commands:?}");
        assert_eq!(telemetry, [format!("SetModulatorOutput Free index={before} slot={first}"), format!("SetModulatorOutput Free index={} slot={second}", before + 1)]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_sequencer_gets_a_position_slot_after_its_output_slot_and_another_modulator_gets_none() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        let before = ctl.model.free_modulators.len();
        ctl.add_modulator(ModulatorLocation::Free, ModSourceSlot::new_steps());
        ctl.add_modulator(ModulatorLocation::Free, ModSourceSlot::new_steps());
        ctl.add_modulator(ModulatorLocation::Free, lfo_source());
        let slots = |m: &ModulatorSlot| (m.output.unwrap().index(), m.position.map(|slot| slot.index()));
        let (first, second, lfo) = (slots(&ctl.model.free_modulators[before]), slots(&ctl.model.free_modulators[before + 1]), slots(&ctl.model.free_modulators[before + 2]));
        assert_eq!((first.1, second.1, lfo.1), (Some(first.0 + 1), Some(second.0 + 1), None), "the position's slot is the one after the output's");
        let (_commands, telemetry) = engine.drain();
        assert_eq!(
            telemetry,
            [
                format!("SetModulatorOutput Free index={before} slot={}", first.0),
                format!("SetModulatorPosition Free index={before} slot={}", first.0 + 1),
                format!("SetModulatorOutput Free index={} slot={}", before + 1, second.0),
                format!("SetModulatorPosition Free index={} slot={}", before + 1, second.0 + 1),
                format!("SetModulatorOutput Free index={} slot={}", before + 2, lfo.0),
            ]
        );
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_step_sequencer_that_finds_one_slot_left_has_its_output_and_no_position() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        // (The last cell is the engine's, the limiter's reduction: the last that is handed out is the one before it.)
        let last = crate::telemetry::CAPACITY - 2;
        while ctl.model.slots.high_water() < last {
            ctl.model.slots.alloc();
        }
        engine.drain();
        let before = ctl.model.free_modulators.len();
        ctl.add_modulator(ModulatorLocation::Free, ModSourceSlot::new_steps());
        let added = &ctl.model.free_modulators[before];
        assert_eq!((added.output.map(|slot| slot.index()), added.position), (Some(last), None));
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands.len(), 1, "the modulator is made all the same: {commands:?}");
        assert_eq!(telemetry, [format!("SetModulatorOutput Free index={before} slot={last}")]);
    }

    #[test]
    fn a_target_gets_an_effective_value_slot_whether_it_drives_a_parameter_or_a_setting_of_a_modulator() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        // (Two modulators, for the first to drive the rate of the second.)
        assert!(ctl.model.free_modulators.len() >= 2);
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        // (The second modulator's cable drives the same filter.)
        assert!(ctl.add_target(ModulatorLocation::Free, 1, cutoff_target(&ctl, 0.5)));
        let cross = NewTarget {
            aim: Driven::Setting { modulator: ctl.model.free_modulators[1].node, field: ModSetting::Rate },
            param_name: "Mod 1 rate".to_string(),
            ..cutoff_target(&ctl, 0.5)
        };
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cross));
        let first = &ctl.model.free_modulators[0].targets;
        let second = &ctl.model.free_modulators[1].targets;
        assert!(first[0].effective.is_some() && second[0].effective.is_some());
        assert_ne!(first[0].effective, second[0].effective);
        // (A modulator's own setting has an effective value too: the engine publishes what it was set to, f132.)
        assert!(first[1].effective.is_some(), "a cable to a setting has a slot");
        assert!(first[1].effective != first[0].effective && first[1].effective != second[0].effective);
        ctl.model.check_ids().unwrap();
        let (_, telemetry) = engine.drain();
        assert!(telemetry.is_empty(), "targets carry their slot, they are not assigned one");
    }

    #[test]
    fn every_cable_to_a_parameter_takes_a_slot_even_when_another_cable_drives_it_already_and_the_next_slot_comes_right_after() {
        let (_engine, mut ctl) = controller_for(WITH_MODULATORS);
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        assert!(ctl.add_target(ModulatorLocation::Free, 1, cutoff_target(&ctl, 0.25)));
        let slot = |modulator: usize| ctl.model.free_modulators[modulator].targets.last().unwrap().effective.map(|slot| slot.index());
        let (first, second) = (slot(0).expect("the first cable has one"), slot(1).expect("so has the second, though the first drives the parameter"));
        assert_eq!(second, first + 1, "one slot each, one after the other");
        // (The layout names the parameter once, by the first cable's slot: what the second took is one that nothing reads.)
        let before = ctl.model.free_modulators.len();
        ctl.add_modulator(ModulatorLocation::Free, lfo_source());
        let output = ctl.model.free_modulators[before].output.unwrap().index();
        assert_eq!(output, second + 1, "no slot is skipped, and none is taken twice");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_target_to_what_the_rack_does_not_reach_is_refused_and_uses_nothing_up() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        sent(&engine);
        let slots_before = ctl.model.slots.high_water();
        // A modulator's own rate, a plugin that is not there and a node that is not a plugin.
        let own = NewTarget { aim: Driven::Setting { modulator: ctl.model.free_modulators[0].node, field: ModSetting::Rate }, ..cutoff_target(&ctl, 0.5) };
        assert!(!ctl.add_target(ModulatorLocation::Free, 0, own));
        let ghost = NewTarget { aim: Driven::Param { plugin: NodeId(9_999), index: 0 }, ..cutoff_target(&ctl, 0.5) };
        assert!(!ctl.add_target(ModulatorLocation::Free, 0, ghost));
        let not_a_plugin = NewTarget { aim: Driven::Param { plugin: ctl.model.groups[0].node, index: 0 }, ..cutoff_target(&ctl, 0.5) };
        assert!(!ctl.add_target(ModulatorLocation::Free, 0, not_a_plugin));
        assert!(sent(&engine).is_empty());
        assert_eq!(ctl.model.slots.high_water(), slots_before, "no slot is used up for nothing");
        assert!(ctl.model.free_modulators.iter().all(|m| m.targets.is_empty()));
    }

    const A_FILTER_AND_A_DETUNE: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator.target]]
param = "detune"
depth = 0.2
"#;

    #[test]
    fn a_cable_that_what_it_drove_is_gone_from_when_it_is_undone_is_left_out_of_the_undo() {
        let (engine, mut ctl) = controller_for(A_FILTER_AND_A_DETUNE);
        let cutoff = ctl.model.free_modulators[0].targets[0].edge;
        ctl.apply(Intent::RemoveEdge { edge: cutoff }).unwrap();
        // Something takes the filter out without the history (as a front-end that edits the model by itself could).
        ctl.model.remove_effect(0, 0).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        // The cable is not put back, in the model or on the audio thread: it would drive nothing.
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1);
        assert!(sent(&engine).iter().all(|command| !command.contains("ModTarget")));
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_that_comes_back_without_the_cable_whose_plugin_went_in_the_meantime_tells_the_audio_thread_the_cables_it_has() {
        let (engine, mut ctl) = controller_for(A_FILTER_AND_A_DETUNE);
        let modulator = ctl.model.free_modulators[0].node;
        ctl.apply(Intent::RemoveNode { node: modulator }).unwrap();
        ctl.model.remove_effect(0, 0).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 1, "the cable to the detune");
        let told = sent(&engine);
        assert_eq!(told.iter().filter(|command| command.starts_with("InsertFreeModulator")).count(), 1, "{told:?}");
        let cables: Vec<&String> = told.iter().filter(|command| command.starts_with("AddFreeModTarget")).collect();
        assert_eq!(cables.len(), 1, "{cables:?}");
        assert!(cables[0].contains(&format!("aim=Param {{ plugin: {:?},", ctl.model.instruments[0].node)), "the cable to the detune, on the instrument: {cables:?}");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_modulator_that_comes_back_without_two_of_its_cables_keeps_the_one_between_them() {
        let (engine, mut ctl) = controller_for(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
slot = 1
param = "resonance"
"#,
        );
        let (modulator, detune) = (ctl.model.free_modulators[0].node, ctl.model.free_modulators[0].targets[1].edge);
        ctl.apply(Intent::RemoveNode { node: modulator }).unwrap();
        // The filter goes without the history, and with it what drove its cutoff and its resonance.
        ctl.model.remove_effect(0, 0).unwrap();
        sent(&engine);
        ctl.undo().unwrap();
        let edges: Vec<EdgeId> = ctl.model.free_modulators[0].targets.iter().map(|target| target.edge).collect();
        assert_eq!(edges, [detune], "the cable to the detune is the one that is left");
        let told = sent(&engine);
        let cables: Vec<&String> = told.iter().filter(|command| command.starts_with("AddFreeModTarget")).collect();
        assert_eq!(cables.len(), 1, "{told:?}");
        assert!(cables[0].contains(&format!("aim=Param {{ plugin: {:?},", ctl.model.instruments[0].node)), "the cable to the detune, on the instrument: {told:?}");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_target_bound_later_carries_its_slot_to_the_real_engine_too() {
        use crate::plugin::chain::LfoWaveform;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(WITH_MODULATORS, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let mut ctl = Controller::new(model, handles, None);
        // The lane's LFO (a 2 Hz sine) gets a target on the filter's cutoff,
        // centred on 0.35 and 0.5 deep.
        assert!(ctl.add_target(ModulatorLocation::Free, 0, cutoff_target(&ctl, 0.5)));
        let mut out = vec![vec![0.0f32; 64]; 2];
        graph.process(&[], &mut out).unwrap();

        let lfo = &ctl.model.free_modulators[0];
        let published = ctl.telemetry.load(lfo.targets[0].effective.unwrap());
        let output = LfoWaveform::Sine.eval(2.0 * 64.0 / 48_000.0);
        assert!((published - (0.35 + 0.5 * output)).abs() < 1e-5, "{published} for an LFO output of {output}");
        assert_eq!(ctl.telemetry.load(lfo.output.unwrap()), output);
    }

    #[test]
    fn the_slots_the_model_holds_are_the_cells_the_real_engine_publishes_into() {
        use crate::plugin::chain::LfoWaveform;
        // A group LFO and a lane LFO, each a square at rate 0 (always +1), driving
        // the filter's `cutoff` (the member's, through the group; the session sets
        // it to 0.5) and `resonance` (left at its default).
        let session = r#"
[[group]]
[[group.modulator]]
waveform = "square"
rate = 0.0
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = -0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.5
[[instrument.modulator]]
waveform = "square"
rate = 0.0
[[instrument.modulator.target]]
slot = 1
param = "resonance"
depth = 0.1
"#;
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(session, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let ctl = Controller::new(model, handles, None);
        let mut out = vec![vec![0.0f32; 64]; 2];
        graph.process(&[], &mut out).unwrap();

        // What the filter says about itself: the parameters' defaults and ranges.
        let filter = crate::plugin::builtin::load("builtin:filter", 48_000.0, 256).unwrap();
        let param = |name: &str| filter.parameters().into_iter().find(|p| p.name == name).unwrap();
        let driven = |name: &str, base: f32, depth: f32| {
            let p = param(name);
            (base + depth * (p.max - p.min)).clamp(p.min, p.max)
        };

        let t = &ctl.telemetry;
        let group_lfo = &hearing_group(&ctl.model, 0)[0];
        let lane_lfo = &ctl.model.free_modulators[0];
        assert_eq!(t.load(group_lfo.output.unwrap()), LfoWaveform::Square.eval(0.0), "the group LFO's output");
        assert_eq!(t.load(lane_lfo.output.unwrap()), 1.0, "the lane LFO's output");
        let cutoff = t.load(group_lfo.targets[0].effective.unwrap());
        assert!(
            (cutoff - driven("cutoff", 0.5, -0.2)).abs() < 1e-6,
            "member 0's cutoff, through the group, centred on the 0.5 the session sets: {cutoff}"
        );
        let resonance = t.load(lane_lfo.targets[0].effective.unwrap());
        assert!(
            (resonance - driven("resonance", param("resonance").default, 0.1)).abs() < 1e-6,
            "the lane's resonance, centred on its default: {resonance}"
        );
        assert_ne!(cutoff, resonance);
        ctl.model.check_ids().unwrap();
        ctl.shutdown();
    }

    // -- lanes and groups --

    #[test]
    fn add_lane_adds_an_empty_full_range_lane_with_a_meter_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(TWO_LANES);
        engine.drain();
        assert_eq!(ctl.add_lane(), 2);
        let lane = &ctl.model.instruments[2];
        assert!(lane.instrument.is_none() && lane.effects.is_empty());
        assert_eq!((lane.range, lane.volume, lane.transpose, lane.group), (None, 1.0, 0, None));
        assert_eq!(lane.pitch_bend_range, 2.0);
        assert!(lane.pattern.is_none());
        let meter = lane.meter.expect("a meter");
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands, ["AddInstrument range=None"]);
        assert_eq!(telemetry, [format!("SetLaneMeter inst=2 peak={} clips={}", meter.peak.index(), meter.clips.index())]);
        assert_ne!(lane.node, ctl.model.instruments[1].node, "a node of its own");
        ctl.model.check_ids().unwrap();
        assert_eq!(ctl.add_lane(), 3);
        assert_ne!(ctl.model.instruments[3].meter, ctl.model.instruments[2].meter, "a meter of its own");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn add_group_adds_an_empty_group_with_a_meter_and_tells_the_audio_thread() {
        let (engine, mut ctl) = controller_for(WITH_MODULATORS);
        engine.drain();
        assert_eq!(ctl.add_group(), 1);
        let group = &ctl.model.groups[1];
        assert!(group.effects.is_empty());
        assert_eq!((group.name.as_deref(), group.volume, group.pan), (None, 1.0, 0.0));
        let meter = group.meter.expect("a meter");
        let (commands, telemetry) = engine.drain();
        assert_eq!(commands, ["AddGroup"]);
        assert_eq!(telemetry, [format!("SetGroupMeter group=1 peak={} clips={}", meter.peak.index(), meter.clips.index())]);
        assert_ne!(group.node, ctl.model.groups[0].node);
        assert_ne!(Some(meter), ctl.model.groups[0].meter);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_and_a_group_the_controller_adds_are_measured_by_the_real_engine() {
        let (handles, engine) = testutil::stub_engine();
        let model = testutil::load_onto(TWO_LANES, &handles.cmd_tx);
        let mut graph = AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
        graph.set_telemetry(handles.telemetry.clone());
        let mut ctl = Controller::new(model, handles, None);
        let lane = ctl.add_lane();
        let (plugin, slot) = ctl.load_plugin("builtin:osc").unwrap();
        ctl.set_instrument(lane, plugin, slot);
        let group = ctl.add_group();
        let _ = ctl.cmd_tx.send(GraphCommand::SetLaneGroup { inst: lane, group: Some(group) });
        ctl.model.instruments[lane].group = Some(group);
        let mut out = vec![vec![0.0f32; 64]; 2];
        // A note, a few buffers of it.
        graph.process(&[(0, [0x90, 60, 127])], &mut out).unwrap();
        for _ in 0..4 {
            graph.process(&[], &mut out).unwrap();
        }
        let peak = |meter: Option<crate::telemetry::MeterSlots>| ctl.telemetry.load(meter.unwrap().peak);
        let new_lane = peak(ctl.model.instruments[lane].meter);
        assert!(new_lane > 0.01, "the new lane's own output: {new_lane}");
        assert_eq!(peak(ctl.model.groups[group].meter), new_lane, "the group holds only that lane");
        assert!(peak(ctl.model.instruments[0].meter) > 0.01, "and the first lane, sounding too: {}", peak(ctl.model.instruments[0].meter));
        ctl.model.check_ids().unwrap();
        ctl.shutdown();
    }

    const THREE_LANES_TWO_GROUPS: &str = r#"
[[group]]
name = "Pad"
[[group]]
name = "Lead"

[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_lane_is_put_in_a_group_moved_to_another_and_taken_out_of_its_group() {
        let (engine, mut ctl) = controller_for(THREE_LANES_TWO_GROUPS);
        sent(&engine);
        assert!(ctl.set_lane_group(0, Some(0)));
        assert!(ctl.set_lane_group(1, Some(1)));
        assert!(ctl.set_lane_group(0, None));
        let groups: Vec<Option<usize>> = ctl.model.instruments.iter().map(|lane| lane.group).collect();
        assert_eq!(groups, [None, Some(1), None]);
        assert_eq!(sent(&engine), ["SetLaneGroup inst=0 group=Some(0)", "SetLaneGroup inst=1 group=Some(1)", "SetLaneGroup inst=0 group=None"]);
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_or_a_group_that_is_not_there_changes_nothing_and_sends_nothing() {
        let (engine, mut ctl) = controller_for(THREE_LANES_TWO_GROUPS);
        sent(&engine);
        assert!(!ctl.set_lane_group(3, Some(0)), "no such lane");
        assert!(!ctl.set_lane_group(3, None), "no such lane to take out of a group");
        assert!(!ctl.set_lane_group(0, Some(2)), "no such group");
        assert!(sent(&engine).is_empty());
        let groups: Vec<Option<usize>> = ctl.model.instruments.iter().map(|lane| lane.group).collect();
        assert_eq!(groups, [None, Some(0), None]);
    }

    /// Group 0's LFO, a square at rate 0 (always +1), drives the detune of its first member, lane 1; lane 0 is in no
    /// group.
    const A_GROUP_DRIVING_ITS_FIRST_MEMBER: &str = r#"
[[group]]
[[group.modulator]]
waveform = "square"
rate = 0.0
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1

[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
group = 0
"#;

    #[test]
    fn a_lane_put_in_the_group_it_is_in_keeps_the_cables_to_it() {
        let (mut graph, mut ctl) = controller_on_a_real_graph(A_GROUP_DRIVING_ITS_FIRST_MEMBER);
        let detune = index_of(ctl.model.instruments[1].instrument.as_ref().unwrap(), "detune");
        assert!(ctl.set_lane_group(1, Some(0)));
        run(&mut graph);
        assert_eq!(hearing_group(&ctl.model, 0)[0].targets.len(), 1, "the group's cable to it is still there");
        assert!(graph.plugin_parameter(1, 0, detune).unwrap() > 1.0, "and still drives its detune");
        ctl.model.check_ids().unwrap();
    }

    #[test]
    fn a_lane_put_in_the_group_it_is_in_has_nothing_taken_out_and_nothing_given_back() {
        let (engine, mut ctl) = controller_for(GROUP_CABLES);
        sent(&engine);
        assert!(ctl.set_lane_group(0, Some(0)));
        assert_eq!(sent(&engine), ["SetLaneGroup inst=0 group=Some(0)"]);
        assert_eq!(hearing_group(&ctl.model, 0)[0].targets.len(), 3);
    }

    // -- What a cable gives back when it is taken out --

    /// A square LFO at rate 0 is a constant +1, so a cable from one adds `depth` of the range to what it drives.
    const ONE_CUTOFF_TWO_CABLES: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.5
[[instrument.modulator]]
waveform = "square"
rate = 0.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.2
[[instrument.modulator]]
waveform = "square"
rate = 0.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.1
[[instrument.modulator]]
type = "envelope"
[[instrument.modulator.target]]
slot = 1
param = "resonance"
depth = 0.5
"#;

    /// Let the audio thread take what the controller has sent and run a few buffers.
    fn run(graph: &mut crate::plugin::chain::AudioGraph) {
        graph.drain_commands();
        let mut out = vec![vec![0.0f32; 64]; 2];
        for _ in 0..3 {
            graph.process(&[], &mut out).unwrap();
        }
    }

    fn index_of(plugin: &PluginSlot, name: &str) -> u32 {
        plugin.params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no parameter {name}")).index
    }

    #[test]
    fn taking_a_cable_out_gives_the_parameter_back_what_the_user_set_unless_another_cable_still_drives_it() {
        let (mut graph, mut ctl) = controller_on_a_real_graph(ONE_CUTOFF_TWO_CABLES);
        let cutoff = index_of(&ctl.model.instruments[0].effects[0], "cutoff");
        run(&mut graph);
        assert_eq!(graph.plugin_parameter(0, 1, cutoff), Some(0.8), "0.5, and 0.2 and 0.1 from the two cables");
        let first = ctl.model.free_modulators[0].targets[0].edge;
        assert!(ctl.remove_edge(first));
        run(&mut graph);
        assert_eq!(graph.plugin_parameter(0, 1, cutoff), Some(0.6), "the other cable drives it still");
        let second = ctl.model.free_modulators[1].targets[0].edge;
        assert!(ctl.remove_edge(second));
        run(&mut graph);
        assert_eq!(graph.plugin_parameter(0, 1, cutoff), Some(0.5), "and now it is what the user set");
        assert_eq!(ctl.model.instruments[0].effects[0].params[cutoff as usize].value, 0.5, "which is what the model says");
    }

    #[test]
    fn taking_a_modulator_out_gives_back_what_its_cables_drove() {
        let (mut graph, mut ctl) = controller_on_a_real_graph(ONE_CUTOFF_TWO_CABLES);
        let filter = &ctl.model.instruments[0].effects[0];
        let (cutoff, resonance) = (index_of(filter, "cutoff"), index_of(filter, "resonance"));
        run(&mut graph);
        // The envelope is at rest: its cable at depth 0.5 takes the resonance (2.0 on a range of 0.5 to 10, in the
        // session's own default 0.707) half the way down to the bottom.
        let driven = graph.plugin_parameter(0, 1, resonance).unwrap();
        let set = ctl.model.instruments[0].effects[0].params[resonance as usize].value;
        assert!(driven < set, "the envelope at rest holds the resonance below what is set: {driven} against {set}");

        assert!(ctl.remove_modulator(ModulatorLocation::Free, 2));
        run(&mut graph);
        assert_eq!(graph.plugin_parameter(0, 1, resonance), Some(set), "the envelope is gone, and the resonance is what is set");
        assert_eq!(graph.plugin_parameter(0, 1, cutoff), Some(0.8), "what the others drive is left to them");
        assert!(ctl.remove_modulator(ModulatorLocation::Free, 0));
        run(&mut graph);
        assert_eq!(graph.plugin_parameter(0, 1, cutoff), Some(0.6));
    }

    /// m0, a square LFO at rate 0, drives the rate of m1 and the depth of its first cable; m1 drives the detune.
    const CABLES_ON_CABLES: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
waveform = "square"
rate = 0.0
[[instrument.modulator.target]]
mod_rate = 1
depth = 0.5
[[instrument.modulator.target]]
mod_depth = [1, 0]
depth = 0.5
[[instrument.modulator]]
waveform = "sine"
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
"#;

    #[test]
    fn taking_out_the_cables_that_drive_a_modulators_rate_and_depth_gives_them_back() {
        let (mut graph, mut ctl) = controller_on_a_real_graph(CABLES_ON_CABLES);
        run(&mut graph);
        let (rate, depths) = graph.free_lfo(1).unwrap();
        assert!(rate > 20.0 && depths[0] > 0.5, "driven: {rate} Hz, depth {}", depths[0]);
        let (set_rate, set_depth) = match ctl.model.free_modulators[1].source {
            crate::app::model::ModSourceSlot::Lfo { rate, .. } => (rate, ctl.model.free_modulators[1].targets[0].depth),
            _ => panic!("an LFO"),
        };
        assert_eq!((set_rate, set_depth), (2.0, 0.1));

        let rate_cable = ctl.model.free_modulators[0].targets[0].edge;
        assert!(ctl.remove_edge(rate_cable));
        run(&mut graph);
        assert_eq!(graph.free_lfo(1).map(|(r, _)| r), Some(2.0), "the rate is its own again");
        assert!(graph.free_lfo(1).unwrap().1[0] > 0.5, "and the depth is still driven");
        let depth_cable = ctl.model.free_modulators[0].targets[0].edge;
        assert!(ctl.remove_edge(depth_cable));
        run(&mut graph);
        assert_eq!(graph.free_lfo(1), Some((2.0, vec![0.1])), "and so is the depth");
    }

    /// Group 0 has a filter on its bus; its square LFO drives the first member's detune, the bus filter's cutoff and
    /// the rate of the first member's own LFO.
    const GROUP_CABLES: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[group.effect.params]
cutoff = 0.5
[[group.modulator]]
waveform = "square"
rate = 0.0
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.2
[[group.modulator.target]]
member = 0
mod_rate = 0
depth = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
waveform = "sine"
rate = 2.0
"#;

    #[test]
    fn a_cable_that_is_not_the_last_on_a_parameter_gives_nothing_back_and_the_last_one_does() {
        // The commands, not what the engine ends up with (a cable that is left writes every buffer): giving
        // the user's value back to a parameter that is still driven would be a click for nothing.
        let (engine, mut ctl) = controller_for(ONE_CUTOFF_TWO_CABLES);
        sent(&engine);
        let first = ctl.model.free_modulators[0].targets[0].edge;
        assert!(ctl.remove_edge(first));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=0"], "the other cable drives the cutoff still");
        let second = ctl.model.free_modulators[1].targets[0].edge;
        assert!(ctl.remove_edge(second));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=1 target=0", "SetParameter inst=0 slot=1 param=0 value=0.500"]);
    }

    /// m0, a square LFO at rate 0, drives the stages of the envelope m1: its attack, decay, sustain and release.
    const CABLES_TO_AN_ENVELOPE: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
waveform = "square"
rate = 0.0
[[instrument.modulator.target]]
mod_attack = 1
depth = 0.1
[[instrument.modulator.target]]
mod_decay = 1
depth = 0.1
[[instrument.modulator.target]]
mod_sustain = 1
depth = 0.1
[[instrument.modulator.target]]
mod_release = 1
depth = 0.1
[[instrument.modulator]]
type = "envelope"
attack = 0.2
decay = 0.4
sustain = 0.6
release = 0.8
"#;

    #[test]
    fn taking_out_the_cables_that_drive_an_envelopes_stages_gives_them_back() {
        let (mut graph, mut ctl) = controller_on_a_real_graph(CABLES_TO_AN_ENVELOPE);
        run(&mut graph);
        let (attack, decay, sustain, release) = graph.free_envelope(1).unwrap();
        assert!(attack > 0.2 && decay > 0.4 && sustain != 0.6 && release > 0.8, "driven: {:?}", (attack, decay, sustain, release));
        // One at a time, from the first: what is let go of is its own again, and what is still driven is not.
        for own_now in [(true, false, false, false), (true, true, false, false), (true, true, true, false), (true, true, true, true)] {
            let edge = ctl.model.free_modulators[0].targets[0].edge;
            assert!(ctl.remove_edge(edge));
            run(&mut graph);
            let now = graph.free_envelope(1).unwrap();
            assert_eq!((now.0 == 0.2, now.1 == 0.4, now.2 == 0.6, now.3 == 0.8), own_now, "after {now:?}");
        }
        assert_eq!(graph.free_envelope(1), Some((0.2, 0.4, 0.6, 0.8)));
    }

    #[test]
    fn taking_out_a_group_modulators_cable_to_a_siblings_rate_gives_the_groups_rate_back() {
        let (engine, mut ctl) = controller_for(testutil::GROUPS);
        sent(&engine);
        // The group's second modulator, an envelope, has a cable to the first one's rate (0.5 Hz) and one to the depth of its third cable.
        let rate_cable = hearing_group(&ctl.model, 0)[1].targets[0].edge;
        assert!(ctl.remove_edge(rate_cable));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=2 target=0", "SetFreeModulatorRate mod=1 rate=0.500"]);
        let depth_cable = hearing_group(&ctl.model, 0)[1].targets[0].edge;
        assert!(ctl.remove_edge(depth_cable));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=2 target=0", "SetFreeModTargetDepth mod=1 target=2 depth=0.250"]);
    }

    #[test]
    fn a_modulator_with_two_cables_to_one_parameter_gives_it_back_once_and_a_cable_to_its_own_modulator_is_not_made() {
        let (engine, mut ctl) = controller_for(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.1
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.2
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.3
"#,
        );
        sent(&engine);
        // (A cable to its own rate does nothing: the file has one, and it is not made.)
        assert_eq!(ctl.model.free_modulators[0].targets.len(), 2);
        assert!(ctl.remove_modulator(ModulatorLocation::Free, 0));
        assert_eq!(sent(&engine), ["RemoveFreeModulator index=0", "SetParameter inst=0 slot=1 param=0 value=1.000"], "the cutoff once");
    }

    #[test]
    fn a_cable_to_the_second_bus_effect_gives_that_effect_its_value_back() {
        let (engine, mut ctl) = controller_for(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"
[[group.modulator]]
rate = 1.0
[[group.modulator.target]]
bus = 0
param = "cutoff"
[[group.modulator.target]]
bus = 1
param = "damping"
[[instrument]]
plugin = "builtin:osc"
group = 0
"#,
        );
        sent(&engine);
        let to_reverb = hearing_group(&ctl.model, 0)[0].targets[1].edge;
        assert!(ctl.remove_edge(to_reverb));
        assert_eq!(sent(&engine), ["RemoveFreeModTarget mod=0 target=1", "SetGroupParameter group=0 fx=1 param=2 value=0.350"]);
    }
}

