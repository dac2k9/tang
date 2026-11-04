//! The session model: the main thread's mirror of what the audio graph holds.
//!
//! One `InstrumentNode` per lane (instrument slot, effects, lane modulators,
//! pattern, group membership) and one `GroupNode` per submix group (bus effects,
//! group modulators). Front-ends edit it alongside the audio graph: every change
//! is applied to both, the model is what gets saved, and the helpers at the end
//! of this file keep the model's indices in step with the audio thread's own
//! fix-ups (`shift_slots_after_insert`, `fixup_group_member_after_remove`, ...
//! in `plugin::chain`) when lanes, effects, modulators or members come and go.
//!
//! Nothing here knows about the terminal: the TUI imports it today, and the
//! controller and the web projection planned in `docs/web-ui.md` build on it.

use crate::app::intent::{Intent, IntentError, ModulatorField};
use crate::plugin;
use crate::plugin::chain::{CableFrom, CableProblem, CableTo, CrossModField, GroupWiring, LaneWiring, ModTargetKind, ModulatorLocation};
use crate::session::{NodeUi, Route};
use crate::source::SourceSet;
use crate::telemetry::{MeterSlots, Slot, SlotAllocator};
use std::collections::{BTreeMap, VecDeque};

pub use crate::ids::{Driven, EdgeId, ModGain, ModSetting, NodeId};

/// The ids that were handed out for something: the nodes and the cables, each in the order they were made.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Made {
    pub nodes: Vec<NodeId>,
    pub edges: Vec<EdgeId>,
}

/// Hands out `NodeId`s and `EdgeId`s, each one once. The two are series of their
/// own.
///
/// One thing is handed out twice: what undo puts back (a node that was taken out, or made, and is made again) has the ids it had, so
/// that every step of the history that names it goes on naming it. `reuse` says which ids come next, before any new one.
#[derive(Debug, Default)]
pub struct NodeIds {
    next: u32,
    next_edge: u32,
    /// The ids to hand out before new ones, in order.
    put_back: VecDeque<NodeId>,
    put_back_edges: VecDeque<EdgeId>,
    /// Every id handed out since `begin_log`, if it was begun.
    log: Option<Made>,
}

impl NodeIds {
    pub fn alloc(&mut self) -> NodeId {
        let id = self.put_back.pop_front().unwrap_or_else(|| {
            let id = NodeId(self.next);
            self.next += 1;
            id
        });
        if let Some(log) = &mut self.log {
            log.nodes.push(id);
        }
        id
    }

    pub fn alloc_edge(&mut self) -> EdgeId {
        let id = self.put_back_edges.pop_front().unwrap_or_else(|| {
            let id = EdgeId(self.next_edge);
            self.next_edge += 1;
            id
        });
        if let Some(log) = &mut self.log {
            log.edges.push(id);
        }
        id
    }

    /// The ids that come next, in this order, in place of new ones: what undo puts back is under the ids it had.
    pub fn reuse(&mut self, nodes: &[NodeId], edges: &[EdgeId]) {
        self.put_back = nodes.iter().copied().collect();
        self.put_back_edges = edges.iter().copied().collect();
    }

    /// What was not used of the ids that were to come next is not to be: new ids go on.
    pub fn drop_ids_to_put_back(&mut self) {
        self.put_back.clear();
        self.put_back_edges.clear();
    }

    /// Write down every id that is handed out, from now until `take_log`.
    pub fn begin_log(&mut self) {
        self.log = Some(Made::default());
    }

    /// The ids handed out since `begin_log`, in order, and nothing is written down after this until the next `begin_log`.
    pub fn take_log(&mut self) -> Made {
        self.log.take().unwrap_or_default()
    }
}

/// Where a node sits in the model, in the positional terms the audio thread's
/// commands use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodePath {
    /// A lane, with the instrument plugin in it.
    Instrument(usize),
    Effect { inst: usize, index: usize },
    Group(usize),
    GroupEffect { group: usize, index: usize },
    Modulator { at: ModulatorLocation, index: usize },
    /// A source of notes: the keys, or a MIDI device.
    Source(usize),
    /// A module that was added to the session as one thing (step 108): the box that a front-end draws in place of its lanes and modulators.
    Module(usize),
}

/// The field of a modulator, as the intents name it, that a cable drives when it drives `setting`.
pub fn field_of_setting(setting: ModSetting) -> ModulatorField {
    match setting {
        ModSetting::Rate => ModulatorField::Rate,
        ModSetting::Attack => ModulatorField::Attack,
        ModSetting::Decay => ModulatorField::Decay,
        ModSetting::Sustain => ModulatorField::Sustain,
        ModSetting::Release => ModulatorField::Release,
    }
}

/// The setting of a modulator that a cable drives when its place in a rack names the field `field` (`None` for the depth of a cable, which is not a setting).
pub fn setting_of(field: CrossModField) -> Option<ModSetting> {
    match field {
        CrossModField::Rate => Some(ModSetting::Rate),
        CrossModField::Attack => Some(ModSetting::Attack),
        CrossModField::Decay => Some(ModSetting::Decay),
        CrossModField::Sustain => Some(ModSetting::Sustain),
        CrossModField::Release => Some(ModSetting::Release),
        CrossModField::Depth(_) => None,
    }
}

/// The field that a place in a rack names when a cable drives `setting`.
pub fn place_field_of(setting: ModSetting) -> CrossModField {
    match setting {
        ModSetting::Rate => CrossModField::Rate,
        ModSetting::Attack => CrossModField::Attack,
        ModSetting::Decay => CrossModField::Decay,
        ModSetting::Sustain => CrossModField::Sustain,
        ModSetting::Release => CrossModField::Release,
    }
}

/// The tempo of a session none of whose patterns has been recorded, in beats a minute: the one the audio thread begins at as well.
pub const DEFAULT_BPM: f32 = crate::plugin::chain::DEFAULT_BPM;
/// The slowest and the fastest tempo there can be, in beats a minute: a tempo that a session file has is held to them, as is one that is set.
pub const BPM_MIN: f32 = 20.0;
pub const BPM_MAX: f32 = 300.0;

/// The master's volume goes from nothing to twice the mix (unity is 1), and the limiter's ceiling from 24 decibels below full scale to full scale (step 102).
pub const MASTER_VOLUME_MAX: f32 = 2.0;
pub const CEILING_MIN: f32 = -24.0;
pub const CEILING_MAX: f32 = 0.0;
/// What the master is when a session does not say: unity, with the limiter in the sound a decibel under full scale (the ceiling that the audio thread starts with, too).
pub const DEFAULT_CEILING: f32 = crate::plugin::chain::DEFAULT_CEILING;

/// The output mix, whose settings the session keeps (step 102): its volume, whether the limiter is in the sound and where its ceiling is. The mix itself is a node of the graph and not of the
/// model (`NodeId::MASTER`: `locate` has nothing for it), and these are what a `[master]` table of the file says and the intents `SetGain` (`MasterVolume`, `LimiterCeiling`) and `SetLimiter` set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MasterNode {
    /// The gain on the mix, 0 to `MASTER_VOLUME_MAX`: 1 leaves it as it is.
    pub volume: f32,
    /// Whether the limiter is in the sound. It is, unless a file says it is not (`limiter = false`): the user's "limiter default on".
    pub limiter: bool,
    /// The level that the limiter holds the sound under, in decibels below full scale (`CEILING_MIN` to `CEILING_MAX`).
    pub ceiling: f32,
}

impl Default for MasterNode {
    fn default() -> Self {
        MasterNode { volume: 1.0, limiter: true, ceiling: DEFAULT_CEILING }
    }
}

/// What a front-end edits: the instrument lanes and the submix groups.
#[derive(Debug)]
pub struct SessionModel {
    pub instruments: Vec<InstrumentNode>,
    pub groups: Vec<GroupNode>,
    pub ids: NodeIds,
    /// Hands out the telemetry slots the nodes publish into.
    pub slots: SlotAllocator,
    /// Where a front-end draws the nodes it has been told a place for, by node: a side table, since the engine and the
    /// TUI have no use for it and a node does not need one. The master is under `NodeId::MASTER`. An id is never
    /// reused, so what is left here of a node that has gone is never read: `graph()` and the save look up the nodes
    /// there are.
    pub ui: BTreeMap<NodeId, NodeUi>,
    /// The tempo that patterns are recorded at, in beats a minute: that of the first recorded pattern of a session that is loaded
    /// (`DEFAULT_BPM` if it has none).
    pub bpm: f32,
    /// The output mix: its volume and its limiter.
    pub master: MasterNode,
    /// The audio cables that have been drawn (P3c of `docs/patch-graph.md` §11.6), in the order of their ids: sends from one place of the audio to another, besides the
    /// way that a lane and a group are wired by themselves.
    pub cables: Vec<AudioCable>,
    /// The sources of notes that a person put in the session (P4b of `docs/patch-graph.md` §11.6): the keys, and the MIDI devices that it names. None until one is added, and then a lane
    /// that no event cable ends at hears every source (the program is as it was), and one that has some hears the sources they start at.
    pub sources: Vec<SourceNode>,
    /// The event cables that have been drawn, in the order of their ids: the notes of a source go to a lane.
    pub event_cables: Vec<EventCable>,
    /// The modulators that belong to no lane and no group (P5b-2 of `docs/patch-graph.md` §11.6), in the order they were made: each hears every note, and its cables drive any plugin
    /// of the session and the settings of the other free modulators and the depths of their cables.
    pub free_modulators: Vec<ModulatorSlot>,
    /// The modules that were added to the session as one thing (step 108), in the order they were made: a record of the lanes and the modulators that came with each, which a front-end draws as one box
    /// (`ModuleInstance`). A record is a node of the graph of its own and has no part in the sound: the engine runs the lanes and the modulators as it runs any.
    pub modules: Vec<ModuleInstance>,
}

impl Default for SessionModel {
    fn default() -> Self {
        SessionModel {
            instruments: Vec::new(),
            groups: Vec::new(),
            ids: NodeIds::default(),
            slots: SlotAllocator::default(),
            ui: BTreeMap::new(),
            bpm: DEFAULT_BPM,
            master: MasterNode::default(),
            cables: Vec::new(),
            sources: Vec::new(),
            event_cables: Vec::new(),
            free_modulators: Vec::new(),
            modules: Vec::new(),
        }
    }
}

/// A module that was added to the session as one thing (step 108, the user's "when we re-add a module it gets rendered like just a single box"): the name that it has in the library, and the nodes that came
/// with it, which a front-end draws as one box and the engine runs as it runs any lane and any modulator (nothing of it is told to the audio thread). Its node, from the series that the other nodes' are,
/// is the box's: where it is drawn (`SessionModel::ui`), what is taken out to take the whole module out, and what the ports of the box are told by (the page derives them from the members' ports).
///
/// The members are the **lanes** (each with the effects of its chain, which are in the lane, so that an effect that is added to a lane of a module is in it) and the **modulators** that were made with it, in the
/// order they were made. A node that is taken out leaves the module it was in, and a module that has no member left is gone: it is a record of what came together, and no more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleInstance {
    pub node: NodeId,
    /// What the box is called: the name that the module has in the library.
    pub name: String,
    /// The lanes and the modulators that are in it, by node, in the order they were made.
    pub members: Vec<NodeId>,
}

/// A source of notes that a session has (P4b of `docs/patch-graph.md` §11.6): a node of the graph with an output of events, which event cables start at. Its id is from the
/// series that the other nodes' are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceNode {
    pub node: NodeId,
    pub kind: SourceKind,
}

/// What a source of notes is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// The computer's keys, the page's piano and keys, and every browser's: they are one source (`Source::KEYS`).
    Keys,
    /// A MIDI device, by the name of its port as the system gives it, or as much of it as tells it ("Launchkey" is both ports of a Launchkey Mini): the sources of the open ports that
    /// have the name (`DeviceWatcher::sources_of`).
    Device(String),
}

impl SourceKind {
    /// What the node of the source is called.
    pub fn display_name(&self) -> String {
        match self {
            SourceKind::Keys => "Keys".to_string(),
            SourceKind::Device(name) => name.clone(),
        }
    }
}

/// An event cable that a session has: the notes of a source go to a lane, which then hears that source (and the others that cables bring it) and no other. Its id is from the series
/// that the other cables' are, so that no two edges of the graph a client gets have one id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventCable {
    pub id: EdgeId,
    /// The node of the source it starts at.
    pub from: NodeId,
    /// The node of the lane it ends at (the lane's instrument is the lane's own node).
    pub to: NodeId,
}

/// An audio cable that a session has: a send from one place of the audio to another (P3c of `docs/patch-graph.md` §11.6). Its id is from the series that the modulation
/// cables' are, so that no two edges of the graph a client gets have one id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioCable {
    pub id: EdgeId,
    pub from: CableFrom,
    pub to: CableTo,
    /// The send level, `0..=CABLE_GAIN_MAX`: 1 is what comes out of `from`, as it is.
    pub gain: f32,
}

/// Whose a link of the audio path is that a lane or a group makes by itself (step 88, P5.0 of `docs/patch-graph.md` §11.6): the node that it is the way out of, or the input of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkOf {
    /// The way out of the lane or the group with this node: the end of its chain, to the bus of its group or the mix. Cut by `Route::None`.
    WayOut(NodeId),
    /// The input of the effect or bus effect with this node: the stage before it in its chain, to it. Cut by `Feed::None`.
    Feed(NodeId),
}

/// A link of the audio path that a lane or a group makes by itself, as the two ends that an audio cable between them would have, and whether it is there. The page draws
/// it as a cable (it has no id and no level of its own: it is the template's, `route` and `feed` are what the model keeps of it), and a cable that is drawn between its two ends gives
/// it back if it is cut (`Controller::apply`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateLink {
    pub of: LinkOf,
    pub from: CableFrom,
    pub to: CableTo,
    /// Whether the link is there: a way out that is not cut, a feed that is not cut.
    pub present: bool,
}

/// The highest send level an intent can set (+6 dB).
pub const CABLE_GAIN_MAX: f32 = 2.0;

/// The level that a cable is drawn at when nothing says another: what comes out of its start, as it is.
pub const NEW_CABLE_GAIN: f32 = 1.0;

/// The tempo a session comes to when its file says none: that of the first of its lanes that has a recorded pattern, else `DEFAULT_BPM`, held
/// to the tempos there can be.
pub fn tempo_of_patterns(lanes: &[InstrumentNode]) -> f32 {
    lanes.iter().find_map(|lane| lane.pattern.as_ref()).map_or(DEFAULT_BPM, |pattern| pattern.bpm.clamp(BPM_MIN, BPM_MAX))
}

impl SessionModel {
    /// The wiring of the lanes and the groups, as the plan of the audio thread is made of it.
    pub fn wiring(&self) -> (Vec<LaneWiring>, Vec<GroupWiring>) {
        (
            self.instruments
                .iter()
                .map(|lane| LaneWiring {
                    node: lane.node,
                    group: lane.group,
                    effects: lane.effects.iter().map(|effect| effect.node).collect(),
                    cut: lane.effects.iter().filter(|effect| !effect.fed).map(|effect| effect.node).collect(),
                    sides: lane.effects.iter().filter(|effect| effect.side.is_some()).map(|effect| effect.node).collect(),
                    route: lane.route,
                })
                .collect(),
            self.groups
                .iter()
                .map(|group| GroupWiring {
                    node: group.node,
                    effects: group.effects.iter().map(|effect| effect.node).collect(),
                    cut: group.effects.iter().filter(|effect| !effect.fed).map(|effect| effect.node).collect(),
                    sides: group.effects.iter().filter(|effect| effect.side.is_some()).map(|effect| effect.node).collect(),
                    route: group.route,
                })
                .collect(),
        )
    }

    /// The links of the audio path that the lanes and the groups make by themselves (what `graph` draws as `Audio` and `Member` edges), each as the two ends that an audio cable between
    /// them would have, and whether it is there: a lane's instrument to its first effect, each effect to the next, and the end of the chain to the bus of the lane's group or to the mix; and a
    /// group's own link into its first bus effect (it starts at the group's node, as the graph draws it), from bus effect to bus effect, and from the end of its chain to the mix. The lanes' in
    /// the order of the lanes, and then the groups'.
    pub fn template_links(&self) -> Vec<TemplateLink> {
        let mut links = Vec::new();
        for lane in &self.instruments {
            let mut before = CableFrom::Instrument(lane.node);
            for effect in &lane.effects {
                links.push(TemplateLink { of: LinkOf::Feed(effect.node), from: before, to: CableTo::Effect(effect.node), present: effect.fed });
                before = CableFrom::Effect(effect.node);
            }
            // (A lane whose group is gone goes to the mix, as the plan and the graph have it.)
            let sink = lane.group.and_then(|group| self.groups.get(group)).map_or(CableTo::Master, |group| CableTo::GroupBus(group.node));
            links.push(TemplateLink { of: LinkOf::WayOut(lane.node), from: before, to: sink, present: lane.route == Route::Out });
        }
        for group in &self.groups {
            let mut before = CableFrom::Group(group.node);
            for effect in &group.effects {
                links.push(TemplateLink { of: LinkOf::Feed(effect.node), from: before, to: CableTo::Effect(effect.node), present: effect.fed });
                before = CableFrom::Effect(effect.node);
            }
            links.push(TemplateLink { of: LinkOf::WayOut(group.node), from: before, to: CableTo::Master, present: group.route == Route::Out });
        }
        links
    }

    /// The link of the audio path that joins `from` and `to`, if one does.
    pub fn template_link(&self, from: CableFrom, to: CableTo) -> Option<TemplateLink> {
        self.template_links().into_iter().find(|link| (link.from, link.to) == (from, to))
    }

    /// Why a cable from `from` to `to` cannot be drawn: an end that is not there, or a loop (the audio would go round). None if it can. (The plan is compiled on the side with it, as
    /// the audio thread does when it is sent one, so that what is refused here is what it would refuse, and the controller can say why.)
    pub fn cable_problem(&self, from: CableFrom, to: CableTo) -> Option<CableProblem> {
        let (lanes, groups) = self.wiring();
        let others = self.cables.iter().map(|cable| (cable.id, cable.from, cable.to));
        // (An id that no cable has: the cable is not there yet.)
        crate::plugin::chain::cable_problem(&lanes, &groups, others, (EdgeId(u32::MAX), from, to), crate::plugin::chain::CHAIN_CHANNELS)
    }

    /// Why the audio would go round if lane `inst` were in group `group` (or in none), with the cables there are: the reason, or None if it would not (or if the lane or
    /// the group is not there, which is for the caller to say). Nothing is changed.
    pub fn loop_if_lane_goes_to(&self, inst: usize, group: Option<usize>) -> Option<String> {
        let (mut lanes, groups) = self.wiring();
        lanes.get_mut(inst)?.group = group.filter(|group| *group < groups.len());
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the audio would go round if the lane or the group at `node` had the way out `route`, with the cables there are: the reason, or None if it would not (or if `node` is
    /// neither). Nothing is changed. Only a way out that is `Out` can close a loop: a link that is not there is on no path.
    pub fn loop_if_route_is(&self, node: NodePath, route: Route) -> Option<String> {
        let (mut lanes, mut groups) = self.wiring();
        match node {
            NodePath::Instrument(inst) => lanes.get_mut(inst)?.route = route,
            NodePath::Group(group) => groups.get_mut(group)?.route = route,
            _ => return None,
        }
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the audio would go round if the link into the effect `node` (of a lane's chain or of a group's bus) from the stage before it were there (`fed`) or not, with the cables there are:
    /// the reason, or None if it would not (or if `node` is no effect). Nothing is changed. Only a link that is there can close a loop: one that is not is on no path.
    pub fn loop_if_feed_is(&self, node: NodeId, fed: bool) -> Option<String> {
        let (mut lanes, mut groups) = self.wiring();
        let cut = match self.locate(node)? {
            NodePath::Effect { inst, .. } => &mut lanes.get_mut(inst)?.cut,
            NodePath::GroupEffect { group, .. } => &mut groups.get_mut(group)?.cut,
            _ => return None,
        };
        cut.retain(|other| *other != node);
        if !fed {
            cut.push(node);
        }
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the audio would go round if the effect `node` (of a lane's chain or of a group's bus) were at place `index` of its chain, the effects between moving one place the
    /// other way, with the cables there are: the reason, or None if it would not (or if the node is no effect, or the chain has no such place). Nothing is changed.
    pub fn loop_if_effect_goes_to(&self, node: NodeId, index: usize) -> Option<String> {
        let (mut lanes, mut groups) = self.wiring();
        let chain = match self.locate(node)? {
            NodePath::Effect { inst, .. } => &mut lanes.get_mut(inst)?.effects,
            NodePath::GroupEffect { group, .. } => &mut groups.get_mut(group)?.effects,
            _ => return None,
        };
        let from = chain.iter().position(|effect| *effect == node)?;
        if index >= chain.len() {
            return None;
        }
        let effect = chain.remove(from);
        chain.insert(index, effect);
        self.wiring_loop(&lanes, &groups)
    }

    /// Why the wiring `lanes` and `groups`, with the cables there are, does not compile: the audio goes round in it.
    fn wiring_loop(&self, lanes: &[LaneWiring], groups: &[GroupWiring]) -> Option<String> {
        let cables = self.cables.iter().map(|cable| (cable.id, cable.from, cable.to));
        crate::plugin::chain::wiring_loop(lanes, groups, cables, crate::plugin::chain::CHAIN_CHANNELS)
    }

    /// The audio cable `id`, if there is one.
    pub fn audio_cable(&self, id: EdgeId) -> Option<&AudioCable> {
        self.cables.binary_search_by_key(&id, |cable| cable.id).ok().map(|at| &self.cables[at])
    }

    /// The audio cable `id`, to change.
    pub fn audio_cable_mut(&mut self, id: EdgeId) -> Option<&mut AudioCable> {
        self.cables.binary_search_by_key(&id, |cable| cable.id).ok().map(|at| &mut self.cables[at])
    }

    /// Put a cable among the cables, in the order of the ids (one that is there under the id is replaced): the place it went to.
    pub fn insert_audio_cable(&mut self, cable: AudioCable) -> usize {
        match self.cables.binary_search_by_key(&cable.id, |other| other.id) {
            Ok(at) => {
                self.cables[at] = cable;
                at
            }
            Err(at) => {
                self.cables.insert(at, cable);
                at
            }
        }
    }

    /// Put an event cable among the event cables, in the order of the ids (one that is there under the id is replaced): the place it went to.
    pub fn insert_event_cable(&mut self, cable: EventCable) -> usize {
        match self.event_cables.binary_search_by_key(&cable.id, |other| other.id) {
            Ok(at) => {
                self.event_cables[at] = cable;
                at
            }
            Err(at) => {
                self.event_cables.insert(at, cable);
                at
            }
        }
    }

    /// The source of notes whose node is `node`, if there is one.
    pub fn source(&self, node: NodeId) -> Option<&SourceNode> {
        self.sources.iter().find(|source| source.node == node)
    }

    /// Put a source of notes among the sources at `index` (at the end for one past it): the place it went to.
    pub fn insert_source_at(&mut self, index: usize, source: SourceNode) -> usize {
        let index = index.min(self.sources.len());
        self.sources.insert(index, source);
        index
    }

    /// The place among the modules of the one that `member` came with: a lane or a modulator (a node is in at most one module).
    pub fn module_of(&self, member: NodeId) -> Option<usize> {
        self.modules.iter().position(|module| module.members.contains(&member))
    }

    /// Put a module among the modules at `index` (at the end for one past it); one that is there under the node is replaced, at the place it has: the place it went to.
    pub fn insert_module_at(&mut self, index: usize, module: ModuleInstance) -> usize {
        if let Some(at) = self.modules.iter().position(|there| there.node == module.node) {
            self.modules[at] = module;
            return at;
        }
        let index = index.min(self.modules.len());
        self.modules.insert(index, module);
        index
    }

    /// `member` leaves the module it is in, because it is taken out of the session: the place of the module and the module as it was (for what puts it back), or none if the node is in none. A module that has no
    /// member left is gone with it.
    pub fn leave_module(&mut self, member: NodeId) -> Option<(usize, ModuleInstance)> {
        let at = self.module_of(member)?;
        let before = self.modules[at].clone();
        self.modules[at].members.retain(|node| *node != member);
        if self.modules[at].members.is_empty() {
            self.modules.remove(at);
        }
        Some((at, before))
    }

    /// The event cable `id`, if there is one.
    pub fn event_cable(&self, id: EdgeId) -> Option<&EventCable> {
        self.event_cables.binary_search_by_key(&id, |cable| cable.id).ok().map(|at| &self.event_cables[at])
    }

    /// The ends that a cable from the port `from` to the port `to` of the graph has, if it is a cable of notes: the node of the source that it starts at (`EventsOut`) and the node of the
    /// lane that it ends at (`EventsIn`; a lane's instrument is the lane's own node). The reason, if they are not that: a node that is not there is not found, and a port or a node of
    /// another kind is invalid.
    pub fn event_cable_ends(&self, from: crate::app::graph::Endpoint, to: crate::app::graph::Endpoint) -> Result<(NodeId, NodeId), IntentError> {
        use crate::app::graph::Port;
        let invalid = |why: &str| IntentError::Invalid(why.to_string());
        if from.port != Port::EventsOut {
            return Err(invalid("a cable of notes starts at an output of events"));
        }
        if to.port != Port::EventsIn {
            return Err(invalid("a cable of notes ends at an input of events"));
        }
        if from.node == NodeId::MASTER {
            return Err(invalid("the mix has no output of events"));
        }
        if self.source(from.node).is_none() {
            return Err(match self.locate(from.node) {
                Some(_) => invalid("only a source of notes has an output of events"),
                None => IntentError::NotFound(from.node),
            });
        }
        if to.node == NodeId::MASTER {
            return Err(invalid("the mix has no input of events"));
        }
        match self.locate(to.node).ok_or(IntentError::NotFound(to.node))? {
            NodePath::Instrument(inst) if !self.lane_takes_notes(inst) => Err(invalid("that instrument takes no notes: an audio input has no input of events")),
            NodePath::Instrument(_) => Ok((from.node, to.node)),
            _ => Err(invalid("a cable of notes ends at a lane: it is the lanes that have an input of events")),
        }
    }

    /// Whether lane `inst` takes notes: its instrument says (`PluginSlot::takes_notes`), and a lane that has none, which may be given one, does. A lane that is not there does.
    pub fn lane_takes_notes(&self, inst: usize) -> bool {
        self.instruments.get(inst).and_then(|lane| lane.instrument.as_ref()).is_none_or(|slot| slot.takes_notes)
    }

    /// The sources that lane `inst` hears (P4b of `docs/patch-graph.md` §11.6): every source if no event cable ends at it, and otherwise the sources of the nodes that the cables that end at it start at,
    /// as `resolve` says them (the keys are one source, a device is the sources of the open ports that its name stands for, and none while it is not plugged in: a lane that has a cable from it
    /// and no other does not hear the others in the meantime). A lane that is not there hears every source, as it will when it is.
    pub fn lane_sources(&self, inst: usize, mut resolve: impl FnMut(&SourceKind) -> SourceSet) -> SourceSet {
        let Some(lane) = self.instruments.get(inst) else { return SourceSet::ALL };
        let mut cables = self.event_cables.iter().filter(|cable| cable.to == lane.node).peekable();
        if cables.peek().is_none() {
            return SourceSet::ALL;
        }
        cables.filter_map(|cable| self.source(cable.from)).fold(SourceSet::NONE, |heard, source| heard.or(resolve(&source.kind)))
    }

    /// Take the event cable `id` out: what it was. None if there is no such cable.
    pub fn remove_event_cable(&mut self, id: EdgeId) -> Option<EventCable> {
        let at = self.event_cables.binary_search_by_key(&id, |cable| cable.id).ok()?;
        Some(self.event_cables.remove(at))
    }

    /// The event cables that start or end at any of `nodes` (a source, a lane), in the order of their ids: what goes with them when they are taken out.
    pub fn event_cables_at(&self, nodes: &[NodeId]) -> Vec<EdgeId> {
        self.event_cables.iter().filter(|cable| nodes.contains(&cable.from) || nodes.contains(&cable.to)).map(|cable| cable.id).collect()
    }

    /// Take the audio cable `id` out: the place it had and what it was. None if there is no such cable.
    pub fn remove_audio_cable(&mut self, id: EdgeId) -> Option<(usize, AudioCable)> {
        let at = self.cables.binary_search_by_key(&id, |cable| cable.id).ok()?;
        Some((at, self.cables.remove(at)))
    }

    /// The audio cables that start or end at any of `nodes` (a lane, an effect of a lane or of a bus, a group), in the order of their ids: what goes with them when they are taken out.
    pub fn audio_cables_at(&self, nodes: &[NodeId]) -> Vec<EdgeId> {
        let starts = |cable: &AudioCable| match cable.from {
            CableFrom::Instrument(id) | CableFrom::Effect(id) | CableFrom::Group(id) => nodes.contains(&id),
        };
        let ends = |cable: &AudioCable| match cable.to {
            CableTo::Effect(id) | CableTo::EffectSide(id) | CableTo::GroupBus(id) => nodes.contains(&id),
            CableTo::Master => false,
        };
        self.cables.iter().filter(|cable| starts(cable) || ends(cable)).map(|cable| cable.id).collect()
    }

    /// The nodes that have the audio of lane `inst`: its instrument's, which is the lane's, and each of its effects.
    pub fn audio_nodes_of_lane(&self, inst: usize) -> Vec<NodeId> {
        self.instruments.get(inst).map_or_else(Vec::new, |lane| std::iter::once(lane.node).chain(lane.effects.iter().map(|effect| effect.node)).collect())
    }

    /// The nodes that have the audio of group `group`: its own, which is its bus and its output, and each of its bus effects.
    pub fn audio_nodes_of_group(&self, group: usize) -> Vec<NodeId> {
        self.groups.get(group).map_or_else(Vec::new, |bus| std::iter::once(bus.node).chain(bus.effects.iter().map(|effect| effect.node)).collect())
    }

    /// Whether a cable, of any kind, has the id: the modulation cables, the audio cables and the event cables are one series.
    pub fn edge_in_use(&self, id: EdgeId) -> bool {
        self.locate_edge(id).is_some() || self.audio_cable(id).is_some() || self.event_cables.iter().any(|cable| cable.id == id)
    }

    /// The ends that a cable from the port `from` to the port `to` of the graph has: an instrument's, an effect's or a group's `AudioOut` to an effect's, a group's or the master's
    /// `AudioIn`. The reason, if they are not that: a node that is not there is not found, and a port or a node of another kind is invalid.
    pub fn cable_ends(&self, from: crate::app::graph::Endpoint, to: crate::app::graph::Endpoint) -> Result<(CableFrom, CableTo), IntentError> {
        use crate::app::graph::Port;
        let invalid = |why: &str| IntentError::Invalid(why.to_string());
        if from.port != Port::AudioOut {
            return Err(invalid("an audio cable starts at an output of audio"));
        }
        if !matches!(to.port, Port::AudioIn | Port::AudioInSide) {
            return Err(invalid("an audio cable ends at an input of audio"));
        }
        let start = if from.node == NodeId::MASTER {
            return Err(invalid("the mix has no output of audio"));
        } else {
            match self.locate(from.node).ok_or(IntentError::NotFound(from.node))? {
                NodePath::Instrument(_) => CableFrom::Instrument(from.node),
                NodePath::Effect { .. } | NodePath::GroupEffect { .. } => CableFrom::Effect(from.node),
                NodePath::Group(_) => CableFrom::Group(from.node),
                NodePath::Modulator { .. } => return Err(invalid("a modulator has no output of audio")),
                NodePath::Source(_) => return Err(invalid("a source of notes has no output of audio: it has an output of events")),
                NodePath::Module(_) => return Err(invalid("a module has no output of audio of its own: cables start at the plugins in it")),
            }
        };
        let side = to.port == Port::AudioInSide;
        let end = if to.node == NodeId::MASTER {
            if side {
                return Err(invalid("the mix has no second input"));
            }
            CableTo::Master
        } else {
            match self.locate(to.node).ok_or(IntentError::NotFound(to.node))? {
                NodePath::Effect { .. } | NodePath::GroupEffect { .. } if side => {
                    if self.plugin_slot(to.node).is_some_and(|slot| slot.side.is_some()) {
                        CableTo::EffectSide(to.node)
                    } else {
                        return Err(invalid("that effect has no second input"));
                    }
                }
                NodePath::Effect { .. } | NodePath::GroupEffect { .. } => CableTo::Effect(to.node),
                NodePath::Group(_) if side => return Err(invalid("a group has one input, its bus")),
                NodePath::Group(_) => CableTo::GroupBus(to.node),
                NodePath::Instrument(_) => return Err(invalid("a lane has no input of audio: a cable ends at an effect, a group or the mix")),
                NodePath::Modulator { .. } => return Err(invalid("a modulator has no input of audio")),
                NodePath::Source(_) => return Err(invalid("a source of notes has no input of audio")),
                NodePath::Module(_) => return Err(invalid("a module has no input of audio of its own: cables end at the plugins in it")),
            }
        };
        Ok((start, end))
    }

    /// The `bpm` that a save writes: none when a load would come to this tempo anyway (`tempo_of_patterns`), so that the files of sessions that never
    /// set one are as they were.
    pub fn bpm_to_save(&self) -> Option<f32> {
        (self.bpm != tempo_of_patterns(&self.instruments)).then_some(self.bpm)
    }

    /// The `[master]` table that a save writes: none when the master is what it is without one (a volume of 1, the limiter on, a ceiling of -1 dB), and else only the keys that are not, so that a
    /// session that never touched it is the file it was.
    pub fn master_to_save(&self) -> Option<crate::session::MasterConfig> {
        let master = self.master;
        let config = crate::session::MasterConfig {
            volume: (master.volume != 1.0).then_some(master.volume),
            limiter: (!master.limiter).then_some(false),
            ceiling: (master.ceiling != DEFAULT_CEILING).then_some(master.ceiling),
        };
        (config != crate::session::MasterConfig::default()).then_some(config)
    }

    /// Where the master mix is drawn, if it has been placed.
    pub fn master_ui(&self) -> Option<NodeUi> {
        self.ui.get(&NodeId::MASTER).copied()
    }

    /// A `NodeId` no other node has had.
    pub fn alloc_node(&mut self) -> NodeId {
        self.ids.alloc()
    }

    /// An `EdgeId` no other modulation edge has had.
    pub fn alloc_edge(&mut self) -> EdgeId {
        self.ids.alloc_edge()
    }

    /// Every node and where it sits: the groups with their bus effects, then the lanes with
    /// their effects, the sources of notes, the modulators and the modules. (A lane's instrument plugin
    /// shares the lane's id.)
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, NodePath)> + '_ {
        let groups = self.groups.iter().enumerate().flat_map(|(group, g)| {
            std::iter::once((g.node, NodePath::Group(group)))
                .chain(g.effects.iter().enumerate().map(move |(index, fx)| {
                    (fx.node, NodePath::GroupEffect { group, index })
                }))
        });
        let lanes = self.instruments.iter().enumerate().flat_map(|(inst, lane)| {
            std::iter::once((lane.node, NodePath::Instrument(inst)))
                .chain(lane.effects.iter().enumerate().map(move |(index, fx)| {
                    (fx.node, NodePath::Effect { inst, index })
                }))
        });
        let sources = self.sources.iter().enumerate().map(|(index, source)| (source.node, NodePath::Source(index)));
        let free = self.free_modulators.iter().enumerate().map(|(index, m)| (m.node, NodePath::Modulator { at: ModulatorLocation::Free, index }));
        let modules = self.modules.iter().enumerate().map(|(index, module)| (module.node, NodePath::Module(index)));
        groups.chain(lanes).chain(sources).chain(free).chain(modules)
    }

    /// Where the node `id` sits, if it is still in the session.
    pub fn locate(&self, id: NodeId) -> Option<NodePath> {
        self.nodes().find(|(node, _)| *node == id).map(|(_, path)| path)
    }

    /// Where the modulation cable `id` is: the modulator's rack, the modulator's
    /// position in it and the target's in the modulator. None if there is no such
    /// cable (any more).
    pub fn locate_edge(&self, id: EdgeId) -> Option<(ModulatorLocation, usize, usize)> {
        self.free_modulators.iter().enumerate().find_map(|(index, m)| {
            m.targets.iter().position(|t| t.edge == id).map(|target| (ModulatorLocation::Free, index, target))
        })
    }

    /// Check that every node has an id of its own, that ids were allocated here,
    /// that a lane's instrument plugin carries the lane's id, and that the
    /// telemetry slots are in order: every lane and group has a meter (two slots),
    /// every modulator an output slot and every target an effective-value slot
    /// (what it drives is a parameter or a setting of a modulator), all different,
    /// allocated here, and none the engine's own.
    #[cfg(test)]
    pub fn check_ids(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for (id, path) in self.nodes() {
            if id.0 >= self.ids.next {
                return Err(format!("{path:?} has {id:?}, which was never allocated"));
            }
            if !seen.insert(id) {
                return Err(format!("{path:?} has {id:?}, which another node has too"));
            }
        }
        for (inst, lane) in self.instruments.iter().enumerate() {
            if let Some(slot) = &lane.instrument {
                if slot.node != lane.node {
                    return Err(format!("lane {inst} is {:?}, its instrument {:?}", lane.node, slot.node));
                }
            }
        }
        // The modules: each has a member, each member is a lane or a modulator that is there, and a node is in one module at most.
        let mut in_a_module = std::collections::HashSet::new();
        for module in &self.modules {
            if module.members.is_empty() {
                return Err(format!("{:?} is a module that has no member", module.node));
            }
            for member in &module.members {
                if !matches!(self.locate(*member), Some(NodePath::Instrument(_) | NodePath::Modulator { .. })) {
                    return Err(format!("{:?} has {member:?}, which is neither a lane nor a modulator that is there", module.node));
                }
                if !in_a_module.insert(*member) {
                    return Err(format!("{member:?} is in two modules"));
                }
            }
        }

        let racks = std::iter::once(("the modulators".to_string(), &self.free_modulators));
        let mut edges = std::collections::HashSet::new();
        let mut taken = std::collections::HashSet::new();
        let mut claim = |slot: Option<Slot>, whose: String| -> Result<(), String> {
            let Some(slot) = slot else { return Err(format!("{whose} has no telemetry slot")) };
            if slot == crate::telemetry::MASTER.peak || slot == crate::telemetry::MASTER.clips {
                return Err(format!("{whose} has {slot:?}, which is the master meter's"));
            }
            if slot.index() >= self.slots.high_water() {
                return Err(format!("{whose} has {slot:?}, which was never allocated"));
            }
            if !taken.insert(slot) {
                return Err(format!("{whose} has {slot:?}, which another node has too"));
            }
            Ok(())
        };
        for (g, group) in self.groups.iter().enumerate() {
            let meter = group.meter;
            claim(meter.map(|m| m.peak), format!("the meter of group {g}"))?;
            claim(meter.map(|m| m.clips), format!("the meter of group {g}"))?;
        }
        for (i, lane) in self.instruments.iter().enumerate() {
            let meter = lane.meter;
            claim(meter.map(|m| m.peak), format!("the meter of lane {i}"))?;
            claim(meter.map(|m| m.clips), format!("the meter of lane {i}"))?;
        }
        for (rack, modulators) in racks {
            for (m, modulator) in modulators.iter().enumerate() {
                claim(modulator.output, format!("modulator {m} of {rack}"))?;
                if matches!(modulator.source, ModSourceSlot::Steps { .. }) {
                    claim(modulator.position, format!("the position of modulator {m} of {rack}"))?;
                } else if modulator.position.is_some() {
                    return Err(format!("modulator {m} of {rack} is not a step sequencer and has a position"));
                }
                // The ear is a node that was made (a lane that is gone is every event).
                match modulator.ear {
                    Some(ear) if ear.0 >= self.ids.next => return Err(format!("modulator {m} of {rack} hears {ear:?}, which was never allocated")),
                    _ => {}
                }
                for (t, target) in modulator.targets.iter().enumerate() {
                    let whose = format!("target {t} of modulator {m} of {rack}");
                    if target.edge.0 >= self.ids.next_edge {
                        return Err(format!("{whose} has {:?}, which was never allocated", target.edge));
                    }
                    if !edges.insert(target.edge) {
                        return Err(format!("{whose} has {:?}, which another edge has too", target.edge));
                    }
                    claim(target.effective, whose)?;
                }
            }
        }
        // The audio cables: their ids are from the series that the modulation cables have, and each is at nodes that are there, and none makes the audio go round.
        let (lanes, groups) = self.wiring();
        let drawn = || self.cables.iter().map(|cable| (cable.id, cable.from, cable.to));
        for cable in &self.cables {
            let whose = format!("audio cable {:?}", cable.id);
            if cable.id.0 >= self.ids.next_edge {
                return Err(format!("{whose} was never allocated"));
            }
            if !edges.insert(cable.id) {
                return Err(format!("{whose} has an id which another edge has too"));
            }
            if let Some(problem) = crate::plugin::chain::cable_problem(&lanes, &groups, drawn(), (cable.id, cable.from, cable.to), crate::plugin::chain::CHAIN_CHANNELS) {
                return Err(format!("{whose}: {problem}"));
            }
        }
        // The sources: the keys once at most, and a device once for a name.
        if self.sources.iter().filter(|source| source.kind == SourceKind::Keys).count() > 1 {
            return Err("the keys are a source more than once".to_string());
        }
        for (at, source) in self.sources.iter().enumerate() {
            if let SourceKind::Device(name) = &source.kind {
                if self.sources[..at].iter().any(|earlier| earlier.kind == source.kind) {
                    return Err(format!("the device {name:?} is a source more than once"));
                }
            }
        }
        // The event cables: ids from the same series, each from a source to a lane, none twice between the same two.
        for (at, cable) in self.event_cables.iter().enumerate() {
            let whose = format!("event cable {:?}", cable.id);
            if cable.id.0 >= self.ids.next_edge {
                return Err(format!("{whose} was never allocated"));
            }
            if !edges.insert(cable.id) {
                return Err(format!("{whose} has an id which another edge has too"));
            }
            if !self.sources.iter().any(|source| source.node == cable.from) {
                return Err(format!("{whose} starts at {:?}, which is not a source", cable.from));
            }
            if !self.instruments.iter().any(|lane| lane.node == cable.to) {
                return Err(format!("{whose} ends at {:?}, which is not a lane", cable.to));
            }
            if self.event_cables[..at].iter().any(|earlier| (earlier.from, earlier.to) == (cable.from, cable.to)) {
                return Err(format!("{whose} is between two nodes that another event cable is between"));
            }
            if at > 0 && self.event_cables[at - 1].id >= cable.id {
                return Err(format!("{whose} is not after the one before it"));
            }
        }
        Ok(())
    }

    /// The modulator rack at `at`, if it exists.
    pub fn modulators_mut(&mut self, at: ModulatorLocation) -> Option<&mut Vec<ModulatorSlot>> {
        match at {
            ModulatorLocation::Free => Some(&mut self.free_modulators),
        }
    }

    /// The modulator rack at `at`, if it exists.
    pub fn modulators(&self, at: ModulatorLocation) -> Option<&Vec<ModulatorSlot>> {
        match at {
            ModulatorLocation::Free => Some(&self.free_modulators),
        }
    }

    /// Take the cable `edge` out of the session, and with it every cable that drives the
    /// depth of a cable that goes: those, and the ones that drive theirs in turn. (A cable
    /// that is left driving the depth of one that is gone would, being addressed by its place,
    /// drive the next one's.) The cables of the same modulator, and of the modulators that
    /// drive their depths, are told the places their neighbours have after, as the audio
    /// thread does it (`RemoveModTarget`).
    ///
    /// The removals, in the order they were made: the rack, the modulator in it, and the
    /// cable's place in the modulator *as it was when its turn came*, so that the audio thread
    /// can be sent the same commands in the same order, and the cable itself with what it was
    /// centred on, so that it can be put back (`insert_target_at`, in the reverse order). A cable
    /// goes after the cables that drive its depth. `None`, and nothing changed, if there is no such cable.
    pub fn remove_edge(&mut self, edge: EdgeId) -> Option<Vec<RemovedCable>> {
        let going = self.cascade_of(edge)?;
        let mut removed = Vec::new();
        for gone in going.into_iter().rev() {
            let (at, mod_index, target_index) = self.locate_edge(gone)?;
            removed.push(self.remove_target_at(at, mod_index, target_index)?);
        }
        Some(removed)
    }

    /// What a cable that drives `aim` is centred on: what the user has set what it drives to (a plugin's parameter, a modulator's rate or stage, the depth of a cable). `None` if
    /// what it drives is not there.
    pub fn centre_of(&self, aim: Driven) -> Option<f32> {
        match aim {
            Driven::Param { plugin, index } => {
                self.plugin_slot(plugin)?.params.iter().find(|row| row.index == index && !matches!(row.kind, ParamKind::Separator)).map(|row| row.value)
            }
            Driven::Setting { modulator, field } => {
                let NodePath::Modulator { at, index } = self.locate(modulator)? else { return None };
                self.modulators(at)?.get(index)?.source.setting(field_of_setting(field))
            }
            Driven::Depth(edge) => {
                let (at, modulator, target) = self.locate_edge(edge)?;
                Some(self.modulators(at)?.get(modulator)?.targets.get(target)?.depth)
            }
            Driven::Gain { node, gain } => self.gain_of(node, gain),
        }
    }

    /// What the user has set the host-side value `gain` of node `node` to (step 97): the volume of a lane that has an instrument (one that has none has no sound to turn down) or of a group, the
    /// mix of an effect of a lane's chain or of a group's bus, the pan of a group. `None` for any other pair, and for a node that is not there.
    pub fn gain_of(&self, node: NodeId, gain: ModGain) -> Option<f32> {
        match (self.locate(node)?, gain) {
            (NodePath::Instrument(inst), ModGain::Volume) => {
                let lane = self.instruments.get(inst)?;
                lane.instrument.as_ref().map(|_| lane.volume)
            }
            (NodePath::Effect { inst, index }, ModGain::Mix) => Some(self.instruments.get(inst)?.effects.get(index)?.mix),
            (NodePath::GroupEffect { group, index }, ModGain::Mix) => Some(self.groups.get(group)?.effects.get(index)?.mix),
            (NodePath::Group(group), ModGain::Volume) => Some(self.groups.get(group)?.volume),
            (NodePath::Group(group), ModGain::Pan) => Some(self.groups.get(group)?.pan),
            _ => None,
        }
    }

    /// The plugin that sits in node `node`: a lane's instrument, or an effect of a lane's chain or of a group's bus. `None` for any other node, and for the node of a lane that has no
    /// instrument.
    pub fn plugin_slot(&self, node: NodeId) -> Option<&PluginSlot> {
        match self.locate(node)? {
            NodePath::Instrument(inst) => self.instruments.get(inst)?.instrument.as_ref(),
            NodePath::Effect { inst, index } => self.instruments.get(inst)?.effects.get(index),
            NodePath::GroupEffect { group, index } => self.groups.get(group)?.effects.get(index),
            NodePath::Group(_) | NodePath::Modulator { .. } | NodePath::Source(_) | NodePath::Module(_) => None,
        }
    }

    /// The cables that go when cable `edge` is taken out: it, and then, in turn, those that drive the
    /// depth of one that goes (which may be one that is there already: they can drive each other's).
    /// `None` if there is no such cable.
    pub fn cascade_of(&self, edge: EdgeId) -> Option<Vec<EdgeId>> {
        self.locate_edge(edge)?;
        let mut going = vec![edge];
        let mut next = 0;
        while let Some(&gone) = going.get(next) {
            next += 1;
            for driver in self.depth_cables_of(gone) {
                if !going.contains(&driver) {
                    going.push(driver);
                }
            }
        }
        Some(going)
    }

    /// How the file and the terminal's popups name what the cable `aim`, of the modulator at `source` in the rack at `at`, drives: the plugin by its node, a sibling's field or a cable's
    /// depth by the sibling's place (a cable's place in its modulator), where the model keeps the node and the port (`Driven`). This is the one place that turns one into the other,
    /// and it says what a rack reaches: any plugin of the session (the instrument of a lane that has one, an effect of a lane's chain, an effect of a group's bus) and the settings and
    /// the cable depths of the other modulators. `None` if what the cable drives is not there, or is the modulator itself (a cable like that does nothing).
    pub fn kind_of(&self, at: ModulatorLocation, source: usize, aim: Driven) -> Option<ModTargetKind> {
        use ModTargetKind as K;
        let ModulatorLocation::Free = at;
        match aim {
            Driven::Param { plugin, index: param_index } => match self.locate(plugin)? {
                NodePath::Instrument(inst) if self.instruments[inst].instrument.is_some() => Some(K::FreeParam { plugin, param_index }),
                NodePath::Effect { .. } | NodePath::GroupEffect { .. } => Some(K::FreeParam { plugin, param_index }),
                _ => None,
            },
            Driven::Setting { modulator, field } => {
                let NodePath::Modulator { index, .. } = self.locate(modulator)? else { return None };
                (index != source).then(|| sibling_kind(index, place_field_of(field)))
            }
            Driven::Depth(edge) => {
                let (_, index, target) = self.locate_edge(edge)?;
                (index != source).then(|| sibling_kind(index, CrossModField::Depth(target)))
            }
            Driven::Gain { node, gain } => self.gain_of(node, gain).map(|_| K::Gain { node, gain }),
        }
    }

    /// How the file and the popups name what the cable at place `target` of the modulator at `mod_index` in the rack at `at` drives (`kind_of`). `None` if there is no such cable, or
    /// what it drives is not there.
    pub fn kind_of_target(&self, at: ModulatorLocation, mod_index: usize, target: usize) -> Option<ModTargetKind> {
        self.kind_of(at, mod_index, self.modulators(at)?.get(mod_index)?.targets.get(target)?.aim)
    }

    /// What the cable `edge` drives.
    pub fn driven_by_edge(&self, edge: EdgeId) -> Option<Driven> {
        let (at, source, target) = self.locate_edge(edge)?;
        Some(self.modulators(at)?.get(source)?.targets.get(target)?.aim)
    }

    /// Whether any cable in the session drives `driven`.
    pub fn is_driven(&self, driven: Driven) -> bool {
        self.racks().any(|(_, rack)| rack.iter().any(|m| m.targets.iter().any(|t| t.aim == driven)))
    }

    /// Every modulator rack there is, with where it is.
    pub(super) fn racks(&self) -> impl Iterator<Item = (ModulatorLocation, &Vec<ModulatorSlot>)> {
        std::iter::once((ModulatorLocation::Free, &self.free_modulators))
    }

    /// Put the lanes at `a` and `b` in each other's places, each with everything it has (its
    /// plugins and effects, volume, range, group and pattern: the tree moves whole nodes), as the
    /// audio thread's `SwapInstruments` does. (A modulator's cables name their plugins by node: they
    /// go with the lanes.) False, and nothing changed, if either is not there or they are one.
    pub fn swap_lanes(&mut self, a: usize, b: usize) -> bool {
        let count = self.instruments.len();
        if a == b || a >= count || b >= count {
            return false;
        }
        self.instruments.swap(a, b);
        true
    }

    /// The cables of other modulators that would be left pointing at nothing if modulator `index`
    /// of the rack at `at` were taken out: those of its siblings that drive its rate, its stages or
    /// the depth of its cables. (Its own cables go with it. The cables that drive the depth of any
    /// of these go with *them*: `remove_edge`.)
    pub fn cables_into(&self, at: ModulatorLocation, index: usize) -> Vec<EdgeId> {
        let Some(rack) = self.modulators(at) else { return Vec::new() };
        let Some(modulator) = rack.get(index) else { return Vec::new() };
        let node = modulator.node;
        let own: Vec<EdgeId> = modulator.targets.iter().map(|t| t.edge).collect();
        let addresses_it = |t: &&ModTargetSlot| {
            matches!(t.aim, Driven::Setting { modulator, .. } if modulator == node) || matches!(t.aim, Driven::Depth(edge) if own.contains(&edge))
        };
        let siblings = rack.iter().enumerate().filter(|(i, _)| *i != index).flat_map(|(_, m)| &m.targets);
        siblings.filter(addresses_it).map(|t| t.edge).collect()
    }

    /// The cables that drive a parameter of the plugin in node `plugin`, an instrument, an effect or a bus
    /// effect, each modulator's by place.
    pub fn cables_into_plugin(&self, plugin: NodeId) -> Vec<EdgeId> {
        self.racks()
            .flat_map(|(_, rack)| rack.iter().flat_map(|m| &m.targets))
            .filter(|t| matches!(t.aim, Driven::Param { plugin: driven, .. } if driven == plugin))
            .map(|t| t.edge)
            .collect()
    }

    /// The cables that drive a host-side value of the node `node` (a lane's volume, an effect's mix, a group's volume or pan): what goes with the node, or with the instrument that
    /// gives a lane's volume its meaning (a lane that has none has no volume to drive).
    pub fn cables_into_gains_of(&self, node: NodeId) -> Vec<EdgeId> {
        self.racks()
            .flat_map(|(_, rack)| rack.iter().flat_map(|m| &m.targets))
            .filter(|t| matches!(t.aim, Driven::Gain { node: driven, .. } if driven == node))
            .map(|t| t.edge)
            .collect()
    }

    /// The cables of the modulators that drive a parameter of a plugin in any of `nodes`, or a volume, a mix or a pan of one: the plugins of a lane or a group that goes out, and the lane's
    /// and the group's own. A modulator is in neither the lane nor the group and stays, so what it drives there is taken out first, each by itself.
    pub fn free_cables_into(&self, nodes: &[NodeId]) -> Vec<EdgeId> {
        let drives_one = |t: &&ModTargetSlot| match t.aim {
            Driven::Param { plugin, .. } => nodes.contains(&plugin),
            Driven::Gain { node, .. } => nodes.contains(&node),
            Driven::Setting { .. } | Driven::Depth(_) => false,
        };
        self.free_modulators.iter().flat_map(|m| &m.targets).filter(drives_one).map(|t| t.edge).collect()
    }

    /// The cables that drive the depth of cable `edge`, in the rack of its modulator.
    fn depth_cables_of(&self, edge: EdgeId) -> Vec<EdgeId> {
        let Some((at, ..)) = self.locate_edge(edge) else { return Vec::new() };
        let Some(rack) = self.modulators(at) else { return Vec::new() };
        rack.iter().flat_map(|m| &m.targets).filter(|t| t.aim == Driven::Depth(edge)).map(|t| t.edge).collect()
    }

    /// Take cable `target` out of modulator `mod_index` in the rack at `at`. The cable, and what it was
    /// centred on; `None`, and nothing changed, if there is no such cable.
    fn remove_target_at(&mut self, at: ModulatorLocation, mod_index: usize, target: usize) -> Option<RemovedCable> {
        // (What it drives is read while the cable is there: another cable that drives its depth is taken out first.)
        let cable = self.modulators(at)?.get(mod_index)?.targets.get(target)?;
        let (centre, still_driven) = (self.centre_of(cable.aim), self.is_driven(Driven::Depth(cable.edge)));
        let slot = self.modulators_mut(at)?[mod_index].targets.remove(target);
        let base_value = centre.unwrap_or(slot.param_min);
        Some(RemovedCable { at, mod_index, target_index: target, slot, base_value, still_driven })
    }

    /// Put a cable into modulator `mod_index` of the rack at `at`, at place `target_index` (the last, for a place past the end): what
    /// `remove_edge` took out, put back (the cables of the removal in the reverse order, each at the place it had when its turn came). The
    /// place it went to; `None`, and nothing changed, if there is no such modulator.
    pub fn insert_target_at(&mut self, at: ModulatorLocation, mod_index: usize, target_index: usize, slot: ModTargetSlot) -> Option<usize> {
        let rack = self.modulators_mut(at)?;
        let place = target_index.min(rack.get(mod_index)?.targets.len());
        rack[mod_index].targets.insert(place, slot);
        Some(place)
    }

    /// Take the modulator at `index` out of the rack at `at`, with its own cables, as the audio thread's `RemoveModulator` and `RemoveGroupModulator`
    /// do. (The controller takes out the cables that address it first, so none is left; one that is, goes with it.) `None`, and nothing changed,
    /// if there is no such modulator.
    /// The node of the lane or the group that `modulator` hears the notes of, if it names one that is there: a node that is neither (it was taken out: an undo brings it back under the same
    /// id) is every event, as it is in the audio thread (`Modulator::ear`).
    pub fn ear_of(&self, modulator: &ModulatorSlot) -> Option<NodeId> {
        modulator.ear.filter(|node| matches!(self.locate(*node), Some(NodePath::Instrument(_) | NodePath::Group(_))))
    }

    /// The place of the session that names the lane or the group `node`, as the file writes what a modulator hears: `instrument[0]` or `group[0]`.
    pub fn heard_place(&self, node: NodeId) -> Option<crate::session::CablePlace> {
        match self.locate(node)? {
            NodePath::Instrument(lane) => Some(crate::session::CablePlace::Instrument(lane)),
            NodePath::Group(group) => Some(crate::session::CablePlace::Group(group)),
            _ => None,
        }
    }

    pub fn remove_modulator_at(&mut self, at: ModulatorLocation, index: usize) -> Option<ModulatorSlot> {
        let rack = self.modulators_mut(at)?;
        if index >= rack.len() {
            return None;
        }
        let removed = rack.remove(index);
        self.prune_dangling_cables();
        Some(removed)
    }

    /// Put a modulator, with its own cables, into the rack at `at` at place `index` (the last, for a place past the end): what `remove_modulator_at`
    /// took out, put back. The place it went to; `None`, and nothing changed, if there is no such rack.
    pub fn insert_modulator_at(&mut self, at: ModulatorLocation, index: usize, modulator: ModulatorSlot) -> Option<usize> {
        let rack = self.modulators_mut(at)?;
        let place = index.min(rack.len());
        rack.insert(place, modulator);
        Some(place)
    }

    /// Take out the cables that drive nothing: what they drove is not there any more, and those that drive the depth of such a cable in turn. The controller takes the cables out first, each by `take_out`, which is what the audio thread is told and
    /// the history written down by; this is for what is left when an edit is made with no controller round it, so that no cable in the model points at nothing.
    fn prune_dangling_cables(&mut self) {
        loop {
            let mut dangling: Vec<(ModulatorLocation, usize, usize)> = Vec::new();
            for (at, rack) in self.racks() {
                for (index, modulator) in rack.iter().enumerate() {
                    for (target, cable) in modulator.targets.iter().enumerate() {
                        if self.kind_of(at, index, cable.aim).is_none() {
                            dangling.push((at, index, target));
                        }
                    }
                }
            }
            if dangling.is_empty() {
                return;
            }
            // (The last first: the places of a modulator's cables do not move under those that are left to go.)
            for (at, index, target) in dangling.into_iter().rev() {
                if let Some(rack) = self.modulators_mut(at) {
                    rack[index].targets.remove(target);
                }
            }
        }
    }

    /// The plugin parameters a modulator in the rack at `at` can drive, in the order a person is offered them: any plugin of the session, the instrument of each lane that has one and
    /// then its effects, lane by lane, and then the effects of each group's bus. Nothing else is a parameter a modulator can drive (the fields of other modulators are another kind of
    /// target).
    pub fn param_targets(&self, at: ModulatorLocation) -> Vec<ParamTarget> {
        let ModulatorLocation::Free = at;
        let mut targets = Vec::new();
        for lane in &self.instruments {
            push_chain(&mut targets, lane, |slot, param_index| {
                let plugin = if slot == 0 { lane.node } else { lane.effects[slot - 1].node };
                ModTargetKind::FreeParam { plugin, param_index }
            });
        }
        for effect in self.groups.iter().flat_map(|bus| &bus.effects) {
            push_params(&mut targets, effect, 0, |param_index| ModTargetKind::FreeParam { plugin: effect.node, param_index });
        }
        targets
    }

    /// The host-side values that a modulator in the rack at `at` can drive (step 97), in the order a person is offered them, after the plugin parameters: lane by lane, the volume of the lane
    /// (if it has an instrument) and the mix of each effect of its chain, and then group by group, the volume and the pan of the group and the mix of each of its bus effects.
    pub fn gain_targets(&self, at: ModulatorLocation) -> Vec<GainTarget> {
        let ModulatorLocation::Free = at;
        let mut targets = Vec::new();
        let mut push = |node: NodeId, gain: ModGain, owner_name: String, value: f32| {
            targets.push(GainTarget { node, gain, kind: ModTargetKind::Gain { node, gain }, owner_name, value });
        };
        for lane in &self.instruments {
            if let Some(instrument) = &lane.instrument {
                push(lane.node, ModGain::Volume, instrument.name.clone(), lane.volume);
            }
            for effect in &lane.effects {
                push(effect.node, ModGain::Mix, effect.name.clone(), effect.mix);
            }
        }
        for (index, group) in self.groups.iter().enumerate() {
            let name = group.display_name(index);
            push(group.node, ModGain::Volume, name.clone(), group.volume);
            push(group.node, ModGain::Pan, name, group.pan);
            for effect in &group.effects {
                push(effect.node, ModGain::Mix, effect.name.clone(), effect.mix);
            }
        }
        targets
    }

    /// The fields of other modulators that the modulator at `mod_index` of the rack at `at` can drive: a rate, the stages of an
    /// envelope and the depths of cables (a cable that pulls a parameter, or a field, pulls it by a depth another modulator can
    /// drive), in the order a person is offered them: those of the other modulators of the rack. Never its own (a modulator does not
    /// drive itself). (An index that is not in the rack leaves none out.)
    pub fn field_targets(&self, at: ModulatorLocation, mod_index: usize) -> Vec<FieldTarget> {
        let ModulatorLocation::Free = at;
        let mut targets = Vec::new();
        push_siblings(&mut targets, &self.free_modulators, mod_index);
        targets
    }

    /// The modulators whose settings the modulator at `mod_index` of the rack at `at` can be drawn to, in the order of
    /// `field_targets`: the others of the rack. A modulator in reach may have nothing to drive (a MIDI control that has no cable
    /// has no setting and no depth), and so is not in `field_targets`.
    pub fn modulators_in_reach(&self, at: ModulatorLocation, mod_index: usize) -> Vec<NodeId> {
        let ModulatorLocation::Free = at;
        self.free_modulators.iter().enumerate().filter(|(index, _)| *index != mod_index).map(|(_, modulator)| modulator.node).collect()
    }

    /// Take the effect at `index` out of lane `inst`'s chain, the way the audio
    /// thread's `RemoveEffect` does: the modulator targets on it go with it (the controller
    /// takes them out first). `None` (and no change) if the lane has no such effect.
    pub fn remove_effect(&mut self, inst: usize, index: usize) -> Option<PluginSlot> {
        let lane = self.instruments.get_mut(inst)?;
        if index >= lane.effects.len() {
            return None;
        }
        let removed = lane.effects.remove(index);
        self.prune_dangling_cables();
        Some(removed)
    }

    /// Put a plugin into lane `inst`'s chain at `index` (the last place, for one past the end): what `remove_effect` took out, put back, the way the audio
    /// thread's `InsertEffect` does it. The place it went to; `None`, and nothing changed, if there is no such lane.
    pub fn insert_effect_at(&mut self, inst: usize, index: usize, effect: PluginSlot) -> Option<usize> {
        let lane = self.instruments.get_mut(inst)?;
        let place = index.min(lane.effects.len());
        lane.effects.insert(place, effect);
        Some(place)
    }

    /// Put a plugin into group `group`'s bus at `index` (the last place, for one past the end): what `remove_bus_effect` took out, put back, the way the
    /// audio thread's `InsertGroupEffect` does it. The place it went to; `None`, and nothing changed, if there is no such group.
    pub fn insert_bus_effect_at(&mut self, group: usize, index: usize, effect: PluginSlot) -> Option<usize> {
        let bus = self.groups.get_mut(group)?;
        let place = index.min(bus.effects.len());
        bus.effects.insert(place, effect);
        Some(place)
    }

    /// Take lane `inst` out, the way the audio thread's `RemoveInstrument` does, with everything it has: its
    /// plugins and pattern. `None` (and no change) if there is no such lane.
    pub fn remove_lane(&mut self, inst: usize) -> Option<InstrumentNode> {
        if inst >= self.instruments.len() {
            return None;
        }
        let lane = self.instruments.remove(inst);
        self.prune_dangling_cables();
        Some(lane)
    }

    /// Put a lane back among the lanes at `index` (the last place, for one past the end): what `remove_lane` took out, put back, the way the audio thread's
    /// `InsertInstrument` does it; a group that is not there is no group. The place it went to.
    pub fn insert_lane_at(&mut self, index: usize, mut lane: InstrumentNode) -> usize {
        if lane.group.is_some_and(|group| group >= self.groups.len()) {
            lane.group = None;
        }
        let place = index.min(self.instruments.len());
        self.instruments.insert(place, lane);
        place
    }

    /// Take group `group` out, with its bus effects and its modulators, the way the audio thread's `RemoveGroup`
    /// does: its member lanes are in no group, and the lanes of the groups after it have the index that is one
    /// less. `None` (and no change) if there is no such group.
    pub fn remove_group(&mut self, group: usize) -> Option<GroupNode> {
        if group >= self.groups.len() {
            return None;
        }
        let removed = self.groups.remove(group);
        for lane in &mut self.instruments {
            match lane.group {
                Some(of) if of == group => lane.group = None,
                Some(of) if of > group => lane.group = Some(of - 1),
                _ => {}
            }
        }
        Some(removed)
    }

    /// Put a group back among the groups at `index` (the last place, for one past the end): what `remove_group` took out, put back, the way the audio thread's
    /// `InsertGroup` does it. The lanes that are in the groups from that place on are told the group they are in now (one more, which the removal told them the other
    /// way), and the lanes in `members` (places among the lanes: one that is no lane is nobody's) are its members again. The cables of the group are not here: the
    /// controller takes them out before the group goes and puts them back after it. The place it went to.
    pub fn insert_group_at(&mut self, index: usize, group: GroupNode, members: &[usize]) -> usize {
        let place = index.min(self.groups.len());
        for lane in &mut self.instruments {
            if let Some(of) = lane.group.as_mut().filter(|of| **of >= place) {
                *of += 1;
            }
        }
        self.groups.insert(place, group);
        for &inst in members {
            if let Some(lane) = self.instruments.get_mut(inst) {
                lane.group = Some(place);
            }
        }
        place
    }

    /// Put lane `inst` in group `group`, or in none, the way the audio thread's `SetLaneGroup` does: the group
    /// it leaves has nothing to drive in it any more, and drops its modulators' cables to it (the controller takes
    /// them out first). False (and no change) if there is no such lane or no such group.
    pub fn set_lane_group(&mut self, inst: usize, group: Option<usize>) -> bool {
        if inst >= self.instruments.len() || group.is_some_and(|g| g >= self.groups.len()) {
            return false;
        }
        self.instruments[inst].group = group;
        self.prune_dangling_cables();
        true
    }

    /// Take the bus effect at `index` out of group `group`, the way the audio thread's `RemoveGroupEffect`
    /// does: the group's modulator cables to it go with it (the controller takes them out first). `None` (and
    /// no change) if the group has no such effect.
    pub fn remove_bus_effect(&mut self, group: usize, index: usize) -> Option<PluginSlot> {
        let bus = self.groups.get_mut(group)?;
        if index >= bus.effects.len() {
            return None;
        }
        let removed = bus.effects.remove(index);
        self.prune_dangling_cables();
        Some(removed)
    }

    /// Move the effect at `from` to `to` in lane `inst`'s chain, the way the audio
    /// thread's `ReorderEffect` does. False (and no change) if either index is outside the chain or they
    /// are the same.
    pub fn move_effect(&mut self, inst: usize, from: usize, to: usize) -> bool {
        let Some(lane) = self.instruments.get_mut(inst) else { return false };
        if from >= lane.effects.len() || to >= lane.effects.len() || from == to {
            return false;
        }
        let effect = lane.effects.remove(from);
        lane.effects.insert(to, effect);
        true
    }

    /// Move the effect at `from` to `to` in group `group`'s bus, the way the audio thread's
    /// `ReorderGroupEffect` does. False (and no change) if either index is outside the bus, they are the
    /// same, or the group is not there.
    pub fn move_bus_effect(&mut self, group: usize, from: usize, to: usize) -> bool {
        let Some(bus) = self.groups.get_mut(group) else { return false };
        if from >= bus.effects.len() || to >= bus.effects.len() || from == to {
            return false;
        }
        let effect = bus.effects.remove(from);
        bus.effects.insert(to, effect);
        true
    }
}

/// Default dry/wet mix for a newly added effect. Half-wet suits blend/parallel
/// effects (reverb, delay), the common DAW default for sends.
pub const DEFAULT_EFFECT_MIX: f32 = 0.5;

/// Default dry/wet mix for a freshly added effect, by plugin. Filters and
/// other tone-shaping inserts must be fully wet — at 50% the dry path passes
/// the very frequencies the filter removes, so e.g. a highpass barely changes
/// the sound — and so must a drive, which is an insert too (what it does to
/// the sound is the point, and half of it is a parallel distortion that is a
/// choice, not a default). A drive's latency (the oversampling it makes its
/// shapes with) is no reason now: the host makes the dry sound as late as the
/// wet one, so a drive that is blended is in step.
/// Blend effects (reverb) keep the half-wet default.
pub fn default_effect_mix(id: &str) -> f32 {
    if id == "builtin:filter" || id == "builtin:drive" || id == "builtin:math" {
        1.0
    } else {
        DEFAULT_EFFECT_MIX
    }
}

// ---------------------------------------------------------------------------
// Plugin slot — main-thread mirror of what the audio thread has
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PluginSlot {
    /// The node the plugin sits in: its own for an effect or a bus effect, the
    /// lane's for an instrument.
    pub node: NodeId,
    pub name: String,
    pub format: String,
    #[allow(dead_code)]
    pub id: String,
    #[allow(dead_code)]
    pub is_instrument: bool,
    pub params: Vec<ParamSlot>,
    /// All presets the plugin reports, cached at load time. Used by the
    /// preset selector popup.
    pub presets: Vec<plugin::Preset>,
    /// Name of the currently-loaded preset (if any). Tracked for save
    /// round-trip and display.
    pub current_preset: Option<String>,
    /// Effect dry/wet mix (1.0 = full wet). Only meaningful for effect slots;
    /// instrument slots leave this at 1.0 and don't read it.
    pub mix: f32,
    /// Whether the link into an effect from the stage before it in its chain is there (step 86): `true` for the way a chain is wired by itself, `false` once it is cut (`SetFeed`), when
    /// the effect is fed by the audio cables that end at it and by nothing else. Only meaningful for an effect or a bus effect; an instrument has nothing before it and leaves this `true`.
    /// A session file keeps it as `feed = "none"`; the engine has it by the effect's node (`GraphCommand::SetEffectFeed`), so it goes where its effect goes.
    pub fed: bool,
    /// What the plugin calls its two audio inputs if it has a second one (`Plugin::side_input`): the effect's node then has a second port that cables can end at (step 95). Only meaningful
    /// for an effect or a bus effect; a plugin has it from the moment it is loaded, and it goes where its effect goes.
    pub side: Option<crate::plugin::SideInput>,
    /// Whether the plugin does anything with the notes it is given (`Plugin::takes_notes`, step 99c): `false` for an audio input, whose lane then has no key range, no pattern and no input of events
    /// (the graph says so, and a cable of notes is refused at it). Read from the plugin when it is loaded; it goes where the plugin goes.
    pub takes_notes: bool,
    /// The parameters (`ParamSlot::index`) a front-end shows as knobs on the plugin's node, in the order they were pinned; none pinned
    /// is the first few (`docs/web-ui.md` §4.4). The engine and the TUI do not use it; a session file keeps it as `pinned`, by name.
    pub pinned: Vec<u32>,
}

impl PluginSlot {
    /// Why a parameter of this plugin has no effect now, if it has none (step 95b): the words of what the plugin says of when it has one (`ParameterInfo::used_when`: `only while nothing is patched at B`), when
    /// that does not hold of what the user has set the plugin's other parameters to and of whether a cable of `cables` ends at its second input. It is the value the user set that counts: a choice that a
    /// modulator moves is read where it is set. The terminal dims such a row and says why under the pane; the page's `unusedBecause` is the same rule, read from the graph.
    pub fn unused_because<'a>(&self, param: &'a ParamSlot, cables: &[AudioCable]) -> Option<&'a str> {
        let used = param.used_when.as_ref()?;
        let value_of = |index: u32| self.params.iter().find(|p| p.index == index).map(|p| p.value);
        let side_patched = cables.iter().any(|cable| cable.to == CableTo::EffectSide(self.node));
        (!used.condition.holds(value_of, side_patched)).then_some(used.why.as_str())
    }

    /// The names of the pinned parameters, in order, as a session file keeps them.
    pub fn pinned_names(&self) -> Vec<String> {
        self.pinned.iter().filter_map(|index| self.params.iter().find(|p| p.index == *index)).map(|p| p.name.clone()).collect()
    }

    /// Pin the parameters of these names, in this order, as a session file says them. A name the plugin has no parameter of (or one
    /// that is no control, a separator) is not a pin, and nor is a name that comes twice (the first place counts): each is logged.
    pub fn pin_names(&mut self, names: &[String]) {
        self.pinned.clear();
        for name in names {
            match self.params.iter().find(|p| p.name == *name && !matches!(p.kind, ParamKind::Separator)) {
                Some(param) if self.pinned.contains(&param.index) => log::warn!("{}: '{name}' is pinned twice", self.name),
                Some(param) => self.pinned.push(param.index),
                None => log::warn!("{}: no parameter '{name}' to pin", self.name),
            }
        }
    }
}

#[derive(Debug, Default)]
pub enum ParamKind {
    #[default]
    Float,
    Enum(Vec<String>),
    Separator,
}

#[derive(Debug, Default)]
pub struct ParamSlot {
    pub name: String,
    pub index: u32,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    pub kind: ParamKind,
    /// What the value is measured in, for showing it (`ParameterInfo::unit`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub unit: Option<String>,
    /// The sections the plugin files the parameter under, outermost first
    /// (`ParameterInfo::group`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub group: Vec<String>,
    /// How many steps there are between `min` and `max`; 0 for a continuous
    /// parameter (`ParameterInfo::step_count`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the graph a client gets (`app::graph`), which only the web runtime sends"))]
    pub step_count: u32,
    /// When the parameter has an effect at all, if it has one only sometimes (`ParameterInfo::used_when`): `PluginSlot::unused_because` reads it against the values the user has set.
    pub used_when: Option<plugin::UsedWhen>,
}

// ---------------------------------------------------------------------------
// Lanes and groups
// ---------------------------------------------------------------------------

/// Main-thread mirror of pattern state for an instrument.
#[derive(Debug)]
pub struct PatternState {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    pub recording: bool,
    /// Transpose playback by scale degrees (in-key) instead of semitones.
    pub in_key: bool,
}

impl PatternState {
    /// A pattern that has nothing recorded: what a lane that has none is given when its recording starts or one of its settings is set, at the tempo `bpm`.
    pub fn empty(bpm: f32) -> Self {
        PatternState { bpm, length_beats: 4.0, looping: true, base_note: None, events: Vec::new(), enabled: false, recording: false, in_key: false }
    }

    /// What a front-end is told of it: its settings and how many notes it has (the notes themselves are not sent).
    pub fn info(&self) -> crate::app::graph::PatternInfo {
        crate::app::graph::PatternInfo {
            recording: self.recording,
            enabled: self.enabled,
            looping: self.looping,
            in_key: self.in_key,
            length_beats: self.length_beats,
            bpm: self.bpm,
            notes: self.events.iter().filter(|event| event.1 == 0x90).count() as u32,
        }
    }
}

#[derive(Debug)]
pub struct InstrumentNode {
    pub node: NodeId,
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    /// Host-side output gain, applied before effects (1.0 = unity).
    pub volume: f32,
    /// Muted: never heard (a mute wins over a solo); the lane's notes, modulators and pattern go on.
    pub muted: bool,
    /// Soloed: while anything is, only what is soloed is heard (and the group a soloed lane is in).
    pub solo: bool,
    /// Where the lane's end goes: to the bus of its group or to the mix (`Route::Out`), or nowhere but its audio cables (`Route::None`).
    pub route: Route,
    pub instrument: Option<PluginSlot>,
    pub effects: Vec<PluginSlot>,
    pub pattern: Option<PatternState>,
    /// Carried through from session config; not editable in the UI.
    pub pitch_bend_range: f64,
    pub remap: std::collections::HashMap<String, crate::session::RemapTarget>,
    /// Submix group membership: index into the session's groups, or None.
    pub group: Option<usize>,
    /// Where the audio thread publishes the lane's output level (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub meter: Option<MeterSlots>,
}

impl InstrumentNode {
    /// The lane has no note remap any more, nor the pitch bend range that went with it: the audio thread drops the remapper
    /// when the instrument it was for goes (a swap, a clear), and the model says what the audio thread has.
    pub fn forget_remap(&mut self) {
        self.remap.clear();
        self.pitch_bend_range = 2.0;
    }
}

/// A submix group: its members are the instruments whose `group` points here;
/// it sums them and runs its own effect chain + volume.
#[derive(Debug)]
pub struct GroupNode {
    pub node: NodeId,
    pub name: Option<String>,
    pub volume: f32,
    /// Stereo balance pan for the group bus (-1 left … 0 center … +1 right).
    pub pan: f32,
    /// Muted: the bus is never heard (a mute wins over a solo); the members and the bus effects go on.
    pub muted: bool,
    /// Soloed: while anything is, only what is soloed is heard, and every member of this group.
    pub solo: bool,
    /// Where the group's end goes: to the mix (`Route::Out`), or nowhere but its audio cables (`Route::None`).
    pub route: Route,
    pub effects: Vec<PluginSlot>,
    /// Where the audio thread publishes the bus's output level (telemetry);
    /// `None` if the slots had run out.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "read by the telemetry layout a client gets (`app::layout`), which only the web runtime sends"))]
    pub meter: Option<MeterSlots>,
}

impl GroupNode {
    /// What the group is called: its name, or "Group N" counting from one by its
    /// position `index` among the session's groups.
    pub fn display_name(&self, index: usize) -> String {
        self.name.clone().unwrap_or_else(|| format!("Group {}", index + 1))
    }
}

#[derive(Debug, Clone)]
pub enum ModSourceSlot {
    /// An LFO: its waveform and rate, whether a note starts it again at the start of its wave, how many seconds it takes to come in after a note (0 for none) and the note length of the tempo
    /// that a cycle of it is instead of its rate, as a place in `SYNC_DIVISIONS` from 1 (0 for none: it goes at its rate, which is kept for when it does).
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
        retrigger: bool,
        fade: f32,
        sync: usize,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// A MIDI-learned control (CC or pitch bend). `source` is None while
    /// `learning` (armed for the next incoming control).
    MidiLearn {
        source: Option<crate::plugin::chain::MidiBindSource>,
        learning: bool,
    },
    /// A number that the person sets, 0 to 1: a knob that drives what has a cable from it. It can be called something (`name`: a person's word for it, "Brightness"; empty is the
    /// default, "Macro"), and a box that stands for a module that it is in shows its knob unless it is told not to (`expose`, on to begin with, which is what a macro is for). Neither is
    /// told to the audio thread: it has nothing of a macro but its value.
    Macro {
        value: f32,
        name: String,
        expose: bool,
    },
    /// A step sequencer: the values it goes through, -1 to 1 (the first `length` of them are in use, the others are kept for when it is longer), how many
    /// steps it goes through a second, how much of each step is spent gliding to the next one's value, whether a note starts it again at the first step, and the note length of the tempo
    /// that a step of it is instead of its rate (a place in `SYNC_DIVISIONS` from 1, or 0 for none).
    Steps {
        values: [f32; crate::plugin::chain::MAX_STEPS],
        length: usize,
        rate: f32,
        glide: f32,
        retrigger: bool,
        sync: usize,
    },
}

impl ModSourceSlot {
    /// What a modulator added without further ado starts as: a 1 Hz sine.
    pub fn new_lfo() -> Self {
        ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 1.0, retrigger: false, fade: 0.0, sync: 0 }
    }

    /// An envelope as the modulate popup makes it: 10 ms attack, 0.3 s decay,
    /// sustain 0.7, 0.5 s release.
    pub fn new_envelope() -> Self {
        ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }
    }

    /// A control that is waiting for the next one that comes in to bind to.
    pub fn new_midi_learn() -> Self {
        ModSourceSlot::MidiLearn { source: None, learning: true }
    }

    /// A control that follows the notes the lane plays (`MidiBindSource::Velocity` or `Key`): bound when it is made, since no controller is moved for it.
    pub fn new_note_control(source: crate::plugin::chain::MidiBindSource) -> Self {
        ModSourceSlot::MidiLearn { source: Some(source), learning: false }
    }

    /// A macro at rest: it drives nothing until it is turned. It has no name of its own (it is `Macro`) and shows its knob on the box of a module that it is in (`expose`).
    pub fn new_macro() -> Self {
        ModSourceSlot::Macro { value: 0.0, name: String::new(), expose: true }
    }

    /// A macro at a value, with no name and exposed: what a test needs a macro to be, apart from what it is set to.
    #[cfg(test)]
    pub fn macro_at(value: f32) -> Self {
        ModSourceSlot::Macro { value, name: String::new(), expose: true }
    }

    /// A step sequencer as the modulate popup makes it: all sixteen steps in use, a staircase that climbs from the bottom in eighths (so that it shows at once
    /// that it goes round), eight steps a second (sixteenths at the 120 beats a minute that a session begins at), no glide, and started again by every note.
    pub fn new_steps() -> Self {
        ModSourceSlot::Steps {
            values: ModSourceSlot::staircase(),
            length: crate::plugin::chain::MAX_STEPS,
            rate: 8.0,
            glide: 0.0,
            retrigger: true,
            sync: 0,
        }
    }

    /// The steps of a step sequencer that is new: a staircase that climbs from -1 in eighths.
    pub fn staircase() -> [f32; crate::plugin::chain::MAX_STEPS] {
        std::array::from_fn(|i| i as f32 / 8.0 - 1.0)
    }

    /// The values of a step sequencer, if it is one.
    pub fn steps(&self) -> Option<[f32; crate::plugin::chain::MAX_STEPS]> {
        match self {
            ModSourceSlot::Steps { values, .. } => Some(*values),
            _ => None,
        }
    }

    /// What the modulator's node is called for a client: its kind, or what a control
    /// is bound to, or, for a macro that the person has named, the name. Not its settings, which a client is told when they change and
    /// which a name would have to follow (`label` has them, for the TUI).
    pub fn node_name(&self) -> String {
        match self {
            ModSourceSlot::Lfo { .. } => "LFO".to_string(),
            ModSourceSlot::Macro { name, .. } if !name.is_empty() => name.clone(),
            ModSourceSlot::Macro { .. } => "Macro".to_string(),
            ModSourceSlot::Steps { .. } => "Steps".to_string(),
            other => other.label(),
        }
    }

    /// A short label for the source (used in the tree and param pane).
    pub fn label(&self) -> String {
        use crate::plugin::chain::MidiBindSource;
        match self {
            ModSourceSlot::Lfo { waveform, rate, sync, .. } if *sync == 0 => format!("LFO {:.1}Hz {}", rate, waveform.name()),
            ModSourceSlot::Lfo { waveform, sync, .. } => format!("LFO {} {}", sync_name(*sync), waveform.name()),
            ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
            ModSourceSlot::MidiLearn { source, learning } => match source {
                Some(MidiBindSource::Cc(n)) => format!("CC {n}"),
                Some(MidiBindSource::PitchBend) => "Pitch Bend".to_string(),
                Some(MidiBindSource::Aftertouch) => "Aftertouch".to_string(),
                Some(MidiBindSource::Velocity) => "Velocity".to_string(),
                Some(MidiBindSource::Key) => "Key".to_string(),
                None if *learning => "MIDI (learning…)".to_string(),
                None => "MIDI (unbound)".to_string(),
            },
            ModSourceSlot::Macro { value, name, .. } if !name.is_empty() => format!("Macro {name} {value:.2}"),
            ModSourceSlot::Macro { value, .. } => format!("Macro {value:.2}"),
            ModSourceSlot::Steps { length, rate, sync, .. } if *sync == 0 => format!("Steps {length} {rate:.1}Hz"),
            ModSourceSlot::Steps { length, sync, .. } => format!("Steps {length} {}", sync_name(*sync)),
        }
    }
}

#[derive(Debug)]
pub struct ModulatorSlot {
    pub node: NodeId,
    pub source: ModSourceSlot,
    pub targets: Vec<ModTargetSlot>,
    /// Where the audio thread publishes the modulator's output (telemetry);
    /// `None` if the slots had run out.
    pub output: Option<Slot>,
    /// Where the audio thread publishes where a step sequencer is (telemetry): the whole part of the number is the step it is at, the fraction how far
    /// into it. `None` for any other modulator, and for a step sequencer if the slots had run out.
    pub position: Option<Slot>,
    /// Whose notes the modulator hears, if it is in the session's own rack (a free modulator): the node of a lane (its instrument's) or of a group, or none for every event, whatever its
    /// source (`Modulator::ear`, which the audio thread is told by `SetModulatorEar`). A lane's modulators and a group's hear their own lane or group, and have none. A node that is neither
    /// a lane nor a group that is there (it was taken out, and an undo brings it back under the same id) is every event too (`SessionModel::ear_of`).
    pub ear: Option<NodeId>,
}

#[derive(Debug)]
pub struct ModTargetSlot {
    /// The edge this target is, for as long as it exists.
    pub edge: EdgeId,
    /// What the target is called in the tree: the parameter's name, or what a setting is called, as it was when the cable was drawn.
    pub param_name: String,
    /// What the cable drives, by the node and the port (a plugin's parameter, a modulator's setting, the depth of a cable by its id) and not by a place in a rack: it stays on what it
    /// drives while the plugins, the lanes, the members of a group and the modulators around it come and go (P5a of `docs/patch-graph.md` §11.6). `SessionModel::kind_of` says the
    /// place that the audio thread's racks name it by.
    pub aim: Driven,
    pub depth: f32,
    #[allow(dead_code)]
    pub param_min: f32,
    #[allow(dead_code)]
    pub param_max: f32,
    /// Where the audio thread publishes the effective value of what the target
    /// drives (telemetry): a plugin's parameter, or a setting of a modulator or the
    /// depth of one of its cables; `None` only if the slots had run out.
    pub effective: Option<Slot>,
}

/// A cable that was taken out of the session (`SessionModel::remove_edge`): where it was, the cable itself, and what it was centred on. What puts it
/// back is `SessionModel::insert_target_at` and a command to the audio thread (`Controller::undo`).
#[derive(Debug)]
pub struct RemovedCable {
    /// The rack it was in.
    pub at: ModulatorLocation,
    /// The modulator in the rack, and the cable's place in it as it was when its turn came.
    pub mod_index: usize,
    pub target_index: usize,
    /// The cable: its id, the slot it publishes to, and all it says.
    pub slot: ModTargetSlot,
    /// What the audio thread had the cable centred on, which is what the user had set what it drove to (`SessionModel::centre_of`).
    pub base_value: f32,
    /// A cable that drives the depth of this one was still there when it went: they drive each other's depths, one of the two goes first
    /// and the other is left driving a place with no cable at it for the moment. Putting them back one at a time cannot tell that place
    /// from the place of the cable that was after it (both are the same number once the cable has gone), so such a removal is not one that
    /// can be put back.
    pub still_driven: bool,
}

/// A plugin parameter a modulator can drive (`SessionModel::param_targets`), with what it
/// takes to bind it (`Controller::add_target`) and to say what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamTarget {
    /// The node of the plugin the parameter is on (a lane's own, for its instrument).
    pub node: NodeId,
    /// The plugin's own index for the parameter.
    pub param: u32,
    /// How the modulator's rack addresses it.
    pub kind: ModTargetKind,
    /// The plugin's slot in its chain: 0 for an instrument (and for a bus effect), 1.. for
    /// the effects of a lane. A target is labelled with its plugin by it.
    pub slot: usize,
    pub plugin_name: String,
    pub param_name: String,
    /// What the user has set the parameter to, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// A field of a modulator that another modulator can drive (`SessionModel::field_targets`): its rate, a stage of its envelope or the
/// depth of one of its cables, with what it takes to bind it (`Controller::add_target`) and to say what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldTarget {
    /// The node of the modulator whose field it is.
    pub node: NodeId,
    /// Which field it is.
    pub field: CrossModField,
    /// The cable, when the field is the depth of one (`Port::Depth` names it by this).
    pub edge: Option<EdgeId>,
    /// How the driving modulator's rack addresses it.
    pub kind: ModTargetKind,
    /// Whose field it is, as a person is told: `Mod 1` for the modulator at index 1 of the rack, `M0 Mod1` for the one at index 1
    /// of the first member of a group.
    pub owner: String,
    /// What the field is called: `rate`, `attack`, ..., or, for a cable's depth, `detune depth`.
    pub field_name: String,
    /// What the target is labelled with once it is bound: the owner and the name.
    pub label: String,
    /// What the field is set to now, where its modulation is centred.
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

impl ParamTarget {
    /// What a cable to this parameter drives, by the node and the port.
    pub fn aim(&self) -> Driven {
        Driven::Param { plugin: self.node, index: self.param }
    }
}

/// A host-side value that a cable can drive (`SessionModel::gain_targets`, step 97): what a front-end needs to list it and what the controller needs to bind a cable to it.
#[derive(Clone, Debug, PartialEq)]
pub struct GainTarget {
    /// The node the value is of: a lane, an effect or a group.
    pub node: NodeId,
    pub gain: ModGain,
    /// How the rack addresses it.
    pub kind: ModTargetKind,
    /// What the node is called: its plugin's name (an instrument's, an effect's) or the group's.
    pub owner_name: String,
    /// What the user has set it to, where its modulation is centred.
    pub value: f32,
}

impl GainTarget {
    /// What a cable to this value drives, by the node and the port.
    pub fn aim(&self) -> Driven {
        Driven::Gain { node: self.node, gain: self.gain }
    }
}

impl FieldTarget {
    /// What a cable to this field drives, by the node and the port: the setting of the modulator, or the depth of the cable.
    pub fn aim(&self) -> Driven {
        match (self.edge, setting_of(self.field)) {
            (Some(edge), _) => Driven::Depth(edge),
            (None, Some(field)) => Driven::Setting { modulator: self.node, field },
            // (A depth is always of a cable, and every other field is a setting: `push_fields` makes them so.)
            (None, None) => unreachable!("a field that is the depth of a cable has the cable"),
        }
    }
}

/// The fields of the modulators of `rack` but the one at `except`, which a modulator of the same rack addresses as its siblings'.
fn push_siblings(targets: &mut Vec<FieldTarget>, rack: &[ModulatorSlot], except: usize) {
    for (index, modulator) in rack.iter().enumerate().filter(|(index, _)| *index != except) {
        push_fields(targets, modulator, format!("Mod {index}"), |field| sibling_kind(index, field));
    }
}

/// The fields of `modulator`: the settings its source has, in the order of the pane (the rate; the four stages), and then the depth
/// of each of its cables. `kind` says how the rack that drives them addresses a field.
fn push_fields(targets: &mut Vec<FieldTarget>, modulator: &ModulatorSlot, owner: String, kind: impl Fn(CrossModField) -> ModTargetKind) {
    let mut push = |field: CrossModField, name: String, edge: Option<EdgeId>, base_value: f32, (param_min, param_max): (f32, f32)| {
        let (node, kind, label, owner) = (modulator.node, kind(field), format!("{owner} {name}"), owner.clone());
        targets.push(FieldTarget { node, field, edge, kind, owner, field_name: name, label, base_value, param_min, param_max });
    };
    const SETTINGS: [(CrossModField, ModulatorField, &str); 5] = [
        (CrossModField::Rate, ModulatorField::Rate, "rate"),
        (CrossModField::Attack, ModulatorField::Attack, "attack"),
        (CrossModField::Decay, ModulatorField::Decay, "decay"),
        (CrossModField::Sustain, ModulatorField::Sustain, "sustain"),
        (CrossModField::Release, ModulatorField::Release, "release"),
    ];
    for (field, setting, name) in SETTINGS {
        if let Some(value) = modulator.source.setting(setting) {
            push(field, name.to_string(), None, value, setting.range());
        }
    }
    for (index, cable) in modulator.targets.iter().enumerate() {
        push(CrossModField::Depth(index), format!("{} depth", cable.param_name), Some(cable.edge), cable.depth, (DEPTH_MIN, DEPTH_MAX));
    }
}

/// How a rack addresses the field `field` of its own modulator at `mod_index`: the other way round of `sibling_field`.
pub fn sibling_kind(mod_index: usize, field: CrossModField) -> ModTargetKind {
    match field {
        CrossModField::Rate => ModTargetKind::ModulatorRate { mod_index },
        CrossModField::Attack => ModTargetKind::ModulatorAttack { mod_index },
        CrossModField::Decay => ModTargetKind::ModulatorDecay { mod_index },
        CrossModField::Sustain => ModTargetKind::ModulatorSustain { mod_index },
        CrossModField::Release => ModTargetKind::ModulatorRelease { mod_index },
        CrossModField::Depth(target_index) => ModTargetKind::ModulatorDepth { mod_index, target_index },
    }
}

/// The parameters of every plugin in `lane`'s chain, the instrument first. `kind` makes the
/// kind of target of a parameter from its plugin's slot and its own index.
fn push_chain(targets: &mut Vec<ParamTarget>, lane: &InstrumentNode, kind: impl Fn(usize, u32) -> ModTargetKind) {
    for (slot, plugin) in std::iter::once(lane.instrument.as_ref()).chain(lane.effects.iter().map(Some)).enumerate() {
        if let Some(plugin) = plugin {
            push_params(targets, plugin, slot, |param_index| kind(slot, param_index));
        }
    }
}

/// The parameters of `plugin`, which is in chain slot `slot` (a separator is a row of a pane,
/// not a parameter, and is left out).
fn push_params(targets: &mut Vec<ParamTarget>, plugin: &PluginSlot, slot: usize, kind: impl Fn(u32) -> ModTargetKind) {
    for row in plugin.params.iter().filter(|row| !matches!(row.kind, ParamKind::Separator)) {
        targets.push(ParamTarget {
            node: plugin.node,
            param: row.index,
            kind: kind(row.index),
            slot,
            plugin_name: plugin.name.clone(),
            param_name: row.name.clone(),
            base_value: row.value,
            param_min: row.min,
            param_max: row.max,
        });
    }
}

// ---------------------------------------------------------------------------
// Plugin ids
// ---------------------------------------------------------------------------

pub fn format_from_id(id: &str) -> String {
    if id.starts_with("builtin:") {
        "Built-in".into()
    } else if id.starts_with("module:") {
        // (A chain of effects that the library has: `app::module`.)
        "Module".into()
    } else if id.starts_with("lv2:") || id.starts_with("http://") || id.starts_with("urn:") {
        "LV2".into()
    } else if id.starts_with("clap:") || id.contains('.') && !id.contains('/') {
        "CLAP".into()
    } else if id.starts_with("vst3:") {
        "VST3".into()
    } else if id.ends_with(".lv2") {
        "LV2".into()
    } else if id.ends_with(".clap") {
        "CLAP".into()
    } else if id.ends_with(".vst3") {
        "VST3".into()
    } else {
        "?".into()
    }
}

// ---------------------------------------------------------------------------
// Modulator rows: how a modulator's source and targets map to parameter rows
// ---------------------------------------------------------------------------

/// Depth range for a modulator target. Negative depth **inverts** the
/// modulation direction — e.g. a control that lowers the parameter instead of
/// raising it, an LFO wobbling in anti-phase, or an envelope that dips below
/// the base. The audio-side math is a plain multiply, so this is only the
/// editor's bound; sessions already round-trip negative depth.
pub const DEPTH_MIN: f32 = -1.0;
pub const DEPTH_MAX: f32 = 1.0;
/// The range of the value of a step of a step sequencer: below and above what the user set, by as much as the depth of the cable says.
pub const STEP_MIN: f32 = -1.0;
pub const STEP_MAX: f32 = 1.0;
/// The longest an LFO's fade can be, in seconds.
pub const MAX_FADE: f32 = 5.0;
/// The note lengths that a cycle of an LFO, or a step of a step sequencer, can be tied to the tempo by (`ModulatorField::Sync`): each with the name that a session file and the screens give it, and
/// how many beats of the tempo it is, a beat being a quarter note and a bar four beats (a session has no time signature: the four beats of a pattern are a bar). They go from the slowest note
/// to the fastest, and a note's dotted length (half as long again), its own and its triplet (two thirds of it) are together, as a plugin lists them, so that a step along the list goes to the
/// next note and not always to a faster one. The place of one in this list, counted from 1, is what a modulator's `sync` is; 0 is none, and the modulator goes at its own rate.
pub const SYNC_DIVISIONS: [(&str, f32); 16] = [
    ("4 bars", 16.0),
    ("2 bars", 8.0),
    ("1 bar", 4.0),
    ("1/2 dotted", 3.0),
    ("1/2", 2.0),
    ("1/2 triplet", 4.0 / 3.0),
    ("1/4 dotted", 1.5),
    ("1/4", 1.0),
    ("1/4 triplet", 2.0 / 3.0),
    ("1/8 dotted", 0.75),
    ("1/8", 0.5),
    ("1/8 triplet", 1.0 / 3.0),
    ("1/16 dotted", 0.375),
    ("1/16", 0.25),
    ("1/16 triplet", 1.0 / 6.0),
    ("1/32", 0.125),
];

/// What a modulator that does not follow the tempo (a `sync` of 0) is called.
pub const FREE: &str = "free";

/// The name of a `sync`: that of its note length, or `free` for none (and for a place the list has not).
pub fn sync_name(sync: usize) -> &'static str {
    sync.checked_sub(1).and_then(|place| SYNC_DIVISIONS.get(place)).map_or(FREE, |&(name, _)| name)
}

/// How many beats of the tempo a cycle or a step is for a `sync`: 0 for none (and for a place the list has not).
pub fn sync_beats(sync: usize) -> f32 {
    sync.checked_sub(1).and_then(|place| SYNC_DIVISIONS.get(place)).map_or(0.0, |&(_, beats)| beats)
}

/// The `sync` that `name` says, whatever its case and the space round it and in it: `free` (or `off`, `none`, or nothing at all) is 0, and a note length is its place in `SYNC_DIVISIONS`.
/// `None` for any other.
pub fn sync_from_name(name: &str) -> Option<usize> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    if matches!(name.as_str(), "" | FREE | "off" | "none") {
        return Some(0);
    }
    SYNC_DIVISIONS.iter().position(|&(known, _)| known == name).map(|place| place + 1)
}

/// How deep a cable pulls when it is first drawn: half way, which a person can hear and turn either way.
pub const NEW_CABLE_DEPTH: f32 = 0.5;
/// How many parameters of a plugin can be pinned to its node: a node that shows more is taller than a canvas has room for, and a plugin of
/// hundreds has its other parameters in the panel.
pub const MAX_PINNED: usize = 64;
/// How long a group's name can be, in characters: a node shows it in a header, and a name that fills a screen is no name.
pub const MAX_NAME: usize = 64;

/// What a row of a modulator's parameter pane is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModulatorRow {
    /// The kind of source, which an LFO and an envelope switch between.
    Type,
    /// A MIDI control's source: shown, not edited.
    Info,
    /// An LFO's waveform, stepped through the four.
    Waveform,
    /// A number of the source: an LFO's rate, a stage of an envelope.
    Setting(ModulatorField),
    /// The "Targets" separator.
    Separator,
    /// The switch for whether a note starts a step sequencer or an LFO again: a choice of two.
    Retrigger,
    /// Whether the rate follows the tempo, and by what note length: a choice of `free` and the note lengths of `SYNC_DIVISIONS`.
    Sync,
    /// The value of the step sequencer's step with this index.
    Step(usize),
    /// The depth of the target with this index.
    Depth(usize),
}

/// The rows of the pane of a step sequencer before its targets': its settings and its choices, its sixteen steps, and the separator.
const STEPS_ROWS: [ModulatorRow; 5 + crate::plugin::chain::MAX_STEPS + 1] = {
    use ModulatorField as F;
    use ModulatorRow as R;
    let mut rows = [R::Separator; 5 + crate::plugin::chain::MAX_STEPS + 1];
    rows[0] = R::Setting(F::Rate);
    rows[1] = R::Sync;
    rows[2] = R::Setting(F::Glide);
    rows[3] = R::Setting(F::Length);
    rows[4] = R::Retrigger;
    let mut step = 0;
    while step < crate::plugin::chain::MAX_STEPS {
        rows[5 + step] = R::Step(step);
        step += 1;
    }
    rows
};

impl ModSourceSlot {
    /// The rows of the pane before the targets' (which follow the separator).
    fn rows(&self) -> &'static [ModulatorRow] {
        use ModulatorField as F;
        use ModulatorRow as R;
        match self {
            ModSourceSlot::Lfo { .. } => &[R::Type, R::Waveform, R::Setting(F::Rate), R::Sync, R::Retrigger, R::Setting(F::Fade), R::Separator],
            ModSourceSlot::Envelope { .. } => {
                &[R::Type, R::Setting(F::Attack), R::Setting(F::Decay), R::Setting(F::Sustain), R::Setting(F::Release), R::Separator]
            }
            ModSourceSlot::MidiLearn { .. } => &[R::Info, R::Separator],
            ModSourceSlot::Macro { .. } => &[R::Setting(F::Value), R::Separator],
            ModSourceSlot::Steps { .. } => &STEPS_ROWS,
        }
    }

    /// The value of a setting, if the source has it (a waveform is its index).
    pub fn setting(&self, field: ModulatorField) -> Option<f32> {
        match (self, field) {
            (ModSourceSlot::Lfo { rate, .. }, ModulatorField::Rate) => Some(*rate),
            (ModSourceSlot::Lfo { waveform, .. }, ModulatorField::Waveform) => Some(waveform.to_index() as f32),
            (ModSourceSlot::Lfo { retrigger, .. }, ModulatorField::Retrigger) => Some(if *retrigger { 1.0 } else { 0.0 }),
            (ModSourceSlot::Lfo { fade, .. }, ModulatorField::Fade) => Some(*fade),
            (ModSourceSlot::Lfo { sync, .. } | ModSourceSlot::Steps { sync, .. }, ModulatorField::Sync) => Some(*sync as f32),
            (ModSourceSlot::Envelope { attack, .. }, ModulatorField::Attack) => Some(*attack),
            (ModSourceSlot::Envelope { decay, .. }, ModulatorField::Decay) => Some(*decay),
            (ModSourceSlot::Envelope { sustain, .. }, ModulatorField::Sustain) => Some(*sustain),
            (ModSourceSlot::Envelope { release, .. }, ModulatorField::Release) => Some(*release),
            (ModSourceSlot::Macro { value, .. }, ModulatorField::Value) => Some(*value),
            (ModSourceSlot::Steps { rate, .. }, ModulatorField::Rate) => Some(*rate),
            (ModSourceSlot::Steps { glide, .. }, ModulatorField::Glide) => Some(*glide),
            (ModSourceSlot::Steps { length, .. }, ModulatorField::Length) => Some(*length as f32),
            (ModSourceSlot::Steps { retrigger, .. }, ModulatorField::Retrigger) => Some(if *retrigger { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
}

/// Number of param-pane rows for a modulator: the rows of its source, the "Targets"
/// separator among them, and one row a target. Scope-independent (lane or group).
pub fn modulator_param_len(m: &ModulatorSlot) -> usize {
    m.source.rows().len() + m.targets.len()
}

/// What row `row` of the pane of `m` is; `None` past the last. Every other question
/// about a row (its range, whether it is an enum, what it is bound to by MIDI learn,
/// which intent it makes) is answered from this.
pub fn modulator_row(m: &ModulatorSlot, row: usize) -> Option<ModulatorRow> {
    let fixed = m.source.rows();
    match fixed.get(row) {
        Some(&found) => Some(found),
        None => {
            let target = row - fixed.len();
            (target < m.targets.len()).then_some(ModulatorRow::Depth(target))
        }
    }
}

/// (min, max) for a modulator pseudo-param at row `pa`, or None for enum /
/// separator rows. Scope-independent.
pub fn modulator_param_range(m: &ModulatorSlot, pa: usize) -> Option<(f32, f32)> {
    match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => Some(field.range()),
        ModulatorRow::Step(_) => Some((STEP_MIN, STEP_MAX)),
        ModulatorRow::Depth(_) => Some((DEPTH_MIN, DEPTH_MAX)),
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync | ModulatorRow::Separator => None,
    }
}

/// True if a modulator pseudo-param at row `pa` is an enum (Type / Waveform / Retrigger / Sync).
pub fn modulator_param_is_enum(m: &ModulatorSlot, pa: usize) -> bool {
    matches!(modulator_row(m, pa), Some(ModulatorRow::Type | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync))
}

impl ModulatorSlot {
    /// The telemetry slots of a modulator that is made with `source`: the one it puts its output in and, for a step sequencer, the one that says where it is, in that
    /// order. Each is `None` if the allocator had run out of them: the modulator works all the same, and a page has nothing to draw from.
    pub fn alloc_slots(slots: &mut SlotAllocator, source: &ModSourceSlot) -> (Option<Slot>, Option<Slot>) {
        let output = slots.alloc();
        let position = if matches!(source, ModSourceSlot::Steps { .. }) { slots.alloc() } else { None };
        (output, position)
    }

    /// The intent that makes row `row` what `f` makes of what it is now: a setting of the
    /// source (an LFO's rate, a stage of an envelope) or the depth of a target. `None` for
    /// a row that is not a number (the type, the waveform, a separator, a control's source).
    pub fn row_intent(&self, row: usize, f: impl FnOnce(f32) -> f32) -> Option<Intent> {
        match modulator_row(self, row)? {
            ModulatorRow::Setting(field) => self.field_intent(field, f),
            ModulatorRow::Retrigger => self.field_intent(ModulatorField::Retrigger, f),
            ModulatorRow::Sync => self.field_intent(ModulatorField::Sync, f),
            ModulatorRow::Step(step) => {
                let mut values = self.source.steps()?;
                let value = values.get_mut(step)?;
                *value = f(*value);
                Some(Intent::SetSteps { node: self.node, values: values.to_vec() })
            }
            ModulatorRow::Depth(target) => {
                let target = self.targets.get(target)?;
                Some(Intent::SetEdgeDepth { edge: target.edge, depth: f(target.depth) })
            }
            ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Separator => None,
        }
    }

    /// The intent that sets `field` to what `f` makes of it now, if the source has it.
    fn field_intent(&self, field: ModulatorField, f: impl FnOnce(f32) -> f32) -> Option<Intent> {
        Some(Intent::SetModulatorField { node: self.node, field, value: f(self.source.setting(field)?) })
    }
}

/// The commands that tell the audio thread where the modulator at `index` of the rack at `location` puts its output and, for a step sequencer, where it is: one for each
/// slot it has, the output's first; and whose notes it hears, if it hears any one's.
pub fn modulator_slot_commands(location: ModulatorLocation, index: usize, output: Option<Slot>, position: Option<Slot>, ear: Option<NodeId>) -> Vec<plugin::chain::GraphCommand> {
    use plugin::chain::GraphCommand;
    let mut commands = Vec::new();
    if let Some(slot) = output {
        commands.push(GraphCommand::SetModulatorOutput { location, index, slot });
    }
    if let Some(slot) = position {
        commands.push(GraphCommand::SetModulatorPosition { location, index, slot });
    }
    if ear.is_some() {
        commands.push(GraphCommand::SetModulatorEar { location, index, ear });
    }
    commands
}

/// Map a modulator's param-pane row `pa` to a cross-mod target on that
/// modulator (index `mod_index`): its rate/ADSR field or one of its target
/// depths. Returns `(kind, min, max, base, label)`, or None for enum/separator
/// rows. Used by MIDI-learn to bind a control to a modulator's own parameter.
pub fn modulator_row_to_cross_mod(
    m: &ModulatorSlot,
    mod_index: usize,
    pa: usize,
) -> Option<(crate::plugin::chain::ModTargetKind, f32, f32, f32, String)> {
    use crate::plugin::chain::ModTargetKind as K;
    match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => {
            let (kind, name) = match field {
                ModulatorField::Rate => (K::ModulatorRate { mod_index }, "rate"),
                ModulatorField::Attack => (K::ModulatorAttack { mod_index }, "attack"),
                ModulatorField::Decay => (K::ModulatorDecay { mod_index }, "decay"),
                ModulatorField::Sustain => (K::ModulatorSustain { mod_index }, "sustain"),
                ModulatorField::Release => (K::ModulatorRelease { mod_index }, "release"),
                // (It has a row of its own, `ModulatorRow::Waveform`, which is not a number for a control to move.)
                ModulatorField::Waveform => return None,
                // (A macro's value is turned by a person: a cable that drives it is not built. Nor is a step sequencer's glide, length or switch.)
                ModulatorField::Value | ModulatorField::Glide | ModulatorField::Length | ModulatorField::Retrigger | ModulatorField::Fade | ModulatorField::Sync => return None,
            };
            let (min, max) = field.range();
            Some((kind, min, max, m.source.setting(field)?, format!("Mod {mod_index} {name}")))
        }
        ModulatorRow::Depth(target) => {
            let depth = m.targets.get(target)?.depth;
            Some((K::ModulatorDepth { mod_index, target_index: target }, DEPTH_MIN, DEPTH_MAX, depth, format!("Mod {mod_index} depth {target}")))
        }
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync | ModulatorRow::Step(_) | ModulatorRow::Separator => None,
    }
}

// ---------------------------------------------------------------------------
// Audio-graph conversion
// ---------------------------------------------------------------------------

/// Convert a `ModSourceSlot` to the audio-thread `ModSource` carried by `GraphCommand`s.
pub fn mod_source_slot_to_graph(slot: &ModSourceSlot) -> crate::plugin::chain::ModSource {
    match slot {
        ModSourceSlot::Lfo { waveform, rate, retrigger, fade, sync } => {
            crate::plugin::chain::ModSource::lfo(*waveform, *rate).with_key(*retrigger, *fade).with_sync(sync_beats(*sync))
        }
        ModSourceSlot::Envelope { attack, decay, sustain, release } => crate::plugin::chain::ModSource::Envelope {
            attack: *attack,
            decay: *decay,
            sustain: *sustain,
            release: *release,
            state: crate::plugin::chain::EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        },
        ModSourceSlot::MidiLearn { source, learning } => crate::plugin::chain::ModSource::MidiLearn {
            source: *source,
            // (Where the control is before it sends anything: the wheel's centre is not 0.)
            value: source.map_or(0.0, crate::plugin::chain::MidiBindSource::rest),
            learning: *learning,
        },
        ModSourceSlot::Macro { value, .. } => crate::plugin::chain::ModSource::Macro { value: *value },
        ModSourceSlot::Steps { values, length, rate, glide, retrigger, sync } => crate::plugin::chain::ModSource::Steps {
            values: *values,
            length: *length,
            rate: *rate,
            glide: *glide,
            retrigger: *retrigger,
            sync: sync_beats(*sync),
            position: 0.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::{LONG_CHAINS, SENDS, SIDED, Sends, load};
    use crate::plugin::chain::{LfoWaveform, MidiBindSource, ModSource};

    /// The parameters of `slot` that have no effect now, by name, with why.
    fn unused_of<'a>(slot: &'a PluginSlot, cables: &[AudioCable]) -> Vec<(&'a str, &'a str)> {
        slot.params.iter().filter_map(|param| slot.unused_because(param, cables).map(|why| (param.name.as_str(), why))).collect()
    }

    fn set(slot: &mut PluginSlot, name: &str, value: f32) {
        slot.params.iter_mut().find(|p| p.name == name).unwrap().value = value;
    }

    #[test]
    fn a_parameter_that_has_an_effect_only_in_some_modes_is_unused_in_the_others_by_the_value_the_user_set() {
        let mut run = load(SIDED);
        let cables = run.model.cables.clone();
        // The group's math effect has nothing patched at its second input: in a ring, which reads neither `amount`, `bits`, `attack` nor `release`, those four have none, and `pitch` and `wave` have (B is the carrier).
        let bus = &mut run.model.groups[0].effects[0];
        let ring = [("amount", "only in the modes am, fold, warp, compare, hold, follow"), ("bits", "only in the modes xor, and, or"), ("attack", "only in the mode follow"), ("release", "only in the mode follow")];
        assert_eq!(unused_of(bus, &cables), ring);
        // The follow mode (11) reads `amount`, `attack` and `release`; xor (7) reads `bits` and not the others.
        set(bus, "mode", 11.0);
        assert_eq!(unused_of(bus, &cables), [ring[1]]);
        set(bus, "mode", 7.0);
        assert_eq!(unused_of(bus, &cables), [ring[0], ring[2], ring[3]]);
        // (The mode that a choice is at is the place that its value rounds to.)
        set(bus, "mode", 10.6);
        assert_eq!(unused_of(bus, &cables), [ring[1]], "10.6 is the twelfth mode, follow");
    }

    #[test]
    fn a_carrier_that_has_a_cable_at_its_second_input_is_unused_and_only_a_cable_at_that_effects_second_input_is_one() {
        let run = load(SIDED);
        let (lane, bus) = (&run.model.instruments[0].effects[1], &run.model.groups[0].effects[0]);
        let words = "only while nothing is patched at B";
        // Lane 0's math effect has the cable of the file at its second input: its carrier is not heard.
        assert_eq!(run.model.cables.len(), 1);
        assert!(unused_of(lane, &run.model.cables).contains(&("pitch", words)) && unused_of(lane, &run.model.cables).contains(&("wave", words)));
        // The group's has none, and a cable at another effect's second input is not a cable at its own: its carrier is heard.
        assert!(!unused_of(bus, &run.model.cables).iter().any(|(name, _)| *name == "pitch" || *name == "wave"));
        // Taken out of the list the lane's carrier is heard, and a cable at the first input of the effect (the input that the chain feeds) is not a cable at the second.
        assert!(!unused_of(lane, &[]).iter().any(|(name, _)| *name == "pitch" || *name == "wave"));
        let at_first_input = AudioCable { to: CableTo::Effect(lane.node), ..run.model.cables[0] };
        assert!(!unused_of(lane, &[at_first_input]).iter().any(|(name, _)| *name == "pitch" || *name == "wave"));
        // And one at the group's second input is the group's, and not the lane's.
        let at_the_bus_effect = AudioCable { to: CableTo::EffectSide(bus.node), ..run.model.cables[0] };
        assert!(!unused_of(lane, &[at_the_bus_effect]).iter().any(|(name, _)| *name == "pitch" || *name == "wave"));
        assert!(unused_of(bus, &[at_the_bus_effect]).contains(&("pitch", words)));
    }

    #[test]
    fn a_plugin_that_says_nothing_of_when_its_parameters_are_used_has_every_one_used_and_so_has_a_rule_that_names_no_parameter() {
        let mut run = load(SIDED);
        let osc = run.model.instruments[0].instrument.as_ref().unwrap();
        assert!(osc.params.iter().all(|p| p.used_when.is_none()));
        assert_eq!(unused_of(osc, &run.model.cables), []);
        // A rule that names a parameter the plugin has not is no rule: the parameter is used.
        let filter = &mut run.model.instruments[0].effects[0];
        let used_when = plugin::UsedWhen { condition: plugin::UsedCondition::Choice { param: 99, values: vec![1] }, why: "only in a mode that is not there".to_string() };
        filter.params[0].used_when = Some(used_when);
        assert_eq!(unused_of(filter, &[]), []);
    }

    #[test]
    fn every_node_is_found_by_its_id() {
        use crate::app::testutil::GROUPS;
        let model = load(GROUPS).model;
        model.check_ids().unwrap();

        let nodes: Vec<(NodeId, NodePath)> = model.nodes().collect();
        let expected = model.groups.len()
            + model.groups.iter().map(|g| g.effects.len()).sum::<usize>()
            + model.instruments.len()
            + model.instruments.iter().map(|l| l.effects.len()).sum::<usize>()
            + model.free_modulators.len();
        assert_eq!(nodes.len(), expected, "one node per group, lane, effect, bus effect and modulator");
        for (id, path) in &nodes {
            assert_eq!(model.locate(*id), Some(*path), "{id:?}");
        }
        let has = |f: fn(&NodePath) -> bool| nodes.iter().any(|(_, p)| f(p));
        assert!(has(|p| matches!(p, NodePath::Instrument(_))));
        assert!(has(|p| matches!(p, NodePath::Effect { .. })));
        assert!(has(|p| matches!(p, NodePath::Group(_))));
        assert!(has(|p| matches!(p, NodePath::GroupEffect { .. })));
        assert!(has(|p| matches!(p, NodePath::Modulator { at: ModulatorLocation::Free, .. })));

        assert_eq!(model.locate(NodeId(9_999)), None, "an id nothing has");
    }

    #[test]
    fn a_lanes_instrument_sits_in_the_lanes_node() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").model;
        let lane = &model.instruments[0];
        assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);
        assert_ne!(lane.effects[0].node, lane.node, "an effect is a node of its own");
    }

    #[test]
    fn ids_are_handed_out_once() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        let b = ids.alloc();
        assert_ne!(a, b);
        let mut model = SessionModel::default();
        let first = model.alloc_node();
        assert_ne!(model.alloc_node(), first);
    }

    #[test]
    fn a_new_model_is_at_120_beats_a_minute() {
        assert_eq!(SessionModel::default().bpm, 120.0);
    }

    #[test]
    fn the_tempo_is_saved_only_when_a_load_would_not_come_to_it() {
        let pattern = "[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = 90.0\nlength_beats = 4.0\nenabled = true\n";
        let mut plain = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        assert_eq!(plain.bpm_to_save(), None, "120 is what a session with no pattern is at");
        plain.bpm = 100.0;
        assert_eq!(plain.bpm_to_save(), Some(100.0));

        let mut patterned = load(pattern).model;
        assert_eq!(patterned.bpm_to_save(), None, "90 is the first pattern's");
        patterned.bpm = 120.0;
        assert_eq!(patterned.bpm_to_save(), Some(120.0), "(a 120 that a load would not come to is written)");
        patterned.bpm = 90.0;
        patterned.instruments[0].pattern = None;
        assert_eq!(patterned.bpm_to_save(), Some(90.0), "without the pattern a load would be at 120");
    }

    #[test]
    fn the_tempo_of_patterns_is_that_of_the_first_and_held_to_the_tempos_there_can_be() {
        let lane = |bpm: &str| format!("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.pattern]\nbpm = {bpm}\nlength_beats = 4.0\nenabled = true\n");
        let tempo = |text: &str| tempo_of_patterns(&load(text).model.instruments);
        assert_eq!(tempo(&format!("[[instrument]]\nplugin = \"builtin:osc\"\n{}{}", lane("140.0"), lane("90.0"))), 140.0, "the first lane that has a pattern decides");
        assert_eq!(tempo(&lane("5.0")), 20.0);
        assert_eq!(tempo(&lane("1000.0")), 300.0);
        assert_eq!(tempo("[[instrument]]\nplugin = \"builtin:osc\"\n"), 120.0);
    }

    #[test]
    fn a_group_is_called_by_its_name_or_else_by_its_number_counting_from_one() {
        let mut model = load("[[group]]\nname = \"Pad\"\n[[group]]\n[[group]]\nname = \"\"\n").model;
        let names: Vec<String> = model.groups.iter().enumerate().map(|(g, group)| group.display_name(g)).collect();
        assert_eq!(names, ["Pad", "Group 2", "Group 3"]);
        model.groups[0].name = None;
        assert_eq!(model.groups[0].display_name(0), "Group 1");
        assert_eq!(model.groups[0].display_name(6), "Group 7", "it is the index it is asked about");
    }

    #[test]
    fn edge_ids_are_a_series_of_their_own() {
        let mut model = SessionModel::default();
        let (n0, e0, n1, e1) = (model.alloc_node(), model.alloc_edge(), model.alloc_node(), model.alloc_edge());
        assert_eq!((n0, n1), (NodeId(0), NodeId(1)), "edges do not use up node ids");
        assert_eq!((e0, e1), (EdgeId(0), EdgeId(1)));
    }

    #[test]
    fn ids_that_are_to_be_put_back_come_first_in_order_and_new_ones_go_on_where_they_were() {
        let mut ids = NodeIds::default();
        let (a, b, c) = (ids.alloc(), ids.alloc(), ids.alloc());
        let (x, y) = (ids.alloc_edge(), ids.alloc_edge());
        // What was taken out comes back under its own ids, in the order it is made; what is made after it is new.
        ids.reuse(&[b, a], &[y]);
        assert_eq!((ids.alloc(), ids.alloc()), (b, a));
        assert_eq!(ids.alloc_edge(), y);
        assert_eq!(ids.alloc(), NodeId(c.0 + 1), "the next new id is the one after the last there was");
        assert_eq!(ids.alloc_edge(), EdgeId(y.0 + 1));
        assert_eq!(x, EdgeId(0));
    }

    #[test]
    fn ids_to_put_back_that_are_not_used_can_be_dropped_and_are_not_given_to_what_is_made_next() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        ids.reuse(&[a], &[]);
        ids.drop_ids_to_put_back();
        assert_ne!(ids.alloc(), a, "an id that nothing asked for is not given again");
        ids.reuse(&[], &[EdgeId(7)]);
        ids.drop_ids_to_put_back();
        assert_ne!(ids.alloc_edge(), EdgeId(7), "an edge's id is dropped as a node's is");
    }

    #[test]
    fn edges_that_are_put_back_come_in_the_order_they_were_said() {
        let mut ids = NodeIds::default();
        let (x, y) = (ids.alloc_edge(), ids.alloc_edge());
        ids.reuse(&[], &[y, x]);
        assert_eq!((ids.alloc_edge(), ids.alloc_edge()), (y, x));
    }

    #[test]
    fn a_log_that_is_begun_again_starts_empty() {
        let mut ids = NodeIds::default();
        ids.begin_log();
        ids.alloc();
        ids.begin_log();
        assert_eq!(ids.take_log(), Made::default());
    }

    #[test]
    fn what_is_to_be_put_back_is_what_was_last_said_and_not_what_was_said_before() {
        let mut ids = NodeIds::default();
        let (a, b) = (ids.alloc(), ids.alloc());
        let (x, y) = (ids.alloc_edge(), ids.alloc_edge());
        ids.reuse(&[a], &[x]);
        ids.reuse(&[b], &[y]);
        assert_eq!((ids.alloc(), ids.alloc_edge()), (b, y));
        assert_eq!((ids.alloc(), ids.alloc_edge()), (NodeId(b.0 + 1), EdgeId(y.0 + 1)), "and what was said first is not left to come after it");
    }

    #[test]
    fn the_ids_handed_out_since_the_log_was_begun_are_told_once_in_order() {
        let mut ids = NodeIds::default();
        ids.alloc(); // (before the log: not in it)
        ids.begin_log();
        let (n, e, m) = (ids.alloc(), ids.alloc_edge(), ids.alloc());
        assert_eq!(ids.take_log(), Made { nodes: vec![n, m], edges: vec![e] });
        ids.alloc();
        assert_eq!(ids.take_log(), Made::default(), "the log ended with the take: nothing handed out since is in it");
    }

    #[test]
    fn an_id_that_is_put_back_is_in_the_log_as_any_is_and_in_its_turn() {
        let mut ids = NodeIds::default();
        let a = ids.alloc();
        ids.begin_log();
        ids.reuse(&[a], &[]);
        ids.alloc();
        ids.alloc();
        assert_eq!(ids.take_log().nodes, [a, NodeId(a.0 + 1)]);
    }

    #[test]
    fn check_ids_notices_edge_ids_that_are_shared_or_strangers() {
        let session = r#"
[[instrument]]
plugin = "builtin:osc"
[[modulator]]
[[modulator.target]]
to = "instrument[0]"
param = "detune"
[[modulator.target]]
to = "instrument[0]"
param = "volume"
[[modulator]]
[[modulator.target]]
to = "instrument[0]"
param = "pan"
"#;
        let fresh = || load(session).model;

        let mut model = fresh();
        assert!(model.check_ids().is_ok());
        let first = model.free_modulators[0].targets[0].edge;
        model.free_modulators[0].targets[1].edge = first;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("another edge has too"), "two targets with one edge: {error}");

        // The cables of two modulators.
        let mut model = fresh();
        model.free_modulators[1].targets[0].edge = model.free_modulators[0].targets[0].edge;
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("another edge has too"), "a target of one modulator and a target of another: {error}");

        let mut model = fresh();
        model.free_modulators[0].targets[0].edge = EdgeId(9_999);
        let error = model.check_ids().unwrap_err();
        assert!(error.contains("never allocated"), "an edge nothing handed out: {error}");
    }

    #[test]
    fn check_ids_notices_duplicates_strangers_and_a_mismatched_instrument() {
        let fresh = || load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").model;

        let mut model = fresh();
        assert!(model.check_ids().is_ok());

        model.instruments[0].effects[0].node = model.instruments[0].node;
        assert!(model.check_ids().unwrap_err().contains("another node has"), "a duplicate");

        let mut model = fresh();
        model.instruments[0].effects[0].node = NodeId(500);
        assert!(model.check_ids().unwrap_err().contains("never allocated"), "a stranger");

        let mut model = fresh();
        let other = model.alloc_node();
        model.instruments[0].instrument.as_mut().unwrap().node = other;
        assert!(model.check_ids().unwrap_err().contains("its instrument"), "an instrument in another node");
    }

    // -- modules that were added as one thing (step 108) --

    /// A session of three lanes and a modulator with two modules: "Room" has the second lane and the modulator, "Echo" the third lane. The nodes of the lanes, of the modulator and of the two boxes.
    fn the_modules() -> (SessionModel, [NodeId; 3], NodeId, [NodeId; 2]) {
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[modulator]]\n").model;
        let lanes = [model.instruments[0].node, model.instruments[1].node, model.instruments[2].node];
        let modulator = model.free_modulators[0].node;
        let boxes = [model.alloc_node(), model.alloc_node()];
        model.modules.push(ModuleInstance { node: boxes[0], name: "Room".to_string(), members: vec![lanes[1], modulator] });
        model.modules.push(ModuleInstance { node: boxes[1], name: "Echo".to_string(), members: vec![lanes[2]] });
        (model, lanes, modulator, boxes)
    }

    #[test]
    fn a_module_is_a_node_that_comes_after_the_modulators_and_is_found_by_its_id_and_so_is_the_module_a_node_is_in() {
        let (model, lanes, modulator, boxes) = the_modules();
        let nodes: Vec<(NodeId, NodePath)> = model.nodes().collect();
        assert_eq!(nodes.last().copied(), Some((boxes[1], NodePath::Module(1))));
        assert_eq!(nodes[nodes.len() - 2], (boxes[0], NodePath::Module(0)));
        assert_eq!(nodes[nodes.len() - 3], (modulator, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 }), "the modules are listed after the modulators");
        assert_eq!(model.locate(boxes[0]), Some(NodePath::Module(0)));
        assert_eq!(model.modules[1].name, "Echo");
        assert_eq!((model.module_of(lanes[1]), model.module_of(modulator), model.module_of(lanes[2]), model.module_of(lanes[0]), model.module_of(boxes[0])), (Some(0), Some(0), Some(1), None, None));
        model.check_ids().unwrap();
    }

    #[test]
    fn a_member_that_leaves_takes_itself_out_of_its_module_and_a_module_with_none_left_is_gone_and_the_module_as_it_was_is_what_puts_it_back() {
        let (mut model, lanes, modulator, boxes) = the_modules();
        let room = model.modules[0].clone();
        let (at, before) = model.leave_module(lanes[1]).expect("it was in a module");
        assert_eq!((at, &before), (0, &room), "the module as it was, and its place");
        assert_eq!(model.modules[0].members, [modulator]);
        assert_eq!(model.leave_module(lanes[1]), None, "a node that is in none leaves none");
        assert_eq!(model.leave_module(lanes[0]), None);
        // The last member: the module is gone, and the one after it is the first now.
        let (at, last) = model.leave_module(modulator).unwrap();
        assert_eq!((at, last.members.clone()), (0, vec![modulator]));
        assert_eq!(model.modules.iter().map(|module| module.node).collect::<Vec<_>>(), [boxes[1]]);
        assert_eq!(model.locate(boxes[0]), None);
        // Put back as it was: at its place when it is gone, in place of what it is now when it is there.
        assert_eq!(model.insert_module_at(0, last), 0);
        assert_eq!(model.modules.iter().map(|module| module.node).collect::<Vec<_>>(), [boxes[0], boxes[1]]);
        assert_eq!(model.insert_module_at(7, room.clone()), 0, "one that is there is replaced where it is, whatever place is asked for");
        assert_eq!(model.modules[0], room);
        let echo = model.modules.pop().unwrap();
        assert_eq!(model.insert_module_at(99, echo), 1, "a place past the end is the end");
        // A module that is not the first one says the place it had when its last member goes, and is put back there.
        let (mut model, lanes, _, boxes) = the_modules();
        let (at, echo) = model.leave_module(lanes[2]).expect("it was in a module");
        assert_eq!(at, 1, "the place of the module that is gone, and not the first");
        assert_eq!(model.modules.iter().map(|module| module.node).collect::<Vec<_>>(), [boxes[0]]);
        assert_eq!(model.insert_module_at(at, echo), 1);
        assert_eq!(model.modules.iter().map(|module| module.node).collect::<Vec<_>>(), [boxes[0], boxes[1]]);
        model.check_ids().unwrap();
    }

    #[test]
    fn check_ids_notices_a_module_that_has_no_member_one_whose_member_is_no_lane_or_modulator_and_a_node_that_is_in_two() {
        let (mut model, ..) = the_modules();
        model.modules[1].members.clear();
        assert!(model.check_ids().unwrap_err().contains("a module that has no member"));
        let (mut model, lanes, ..) = the_modules();
        model.modules[1].members = vec![lanes[2], NodeId(900)];
        assert!(model.check_ids().unwrap_err().contains("neither a lane nor a modulator that is there"));
        let (mut model, _, _, boxes) = the_modules();
        model.modules[1].members = vec![boxes[0]];
        assert!(model.check_ids().unwrap_err().contains("neither a lane nor a modulator that is there"), "a module is no member of another");
        let (mut model, lanes, ..) = the_modules();
        model.modules[1].members.push(lanes[1]);
        assert!(model.check_ids().unwrap_err().contains("is in two modules"));
        let (mut model, _, _, boxes) = the_modules();
        model.modules[1].node = boxes[0];
        assert!(model.check_ids().unwrap_err().contains("another node has too"));
        let (mut model, ..) = the_modules();
        model.modules[1].node = NodeId(9_000);
        assert!(model.check_ids().unwrap_err().contains("never allocated"));
    }

    // -- sources of notes and the cables from them (P4b of docs/patch-graph.md §11.6) --

    fn the_sources() -> (SessionModel, crate::app::testutil::Routed) {
        let model = load(crate::app::testutil::SOURCES).model;
        let nodes = crate::app::testutil::Routed::of(&model);
        (model, nodes)
    }

    #[test]
    fn the_sources_are_nodes_that_come_after_the_groups_and_the_lanes_and_are_found_by_their_ids() {
        let (model, n) = the_sources();
        let nodes: Vec<(NodeId, NodePath)> = model.nodes().collect();
        assert_eq!(
            nodes,
            [
                (n.lane[0], NodePath::Instrument(0)),
                (n.lane[1], NodePath::Instrument(1)),
                (n.lane[2], NodePath::Instrument(2)),
                (n.keys, NodePath::Source(0)),
                (n.devices[0], NodePath::Source(1)),
                (n.devices[1], NodePath::Source(2)),
            ]
        );
        assert_eq!(model.locate(n.devices[0]), Some(NodePath::Source(1)));
        assert_eq!(model.locate(NodeId(900)), None);
    }

    #[test]
    fn the_cables_of_notes_have_ids_of_the_series_that_the_other_cables_have() {
        let (mut model, n) = the_sources();
        assert!(model.edge_in_use(EdgeId(0)) && model.edge_in_use(EdgeId(2)), "the cables of notes");
        assert!(model.edge_in_use(EdgeId(3)), "and the audio cable after them");
        assert!(!model.edge_in_use(EdgeId(4)));
        // The id that an event cable has is not handed out again.
        assert_eq!(model.alloc_edge(), EdgeId(4));
        // (A cable that goes out of the model frees nothing: ids are not reused but for what undo puts back.)
        model.event_cables.retain(|cable| cable.to != n.lane[1]);
        assert!(!model.edge_in_use(EdgeId(1)));
    }

    #[test]
    fn an_event_cable_goes_in_among_the_event_cables_in_the_order_of_the_ids_and_one_that_is_there_under_the_id_is_replaced() {
        let (mut model, n) = the_sources();
        model.ids.reuse(&[], &[EdgeId(10), EdgeId(11)]);
        let first = model.alloc_edge();
        let second = model.alloc_edge();
        assert_eq!((first, second), (EdgeId(10), EdgeId(11)));
        assert_eq!(model.insert_event_cable(EventCable { id: second, from: n.keys, to: n.lane[1] }), 3, "at the end: the id is the highest");
        assert_eq!(model.insert_event_cable(EventCable { id: EdgeId(5), from: n.keys, to: n.lane[2] }), 3, "in the middle, before the one that has a higher id");
        assert_eq!(model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [0, 1, 2, 5, 11]);
        let place = model.insert_event_cable(EventCable { id: EdgeId(5), from: n.devices[0], to: n.lane[2] });
        assert_eq!(place, 3);
        assert_eq!(model.event_cables.len(), 5, "replaced");
        assert_eq!((model.event_cables[3].from, model.event_cables[3].to), (n.devices[0], n.lane[2]));
    }

    #[test]
    fn check_ids_notices_sources_and_cables_of_notes_that_are_not_what_they_should_be() {
        let (model, _) = the_sources();
        model.check_ids().unwrap();
        // The keys twice, and a device by one name twice.
        let (mut model, _) = the_sources();
        let node = model.alloc_node();
        model.sources.push(SourceNode { node, kind: SourceKind::Keys });
        assert!(model.check_ids().unwrap_err().contains("the keys are a source more than once"));
        let (mut model, _) = the_sources();
        let node = model.alloc_node();
        model.sources.push(SourceNode { node, kind: SourceKind::Device("Seaboard".to_string()) });
        assert!(model.check_ids().unwrap_err().contains("\"Seaboard\" is a source more than once"));
        // A node that is not a source's id: a stranger, or another node's.
        let (mut model, n) = the_sources();
        model.sources[0].node = n.lane[0];
        assert!(model.check_ids().unwrap_err().contains("another node has too"));
        // An id that nothing handed out, and one that an audio cable has.
        let (mut model, n) = the_sources();
        model.insert_event_cable(EventCable { id: EdgeId(9_999), from: n.keys, to: n.lane[2] });
        assert!(model.check_ids().unwrap_err().contains("never allocated"));
        let (mut model, n) = the_sources();
        model.insert_event_cable(EventCable { id: EdgeId(3), from: n.keys, to: n.lane[2] });
        assert!(model.check_ids().unwrap_err().contains("another edge has too"));
        // A start that is not a source (a lane, a stranger) and an end that is not a lane (a source, a stranger).
        let (mut model, n) = the_sources();
        model.event_cables[0].from = n.lane[2];
        assert!(model.check_ids().unwrap_err().contains("which is not a source"));
        let (mut model, _) = the_sources();
        model.event_cables[0].from = NodeId(900);
        assert!(model.check_ids().unwrap_err().contains("which is not a source"));
        let (mut model, n) = the_sources();
        model.event_cables[0].to = n.keys;
        assert!(model.check_ids().unwrap_err().contains("which is not a lane"));
        let (mut model, _) = the_sources();
        model.event_cables[0].to = NodeId(900);
        assert!(model.check_ids().unwrap_err().contains("which is not a lane"));
        // Two between the same nodes, and out of the order of their ids.
        let (mut model, n) = the_sources();
        model.event_cables[1] = EventCable { id: EdgeId(1), from: n.keys, to: n.lane[0] };
        assert!(model.check_ids().unwrap_err().contains("between two nodes that another event cable is between"));
        let (mut model, _) = the_sources();
        model.event_cables.swap(1, 2);
        assert!(model.check_ids().unwrap_err().contains("is not after the one before it"));
    }

    /// What the open devices stand for in these tests: the keys are the keys; the first device is two ports, sources 1 and 2; the second is not plugged in.
    fn open_ports(kind: &SourceKind) -> SourceSet {
        match kind {
            SourceKind::Keys => SourceSet::NONE.with(crate::source::Source::KEYS),
            SourceKind::Device(name) if name.starts_with("Launchkey") => SourceSet::NONE.with(crate::source::Source::device(0)).with(crate::source::Source::device(1)),
            SourceKind::Device(_) => SourceSet::NONE,
        }
    }

    fn numbers(sources: SourceSet) -> Vec<u8> {
        sources.iter().map(|source| source.number()).collect()
    }

    #[test]
    fn a_lane_that_no_cable_of_notes_ends_at_hears_every_source_and_one_that_has_some_hears_the_sources_they_start_at() {
        let (model, _) = the_sources();
        assert_eq!(numbers(model.lane_sources(0, open_ports)), [0], "the keys");
        assert_eq!(numbers(model.lane_sources(1, open_ports)), [1, 2], "both ports of the first device, and the second device that is not plugged in adds none");
        assert_eq!(model.lane_sources(2, open_ports), SourceSet::ALL, "no cable of notes ends at it");
        assert_eq!(model.lane_sources(9, open_ports), SourceSet::ALL, "a lane that is not there hears what a lane does until it is told otherwise");
    }

    #[test]
    fn a_lane_whose_cables_start_at_devices_that_are_not_plugged_in_hears_nothing_and_not_every_source() {
        let (model, _) = the_sources();
        let nobody = |_: &SourceKind| SourceSet::NONE;
        assert!(model.lane_sources(0, nobody).is_empty());
        assert!(model.lane_sources(1, nobody).is_empty());
        assert_eq!(model.lane_sources(2, nobody), SourceSet::ALL, "a lane with no cable is not one of them");
    }

    #[test]
    fn each_cable_of_a_lane_is_asked_about_its_source_once_and_a_cable_from_what_is_no_source_counts_for_nothing() {
        let (mut model, n) = the_sources();
        let mut asked = Vec::new();
        model.lane_sources(1, |kind| {
            asked.push(kind.clone());
            SourceSet::NONE
        });
        assert_eq!(asked, [SourceKind::Device("Launchkey Mini MK4 37".to_string()), SourceKind::Device("Seaboard".to_string())], "in the order of the cables");
        // (A cable from a node that is no source: only a model that was changed behind the controller's back has one.)
        model.event_cables[0].from = n.lane[2];
        assert!(model.lane_sources(0, open_ports).is_empty(), "it hears nothing: it has a cable, and what the cable starts at is no source");
    }

    #[test]
    fn an_event_cable_is_taken_out_by_its_id_and_the_cables_at_a_node_are_found_in_the_order_of_their_ids() {
        let (mut model, n) = the_sources();
        assert_eq!(model.event_cables_at(&[n.lane[1]]), [EdgeId(1), EdgeId(2)], "what ends at the lane");
        assert_eq!(model.event_cables_at(&[n.keys]), [EdgeId(0)], "what starts at the source");
        assert_eq!(model.event_cables_at(&[n.devices[1], n.lane[0]]), [EdgeId(0), EdgeId(2)], "at either of the nodes, once each, in the order of the ids");
        assert!(model.event_cables_at(&[n.lane[2], NodeId(900)]).is_empty());
        let taken = model.remove_event_cable(EdgeId(1)).unwrap();
        assert_eq!((taken.id, taken.from, taken.to), (EdgeId(1), n.devices[0], n.lane[1]));
        assert_eq!(model.event_cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(model.remove_event_cable(EdgeId(1)), None, "it is gone");
        assert_eq!(model.remove_event_cable(EdgeId(3)), None, "an audio cable's is not one of these");
        assert_eq!(model.source(n.devices[1]).map(|source| source.kind.clone()), Some(SourceKind::Device("Seaboard".to_string())));
        assert!(model.source(n.lane[0]).is_none(), "a lane is no source");
    }

    #[test]
    fn an_audio_cable_cannot_start_or_end_at_a_source_of_notes() {
        let (model, n) = the_sources();
        let audio = |node: NodeId, port| crate::app::graph::Endpoint { node, port };
        use crate::app::graph::Port;
        let refused = |from, to| model.cable_ends(from, to).unwrap_err();
        assert!(matches!(refused(audio(n.keys, Port::AudioOut), audio(NodeId::MASTER, Port::AudioIn)), IntentError::Invalid(why) if why.contains("no output of audio")));
        assert!(matches!(refused(audio(n.lane[0], Port::AudioOut), audio(n.devices[0], Port::AudioIn)), IntentError::Invalid(why) if why.contains("no input of audio")));
    }

    #[test]
    fn a_source_goes_in_among_the_sources_at_its_place_and_at_the_end_for_a_place_past_it() {
        let (mut model, n) = the_sources();
        let device = |node: u32| SourceNode { node: NodeId(node), kind: SourceKind::Device(format!("D{node}")) };
        assert_eq!(model.insert_source_at(1, device(100)), 1);
        assert_eq!(model.insert_source_at(99, device(101)), 4, "a place past the end is the end");
        assert_eq!(model.sources.iter().map(|source| source.node).collect::<Vec<_>>(), [n.keys, NodeId(100), n.devices[0], n.devices[1], NodeId(101)]);
    }

    #[test]
    fn an_event_cable_is_found_by_its_id_among_the_event_cables_and_an_audio_cables_id_is_not_one_of_them() {
        let (model, n) = the_sources();
        assert_eq!(model.event_cable(EdgeId(1)), Some(&EventCable { id: EdgeId(1), from: n.devices[0], to: n.lane[1] }));
        assert_eq!(model.event_cable(EdgeId(3)), None, "the audio cable's");
        assert_eq!(model.event_cable(EdgeId(9)), None, "none at all");
    }

    // -- audio cables (P3c of docs/patch-graph.md §11.6) --

    fn the_sends() -> (SessionModel, Sends) {
        let model = load(SENDS).model;
        let nodes = Sends::of(&model);
        (model, nodes)
    }

    fn cable(id: u32, from: CableFrom, to: CableTo) -> AudioCable {
        AudioCable { id: EdgeId(id), from, to, gain: 1.0 }
    }

    #[test]
    fn the_wiring_is_the_lanes_with_their_groups_and_effects_and_the_groups_with_their_bus_effects() {
        let (model, n) = the_sends();
        let (lanes, groups) = model.wiring();
        assert_eq!(lanes.iter().map(|lane| (lane.node, lane.group, lane.effects.clone())).collect::<Vec<_>>(), [(n.lane[0], Some(0), vec![n.fx[0], n.fx[1]]), (n.lane[1], None, vec![n.fx[2]])]);
        assert_eq!(groups.iter().map(|group| (group.node, group.effects.clone())).collect::<Vec<_>>(), [(n.group, vec![n.bus])]);
    }

    #[test]
    fn the_links_the_lanes_and_the_groups_make_by_themselves_are_listed_by_the_two_ends_a_cable_between_them_would_have() {
        let (model, n) = the_sends();
        let link = |of, from, to| TemplateLink { of, from, to, present: true };
        assert_eq!(
            model.template_links(),
            [
                // Lane 0 is in the group, so its chain ends at the group's bus.
                link(LinkOf::Feed(n.fx[0]), CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[0])),
                link(LinkOf::Feed(n.fx[1]), CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[1])),
                link(LinkOf::WayOut(n.lane[0]), CableFrom::Effect(n.fx[1]), CableTo::GroupBus(n.group)),
                // Lane 1 is in none: its chain ends at the mix.
                link(LinkOf::Feed(n.fx[2]), CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[2])),
                link(LinkOf::WayOut(n.lane[1]), CableFrom::Effect(n.fx[2]), CableTo::Master),
                // The group's own link into its first bus effect starts at the group's node, as the graph draws it.
                link(LinkOf::Feed(n.bus), CableFrom::Group(n.group), CableTo::Effect(n.bus)),
                link(LinkOf::WayOut(n.group), CableFrom::Effect(n.bus), CableTo::Master),
            ]
        );
        // A link is found by its two ends, and a pair of places that no link joins has none.
        assert_eq!(model.template_link(CableFrom::Effect(n.fx[2]), CableTo::Master), Some(link(LinkOf::WayOut(n.lane[1]), CableFrom::Effect(n.fx[2]), CableTo::Master)));
        assert_eq!(model.template_link(CableFrom::Instrument(n.lane[1]), CableTo::Master), None, "lane 1's instrument goes into its effect, and not to the mix");
        assert_eq!(model.template_link(CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[2])), None);
        assert_eq!(model.template_link(CableFrom::Effect(n.fx[1]), CableTo::Master), None, "lane 0 goes to the bus of its group, and not to the mix");
    }

    #[test]
    fn a_link_is_there_unless_the_way_out_or_the_feed_it_is_of_is_cut() {
        let (mut model, n) = the_sends();
        let absent = |model: &SessionModel| model.template_links().iter().filter(|link| !link.present).map(|link| link.of).collect::<Vec<_>>();
        assert_eq!(absent(&model), []);
        model.instruments[0].route = Route::None;
        model.instruments[0].effects[1].fed = false;
        model.groups[0].route = Route::None;
        model.groups[0].effects[0].fed = false;
        assert_eq!(absent(&model), [LinkOf::Feed(n.fx[1]), LinkOf::WayOut(n.lane[0]), LinkOf::Feed(n.bus), LinkOf::WayOut(n.group)]);
    }

    #[test]
    fn a_lane_with_no_effect_and_a_group_with_no_bus_effect_have_their_way_out_at_their_own_node_and_a_lane_whose_group_is_gone_goes_to_the_mix() {
        let mut model = load("[[group]]\n\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let (lanes, group) = (model.instruments.iter().map(|lane| lane.node).collect::<Vec<_>>(), model.groups[0].node);
        let way_out = |of, from, to| TemplateLink { of, from, to, present: true };
        assert_eq!(
            model.template_links(),
            [
                way_out(LinkOf::WayOut(lanes[0]), CableFrom::Instrument(lanes[0]), CableTo::GroupBus(group)),
                way_out(LinkOf::WayOut(lanes[1]), CableFrom::Instrument(lanes[1]), CableTo::Master),
                way_out(LinkOf::WayOut(group), CableFrom::Group(group), CableTo::Master),
            ]
        );
        // (The model keeps the lanes in step, but a front-end must not crash if it did not: a group that is not there is no group.)
        model.instruments[0].group = Some(7);
        assert_eq!(model.template_links()[0], way_out(LinkOf::WayOut(lanes[0]), CableFrom::Instrument(lanes[0]), CableTo::Master));
    }

    #[test]
    fn a_cable_is_drawn_from_an_instrument_an_effect_or_a_group_to_an_effect_a_bus_or_the_mix() {
        let (model, n) = the_sends();
        let drawn = [
            (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[2])),
            (CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
            (CableFrom::Effect(n.bus), CableTo::Effect(n.fx[2])),
            (CableFrom::Effect(n.fx[0]), CableTo::GroupBus(n.group)),
            (CableFrom::Group(n.group), CableTo::Effect(n.fx[2])),
            (CableFrom::Instrument(n.lane[0]), CableTo::Master),
            (CableFrom::Effect(n.fx[1]), CableTo::Master),
            (CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.fx[1])),
        ];
        for (from, to) in drawn {
            assert_eq!(model.cable_problem(from, to), None, "{from:?} to {to:?}");
        }
    }

    #[test]
    fn a_cable_is_not_drawn_from_or_to_what_is_not_there_or_is_not_what_it_says() {
        let (model, n) = the_sends();
        assert_eq!(model.cable_problem(CableFrom::Instrument(NodeId(900)), CableTo::Master), Some(CableProblem::NoStart));
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.group), CableTo::Master), Some(CableProblem::NoStart), "a group is no instrument");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.fx[0]), CableTo::Master), Some(CableProblem::NoStart), "an effect is no instrument");
        assert_eq!(model.cable_problem(CableFrom::Group(n.lane[0]), CableTo::Master), Some(CableProblem::NoStart), "a lane is no group");
        assert_eq!(model.cable_problem(CableFrom::Effect(n.lane[0]), CableTo::Master), Some(CableProblem::NoStart), "a lane is no effect");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::Effect(NodeId(901))), Some(CableProblem::NoEnd));
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.lane[1])), Some(CableProblem::NoEnd), "a lane has no input");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.lane[1])), Some(CableProblem::NoEnd), "a lane is no group");
        assert_eq!(model.cable_problem(CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.bus)), Some(CableProblem::NoEnd), "an effect is no group");
    }

    #[test]
    fn a_cable_that_would_make_the_audio_go_round_is_not_drawn() {
        let (mut model, n) = the_sends();
        let loops = |model: &SessionModel, from: CableFrom, to: CableTo| matches!(model.cable_problem(from, to), Some(CableProblem::Loop(_)));
        // Back along a chain, and into itself.
        assert!(loops(&model, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0])));
        assert!(loops(&model, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[0])));
        // A group's output into the chain of one of its members, and the bus into a member.
        assert!(loops(&model, CableFrom::Group(n.group), CableTo::Effect(n.fx[0])));
        assert!(loops(&model, CableFrom::Effect(n.bus), CableTo::Effect(n.fx[1])));
        assert!(!loops(&model, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])), "the lane that is in no group is not one of its members");
        // Two cables that are no loop by themselves and are one together.
        model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.fx[0])));
        assert!(loops(&model, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[2])), "lane 1's effect feeds lane 0's, and lane 0's last effect would feed lane 1's");
        assert!(!loops(&model, CableFrom::Effect(n.fx[1]), CableTo::Master));
    }

    #[test]
    fn the_cables_are_kept_in_the_order_of_their_ids_and_are_taken_out_by_id_with_the_place_they_had() {
        let (mut model, n) = the_sends();
        let (a, b, c) = (cable(5, CableFrom::Instrument(n.lane[0]), CableTo::Master), cable(3, CableFrom::Instrument(n.lane[1]), CableTo::Master), cable(9, CableFrom::Effect(n.fx[0]), CableTo::Master));
        assert_eq!([model.insert_audio_cable(a), model.insert_audio_cable(b), model.insert_audio_cable(c)], [0, 0, 2]);
        assert_eq!(model.cables.iter().map(|cable| cable.id.0).collect::<Vec<_>>(), [3, 5, 9]);
        assert_eq!(model.audio_cable(EdgeId(5)), Some(&a));
        assert_eq!(model.audio_cable(EdgeId(4)), None);
        assert_eq!(model.remove_audio_cable(EdgeId(5)), Some((1, a)));
        assert_eq!(model.remove_audio_cable(EdgeId(5)), None, "it is not there any more");
        assert_eq!(model.insert_audio_cable(a), 1, "it goes back where it was");
        assert_eq!(model.cables, [b, a, c]);
        // A cable that is put in under the id of one that is there takes its place, and does not go beside it: no two cables have one id.
        let again = AudioCable { gain: 0.5, ..a };
        assert_eq!(model.insert_audio_cable(again), 1);
        assert_eq!(model.cables, [b, again, c]);
    }

    #[test]
    fn the_cables_at_a_node_are_those_that_start_or_end_there() {
        let (mut model, n) = the_sends();
        for (id, from, to) in [
            (1, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (2, CableFrom::Effect(n.fx[0]), CableTo::Master),
            (3, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])),
            (4, CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.group)),
            (5, CableFrom::Effect(n.bus), CableTo::Master),
            (6, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
        ] {
            model.insert_audio_cable(cable(id, from, to));
        }
        let at = |node: NodeId| model.audio_cables_at(&[node]).iter().map(|edge| edge.0).collect::<Vec<_>>();
        assert_eq!(at(n.lane[0]), [4], "a lane: what its instrument sends");
        assert_eq!(at(n.lane[1]), [1]);
        assert_eq!(at(n.fx[0]), [1, 2], "an effect: what it sends and what it takes");
        assert_eq!(at(n.fx[1]), Vec::<u32>::new());
        assert_eq!(at(n.fx[2]), [3, 6]);
        assert_eq!(at(n.group), [3, 4], "a group: what it sends and what its bus takes");
        assert_eq!(at(n.bus), [5, 6]);
        assert_eq!(at(NodeId::MASTER), Vec::<u32>::new(), "the mix is no node that goes");
    }

    #[test]
    fn a_lane_is_not_put_in_a_group_when_a_cable_from_the_group_leads_back_into_the_lane() {
        let (mut model, n) = the_sends();
        // Lane 1 is in no group. A cable from the group's output into lane 1's effect is no loop, until the lane is a member of the group.
        model.insert_audio_cable(cable(1, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])));
        assert!(model.loop_if_lane_goes_to(1, Some(0)).is_some(), "its own output would feed the group that feeds it");
        assert_eq!(model.loop_if_lane_goes_to(1, None), None, "the lane stays out of the group");
        assert_eq!(model.loop_if_lane_goes_to(0, None), None, "lane 0 leaves the group");
        assert_eq!(model.loop_if_lane_goes_to(0, Some(0)), None, "lane 0 is in it already");
        // The same through a bus effect, and through a cable that leaves the lane's chain.
        model.remove_audio_cable(EdgeId(1));
        model.insert_audio_cable(cable(2, CableFrom::Effect(n.bus), CableTo::Effect(n.fx[2])));
        assert!(model.loop_if_lane_goes_to(1, Some(0)).is_some());
        model.remove_audio_cable(EdgeId(2));
        model.insert_audio_cable(cable(3, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus)));
        assert_eq!(model.loop_if_lane_goes_to(1, Some(0)), None, "a cable from the lane into an effect of the bus is no loop: the lane is upstream of both");
        // A lane or a group that is not there changes nothing.
        assert_eq!(model.loop_if_lane_goes_to(9, Some(0)), None);
        assert_eq!(model.loop_if_lane_goes_to(1, Some(9)), None);
    }

    #[test]
    fn an_effect_is_not_moved_to_a_place_where_the_audio_would_go_round() {
        let (mut model, n) = the_sends();
        // Lane 0's chain is fx0 and then fx1. fx0 feeds lane 1's effect, which feeds fx1: no loop while fx0 is first.
        model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[2])));
        model.insert_audio_cable(cable(2, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.fx[1])));
        assert!(model.loop_if_effect_goes_to(n.fx[1], 0).is_some(), "fx1 would feed fx0, which feeds fx1 by the cables");
        assert!(model.loop_if_effect_goes_to(n.fx[0], 1).is_some(), "the same two effects, the same swap");
        assert_eq!(model.loop_if_effect_goes_to(n.fx[0], 0), None, "to where it is");
        // Without the second cable the swap is fine.
        model.remove_audio_cable(EdgeId(2));
        assert_eq!(model.loop_if_effect_goes_to(n.fx[1], 0), None);
        // A bus effect is moved on its bus, and what is not an effect, or is not there, has no place to go to.
        assert_eq!(model.loop_if_effect_goes_to(n.bus, 0), None);
        assert_eq!(model.loop_if_effect_goes_to(n.lane[0], 0), None);
        assert_eq!(model.loop_if_effect_goes_to(NodeId(900), 0), None);
        assert_eq!(model.loop_if_effect_goes_to(n.fx[1], 7), None, "a place the chain has not");
    }

    #[test]
    fn an_effect_of_a_bus_is_not_moved_to_a_place_where_the_audio_would_go_round() {
        let mut model = load(
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
        )
        .model;
        let (first, second, lane_fx) = (model.groups[0].effects[0].node, model.groups[0].effects[1].node, model.instruments[0].effects[0].node);
        model.insert_audio_cable(cable(1, CableFrom::Effect(first), CableTo::Effect(lane_fx)));
        model.insert_audio_cable(cable(2, CableFrom::Effect(lane_fx), CableTo::Effect(second)));
        assert!(model.loop_if_effect_goes_to(second, 0).is_some());
        assert!(model.loop_if_effect_goes_to(first, 1).is_some());
    }

    #[test]
    fn a_link_into_an_effect_is_not_given_back_where_the_audio_would_go_round_and_cutting_one_is_no_loop() {
        let (mut model, n) = the_sends();
        // The link into lane 0's delay and the one into the group's bus effect are cut, and a cable goes from the delay back to the filter before it and one from the bus effect into the delay
        // (lane 0 is in the group): each is a loop only through the link that is cut.
        model.instruments[0].effects[1].fed = false;
        model.groups[0].effects[0].fed = false;
        model.insert_audio_cable(cable(1, CableFrom::Effect(n.fx[1]), CableTo::Effect(n.fx[0])));
        model.insert_audio_cable(cable(2, CableFrom::Effect(n.bus), CableTo::Effect(n.fx[1])));
        assert!(model.loop_if_feed_is(n.fx[1], true).is_some(), "given back, the delay's link would close the loop through the cable to the filter");
        assert!(model.loop_if_feed_is(n.bus, true).is_some(), "given back, the bus effect's link would close the loop through the cable into lane 0's delay");
        // Cutting a link that is cut is no loop (it is looked at as cut, and not as if the link were there).
        assert_eq!(model.loop_if_feed_is(n.fx[1], false), None);
        assert_eq!(model.loop_if_feed_is(n.bus, false), None);
        // The links that are there are as they are, and the wiring has the cuts that the model says, a lane's and a bus's: neither cable is a loop.
        assert_eq!(model.loop_if_feed_is(n.fx[0], true), None);
        assert_eq!(model.loop_if_feed_is(n.fx[2], true), None);
        // What is no effect, and what is not there, has no link to give back.
        for node in [n.lane[0], n.group, NodeId::MASTER, NodeId(900)] {
            assert_eq!(model.loop_if_feed_is(node, true), None, "{node:?}");
        }
    }

    #[test]
    fn what_goes_with_a_lane_or_a_group_is_the_cables_at_its_node_and_at_each_of_its_effects_once() {
        let (mut model, n) = the_sends();
        for (id, from, to) in [
            (1, CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0])),
            (2, CableFrom::Effect(n.fx[0]), CableTo::Effect(n.fx[1])),
            (3, CableFrom::Group(n.group), CableTo::Effect(n.fx[2])),
            (4, CableFrom::Instrument(n.lane[0]), CableTo::GroupBus(n.group)),
            (5, CableFrom::Effect(n.bus), CableTo::Master),
            (6, CableFrom::Effect(n.fx[2]), CableTo::Effect(n.bus)),
        ] {
            model.insert_audio_cable(cable(id, from, to));
        }
        assert_eq!(model.audio_nodes_of_lane(0), [n.lane[0], n.fx[0], n.fx[1]]);
        assert_eq!(model.audio_nodes_of_lane(1), [n.lane[1], n.fx[2]]);
        assert_eq!(model.audio_nodes_of_group(0), [n.group, n.bus]);
        assert_eq!((model.audio_nodes_of_lane(9), model.audio_nodes_of_group(9)), (vec![], vec![]));
        let ids = |nodes: Vec<NodeId>| model.audio_cables_at(&nodes).iter().map(|edge| edge.0).collect::<Vec<_>>();
        // (Cable 2 is between two effects of lane 0, and is one cable and not two.)
        assert_eq!(ids(model.audio_nodes_of_lane(0)), [1, 2, 4]);
        assert_eq!(ids(model.audio_nodes_of_lane(1)), [1, 3, 6]);
        assert_eq!(ids(model.audio_nodes_of_group(0)), [3, 4, 5, 6]);
        assert_eq!(ids(vec![]), Vec::<u32>::new());
    }

    #[test]
    fn the_ends_of_a_cable_are_found_from_the_ports_of_the_graph() {
        use crate::app::graph::{Endpoint, Port};
        let (model, n) = the_sends();
        let (out, into) = (|node| Endpoint { node, port: Port::AudioOut }, |node| Endpoint { node, port: Port::AudioIn });
        assert_eq!(model.cable_ends(out(n.lane[1]), into(n.fx[0])), Ok((CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.fx[0]))));
        assert_eq!(model.cable_ends(out(n.fx[0]), into(n.bus)), Ok((CableFrom::Effect(n.fx[0]), CableTo::Effect(n.bus))));
        assert_eq!(model.cable_ends(out(n.group), into(n.group)), Ok((CableFrom::Group(n.group), CableTo::GroupBus(n.group))));
        assert_eq!(model.cable_ends(out(n.bus), into(NodeId::MASTER)), Ok((CableFrom::Effect(n.bus), CableTo::Master)));
        // The wrong port, a node that cannot have that port, and a node that is not there.
        assert!(matches!(model.cable_ends(into(n.lane[1]), into(n.fx[0])), Err(IntentError::Invalid(_))));
        assert!(matches!(model.cable_ends(out(n.lane[1]), out(n.fx[0])), Err(IntentError::Invalid(_))));
        assert!(matches!(model.cable_ends(out(n.lane[1]), into(n.lane[0])), Err(IntentError::Invalid(_))), "a lane has no input");
        assert!(matches!(model.cable_ends(out(NodeId::MASTER), into(n.fx[0])), Err(IntentError::Invalid(_))), "the mix has no output");
        assert_eq!(model.cable_ends(out(NodeId(900)), into(n.fx[0])), Err(IntentError::NotFound(NodeId(900))));
        assert_eq!(model.cable_ends(out(n.lane[0]), into(NodeId(901))), Err(IntentError::NotFound(NodeId(901))));
        assert!(matches!(model.cable_ends(out(n.lane[0]), Endpoint { node: n.fx[0], port: Port::Rate }), Err(IntentError::Invalid(_))));
    }

    /// A session with a free LFO and a free envelope, a modulator in a lane and one in a group, and a lane that has no instrument.
    const FREE_AMONG_OTHERS: &str = r#"
[[group]]
[[group.modulator]]
type = "lfo"

[[modulator]]
type = "lfo"
[[modulator]]
type = "envelope"
[[modulator.target]]
to = "instrument[0]"
param = "detune"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator.target]]
param = "detune"

[[instrument]]
"#;

    #[test]
    fn the_free_modulators_are_nodes_after_the_sources_and_their_cables_are_found_like_any_others() {
        let mut model = load(FREE_AMONG_OTHERS).model;
        let free: Vec<(NodeId, NodePath)> = model.nodes().filter(|(_, path)| matches!(path, NodePath::Modulator { at: ModulatorLocation::Free, .. })).collect();
        assert_eq!(free.len(), 4, "the lane's and the group's, which were moved, and the two of the file's own");
        assert_eq!(free[1].1, NodePath::Modulator { at: ModulatorLocation::Free, index: 1 });
        let order: Vec<NodeId> = model.nodes().map(|(id, _)| id).collect();
        assert!(order.iter().position(|id| *id == free[0].0).unwrap() > order.iter().position(|id| *id == model.instruments[1].node).unwrap(), "after the lanes");
        // The sources are nodes before the free modulators (groups, lanes, sources and then the free modulators).
        let with_keys = load("[keys]\n[[modulator]]\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let places: Vec<NodePath> = with_keys.nodes().map(|(_, path)| path).collect();
        let keys = places.iter().position(|path| matches!(path, NodePath::Source(_))).expect("the keys are a source");
        let lfo = places.iter().position(|path| matches!(path, NodePath::Modulator { at: ModulatorLocation::Free, .. })).expect("a free modulator");
        assert!(keys < lfo, "after the sources: {places:?}");
        assert_eq!(model.locate(free[0].0), Some(NodePath::Modulator { at: ModulatorLocation::Free, index: 0 }));
        let cable = model.free_modulators[3].targets[0].edge;
        assert_eq!(model.locate_edge(cable), Some((ModulatorLocation::Free, 3, 0)));
        assert_eq!(model.driven_by_edge(cable), Some(model.free_modulators[3].targets[0].aim));
        assert!(model.edge_in_use(cable));
        assert!(model.is_driven(model.free_modulators[3].targets[0].aim));
        assert_eq!(model.centre_of(model.free_modulators[3].targets[0].aim), Some(0.0), "the oscillator's detune, as set");
        model.check_ids().unwrap();
        // A node that two of them share, or a cable that is not alone in its id, is told.
        let node = model.free_modulators[0].node;
        model.free_modulators[3].node = node;
        assert!(model.check_ids().is_err(), "two free modulators with one node");
        model.free_modulators[3].node = free[3].0;
        let twin = {
            let cable = &model.free_modulators[3].targets[0];
            ModTargetSlot { edge: cable.edge, param_name: cable.param_name.clone(), aim: cable.aim, depth: cable.depth, param_min: cable.param_min, param_max: cable.param_max, effective: cable.effective }
        };
        model.free_modulators[0].targets.push(twin);
        assert!(model.check_ids().is_err(), "two cables with one id");
    }

    #[test]
    fn a_plugin_taken_out_of_the_model_takes_the_free_cables_that_drove_it_and_a_free_modulator_taken_out_those_that_drove_its_settings() {
        let mut model = load(crate::app::testutil::FREE_MODULATORS).model;
        assert_eq!(model.free_modulators[0].targets.len(), 4);
        model.remove_effect(0, 0).unwrap();
        // (The cable to the lane's reverb is gone, the rest of the LFO's are there.)
        assert_eq!(model.free_modulators[0].targets.len(), 3);
        assert!(model.free_modulators[0].targets.iter().all(|t| t.param_name != "damping"));
        model.check_ids().unwrap();
        // The envelope goes: the LFO's cable to its attack goes with it, and what the envelope drove is no longer driven.
        let envelope = model.remove_modulator_at(ModulatorLocation::Free, 1).unwrap();
        assert_eq!(envelope.targets.len(), 2);
        assert!(model.free_modulators[0].targets.iter().all(|t| !matches!(t.aim, Driven::Setting { .. })), "no cable left that drives nothing");
        assert_eq!(model.free_modulators[0].targets.len(), 2);
        model.check_ids().unwrap();
        // And a lane that goes with its plugins.
        let mut model = load(crate::app::testutil::FREE_MODULATORS).model;
        model.remove_lane(0).unwrap();
        assert_eq!(model.free_modulators[0].targets.len(), 2, "the bus effect's cable and the attack's");
        // (The envelope's cable to the depth of the LFO's first one, which drove the first lane's oscillator, went with it; its cable to the second lane's filter is untouched.)
        assert_eq!(model.free_modulators[1].targets.len(), 1);
        assert!(matches!(model.free_modulators[1].targets[0].aim, Driven::Param { plugin, .. } if plugin == model.instruments[0].effects[0].node));
        model.check_ids().unwrap();
    }

    // -- What a modulator can drive of the others' --

    // -- Taking a cable out --

    use crate::app::testutil::{CABLES_THAT_DRIVE_CABLES, CABLES_THAT_DRIVE_EACH_OTHER};

    // -- Putting a cable back --

    // -- Putting an effect back --

    // -- Putting a lane back --

    // -- Putting a group back --

    // -- Putting a modulator back --

    // -- Lanes change places --

    // -- A cable is kept by the node and the port it drives: the places are said again --

    #[test]
    fn kind_of_names_what_the_lists_of_targets_name_in_every_rack_of_every_session() {
        use crate::app::testutil::{CHAINS, CROSS_MOD, CROSS_MOD_RACKS, GROUPS, LANES_IN_GROUPS};
        let mut checked = 0;
        for session in [CABLES_THAT_DRIVE_CABLES, CABLES_THAT_DRIVE_EACH_OTHER, CHAINS, CROSS_MOD, CROSS_MOD_RACKS, GROUPS, LANES_IN_GROUPS, LONG_CHAINS] {
            let model = load(session).model;
            // (A rack that has no modulator is asked as its first would be.)
            let racks: Vec<(ModulatorLocation, usize)> = model.racks().flat_map(|(at, rack)| (0..rack.len().max(1)).map(move |source| (at, source))).collect();
            for (at, source) in racks {
                for target in model.param_targets(at) {
                    assert_eq!(model.kind_of(at, source, target.aim()), Some(target.kind.clone()), "{at:?} {source}: {target:?}");
                    checked += 1;
                }
                for target in model.field_targets(at, source) {
                    assert_eq!(model.kind_of(at, source, target.aim()), Some(target.kind.clone()), "{at:?} {source}: {target:?}");
                    checked += 1;
                }
            }
        }
        assert!(checked > 500, "{checked} places are enough to say something");
    }

    /// A session of two lanes (the second has no instrument, only an effect), a group with a bus effect and two modulators: the first drives the first lane's `detune`, and the second drives the first one's rate.
    fn two_modulators() -> SessionModel {
        load(
            r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
[[instrument.effect]]
plugin = "builtin:filter"
[[modulator]]
type = "lfo"
[[modulator.target]]
to = "instrument[0]"
param = "detune"
[[modulator]]
type = "lfo"
[[modulator.target]]
mod_rate = 0
"#,
        )
        .model
    }

    #[test]
    fn kind_of_says_none_for_what_a_modulator_does_not_reach_and_the_lists_leave_out_the_same() {
        let model = two_modulators();
        let at = ModulatorLocation::Free;
        let (empty_lane, group) = (model.instruments[1].node, model.groups[0].node);
        let (first, second) = (model.free_modulators[0].node, model.free_modulators[1].node);
        let own_cable = model.free_modulators[0].targets[0].edge;
        // The instrument of a lane that has none has no parameter to drive, a group has none, and a node that is not there is nothing.
        assert_eq!(model.kind_of(at, 0, Driven::Param { plugin: empty_lane, index: 0 }), None);
        assert_eq!(model.kind_of(at, 0, Driven::Param { plugin: group, index: 0 }), None);
        assert_eq!(model.kind_of(at, 0, Driven::Param { plugin: NodeId(9_999), index: 0 }), None);
        assert!(model.param_targets(at).iter().all(|t| t.node != empty_lane && t.node != group));
        // A modulator drives neither its own settings nor the depth of its own cables: another's it does.
        assert_eq!(model.kind_of(at, 0, Driven::Setting { modulator: first, field: ModSetting::Rate }), None);
        assert!(model.kind_of(at, 1, Driven::Setting { modulator: first, field: ModSetting::Rate }).is_some());
        assert_eq!(model.kind_of(at, 0, Driven::Depth(own_cable)), None);
        assert!(model.kind_of(at, 1, Driven::Depth(own_cable)).is_some());
        // The lists say the same: the others, and never the modulator itself.
        assert!(model.field_targets(at, 0).iter().all(|t| t.node != first));
        assert!(model.field_targets(at, 1).iter().any(|t| t.node == first));
        assert_eq!(model.modulators_in_reach(at, 0), vec![second]);
        assert_eq!(model.modulators_in_reach(at, 1), vec![first]);
    }

    #[test]
    fn a_host_side_value_is_there_for_the_node_that_has_it_and_a_cable_to_it_has_a_place_and_for_no_other() {
        use crate::app::testutil::GAINS;
        let model = load(GAINS).model;
        let at = ModulatorLocation::Free;
        let (group, lane, empty_lane) = (model.groups[0].node, model.instruments[0].node, model.instruments[1].node);
        let (filter, reverb, delay) = (model.instruments[0].effects[0].node, model.groups[0].effects[0].node, model.instruments[1].effects[0].node);
        let has: [(NodeId, ModGain, f32); 6] = [
            (lane, ModGain::Volume, 0.7),
            (filter, ModGain::Mix, 0.9),
            (group, ModGain::Volume, 0.8),
            (group, ModGain::Pan, -0.25),
            (reverb, ModGain::Mix, 0.6),
            (delay, ModGain::Mix, 0.4),
        ];
        for (node, gain, value) in has {
            assert_eq!(model.gain_of(node, gain), Some(value), "{node:?} {gain:?}");
            assert_eq!(model.centre_of(Driven::Gain { node, gain }), Some(value), "what a cable to it is centred on");
            assert_eq!(model.kind_of(at, 0, Driven::Gain { node, gain }), Some(ModTargetKind::Gain { node, gain }), "{node:?} {gain:?}");
        }
        // A lane that has no instrument has no volume to drive, an effect no volume or pan, a lane no pan or mix, a group no mix, and a node that is not there nothing.
        let has_not = [
            (empty_lane, ModGain::Volume),
            (filter, ModGain::Volume),
            (filter, ModGain::Pan),
            (lane, ModGain::Pan),
            (lane, ModGain::Mix),
            (group, ModGain::Mix),
            (NodeId(9_999), ModGain::Volume),
            (NodeId(9_999), ModGain::Mix),
            (NodeId(9_999), ModGain::Pan),
        ];
        for (node, gain) in has_not {
            assert_eq!(model.gain_of(node, gain), None, "{node:?} {gain:?}");
            assert_eq!(model.centre_of(Driven::Gain { node, gain }), None, "{node:?} {gain:?}");
            assert_eq!(model.kind_of(at, 0, Driven::Gain { node, gain }), None, "{node:?} {gain:?}");
        }
        // The lists say the same: the volume of the lane that has none is not offered.
        assert!(model.gain_targets(at).iter().all(|t| model.gain_of(t.node, t.gain).is_some()));
        assert!(model.gain_targets(at).iter().all(|t| !(t.node == empty_lane && t.gain == ModGain::Volume)));
    }

    #[test]
    fn the_cables_that_would_be_left_pointing_at_nothing_are_those_of_the_others_and_never_a_modulators_own() {
        let mut model = two_modulators();
        let first = model.free_modulators[0].node;
        let others = model.free_modulators[1].targets[0].edge;
        assert_eq!(model.cables_into(ModulatorLocation::Free, 0), vec![others], "the second one's cable to the first one's rate");
        assert_eq!(model.cables_into(ModulatorLocation::Free, 1), Vec::<EdgeId>::new(), "nothing drives the second");
        // (Only a model that was put together by hand has a cable that drives its own modulator: the controller refuses it.)
        let own = ModTargetSlot { edge: EdgeId(model.ids.next_edge), param_name: "rate".to_string(), aim: Driven::Setting { modulator: first, field: ModSetting::Rate }, depth: 0.5, param_min: 0.0, param_max: 1.0, effective: None };
        model.free_modulators[0].targets.push(own);
        assert_eq!(model.cables_into(ModulatorLocation::Free, 0), vec![others], "its own cable goes with it and is not one that is left");
    }

    #[test]
    fn an_ear_that_was_never_allocated_is_refused_by_the_check_and_the_last_one_that_was_is_not() {
        let mut model = two_modulators();
        assert_eq!(model.check_ids(), Ok(()));
        model.free_modulators[0].ear = Some(NodeId(model.ids.next - 1));
        assert_eq!(model.check_ids(), Ok(()), "a node that was made");
        model.free_modulators[0].ear = Some(NodeId(model.ids.next));
        assert!(model.check_ids().unwrap_err().contains("which was never allocated"));
    }

    fn lfo(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 2.0, retrigger: false, fade: 0.0, sync: 0 },
            targets,
            output: None,
            position: None,
            ear: None,
        }
    }

    fn adsr(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
            targets,
            output: None,
            position: None,
            ear: None,
        }
    }

    fn midi(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(0),
            source: ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false },
            targets,
            output: None,
            position: None,
            ear: None,
        }
    }

    #[test]
    fn mod_source_slot_converts_to_a_fresh_audio_source() {
        match mod_source_slot_to_graph(&ModSourceSlot::Lfo { waveform: LfoWaveform::Saw, rate: 3.5, retrigger: false, fade: 0.0, sync: 0 }) {
            ModSource::Lfo { waveform, rate, phase, retrigger, fade, sync, age } => {
                assert_eq!((waveform, rate, phase, retrigger, fade, sync, age), (LfoWaveform::Saw, 3.5, 0.0, false, 0.0, 0.0, f32::INFINITY));
            }
            other => panic!("{other:?}"),
        }
        // The key goes with it: a note that starts it again and a fade that is as long as it was set to; it rests at full until a note.
        match mod_source_slot_to_graph(&ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, retrigger: true, fade: 0.5, sync: 0 }) {
            ModSource::Lfo { retrigger, fade, age, .. } => assert_eq!((retrigger, fade, age), (true, 0.5, f32::INFINITY)),
            other => panic!("{other:?}"),
        }
        let env = ModSourceSlot::Envelope { attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 };
        match mod_source_slot_to_graph(&env) {
            ModSource::Envelope { attack, decay, sustain, release, state, level, notes_held } => {
                assert_eq!((attack, decay, sustain, release), (0.1, 0.2, 0.3, 0.4));
                assert_eq!((state, level, notes_held), (crate::plugin::chain::EnvState::Idle, 0.0, 0));
            }
            other => panic!("{other:?}"),
        }
        let learned = ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false };
        match mod_source_slot_to_graph(&learned) {
            ModSource::MidiLearn { source, value, learning } => {
                assert_eq!((source, value, learning), (Some(MidiBindSource::PitchBend), 0.5, false));
            }
            other => panic!("{other:?}"),
        }
        let armed = ModSourceSlot::MidiLearn { source: None, learning: true };
        assert!(matches!(
            mod_source_slot_to_graph(&armed),
            ModSource::MidiLearn { source: None, learning: true, .. }
        ));
    }

    #[test]
    fn a_bound_control_goes_to_the_audio_thread_resting_where_its_knob_or_wheel_does() {
        // (The wheel sends nothing until it is moved, and its centre is where it is: half of the range of its 14 bits.)
        // (A key rests at middle C, which is where it adds nothing: note 60 of 127.)
        for (source, rest) in [
            (MidiBindSource::Cc(74), 0.0),
            (MidiBindSource::PitchBend, 0.5),
            (MidiBindSource::Aftertouch, 0.0),
            (MidiBindSource::Velocity, 0.0),
            (MidiBindSource::Key, 60.0 / 127.0),
        ] {
            match mod_source_slot_to_graph(&ModSourceSlot::MidiLearn { source: Some(source), learning: false }) {
                ModSource::MidiLearn { value, .. } => assert_eq!(value, rest, "{source:?}"),
                other => panic!("{other:?}"),
            }
        }
        // A control that is waiting to learn is bound to nothing, and rests at 0 (the base shows through whatever its value).
        match mod_source_slot_to_graph(&ModSourceSlot::new_midi_learn()) {
            ModSource::MidiLearn { value, .. } => assert_eq!(value, 0.0),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_macro_goes_to_the_audio_thread_as_a_macro_of_its_value() {
        assert!(matches!(mod_source_slot_to_graph(&ModSourceSlot::macro_at(0.4)), ModSource::Macro { value } if value == 0.4));
        assert!(matches!(mod_source_slot_to_graph(&ModSourceSlot::new_macro()), ModSource::Macro { value } if value == 0.0));
    }

    #[test]
    fn a_new_macro_is_at_rest() {
        assert!(matches!(ModSourceSlot::new_macro(), ModSourceSlot::Macro { value, .. } if value == 0.0));
    }

    #[test]
    fn a_macro_is_labelled_with_its_value_and_its_node_is_called_a_macro_whatever_the_value() {
        assert_eq!(ModSourceSlot::macro_at(0.5).label(), "Macro 0.50");
        assert_eq!(ModSourceSlot::macro_at(0.0).label(), "Macro 0.00");
        assert_eq!(ModSourceSlot::macro_at(1.0).label(), "Macro 1.00");
        // (A node's name is its kind: a client is told the settings when they change, and a name would have to follow them.)
        assert_eq!(ModSourceSlot::macro_at(0.5).node_name(), "Macro");
        assert_eq!(ModSourceSlot::macro_at(0.9).node_name(), "Macro");
    }

    #[test]
    fn a_macro_that_is_called_something_is_called_so_by_its_node_and_in_the_tree_and_one_that_is_not_called_anything_is_a_macro() {
        let named = |name: &str| ModSourceSlot::Macro { value: 0.5, name: name.to_string(), expose: true };
        assert_eq!(named("Brightness").node_name(), "Brightness");
        assert_eq!(named("Brightness").label(), "Macro Brightness 0.50", "the kind first, as every other label has it, and then the name and the value");
        assert_eq!(named("").node_name(), "Macro");
        assert_eq!(named("").label(), "Macro 0.50", "no name is the label that a macro always had");
        // A new macro is not called anything and shows its knob; whether a box shows it is not what the audio thread is told.
        assert!(matches!(ModSourceSlot::new_macro(), ModSourceSlot::Macro { ref name, expose: true, .. } if name.is_empty()));
        let hidden = ModSourceSlot::Macro { value: 0.4, name: "Wet".to_string(), expose: false };
        assert!(matches!(mod_source_slot_to_graph(&hidden), ModSource::Macro { value } if value == 0.4));
    }

    #[test]
    fn new_modulators_start_as_a_sine_of_one_hertz_or_the_default_envelope() {
        use crate::plugin::chain::LfoWaveform;
        assert!(matches!(
            ModSourceSlot::new_lfo(),
            ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate, .. } if rate == 1.0
        ));
        assert!(matches!(
            ModSourceSlot::new_envelope(),
            ModSourceSlot::Envelope { attack, decay, sustain, release }
                if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
        ));
    }

    #[test]
    fn the_note_lengths_go_from_the_slowest_note_to_the_fastest_and_each_has_a_name_of_its_own_and_the_beats_it_is() {
        // The notes (the lengths that are neither dotted nor triplets) halve all the way: four bars to a thirty-second.
        let notes: Vec<f32> = SYNC_DIVISIONS.iter().filter(|(name, _)| !name.ends_with("dotted") && !name.ends_with("triplet")).map(|&(_, beats)| beats).collect();
        assert_eq!(notes, [16.0, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125]);
        // A dotted length is half as long again as its note, and a triplet two thirds of it.
        for &(name, beats) in &SYNC_DIVISIONS {
            for (suffix, times) in [(" dotted", 1.5), (" triplet", 2.0 / 3.0)] {
                if let Some(note) = name.strip_suffix(suffix) {
                    let &(_, of_the_note) = SYNC_DIVISIONS.iter().find(|&&(other, _)| other == note).unwrap_or_else(|| panic!("{name} has no note {note}"));
                    assert!((beats - of_the_note * times).abs() < 1e-6, "{name}: {beats} of {of_the_note}");
                }
            }
        }
        let mut names: Vec<&str> = SYNC_DIVISIONS.iter().map(|&(name, _)| name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SYNC_DIVISIONS.len(), "no two are called the same");
        // A sync is a place in the list from 1, and 0 is none. A bar is four beats, a dotted note is a half as long again and a triplet two thirds of it.
        assert_eq!([sync_name(0), sync_name(1), sync_name(8), sync_name(16)], ["free", "4 bars", "1/4", "1/32"]);
        assert_eq!([sync_beats(0), sync_beats(3), sync_beats(8), sync_beats(7), sync_beats(11), sync_beats(16)], [0.0, 4.0, 1.0, 1.5, 0.5, 0.125]);
        assert!((sync_beats(9) - 2.0 / 3.0).abs() < 1e-6 && (sync_beats(15) - 1.0 / 6.0).abs() < 1e-6, "a triplet of quarters and of sixteenths");
        // A place that the list has not is none.
        for beyond in [17, 99, usize::MAX] {
            assert_eq!((sync_name(beyond), sync_beats(beyond)), ("free", 0.0), "{beyond}");
        }
    }

    #[test]
    fn a_note_length_is_found_by_its_name_in_any_case_and_none_by_free_and_what_is_neither_is_not_found() {
        for place in 1..=SYNC_DIVISIONS.len() {
            assert_eq!(sync_from_name(sync_name(place)), Some(place), "{}", sync_name(place));
        }
        for (written, place) in [("1/8", 11), ("  1/8   Triplet ", 12), ("4 BARS", 1), ("1/16 Dotted", 13), ("free", 0), ("Off", 0), ("NONE", 0), ("", 0), ("  ", 0)] {
            assert_eq!(sync_from_name(written), Some(place), "{written:?}");
        }
        for unknown in ["1/7", "1/8t", "1/8.", "8 bars", "1 bars", "triplet", "1/8 triplets", "0"] {
            assert_eq!(sync_from_name(unknown), None, "{unknown:?}");
        }
    }

    /// What every note length comes to at some tempos, as the web client's tests read it (`web/src/client/fixtures/sync.json`): they pin the page's `rateAt` to the
    /// engine's `rate_at_tempo`. A row is a place in the list, its name, a tempo and the rate (cycles or steps a second) that the engine goes at then.
    /// `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_what_a_note_length_comes_to_is_current() {
        #[derive(serde::Serialize)]
        struct Row {
            place: usize,
            name: &'static str,
            bpm: f32,
            rate: f32,
        }
        let mut rows = Vec::new();
        for place in 1..=SYNC_DIVISIONS.len() {
            for bpm in [BPM_MIN, 90.0, DEFAULT_BPM, 133.0, BPM_MAX] {
                rows.push(Row { place, name: sync_name(place), bpm, rate: crate::plugin::chain::rate_at_tempo(bpm, sync_beats(place)) });
            }
        }
        // (The numbers are written as the wire writes them: the 32 bits, in the fewest digits.)
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "rows": crate::app::testutil::wire(&rows) })).unwrap() + "\n";
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/sync.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the note lengths come to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn what_a_modulator_that_follows_the_tempo_goes_at_is_what_its_note_length_and_the_tempo_make() {
        let at = |place: usize, bpm: f32| crate::plugin::chain::rate_at_tempo(bpm, sync_beats(place));
        let near = |got: f32, want: f32| (got - want).abs() < 1e-4;
        // A sixteenth note at 120 beats a minute is eight a second, a quarter note two, a bar a half of one; the tempo scales all of them.
        assert!(near(at(14, 120.0), 8.0));
        assert!(near(at(8, 120.0), 2.0));
        assert!(near(at(3, 120.0), 0.5));
        assert!(near(at(8, 90.0), 1.5));
        assert!(near(at(14, 60.0), 4.0));
        assert!(near(at(12, 120.0), 6.0), "a triplet of eighths: three in the time of two");
        assert!(near(at(10, 120.0), 2.0 * 4.0 / 3.0), "a dotted eighth: two in the time of three");
    }

    #[test]
    fn an_lfo_and_a_step_sequencer_that_follow_the_tempo_are_labelled_by_the_note_length_and_not_the_rate_they_keep() {
        let lfo = |sync| ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 4.0, retrigger: false, fade: 0.0, sync };
        assert_eq!(lfo(0).label(), "LFO 4.0Hz sine");
        assert_eq!(lfo(11).label(), "LFO 1/8 sine");
        assert_eq!(lfo(12).label(), "LFO 1/8 triplet sine");
        let mut steps = ModSourceSlot::new_steps();
        assert_eq!(steps.label(), "Steps 16 8.0Hz");
        if let ModSourceSlot::Steps { sync, .. } = &mut steps {
            *sync = 14;
        }
        assert_eq!(steps.label(), "Steps 16 1/16");
    }

    #[test]
    fn an_lfo_and_a_step_sequencer_go_to_the_engine_with_the_beats_of_their_note_length_and_none_for_none() {
        let beats = |source: &ModSourceSlot| match mod_source_slot_to_graph(source) {
            ModSource::Lfo { sync, .. } | ModSource::Steps { sync, .. } => sync,
            other => panic!("{other:?}"),
        };
        let lfo = |sync| ModSourceSlot::Lfo { waveform: LfoWaveform::Saw, rate: 3.5, retrigger: false, fade: 0.0, sync };
        assert_eq!([0, 3, 11, 16].map(|sync| beats(&lfo(sync))), [0.0, 4.0, 0.5, 0.125]);
        let mut steps = ModSourceSlot::new_steps();
        assert_eq!(beats(&steps), 0.0);
        if let ModSourceSlot::Steps { sync, .. } = &mut steps {
            *sync = 14;
        }
        assert_eq!(beats(&steps), 0.25);
        // (The rate is sent as it is, whether or not it is followed: the engine keeps it for when the modulator is free again.)
        assert!(matches!(mod_source_slot_to_graph(&lfo(11)), ModSource::Lfo { rate, .. } if rate == 3.5));
    }

    #[test]
    fn source_labels_are_what_the_tree_shows() {
        let label = |s: ModSourceSlot| s.label();
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Triangle, rate: 0.25, retrigger: false, fade: 0.0, sync: 0 }), "LFO 0.2Hz triangle");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 12.0, retrigger: false, fade: 0.0, sync: 0 }), "LFO 12.0Hz sine");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Random, rate: 4.0, retrigger: false, fade: 0.0, sync: 0 }), "LFO 4.0Hz random");
        assert_eq!(label(ModSourceSlot::Lfo { waveform: LfoWaveform::Smooth, rate: 0.5, retrigger: false, fade: 0.0, sync: 0 }), "LFO 0.5Hz smooth");
        assert_eq!(label(ModSourceSlot::Envelope { attack: 0.0, decay: 0.0, sustain: 0.0, release: 0.0 }), "ADSR");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false }), "CC 74");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::PitchBend), learning: false }), "Pitch Bend");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Aftertouch), learning: false }), "Aftertouch");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false }), "Velocity");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }), "Key");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: None, learning: true }), "MIDI (learning…)");
        assert_eq!(label(ModSourceSlot::MidiLearn { source: None, learning: false }), "MIDI (unbound)");
    }

    fn knob(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot { node: NodeId(7), source: ModSourceSlot::macro_at(0.25), targets, output: None, position: None, ear: None }
    }

    /// An LFO that a note starts again and that comes in over three quarters of a second.
    fn keyed_lfo(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot {
            node: NodeId(7),
            source: ModSourceSlot::Lfo { waveform: LfoWaveform::Sine, rate: 2.0, retrigger: true, fade: 0.75, sync: 0 },
            targets,
            output: None,
            position: None,
            ear: None,
        }
    }

    #[test]
    fn an_lfo_says_the_value_of_its_key_settings_and_a_row_makes_the_intent_that_sets_it() {
        use ModulatorField as F;
        let m = keyed_lfo(vec![]);
        assert_eq!([F::Retrigger, F::Fade].map(|field| m.source.setting(field)), [Some(1.0), Some(0.75)]);
        assert_eq!(ModSourceSlot::new_lfo().setting(F::Retrigger), Some(0.0), "a new one runs free");
        assert_eq!(ModSourceSlot::new_lfo().setting(F::Fade), Some(0.0), "and does not fade in");
        let set = |field, value| Some(Intent::SetModulatorField { node: NodeId(7), field, value });
        assert_eq!(m.row_intent(4, |on| 1.0 - on), set(F::Retrigger, 0.0), "the switch is turned over by the closure");
        assert_eq!(m.row_intent(5, |fade| fade + 0.25), set(F::Fade, 1.0));
        assert_eq!(m.row_intent(6, |value| value), None, "the separator");
        // The settings that are not an LFO's: an envelope, a control and a macro have neither, and a step sequencer has a retrigger of its own but no fade.
        for other in [adsr(vec![]), midi(vec![]), knob(vec![])] {
            for field in [F::Retrigger, F::Fade] {
                assert_eq!(other.source.setting(field), None, "{field:?}");
            }
        }
        assert_eq!(sequencer(vec![]).source.setting(F::Fade), None);
        assert_eq!(sequencer(vec![]).source.setting(F::Retrigger), Some(1.0));
    }

    fn sequencer(targets: Vec<ModTargetSlot>) -> ModulatorSlot {
        ModulatorSlot { node: NodeId(7), source: ModSourceSlot::new_steps(), targets, output: None, position: None, ear: None }
    }

    #[test]
    fn a_step_sequencer_says_the_value_of_its_settings_and_a_row_makes_the_intent_that_sets_it() {
        use ModulatorField as F;
        let m = sequencer(vec![]);
        assert_eq!(
            [F::Rate, F::Glide, F::Length, F::Retrigger, F::Sync].map(|field| m.source.setting(field)),
            [Some(8.0), Some(0.0), Some(16.0), Some(1.0), Some(0.0)],
            "(a new one is not tied to the tempo)"
        );
        for field in [F::Waveform, F::Attack, F::Decay, F::Sustain, F::Release, F::Value] {
            assert_eq!(m.source.setting(field), None, "{field:?}");
        }
        // The other kinds have none of its settings (an LFO has a switch of the same name, which is the key's: a note starts it again).
        for other in [lfo(vec![]), adsr(vec![]), midi(vec![]), knob(vec![])] {
            for field in [F::Glide, F::Length] {
                assert_eq!(other.source.setting(field), None, "{field:?}");
            }
        }
        for other in [adsr(vec![]), midi(vec![]), knob(vec![])] {
            assert_eq!(other.source.setting(F::Retrigger), None);
        }
        let set = |field, value| Some(Intent::SetModulatorField { node: NodeId(7), field, value });
        assert_eq!(m.row_intent(0, |rate| rate * 2.0), set(F::Rate, 16.0));
        assert_eq!(m.row_intent(1, |sync| sync + 14.0), set(F::Sync, 14.0), "a place in the list of note lengths");
        assert_eq!(m.row_intent(2, |glide| glide + 0.5), set(F::Glide, 0.5));
        assert_eq!(m.row_intent(3, |length| length - 1.0), set(F::Length, 15.0));
        assert_eq!(m.row_intent(4, |on| 1.0 - on), set(F::Retrigger, 0.0), "the switch is turned over by the closure");
        // A step is one of the values, and the intent has all of them: the one that is turned and the others as they are.
        let steps = |i: usize, value: f32| {
            let ModSourceSlot::Steps { mut values, .. } = ModSourceSlot::new_steps() else { unreachable!() };
            values[i] = value;
            Some(Intent::SetSteps { node: NodeId(7), values: values.to_vec() })
        };
        assert_eq!(m.row_intent(5 + 3, |value| value + 0.25), steps(3, -0.625 + 0.25));
        assert_eq!(m.row_intent(5 + 15, |_| -0.5), steps(15, -0.5), "a value that was typed");
        assert_eq!(m.row_intent(5 + 16, |value| value), None, "the separator");
    }

    #[test]
    fn a_step_sequencer_is_named_and_labelled_by_what_it_goes_through_and_how_fast() {
        let source = ModSourceSlot::new_steps();
        assert_eq!(source.node_name(), "Steps");
        assert_eq!(source.label(), "Steps 16 8.0Hz");
        let mut shorter = ModSourceSlot::new_steps();
        if let ModSourceSlot::Steps { length, rate, .. } = &mut shorter {
            *length = 5;
            *rate = 0.25;
        }
        assert_eq!(shorter.label(), "Steps 5 0.2Hz");
    }

    #[test]
    fn a_step_sequencer_goes_to_the_engine_with_all_that_it_has_and_at_its_first_step() {
        let mut values = [0.0; 16];
        values[..3].copy_from_slice(&[0.5, -0.25, 1.0]);
        let source = ModSourceSlot::Steps { values, length: 3, rate: 6.5, glide: 0.25, retrigger: false, sync: 0 };
        match mod_source_slot_to_graph(&source) {
            ModSource::Steps { values: sent, length, rate, glide, retrigger, sync, position } => {
                assert_eq!((sent, length, rate, glide, retrigger, sync, position), (values, 3, 6.5, 0.25, false, 0.0, 0.0));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_source_says_the_value_of_a_setting_it_has_and_nothing_of_one_it_has_not() {
        use ModulatorField as F;
        let triangle = ModSourceSlot::Lfo { waveform: LfoWaveform::Triangle, rate: 2.5, retrigger: false, fade: 0.0, sync: 0 };
        let envelope = ModSourceSlot::Envelope { attack: 0.1, decay: 0.2, sustain: 0.3, release: 0.4 };
        let control = ModSourceSlot::MidiLearn { source: None, learning: true };
        let all = [F::Rate, F::Waveform, F::Attack, F::Decay, F::Sustain, F::Release];
        let of = |source: &ModSourceSlot| all.map(|field| source.setting(field));
        assert_eq!(of(&triangle), [Some(2.5), Some(1.0), None, None, None, None]);
        assert_eq!(of(&envelope), [None, None, Some(0.1), Some(0.2), Some(0.3), Some(0.4)]);
        assert_eq!(of(&control), [None; 6]);
    }

    #[test]
    fn filters_default_fully_wet_everything_else_half_wet() {
        assert_eq!(default_effect_mix("builtin:filter"), 1.0);
        assert_eq!(default_effect_mix("builtin:drive"), 1.0);
        assert_eq!(default_effect_mix("builtin:math"), 1.0, "an insert, as the drive is: the sound that it makes is the point");
        assert_eq!(default_effect_mix("builtin:reverb"), DEFAULT_EFFECT_MIX);
        assert_eq!(default_effect_mix("lv2:http://calf.sourceforge.net/plugins/Reverb"), 0.5);
    }

    #[test]
    fn plugin_format_comes_from_the_id() {
        for (id, format) in [
            ("builtin:osc", "Built-in"),
            ("lv2:http://tytel.org/helm", "LV2"),
            ("http://tytel.org/helm", "LV2"),
            ("urn:example:plugin", "LV2"),
            ("clap:com.u-he.diva", "CLAP"),
            ("com.u-he.diva", "CLAP"),
            ("vst3:Pianoteq 9", "VST3"),
            ("./Reverb.lv2", "LV2"),
            ("./Synth.clap", "CLAP"),
            ("/Library/Audio/Plug-Ins/VST3/Pianoteq 9.vst3", "VST3"),
            ("mystery", "?"),
        ] {
            assert_eq!(format_from_id(id), format, "{id}");
        }
    }
}
