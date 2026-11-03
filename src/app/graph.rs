//! The session as a graph: what a front-end draws; see `docs/web-ui.md` §4.2.
//!
//! The engine is lane-shaped (instrument lanes with effect chains, submix groups,
//! modulators that drive parameters), the wire model is graph-shaped: nodes with
//! ports, and edges between the ports. `SessionModel::graph` projects the one into
//! the other, on demand. Nothing here is stored: a node is a lane, a group, an
//! effect or a modulator, with the `NodeId` the model gave it, and an edge is
//! either a link of the audio path (derived) or a modulation target, which has the
//! `EdgeId` the model gave it.
//!
//! ```text
//!   Instrument ──audio──▶ Effect ──audio──▶ Effect ──member──▶ Group ──audio──▶ Effect ──audio──▶ Master
//!   Modulator ──modulation(amount)──▶ a Param port of a plugin node, or a field of another Modulator
//! ```
//!
//! A node's `parent` is the node it sits in: a lane's effects and modulators sit in
//! the lane, a group's bus effects and modulators in the group, and a lane in the
//! group it is a member of. The master mix is a node of its own, `NodeId::MASTER`.
//!
//! The types are the wire format: JSON with camel-case names, and a tag (`kind`,
//! `type`) on each variant; the TypeScript is generated from them
//! (`app::protocol`).

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends the graph"))]

use serde::{Deserialize, Serialize};

use crate::app::model::{Driven, EdgeId, FieldTarget, ModGain, ModSetting, ModSourceSlot, ModulatorSlot, NodeId, ParamKind, ParamSlot, PatternState, PluginSlot, SessionModel, SourceKind};
use crate::plugin::chain::{CrossModField, LfoWaveform, MidiBindSource, ModTargetKind, ModulatorLocation};
use crate::session::{Feed, NodeUi, Route};

/// A session as a front-end draws it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// One node of the graph. What it is, and what it carries, is in `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Node {
    pub id: NodeId,
    /// The node this one sits in, if any.
    pub parent: Option<NodeId>,
    /// What to call it: the plugin's name, the group's name, what kind a modulator is. A
    /// modulator's settings are not in its name (they change while the node stays the same):
    /// they are in `kind`.
    pub name: String,
    /// Where a front-end draws the node, in the pixels of its canvas, if the session says (it is in the file as the
    /// node's `ui`); a node that has no place is laid out by the front-end, and has no `ui` on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "web", ts(optional))]
    pub ui: Option<NodeUi>,
    #[serde(flatten)]
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum NodeKind {
    /// An instrument lane: the instrument plugin (null while the lane is empty),
    /// the key range it answers to, its host-side output volume, whether it is muted or soloed, where its end goes (`route`: to its group's bus or the mix, or nowhere but its
    /// audio cables) and its pattern (null while it has none).
    Instrument { plugin: Option<Plugin>, range: Option<NoteRange>, transpose: i8, volume: f32, muted: bool, solo: bool, route: Route, pattern: Option<PatternInfo> },
    /// An effect in a lane's chain or in a group's bus chain, with its dry/wet mix and whether the stage before it feeds it (`feed`: `chain`, or `none` once the link into it is cut, when
    /// it is fed by the audio cables that end at it and by nothing else). An effect that has a second input (`side`: absent on the wire for the others) has a second port,
    /// `audioInSide`, that cables can end at, and says what it calls its two inputs.
    Effect {
        plugin: Plugin,
        mix: f32,
        feed: Feed,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "web", ts(optional))]
        side: Option<SideInfo>,
    },
    /// A modulator, lane-scoped, group-scoped or in no lane and no group (`parent` null). What it drives are its modulation edges. A modulator that is in no lane and no group has an ear
    /// (`hears`): the node of the lane or the group whose notes it hears, as the lane's instrument or the group's members are given them. None, which is absent on the wire, is every event,
    /// whatever its source, and so is a node that is gone; a lane's modulator and a group's hear their own lane or group and have none.
    Modulator {
        source: ModulatorSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "web", ts(optional))]
        hears: Option<NodeId>,
    },
    /// A submix group: its volume and stereo balance, whether it is muted or soloed, and where its end goes (`route`: to the master, or nowhere but its audio cables).
    Group { volume: f32, pan: f32, muted: bool, solo: bool, route: Route },
    /// The master mix: its volume, whether the limiter on it is in the sound (it holds the mix under its `ceiling`, in decibels below full scale, instead of letting it clip) and the ceiling.
    Master { volume: f32, limiter: bool, ceiling: f32 },
    /// A source of notes that a person put in the session: the keys, or a MIDI device. Its output of events is where the event cables start.
    Source { source: SourceInfo },
    /// A module that was added to the session as one thing (step 108): `name` is what it is called in the library, and `members` the lanes and the modulators that came with it, which a front-end draws as one
    /// box (the effects of a lane are in the lane: they are in the module with it). The nodes in it are in the graph as any are, and the cables to them: a module is the record of what came together, and has no
    /// part in the sound.
    Module { members: Vec<NodeId> },
}

/// What an effect that has a second audio input calls its two inputs (`Plugin::side_input`): what a front-end names the two handles by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SideInfo {
    /// The first input, which the chain feeds.
    pub main: String,
    /// The second, which only cables end at.
    pub side: String,
}

/// What a source of notes is, as a front-end sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SourceInfo {
    /// The computer's keys, the page's piano and keys, and every browser's.
    Keys,
    /// A MIDI device, by the name of its port or as much of it as tells it.
    Device { name: String },
}

impl From<SourceInfo> for SourceKind {
    fn from(info: SourceInfo) -> Self {
        match info {
            SourceInfo::Keys => SourceKind::Keys,
            SourceInfo::Device { name } => SourceKind::Device(name),
        }
    }
}

impl From<&SourceKind> for SourceInfo {
    fn from(kind: &SourceKind) -> Self {
        match kind {
            SourceKind::Keys => SourceInfo::Keys,
            SourceKind::Device(name) => SourceInfo::Device { name: name.clone() },
        }
    }
}

/// The pattern of a lane as a front-end sees it: its settings, how long it is and how many notes it has (not the notes: a front-end that draws them asks for them).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PatternInfo {
    /// A recording is going on: the metronome counts in first, and nothing is recorded before it has.
    pub recording: bool,
    /// Whether a held key plays it.
    pub enabled: bool,
    /// Whether it goes round again while the key is held, or plays once.
    pub looping: bool,
    /// Whether it is transposed by the degrees of the piano's scale, and not by semitones.
    pub in_key: bool,
    /// How long it is, and what the next recording is counted in.
    pub length_beats: f32,
    /// The tempo it was recorded at, which its length in frames is counted in.
    pub bpm: f32,
    /// How many notes it has.
    pub notes: u32,
}

/// A range of MIDI notes, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct NoteRange {
    pub low: u8,
    pub high: u8,
}

/// The plugin in an instrument or effect node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    /// What the plugin was loaded from (`builtin:osc`, a path, a URI).
    pub id: String,
    /// `Built-in`, `LV2`, `CLAP` or `VST3`.
    pub format: String,
    pub is_instrument: bool,
    pub params: Vec<Param>,
    pub presets: Vec<Preset>,
    /// The name of the preset last loaded, if any.
    pub current_preset: Option<String>,
    /// The `index` of the parameters a front-end shows as knobs on the node, in order; none is left to the front-end (the page shows the
    /// first few). Kept in the session; the engine does not use it.
    pub pinned: Vec<u32>,
    /// Whether the plugin does anything with the notes it is given (`Plugin::takes_notes`, step 99c): an audio input does not, and its lane has no key range, no pattern and no input of events. Left out of the wire when it
    /// does (every other plugin does), so that a front-end reads an absent one as true.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    #[cfg_attr(feature = "web", ts(as = "Option<bool>", optional))]
    pub takes_notes: bool,
}

/// What a plugin that does not say that it takes no notes does (`Plugin::takes_notes`): the serde default of that field.
fn yes() -> bool {
    true
}

/// Whether `value` is true, for a field that is left out of the wire while it is (`Plugin::takes_notes`).
fn is_yes(value: &bool) -> bool {
    *value
}

/// A parameter of a plugin, with the value the user set (modulation is not in it:
/// that is telemetry).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Param {
    /// The plugin's own index for it, which the intents and the ports name.
    pub index: u32,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub value: f32,
    /// The names of the values of an enum parameter (the value is an index into
    /// them); null for a number.
    pub labels: Option<Vec<String>>,
    pub unit: Option<String>,
    pub group: Vec<String>,
    pub step_count: u32,
    /// When the parameter has an effect at all, for one that has it only sometimes (step 95b: the Math's `bits` only in its logic modes, its carrier's `pitch` only while nothing is patched
    /// at its second input); absent for one that always does. A front-end works out from the values and the cables whether it has one now, and dims the knob if it has not (`unusedBecause` in the page).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "web", ts(optional))]
    pub used_when: Option<crate::plugin::UsedWhen>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Preset {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ModulatorSource {
    /// An LFO: its waveform and rate in Hz, whether a note starts it again at the start of its wave, the seconds it takes to come in from nothing after a note (0 for none) and the note
    /// length of the tempo that a cycle of it is in place of the rate: 0 for none, or the place of the note length in `SYNC_DIVISIONS`, from 1 (the rate is kept for when it is none again).
    Lfo { waveform: Waveform, rate: f32, retrigger: bool, fade: f32, sync: u32 },
    Envelope { attack: f32, decay: f32, sustain: f32, release: f32 },
    /// A hardware control. `control` is null until one is bound; `learning` is
    /// true while it waits for the next control to come in.
    MidiLearn { control: Option<MidiControl>, learning: bool },
    /// A knob: a number that a person sets, 0 to 1.
    Macro { value: f32 },
    /// A step sequencer: all of its `MAX_STEPS` values, -1 to 1, the first `length` of them in use, how many steps it goes through a second, how much of each is
    /// spent going to the next one's value (0 to 1), whether a note starts it again at the first step, and the note length of the tempo that a step of it is in place of the rate (as
    /// for an LFO: 0 for none).
    Steps { values: Vec<f32>, length: u32, rate: f32, glide: f32, retrigger: bool, sync: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Waveform {
    Sine,
    Triangle,
    Saw,
    Square,
    /// A number of its own for every cycle, held for the whole of it.
    Random,
    /// The numbers of `Random`, glided between.
    Smooth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MidiControl {
    Cc { number: u8 },
    PitchBend,
    Aftertouch,
    /// The velocity of the last note of the lane.
    Velocity,
    /// The number of the last note of the lane.
    Key,
}

/// A connection from a port of one node to a port of another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Edge {
    pub from: Endpoint,
    pub to: Endpoint,
    #[serde(flatten)]
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EdgeKind {
    /// A link of the audio path: instrument to its first effect, effect to effect,
    /// the end of a chain to the master. Derived; there is nothing to edit.
    Audio,
    /// A lane feeding the group it is a member of. Derived.
    Member,
    /// A modulator driving a parameter or a modulator field: a modulation target.
    /// `amount` is its depth, -1 to 1.
    Modulation { id: EdgeId, amount: f32 },
    /// An audio cable that a person drew: a send from an output of audio (an instrument's, an effect's, a group's) to an input of audio (an effect's, a group's bus, the
    /// master's), at the level `gain`. It is added to what the input has already, so the links of the audio path stay as they are.
    Cable { id: EdgeId, gain: f32 },
    /// An event cable that a person drew: the notes of a source go to a lane, which then hears that source and the others that cables bring it, and no other. It has no level.
    Events { id: EdgeId },
    /// A source feeding a lane that no event cable ends at, which hears every source. Derived; there is nothing to edit (draw a cable to the lane, and it hears the sources
    /// that its cables start at). There are none while the session has no source node.
    Hears,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct Endpoint {
    pub node: NodeId,
    pub port: Port,
}

/// Where on a node an edge attaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Port {
    /// What a node takes in (an effect, a group, the master).
    AudioIn,
    /// What an effect that has a second input takes in there (`NodeKind::Effect::side`): where an audio cable ends beside the one that the chain feeds.
    AudioInSide,
    /// What a node puts out (an instrument, an effect, a group).
    AudioOut,
    /// What a source of notes puts out: where an event cable starts.
    EventsOut,
    /// What a lane takes in: where an event cable ends.
    EventsIn,
    /// A modulator's output.
    ModOut,
    /// A parameter of the plugin in a node, by the plugin's own index.
    Param { index: u32 },
    /// The output volume that the host keeps for a lane (before its effects) or a group (step 97): where a cable from a modulator can end, beside the plugin's parameters.
    Volume,
    /// The dry/wet mix that the host keeps for an effect (step 97).
    Mix,
    /// The stereo balance that the host keeps for a group (step 97).
    Pan,
    /// A LFO's rate.
    Rate,
    /// An envelope's attack, decay, sustain and release.
    Attack,
    Decay,
    Sustain,
    Release,
    /// The depth of a modulation edge that starts at this modulator.
    Depth { edge: EdgeId },
}

impl Port {
    /// The port that a cable from a modulator ends at to drive the host-side value `gain` of a node.
    pub fn of_gain(gain: ModGain) -> Port {
        match gain {
            ModGain::Volume => Port::Volume,
            ModGain::Mix => Port::Mix,
            ModGain::Pan => Port::Pan,
        }
    }

    /// The host-side value that this port is, if it is one.
    pub fn gain(self) -> Option<ModGain> {
        match self {
            Port::Volume => Some(ModGain::Volume),
            Port::Mix => Some(ModGain::Mix),
            Port::Pan => Some(ModGain::Pan),
            _ => None,
        }
    }
}

impl crate::app::model::AudioCable {
    /// The port that the cable starts at and the one it ends at, as the graph says them: an output of audio of what it starts at, an input of audio of what it ends at (the
    /// master's for the mix).
    pub fn endpoints(&self) -> (Endpoint, Endpoint) {
        use crate::plugin::chain::{CableFrom, CableTo};
        let from = match self.from {
            CableFrom::Instrument(node) | CableFrom::Effect(node) | CableFrom::Group(node) => node,
        };
        let (to, port) = match self.to {
            CableTo::Effect(node) | CableTo::GroupBus(node) => (node, Port::AudioIn),
            CableTo::EffectSide(node) => (node, Port::AudioInSide),
            CableTo::Master => (NodeId::MASTER, Port::AudioIn),
        };
        (Endpoint { node: from, port: Port::AudioOut }, Endpoint { node: to, port })
    }
}

impl FieldTarget {
    /// The port of its modulator's node that a cable to it ends at.
    pub fn port(&self) -> Port {
        match (self.field, self.edge) {
            (CrossModField::Rate, _) => Port::Rate,
            (CrossModField::Attack, _) => Port::Attack,
            (CrossModField::Decay, _) => Port::Decay,
            (CrossModField::Sustain, _) => Port::Sustain,
            (CrossModField::Release, _) => Port::Release,
            (CrossModField::Depth(_), Some(edge)) => Port::Depth { edge },
            // (`field_targets` names the cable of every depth.)
            (CrossModField::Depth(_), None) => unreachable!("the depth of a cable names the cable"),
        }
    }
}

impl From<LfoWaveform> for Waveform {
    fn from(waveform: LfoWaveform) -> Self {
        match waveform {
            LfoWaveform::Sine => Waveform::Sine,
            LfoWaveform::Triangle => Waveform::Triangle,
            LfoWaveform::Saw => Waveform::Saw,
            LfoWaveform::Square => Waveform::Square,
            LfoWaveform::Random => Waveform::Random,
            LfoWaveform::Smooth => Waveform::Smooth,
        }
    }
}

impl From<MidiBindSource> for MidiControl {
    fn from(source: MidiBindSource) -> Self {
        match source {
            MidiBindSource::Cc(number) => MidiControl::Cc { number },
            MidiBindSource::PitchBend => MidiControl::PitchBend,
            MidiBindSource::Aftertouch => MidiControl::Aftertouch,
            MidiBindSource::Velocity => MidiControl::Velocity,
            MidiBindSource::Key => MidiControl::Key,
        }
    }
}

impl SessionModel {
    /// The session as a graph: the sources of notes first, then groups (each followed by its bus effects and
    /// modulators), then lanes (each followed by its effects and modulators), then the modulators that are in no lane and
    /// no group (they have no parent), then the modules that were added as one thing, then the master. The edges are the audio path, the sources' links to the lanes that hear every source, the audio cables, the event cables, then the
    /// modulation targets in the order of the nodes that own them: the groups' modulators', the lanes' and the free ones'.
    pub fn graph(&self) -> Graph {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // The sources of notes come first: the notes go from them to the lanes.
        nodes.extend(self.sources.iter().map(|source| Node {
            id: source.node,
            parent: None,
            name: source.kind.display_name(),
            ui: None,
            kind: NodeKind::Source { source: SourceInfo::from(&source.kind) },
        }));

        for (g, group) in self.groups.iter().enumerate() {
            nodes.push(Node {
                id: group.node,
                parent: None,
                name: group.display_name(g),
                ui: None,
                kind: NodeKind::Group { volume: group.volume, pan: group.pan, muted: group.muted, solo: group.solo, route: group.route },
            });
            nodes.extend(group.effects.iter().map(|fx| effect_node(fx, group.node)));
            // Out of the group, through its bus effects, to the master (unless its way out is cut: then the chain ends at its last effect).
            audio_chain(&mut edges, group.node, group.effects.iter().map(|fx| (fx.node, fx.fed)), (group.route == Route::Out).then_some((NodeId::MASTER, EdgeKind::Audio)));
        }

        for lane in &self.instruments {
            // A lane whose group is gone (the model keeps them in step, but a front-end
            // must not crash if it did not) goes to the master like an ungrouped one.
            let group = lane.group.and_then(|g| self.groups.get(g));
            nodes.push(Node {
                id: lane.node,
                parent: group.map(|g| g.node),
                name: lane.instrument.as_ref().map_or_else(|| "(empty)".to_string(), |plugin| plugin.name.clone()),
                ui: None,
                kind: NodeKind::Instrument {
                    plugin: lane.instrument.as_ref().map(plugin),
                    range: lane.range.map(|(low, high)| NoteRange { low, high }),
                    transpose: lane.transpose,
                    volume: lane.volume,
                    muted: lane.muted,
                    solo: lane.solo,
                    route: lane.route,
                    pattern: lane.pattern.as_ref().map(PatternState::info),
                },
            });
            nodes.extend(lane.effects.iter().map(|fx| effect_node(fx, lane.node)));
            // Out of the instrument, through the lane's effects, to its group or the master (unless its way out is cut).
            let sink = match group {
                Some(group) => (group.node, EdgeKind::Member),
                None => (NodeId::MASTER, EdgeKind::Audio),
            };
            audio_chain(&mut edges, lane.node, lane.effects.iter().map(|fx| (fx.node, fx.fed)), (lane.route == Route::Out).then_some(sink));
        }

        // The modulators belong to no lane and no group: they have no parent (what they hear is `hears`).
        nodes.extend(self.free_modulators.iter().map(|m| modulator_node(m, None, self.ear_of(m))));

        // The modules that were added as one thing come after the modulators: each a node of its own, with the nodes that came with it.
        nodes.extend(self.modules.iter().map(|module| Node { id: module.node, parent: None, name: module.name.clone(), ui: None, kind: NodeKind::Module { members: module.members.clone() } }));

        nodes.push(Node { id: NodeId::MASTER, parent: None, name: "Master".to_string(), ui: None, kind: NodeKind::Master { volume: self.master.volume, limiter: self.master.limiter, ceiling: self.master.ceiling } });
        // Where each of these nodes is drawn, if it has been placed (the table also has the places of nodes that have gone).
        for node in &mut nodes {
            node.ui = self.ui.get(&node.id).copied();
        }
        self.hears_edges(&mut edges);
        self.audio_cable_edges(&mut edges);
        self.event_cable_edges(&mut edges);
        self.modulation_edges(&mut edges);
        Graph { nodes, edges }
    }

    /// One edge from each source to each lane that no event cable ends at, which hears every source: the sources in order, and for each the lanes in order. A lane whose instrument takes no notes
    /// (an audio input) has no input of events, and hears nothing that is drawn.
    fn hears_edges(&self, edges: &mut Vec<Edge>) {
        for source in &self.sources {
            for (inst, lane) in self.instruments.iter().enumerate() {
                if self.lane_takes_notes(inst) && !self.event_cables.iter().any(|cable| cable.to == lane.node) {
                    edges.push(Edge { from: Endpoint { node: source.node, port: Port::EventsOut }, to: Endpoint { node: lane.node, port: Port::EventsIn }, kind: EdgeKind::Hears });
                }
            }
        }
    }

    /// One edge for each event cable, in the order of their ids.
    fn event_cable_edges(&self, edges: &mut Vec<Edge>) {
        edges.extend(self.event_cables.iter().map(|cable| Edge {
            from: Endpoint { node: cable.from, port: Port::EventsOut },
            to: Endpoint { node: cable.to, port: Port::EventsIn },
            kind: EdgeKind::Events { id: cable.id },
        }));
    }

    /// One edge for each audio cable, in the order of their ids.
    fn audio_cable_edges(&self, edges: &mut Vec<Edge>) {
        edges.extend(self.cables.iter().map(|cable| {
            let (from, to) = cable.endpoints();
            Edge { from, to, kind: EdgeKind::Cable { id: cable.id, gain: cable.gain } }
        }));
    }

    /// One edge for each modulation target that still has something to drive.
    fn modulation_edges(&self, edges: &mut Vec<Edge>) {
        for (at, rack) in self.racks() {
            for (index, modulator) in rack.iter().enumerate() {
                for target in &modulator.targets {
                    // (What a rack does not reach is not drawn, as it is not told to the audio thread: the model has no such cable, and a test that makes one sees it.)
                    if self.kind_of(at, index, target.aim).is_none() {
                        continue;
                    }
                    if let Some(to) = self.endpoint_of_aim(target.aim) {
                        edges.push(Edge {
                            from: Endpoint { node: modulator.node, port: Port::ModOut },
                            to,
                            kind: EdgeKind::Modulation { id: target.edge, amount: target.depth },
                        });
                    }
                }
            }
        }
    }

    /// Where `aim` points, as a port of a node: how `AddModEdge` names it. `None` if what it points at is not there.
    pub fn endpoint_of_aim(&self, aim: Driven) -> Option<Endpoint> {
        match aim {
            Driven::Param { plugin, index } => Some(Endpoint { node: plugin, port: Port::Param { index } }),
            Driven::Setting { modulator, field } => {
                let port = match field {
                    ModSetting::Rate => Port::Rate,
                    ModSetting::Attack => Port::Attack,
                    ModSetting::Decay => Port::Decay,
                    ModSetting::Sustain => Port::Sustain,
                    ModSetting::Release => Port::Release,
                };
                Some(Endpoint { node: modulator, port })
            }
            Driven::Depth(edge) => {
                let (at, index, _) = self.locate_edge(edge)?;
                Some(Endpoint { node: self.modulators(at)?.get(index)?.node, port: Port::Depth { edge } })
            }
            Driven::Gain { node, gain } => Some(Endpoint { node, port: Port::of_gain(gain) }),
        }
    }

    /// Where a target of kind `kind` of a modulator in the rack at `at` points, as a port of a node: how `AddModEdge` names it. `None` if there
    /// is no such rack, or what the target points at is not there.
    pub fn endpoint_of(&self, at: ModulatorLocation, kind: &ModTargetKind) -> Option<Endpoint> {
        self.aim(at, self.modulators(at)?, kind)
    }

    /// Where a target of a modulator in the rack `rack` at `at` points, as a port;
    /// `None` if what it points at is not there (or is not something that rack can
    /// drive).
    pub(super) fn aim(&self, at: ModulatorLocation, rack: &[ModulatorSlot], kind: &ModTargetKind) -> Option<Endpoint> {
        use ModTargetKind as K;
        let ModulatorLocation::Free = at;
        let sibling = |mod_index: usize, port: Port| rack.get(mod_index).map(|m| Endpoint { node: m.node, port });
        match kind {
            K::FreeParam { plugin, param_index } => self.plugin_slot(*plugin).map(|_| Endpoint { node: *plugin, port: Port::Param { index: *param_index } }),
            K::Gain { node, gain } => self.gain_of(*node, *gain).map(|_| Endpoint { node: *node, port: Port::of_gain(*gain) }),
            K::ModulatorRate { mod_index } => sibling(*mod_index, Port::Rate),
            K::ModulatorAttack { mod_index } => sibling(*mod_index, Port::Attack),
            K::ModulatorDecay { mod_index } => sibling(*mod_index, Port::Decay),
            K::ModulatorSustain { mod_index } => sibling(*mod_index, Port::Sustain),
            K::ModulatorRelease { mod_index } => sibling(*mod_index, Port::Release),
            K::ModulatorDepth { mod_index, target_index } => {
                let modulator = rack.get(*mod_index)?;
                Some(Endpoint { node: modulator.node, port: Port::Depth { edge: modulator.targets.get(*target_index)?.edge } })
            }
        }
    }
}

/// The edges of a serial audio chain: out of `first`, in and out of each of
/// `through` (an effect, and whether the stage before it feeds it), and into the node of `sink`, if there is one, by a link of the kind that it says (a chain whose way out is cut has none).
/// The link into an effect that is not fed is not there, and the chain goes on from the effect all the same.
fn audio_chain(edges: &mut Vec<Edge>, first: NodeId, through: impl Iterator<Item = (NodeId, bool)>, sink: Option<(NodeId, EdgeKind)>) {
    let mut from = first;
    for (node, fed) in through {
        if fed {
            edges.push(Edge {
                from: Endpoint { node: from, port: Port::AudioOut },
                to: Endpoint { node, port: Port::AudioIn },
                kind: EdgeKind::Audio,
            });
        }
        from = node;
    }
    if let Some((sink, last)) = sink {
        edges.push(Edge {
            from: Endpoint { node: from, port: Port::AudioOut },
            to: Endpoint { node: sink, port: Port::AudioIn },
            kind: last,
        });
    }
}

fn effect_node(slot: &PluginSlot, parent: NodeId) -> Node {
    let feed = if slot.fed { Feed::Chain } else { Feed::None };
    let side = slot.side.map(|side| SideInfo { main: side.main.to_string(), side: side.side.to_string() });
    Node { id: slot.node, parent: Some(parent), name: slot.name.clone(), ui: None, kind: NodeKind::Effect { plugin: plugin(slot), mix: slot.mix, feed, side } }
}

fn modulator_node(modulator: &ModulatorSlot, parent: Option<NodeId>, hears: Option<NodeId>) -> Node {
    let source = match &modulator.source {
        ModSourceSlot::Lfo { waveform, rate, retrigger, fade, sync } => {
            ModulatorSource::Lfo { waveform: (*waveform).into(), rate: *rate, retrigger: *retrigger, fade: *fade, sync: *sync as u32 }
        }
        ModSourceSlot::Envelope { attack, decay, sustain, release } => {
            ModulatorSource::Envelope { attack: *attack, decay: *decay, sustain: *sustain, release: *release }
        }
        ModSourceSlot::MidiLearn { source, learning } => {
            ModulatorSource::MidiLearn { control: source.map(MidiControl::from), learning: *learning }
        }
        ModSourceSlot::Macro { value } => ModulatorSource::Macro { value: *value },
        ModSourceSlot::Steps { values, length, rate, glide, retrigger, sync } => {
            ModulatorSource::Steps { values: values.to_vec(), length: *length as u32, rate: *rate, glide: *glide, retrigger: *retrigger, sync: *sync as u32 }
        }
    };
    Node { id: modulator.node, parent, name: modulator.source.node_name(), ui: None, kind: NodeKind::Modulator { source, hears } }
}

fn plugin(slot: &PluginSlot) -> Plugin {
    Plugin {
        id: slot.id.clone(),
        format: slot.format.clone(),
        is_instrument: slot.is_instrument,
        params: slot.params.iter().filter_map(param).collect(),
        presets: slot.presets.iter().map(|p| Preset { id: p.id.clone(), name: p.name.clone() }).collect(),
        current_preset: slot.current_preset.clone(),
        pinned: slot.pinned.clone(),
        takes_notes: slot.takes_notes,
    }
}

/// A plugin's parameter. A separator is a row of the TUI's parameter pane, not a
/// parameter, and has none.
fn param(slot: &ParamSlot) -> Option<Param> {
    let labels = match &slot.kind {
        ParamKind::Float => None,
        ParamKind::Enum(labels) => Some(labels.clone()),
        ParamKind::Separator => return None,
    };
    Some(Param {
        index: slot.index,
        name: slot.name.clone(),
        min: slot.min,
        max: slot.max,
        default: slot.default,
        value: slot.value,
        labels,
        unit: slot.unit.clone(),
        group: slot.group.clone(),
        step_count: slot.step_count,
        used_when: slot.used_when.clone(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::model::ModTargetSlot;
    use crate::app::testutil::{DISABLED_PATTERN, GROUPS, LEGACY, MIDI, PINNED, PLACED, SETTINGS, load};

    const OSC: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    fn out(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioOut }
    }

    fn into(node: NodeId) -> Endpoint {
        Endpoint { node, port: Port::AudioIn }
    }

    fn at(node: NodeId, port: Port) -> Endpoint {
        Endpoint { node, port }
    }

    fn param(node: NodeId, index: u32) -> Endpoint {
        at(node, Port::Param { index })
    }

    fn audio(from: Endpoint, to: Endpoint) -> Edge {
        Edge { from, to, kind: EdgeKind::Audio }
    }

    fn member(from: Endpoint, to: Endpoint) -> Edge {
        Edge { from, to, kind: EdgeKind::Member }
    }

    fn modulation(modulator: NodeId, to: Endpoint, id: EdgeId, amount: f32) -> Edge {
        Edge { from: at(modulator, Port::ModOut), to, kind: EdgeKind::Modulation { id, amount } }
    }

    fn cable_edge(from: Endpoint, to: Endpoint, id: u32, gain: f32) -> Edge {
        Edge { from, to, kind: EdgeKind::Cable { id: EdgeId(id), gain } }
    }

    #[test]
    fn an_audio_cable_is_an_edge_from_an_output_to_an_input_with_its_id_and_level_after_the_links_of_the_audio_path() {
        use crate::app::model::AudioCable;
        use crate::plugin::chain::{CableFrom, CableTo};
        let mut model = load(GROUPS).model;
        let (lane, group) = (model.instruments[0].node, model.groups[0].node);
        let bus = model.groups[0].effects[0].node;
        let fx = model.instruments[0].effects.first().map_or(bus, |fx| fx.node);
        let path = model.graph().edges;
        for (id, from, to, gain) in [
            (9, CableFrom::Group(group), CableTo::Master, 0.5),
            (4, CableFrom::Instrument(lane), CableTo::Effect(bus), 1.0),
            (6, CableFrom::Effect(bus), CableTo::GroupBus(group), 0.25),
            (7, CableFrom::Effect(fx), CableTo::Effect(bus), 2.0),
        ] {
            model.insert_audio_cable(AudioCable { id: EdgeId(id), from, to, gain });
        }
        let edges = model.graph().edges;
        // (The scene has modulation edges, which come after the audio ones, cables included.)
        let links = path.iter().take_while(|edge| !matches!(edge.kind, EdgeKind::Modulation { .. })).count();
        assert!(links < path.len(), "the scene has modulation edges");
        assert_eq!(edges[..links], path[..links], "the links of the audio path are as they were");
        assert_eq!(edges[links + 4..], path[links..], "and so are the modulation edges, after the cables");
        assert_eq!(
            edges[links..links + 4],
            [
                cable_edge(out(lane), into(bus), 4, 1.0),
                cable_edge(out(bus), into(group), 6, 0.25),
                cable_edge(out(fx), into(bus), 7, 2.0),
                cable_edge(out(group), into(NodeId::MASTER), 9, 0.5),
            ],
            "in the order of their ids, each from the output of what it starts at to the input of what it ends at"
        );
        assert_eq!(
            serde_json::to_value(&edges[links]).unwrap(),
            json!({"from": {"node": lane.0, "port": {"type": "audioOut"}}, "to": {"node": bus.0, "port": {"type": "audioIn"}}, "kind": "cable", "id": 4, "gain": 1.0})
        );
    }

    #[test]
    fn a_free_modulator_is_a_node_with_no_parent_after_the_lanes_and_its_cables_are_edges_from_it_to_the_ports_they_drive() {
        let model = load(crate::app::testutil::FREE_MODULATORS).model;
        let graph = model.graph();
        let modulators: Vec<&Node> = graph.nodes.iter().filter(|node| matches!(node.kind, NodeKind::Modulator { .. })).collect();
        assert_eq!(modulators.len(), 2);
        assert!(modulators.iter().all(|node| node.parent.is_none()), "in no lane and no group");
        assert_eq!(modulators.iter().map(|node| node.id).collect::<Vec<_>>(), model.free_modulators.iter().map(|m| m.node).collect::<Vec<_>>());
        let position = |id: NodeId| graph.nodes.iter().position(|node| node.id == id).unwrap();
        assert!(position(modulators[0].id) > position(model.instruments[1].effects[0].node), "after the lanes' nodes");
        assert_eq!(graph.nodes.last().map(|node| node.id), Some(NodeId::MASTER), "and before the master");
        assert_eq!(
            serde_json::to_value(modulators[0]).unwrap()["parent"],
            serde_json::Value::Null,
            "a client reads the lack of a parent as a node of its own"
        );
        let (osc, reverb, bus, filter) = (model.instruments[0].node, model.instruments[0].effects[0].node, model.groups[0].effects[0].node, model.instruments[1].effects[0].node);
        let (lfo, envelope) = (&model.free_modulators[0], &model.free_modulators[1]);
        let index = |slot: &PluginSlot, name: &str| slot.params.iter().find(|row| row.name == name).unwrap().index;
        let cable = |from: &crate::app::model::ModulatorSlot, n: usize| (from.targets[n].edge, from.targets[n].depth);
        let edge = |from: &crate::app::model::ModulatorSlot, n: usize, to: Endpoint| modulation(from.node, to, cable(from, n).0, cable(from, n).1);
        let modulation_edges: Vec<Edge> = graph.edges.iter().filter(|edge| matches!(edge.kind, EdgeKind::Modulation { .. })).cloned().collect();
        assert_eq!(
            modulation_edges,
            [
                edge(lfo, 0, param(osc, index(model.instruments[0].instrument.as_ref().unwrap(), "detune"))),
                edge(lfo, 1, param(reverb, index(&model.instruments[0].effects[0], "damping"))),
                edge(lfo, 2, param(bus, index(&model.groups[0].effects[0], "cutoff"))),
                edge(lfo, 3, at(envelope.node, Port::Attack)),
                edge(envelope, 0, param(filter, index(&model.instruments[1].effects[0], "cutoff"))),
                edge(envelope, 1, at(lfo.node, Port::Depth { edge: lfo.targets[0].edge })),
            ],
            "a plugin's parameter by its node wherever the plugin is, and a setting or a depth of a sibling free modulator"
        );
    }

    #[test]
    fn what_a_free_modulator_is_told_to_aim_at_is_a_port_of_a_plugin_that_is_there() {
        use crate::plugin::chain::ModTargetKind as K;
        let model = load(crate::app::testutil::FREE_MODULATORS).model;
        let osc = model.instruments[0].node;
        assert_eq!(model.endpoint_of(ModulatorLocation::Free, &K::FreeParam { plugin: osc, param_index: 1 }), Some(param(osc, 1)));
        assert_eq!(model.endpoint_of(ModulatorLocation::Free, &K::FreeParam { plugin: NodeId(9_999), param_index: 1 }), None, "a node that is not there");
        assert_eq!(model.endpoint_of(ModulatorLocation::Free, &K::FreeParam { plugin: model.free_modulators[0].node, param_index: 0 }), None, "a node that is not a plugin");
    }

    #[test]
    fn a_free_modulator_has_no_audio_ports_and_its_node_says_what_it_is() {
        let model = load("[[modulator]]\ntype = \"macro\"\nvalue = 0.5\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let graph = model.graph();
        let node = graph.nodes.iter().find(|node| matches!(node.kind, NodeKind::Modulator { .. })).unwrap();
        assert_eq!(node.name, "Macro");
        assert!(matches!(&node.kind, NodeKind::Modulator { source: ModulatorSource::Macro { value }, .. } if *value == 0.5));
        assert!(graph.edges.iter().all(|edge| edge.from.node != node.id && edge.to.node != node.id), "a modulator with no cable has no edge");
    }

    fn events(from: Endpoint, to: Endpoint, id: u32) -> Edge {
        Edge { from, to, kind: EdgeKind::Events { id: EdgeId(id) } }
    }

    fn hears(from: Endpoint, to: Endpoint) -> Edge {
        Edge { from, to, kind: EdgeKind::Hears }
    }

    fn notes_out(node: NodeId) -> Endpoint {
        at(node, Port::EventsOut)
    }

    fn notes_in(node: NodeId) -> Endpoint {
        at(node, Port::EventsIn)
    }

    #[test]
    fn a_session_with_no_source_has_no_node_of_one_and_no_edge_from_one() {
        for text in [crate::app::testutil::SENDS, GROUPS, OSC] {
            let graph = load(text).model.graph();
            assert!(graph.nodes.iter().all(|n| !matches!(n.kind, NodeKind::Source { .. })), "{text}");
            assert!(graph.edges.iter().all(|e| !matches!(e.kind, EdgeKind::Events { .. } | EdgeKind::Hears)), "{text}");
        }
    }

    #[test]
    fn the_sources_are_the_first_nodes_with_the_names_and_the_places_the_session_gave_them() {
        let model = load(crate::app::testutil::SOURCES).model;
        let n = crate::app::testutil::Routed::of(&model);
        let graph = model.graph();
        let first = &graph.nodes[..3];
        assert_eq!(first.iter().map(|node| (node.id, node.parent, node.name.as_str())).collect::<Vec<_>>(), [(n.keys, None, "Keys"), (n.devices[0], None, "Launchkey Mini MK4 37"), (n.devices[1], None, "Seaboard")]);
        assert_eq!(first.iter().map(|node| node.ui).collect::<Vec<_>>(), [Some(NodeUi { x: 0.0, y: 40.0 }), Some(NodeUi { x: 0.0, y: 300.5 }), None]);
        assert_eq!(
            first.iter().map(|node| node.kind.clone()).collect::<Vec<_>>(),
            [
                NodeKind::Source { source: SourceInfo::Keys },
                NodeKind::Source { source: SourceInfo::Device { name: "Launchkey Mini MK4 37".to_string() } },
                NodeKind::Source { source: SourceInfo::Device { name: "Seaboard".to_string() } },
            ]
        );
        assert_eq!(graph.nodes.len(), 3 + 3 + 1, "the sources, the lanes and the master, and nothing else");
        assert_eq!(
            serde_json::to_value(&graph.nodes[0]).unwrap(),
            json!({"id": n.keys.0, "parent": null, "name": "Keys", "ui": {"x": 0.0, "y": 40.0}, "kind": "source", "source": {"type": "keys"}})
        );
        assert_eq!(
            serde_json::to_value(&graph.nodes[2]).unwrap(),
            json!({"id": n.devices[1].0, "parent": null, "name": "Seaboard", "kind": "source", "source": {"type": "device", "name": "Seaboard"}})
        );
    }

    #[test]
    fn an_event_cable_is_an_edge_from_the_output_of_events_of_a_source_to_the_input_of_events_of_a_lane_and_a_lane_that_has_none_is_fed_by_every_source() {
        let model = load(crate::app::testutil::SOURCES).model;
        let n = crate::app::testutil::Routed::of(&model);
        let edges = model.graph().edges;
        let drawn: Vec<&Edge> = edges.iter().filter(|e| matches!(e.kind, EdgeKind::Events { .. })).collect();
        assert_eq!(
            drawn,
            [
                &events(notes_out(n.keys), notes_in(n.lane[0]), 0),
                &events(notes_out(n.devices[0]), notes_in(n.lane[1]), 1),
                &events(notes_out(n.devices[1]), notes_in(n.lane[1]), 2),
            ],
            "in the order of their ids"
        );
        let heard: Vec<&Edge> = edges.iter().filter(|e| matches!(e.kind, EdgeKind::Hears)).collect();
        assert_eq!(
            heard,
            [&hears(notes_out(n.keys), notes_in(n.lane[2])), &hears(notes_out(n.devices[0]), notes_in(n.lane[2])), &hears(notes_out(n.devices[1]), notes_in(n.lane[2]))],
            "lane 2 has no cable and every source feeds it: the sources in order, and for each the lanes that have none; lanes 0 and 1 have cables"
        );
        // The links come after the audio path and before the audio cables, which come before the cables of notes.
        let place = |kind: fn(&EdgeKind) -> bool| edges.iter().position(|e| kind(&e.kind)).unwrap();
        let (audio_path, heard_at, audio_cable, notes) = (
            place(|k| matches!(k, EdgeKind::Audio | EdgeKind::Member)),
            place(|k| matches!(k, EdgeKind::Hears)),
            place(|k| matches!(k, EdgeKind::Cable { .. })),
            place(|k| matches!(k, EdgeKind::Events { .. })),
        );
        assert!(audio_path < heard_at && heard_at < audio_cable && audio_cable < notes, "{audio_path} {heard_at} {audio_cable} {notes}");
        assert_eq!(
            serde_json::to_value(drawn[1]).unwrap(),
            json!({"from": {"node": n.devices[0].0, "port": {"type": "eventsOut"}}, "to": {"node": n.lane[1].0, "port": {"type": "eventsIn"}}, "kind": "events", "id": 1})
        );
        assert_eq!(
            serde_json::to_value(heard[0]).unwrap(),
            json!({"from": {"node": n.keys.0, "port": {"type": "eventsOut"}}, "to": {"node": n.lane[2].0, "port": {"type": "eventsIn"}}, "kind": "hears"})
        );
    }

    #[test]
    fn a_session_with_the_keys_and_no_cable_has_the_keys_feed_every_lane() {
        let model = load("[keys]\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let keys = model.sources[0].node;
        let lanes = [model.instruments[0].node, model.instruments[1].node];
        let edges = model.graph().edges;
        let notes: Vec<&Edge> = edges.iter().filter(|e| matches!(e.kind, EdgeKind::Hears | EdgeKind::Events { .. })).collect();
        assert_eq!(notes, [&hears(notes_out(keys), notes_in(lanes[0])), &hears(notes_out(keys), notes_in(lanes[1]))]);
    }

    #[test]
    fn a_source_and_a_cable_do_not_change_the_edges_of_the_audio_path() {
        let plain = load(&format!("{OSC}{OSC}")).model.graph();
        let routed = load(&format!("[keys]\n{OSC}{OSC}[[cable]]\nfrom = \"keys\"\nto = \"instrument[1]\"\n")).model.graph();
        let links = |graph: &Graph| graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Audio | EdgeKind::Member)).cloned().collect::<Vec<_>>();
        // (The nodes' ids are not the same: the keys take the next, after the lanes' and the master's has no id of the series.)
        assert_eq!(links(&plain), links(&routed));
    }

    fn node(graph: &Graph, id: NodeId) -> &Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("no node {id:?} in {graph:#?}"))
    }

    fn plugin_of(node: &Node) -> &Plugin {
        match &node.kind {
            NodeKind::Instrument { plugin: Some(plugin), .. } | NodeKind::Effect { plugin, .. } => plugin,
            other => panic!("{other:?} has no plugin"),
        }
    }

    fn the_param<'a>(plugin: &'a Plugin, name: &str) -> &'a Param {
        plugin.params.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no parameter {name}"))
    }

    #[test]
    fn the_plugin_of_a_node_says_which_of_its_parameters_are_pinned_to_it_in_order() {
        let model = load(PINNED).model;
        let graph = model.graph();
        let (group, lane, plain) = (&model.groups[0], &model.instruments[0], &model.instruments[1]);
        let osc = plugin_of(node(&graph, lane.node));
        let index = |plugin: &Plugin, name: &str| the_param(plugin, name).index;
        assert_eq!(osc.pinned, [index(osc, "sustain"), index(osc, "detune"), index(osc, "waveform")]);
        let filter = plugin_of(node(&graph, lane.effects[0].node));
        assert_eq!(filter.pinned, [index(filter, "resonance"), index(filter, "cutoff")]);
        let bus = plugin_of(node(&graph, group.effects[0].node));
        assert_eq!(bus.pinned, [index(bus, "drive"), index(bus, "cutoff")]);
        assert!(plugin_of(node(&graph, plain.node)).pinned.is_empty());
        // (And on the wire it is `pinned`, a list, for every plugin.)
        let json = serde_json::to_value(osc).unwrap();
        assert_eq!(json["pinned"], json!([index(osc, "sustain"), index(osc, "detune"), index(osc, "waveform")]));
        assert_eq!(serde_json::to_value(plugin_of(node(&graph, plain.node))).unwrap()["pinned"], json!([]));
    }

    // -- Nodes --

    #[test]
    fn a_lone_instrument_is_a_node_that_feeds_the_master() {
        let model = load(OSC).model;
        let lane = model.instruments[0].node;
        let graph = model.graph();
        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [lane, NodeId::MASTER]);
        assert_eq!(graph.edges, [audio(out(lane), into(NodeId::MASTER))]);
        assert_eq!(
            graph.nodes[1],
            Node { id: NodeId::MASTER, parent: None, name: "Master".to_string(), ui: None, kind: NodeKind::Master { volume: 1.0, limiter: true, ceiling: -1.0 } },
            "the output mix says its volume and its limiter, which is in the sound unless a session says it is not"
        );

        let osc = &graph.nodes[0];
        assert_eq!((osc.parent, osc.name.as_str()), (None, "Oscillator"));
        let NodeKind::Instrument { plugin: Some(plugin), range, transpose, volume, .. } = &osc.kind else {
            panic!("{:?}", osc.kind)
        };
        assert_eq!((*range, *transpose, *volume), (None, 0, 1.0));
        assert_eq!((plugin.id.as_str(), plugin.format.as_str(), plugin.is_instrument), ("builtin:osc", "Built-in", true));
        assert_eq!((plugin.presets.len(), plugin.current_preset.as_deref()), (0, None));
        let names: Vec<&str> = plugin.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "unison", "spread", "width", "pulse", "fm", "fm_ratio", "sub", "sync", "drift", "velocity", "bend", "quantize"]);
    }

    #[test]
    fn a_node_that_has_a_place_says_where_and_one_that_has_none_says_nothing() {
        let model = load(PLACED).model;
        let graph = model.graph();
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        let (group, lane) = (&model.groups[0], &model.instruments[0]);
        assert_eq!(node(&graph, NodeId::MASTER).ui, at(1200.0, 300.5));
        assert_eq!(node(&graph, group.node).ui, at(800.0, 40.0));
        assert_eq!(node(&graph, group.effects[0].node).ui, at(1100.0, 40.0));
        // (The modulators of a lane and of a group are the session's own: the lane's first, and then the group's.)
        assert_eq!(node(&graph, model.free_modulators[1].node).ui, at(800.0, -120.0));
        assert_eq!(node(&graph, lane.node).ui, at(0.0, 0.0), "a lane's instrument shares the lane's node");
        assert_eq!(node(&graph, lane.effects[0].node).ui, at(376.0, 0.25));
        assert_eq!(node(&graph, model.free_modulators[0].node).ui, at(376.0, -200.0));
        assert_eq!(node(&graph, model.instruments[1].node).ui, None);
        assert_eq!(graph.nodes.iter().filter(|n| n.ui.is_some()).count(), 7);
        // On the wire a node with a place has a `ui` and one without has none: a session with no places is the JSON it was.
        assert_eq!(serde_json::to_value(node(&graph, lane.node)).unwrap()["ui"], json!({"x": 0.0, "y": 0.0}));
        let unplaced = serde_json::to_value(node(&graph, model.instruments[1].node)).unwrap();
        assert!(unplaced.get("ui").is_none(), "{unplaced}");
    }

    #[test]
    fn the_place_of_a_node_that_has_gone_is_not_in_the_graph() {
        let mut model = load(PLACED).model;
        let gone = model.instruments[0].effects.remove(0).node;
        assert!(model.ui.contains_key(&gone));
        let graph = model.graph();
        assert!(graph.nodes.iter().all(|n| n.id != gone));
        assert_eq!(graph.nodes.iter().filter(|n| n.ui.is_some()).count(), 6);
    }

    #[test]
    fn a_lanes_pattern_is_on_its_node_with_its_settings_and_how_many_notes_it_has() {
        // (SETTINGS has a pattern of 2 beats recorded at 90, in key, which does not loop: one note on and one off.)
        let model = load(SETTINGS).model;
        let graph = model.graph();
        let NodeKind::Instrument { pattern, .. } = &graph.nodes[0].kind else { panic!("{:?}", graph.nodes[0].kind) };
        assert_eq!(*pattern, Some(PatternInfo { recording: false, enabled: true, looping: false, in_key: true, length_beats: 2.0, bpm: 90.0, notes: 1 }));
        // A lane that has none says so, and a pattern that is being recorded says that.
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let none = |model: &SessionModel| match &model.graph().nodes[0].kind {
            NodeKind::Instrument { pattern, .. } => pattern.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(none(&model), None);
        model.instruments[0].pattern = Some(PatternState::empty(100.0));
        model.instruments[0].pattern.as_mut().unwrap().recording = true;
        assert_eq!(none(&model), Some(PatternInfo { recording: true, enabled: false, looping: true, in_key: false, length_beats: 4.0, bpm: 100.0, notes: 0 }));
    }

    #[test]
    fn an_instruments_settings_are_on_its_node() {
        let model = load(SETTINGS).model;
        let graph = model.graph();
        let NodeKind::Instrument { plugin: Some(plugin), range, transpose, volume, .. } = &graph.nodes[0].kind else {
            panic!("{:?}", graph.nodes[0].kind)
        };
        assert_eq!((*range, *transpose, *volume), (Some(NoteRange { low: 36, high: 83 }), -12, 0.5));
        let waveform = the_param(plugin, "waveform");
        assert_eq!(waveform.value, 2.0, "the session's override, not the default");
        assert_eq!(waveform.default, 0.0);
        assert_eq!(waveform.labels.as_deref(), Some(&["sine", "triangle", "square", "saw", "noise"].map(String::from)[..]));
        assert_eq!(waveform.step_count, 4);
    }

    #[test]
    fn a_parameter_says_what_the_plugin_says_about_it() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[instrument.params]\nattack = 0.25\n").model;
        let graph = model.graph();
        let attack = the_param(plugin_of(&graph.nodes[0]), "attack").clone();
        assert_eq!(
            attack,
            Param {
                index: 3,
                name: "attack".to_string(),
                min: 0.0,
                max: 10.0,
                default: 0.004,
                value: 0.25,
                labels: None,
                unit: Some("s".to_string()),
                group: vec!["Envelope".to_string()],
                step_count: 0,
                used_when: None,
            }
        );
    }

    #[test]
    fn a_lanes_effects_follow_it_in_a_chain_and_sit_in_it() {
        let model = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n\
             [[instrument.effect]]\nplugin = \"builtin:filter\"\nmix = 1.0\n\
             [[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = 0.25\n",
        )
        .model;
        let lane = &model.instruments[0];
        let (osc, filter, reverb) = (lane.node, lane.effects[0].node, lane.effects[1].node);
        let graph = model.graph();
        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [osc, filter, reverb, NodeId::MASTER]);
        assert_eq!(
            graph.edges,
            [audio(out(osc), into(filter)), audio(out(filter), into(reverb)), audio(out(reverb), into(NodeId::MASTER))]
        );
        assert_eq!((node(&graph, filter).parent, node(&graph, reverb).parent), (Some(osc), Some(osc)));
        assert_eq!((node(&graph, filter).name.as_str(), node(&graph, reverb).name.as_str()), ("Filter", "Reverb"));
        let NodeKind::Effect { plugin, mix, feed, side } = &node(&graph, reverb).kind else { panic!() };
        assert_eq!((plugin.id.as_str(), *mix, plugin.is_instrument, *feed), ("builtin:reverb", 0.25, false, Feed::Chain));
        assert_eq!(*side, None, "a reverb has the one input");
        assert!(!plugin.presets.is_empty(), "the reverb's presets come along");
    }

    #[test]
    fn an_empty_lane_is_an_instrument_node_without_a_plugin() {
        let mut model = load(OSC).model;
        model.instruments[0].instrument = None;
        let graph = model.graph();
        assert!(matches!(&graph.nodes[0].kind, NodeKind::Instrument { plugin: None, .. }), "{:?}", graph.nodes[0].kind);
        assert_eq!(graph.nodes[0].name, "(empty)");
        assert_eq!(graph.edges, [audio(out(model.instruments[0].node), into(NodeId::MASTER))]);
    }

    // -- Groups --

    const GROUPED: &str = r#"
[[group]]
name = "Pad"
volume = 0.8
pan = -0.25
[[group.effect]]
plugin = "builtin:reverb"
mix = 0.6

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
mix = 1.0

[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_group_is_a_node_its_lanes_feed_and_its_bus_effects_follow() {
        let model = load(GROUPED).model;
        let pad = model.groups[0].node;
        let bus = model.groups[0].effects[0].node;
        let (lane0, filter, lane1) = (model.instruments[0].node, model.instruments[0].effects[0].node, model.instruments[1].node);
        let graph = model.graph();

        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [pad, bus, lane0, filter, lane1, NodeId::MASTER], "groups first, then lanes, then the master");
        assert_eq!(
            graph.edges,
            [
                audio(out(pad), into(bus)),
                audio(out(bus), into(NodeId::MASTER)),
                audio(out(lane0), into(filter)),
                member(out(filter), into(pad)),
                audio(out(lane1), into(NodeId::MASTER)),
            ]
        );

        let parents: Vec<(NodeId, Option<NodeId>)> = graph.nodes.iter().map(|n| (n.id, n.parent)).collect();
        assert_eq!(
            parents,
            [(pad, None), (bus, Some(pad)), (lane0, Some(pad)), (filter, Some(lane0)), (lane1, None), (NodeId::MASTER, None)]
        );
        assert_eq!(node(&graph, pad).name, "Pad");
        assert_eq!(node(&graph, pad).kind, NodeKind::Group { volume: 0.8, pan: -0.25, muted: false, solo: false, route: Route::Out });
        assert!(matches!(&node(&graph, bus).kind, NodeKind::Effect { mix, .. } if *mix == 0.6));
    }

    #[test]
    fn a_lane_and_a_group_say_whether_they_are_muted_or_soloed() {
        let model = load("[[group]]\nsolo = true\n[[instrument]]\nplugin = \"builtin:osc\"\nmute = true\ngroup = 0\n[[instrument]]\nplugin = \"builtin:osc\"\nsolo = true\nmute = true\n[[instrument]]\nplugin = \"builtin:osc\"\n").model;
        let graph = model.graph();
        let flags = |id: NodeId| match &node(&graph, id).kind {
            NodeKind::Instrument { muted, solo, .. } | NodeKind::Group { muted, solo, .. } => (*muted, *solo),
            other => panic!("{other:?}"),
        };
        assert_eq!(flags(model.groups[0].node), (false, true));
        assert_eq!(model.instruments.iter().map(|lane| flags(lane.node)).collect::<Vec<_>>(), [(true, false), (true, true), (false, false)]);
    }

    #[test]
    fn a_lane_and_a_group_say_which_way_out_they_have_and_no_link_leaves_one_that_is_cut() {
        let model = load(
            "[[group]]\nroute = \"none\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\n\
             [[group]]\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nroute = \"none\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n",
        )
        .model;
        let graph = model.graph();
        let route = |id: NodeId| match &node(&graph, id).kind {
            NodeKind::Instrument { route, .. } | NodeKind::Group { route, .. } => *route,
            other => panic!("{other:?}"),
        };
        assert_eq!(model.groups.iter().map(|group| route(group.node)).collect::<Vec<_>>(), [Route::None, Route::Out]);
        assert_eq!(model.instruments.iter().map(|lane| route(lane.node)).collect::<Vec<_>>(), [Route::None, Route::Out, Route::None, Route::Out]);
        let (g0, g1, bus) = (model.groups[0].node, model.groups[1].node, model.groups[0].effects[0].node);
        let (l0, l1, l2, l3, filter) = (model.instruments[0].node, model.instruments[1].node, model.instruments[2].node, model.instruments[3].node, model.instruments[0].effects[0].node);
        assert_eq!(
            graph.edges,
            [
                // group 0: out of its bus effect, to nowhere; group 1 to the master.
                audio(out(g0), into(bus)),
                audio(out(g1), into(NodeId::MASTER)),
                // lane 0, in group 0, has its filter and no link into the group; lane 1 is a member of group 1; lane 2, in no group, goes nowhere; lane 3 goes to the master.
                audio(out(l0), into(filter)),
                member(out(l1), into(g1)),
                audio(out(l3), into(NodeId::MASTER)),
            ]
        );
        assert_eq!(model.instruments[2].node, l2);
    }

    #[test]
    fn an_effect_says_whether_the_stage_before_it_feeds_it_and_no_link_goes_into_one_that_is_not_fed() {
        let model = load(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\nfeed = \"none\"\n[[group.effect]]\nplugin = \"builtin:filter\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\nfeed = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:drive\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n",
        )
        .model;
        let graph = model.graph();
        let (group, bus) = (model.groups[0].node, [model.groups[0].effects[0].node, model.groups[0].effects[1].node]);
        let (lane0, fx) = (model.instruments[0].node, [model.instruments[0].effects[0].node, model.instruments[0].effects[1].node, model.instruments[0].effects[2].node]);
        let (lane1, solo_fx) = (model.instruments[1].node, model.instruments[1].effects[0].node);
        // (No link goes into an effect that is not fed, and the chain goes on from it: the next effect is fed by it, and the end of the chain is where it was.)
        assert_eq!(
            graph.edges,
            [
                audio(out(bus[0]), into(bus[1])),
                audio(out(bus[1]), into(NodeId::MASTER)),
                audio(out(lane0), into(fx[0])),
                audio(out(fx[1]), into(fx[2])),
                member(out(fx[2]), into(group)),
                audio(out(solo_fx), into(NodeId::MASTER)),
            ]
        );
        let feed = |id: NodeId| match &node(&graph, id).kind {
            NodeKind::Effect { feed, .. } => *feed,
            other => panic!("{other:?}"),
        };
        assert_eq!(bus.map(feed), [Feed::None, Feed::Chain]);
        assert_eq!(fx.map(feed), [Feed::Chain, Feed::None, Feed::Chain]);
        assert_eq!(feed(solo_fx), Feed::None);
        assert_eq!((node(&graph, lane1).parent, node(&graph, solo_fx).parent), (None, Some(lane1)));
    }

    #[test]
    fn the_last_effect_of_a_chain_that_is_not_fed_still_ends_the_chain_and_a_chain_whose_way_out_is_cut_has_no_link_out() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\nfeed = \"none\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\nfeed = \"none\"\n").model;
        let graph = model.graph();
        let (a, b) = (model.instruments[1].effects[0].node, model.instruments[1].effects[1].node);
        // Lane 0 has every link cut, and no way out: nothing at all leaves it. Lane 1's delay is fed by nothing and ends the chain.
        assert_eq!(graph.edges, [audio(out(model.instruments[1].node), into(a)), audio(out(b), into(NodeId::MASTER))]);
    }

    #[test]
    fn a_group_with_no_bus_effects_feeds_the_master_directly() {
        let model = load("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n").model;
        let (group, lane) = (model.groups[0].node, model.instruments[0].node);
        let graph = model.graph();
        assert_eq!(graph.edges, [audio(out(group), into(NodeId::MASTER)), member(out(lane), into(group))]);
        assert_eq!(node(&graph, group).name, "Group 1", "unnamed, so by its number");
    }

    #[test]
    fn a_group_nobody_is_in_is_still_a_node() {
        let model = load("[[group]]\nname = \"Empty\"\n").model;
        let graph = model.graph();
        let group = model.groups[0].node;
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges, [audio(out(group), into(NodeId::MASTER))]);
    }

    #[test]
    fn a_lane_whose_group_is_gone_goes_to_the_master() {
        let mut model = load("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n").model;
        model.instruments[0].group = Some(7);
        let graph = model.graph();
        let lane = model.instruments[0].node;
        assert!(graph.edges.contains(&audio(out(lane), into(NodeId::MASTER))), "{:?}", graph.edges);
        assert_eq!(node(&graph, lane).parent, None);
    }

    // -- Modulators --

    const MODULATED: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"

[[instrument.modulator]]
type = "lfo"
waveform = "saw"
rate = 2.0
[[instrument.modulator.target]]
param = "detune"
depth = 0.1
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = -0.3

[[instrument.modulator]]
type = "envelope"
attack = 0.02
decay = 0.4
sustain = 0.5
release = 1.0
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.5
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.2
[[instrument.modulator.target]]
mod_sustain = 1
depth = 0.1
"#;

    #[test]
    fn a_modulators_name_does_not_carry_its_settings_which_change_under_a_client() {
        // A client that is told a rate has changed would otherwise be left with a name that
        // says the old one (and could not make the new one the way the controller does).
        let mut model = load(MODULATED).model;
        let lfo = model.free_modulators[0].node;
        let name = |model: &SessionModel| node(&model.graph(), lfo).name.clone();
        assert_eq!(name(&model), "LFO");
        model.free_modulators[0].source = ModSourceSlot::Lfo { waveform: LfoWaveform::Square, rate: 17.5, retrigger: false, fade: 0.0, sync: 0 };
        assert_eq!(name(&model), "LFO");

        let envelope = model.free_modulators[1].node;
        model.free_modulators[1].source = ModSourceSlot::Envelope { attack: 1.0, decay: 2.0, sustain: 0.1, release: 3.0 };
        assert_eq!(node(&model.graph(), envelope).name, "ADSR");

        // What a control is bound to is not a setting, and is in its name.
        let learn = |source, learning| ModSourceSlot::MidiLearn { source, learning };
        for (slot, expected) in [
            (learn(Some(MidiBindSource::Cc(74)), false), "CC 74"),
            (learn(Some(MidiBindSource::PitchBend), false), "Pitch Bend"),
            (learn(Some(MidiBindSource::Velocity), false), "Velocity"),
            (learn(Some(MidiBindSource::Key), false), "Key"),
            (learn(None, true), "MIDI (learning…)"),
        ] {
            model.free_modulators[1].source = slot;
            assert_eq!(node(&model.graph(), envelope).name, expected);
        }
    }

    #[test]
    fn a_macro_is_a_node_called_a_macro_whatever_its_value_and_says_its_value() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"macro\"\nvalue = 0.25\n").model;
        let knob = model.free_modulators[0].node;
        let graph = model.graph();
        let found = node(&graph, knob);
        assert_eq!(found.name, "Macro");
        assert_eq!(found.kind, NodeKind::Modulator { source: ModulatorSource::Macro { value: 0.25 }, hears: Some(model.instruments[0].node) });
        assert_eq!(found.parent, None, "in no lane: it hears the lane that it was in");
    }

    #[test]
    fn a_step_sequencer_is_a_node_called_steps_whatever_it_goes_through_and_says_all_that_it_has() {
        let mut model = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\ntype = \"steps\"\nsteps = [0.5, -0.25, 1.0]\nrate = 6.5\nglide = 0.25\nretrigger = false\n",
        )
        .model;
        let sequencer = model.free_modulators[0].node;
        let found = |model: &SessionModel| node(&model.graph(), sequencer).clone();
        let mut values = vec![0.0; 16];
        values[..3].copy_from_slice(&[0.5, -0.25, 1.0]);
        let found_first = found(&model);
        assert_eq!(found_first.name, "Steps");
        assert_eq!(found_first.kind, NodeKind::Modulator { source: ModulatorSource::Steps { values, length: 3, rate: 6.5, glide: 0.25, retrigger: false, sync: 0 }, hears: Some(model.instruments[0].node) });
        assert_eq!(found_first.parent, None);
        // Its name does not say how fast it goes or how long it is, which change under a client.
        if let ModSourceSlot::Steps { length, rate, .. } = &mut model.free_modulators[0].source {
            *length = 9;
            *rate = 0.5;
        }
        assert_eq!(found(&model).name, "Steps");
    }

    #[test]
    fn a_modulator_is_a_node_of_its_own_that_hears_its_lane_with_an_edge_for_each_target() {
        let model = load(MODULATED).model;
        let lane = &model.instruments[0];
        let (osc, filter) = (lane.node, lane.effects[0].node);
        let (lfo, env) = (model.free_modulators[0].node, model.free_modulators[1].node);
        let lfo_edges: Vec<EdgeId> = model.free_modulators[0].targets.iter().map(|t| t.edge).collect();
        let env_edges: Vec<EdgeId> = model.free_modulators[1].targets.iter().map(|t| t.edge).collect();
        let graph = model.graph();

        let ids: Vec<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [osc, filter, lfo, env, NodeId::MASTER]);
        assert_eq!((node(&graph, lfo).parent, node(&graph, env).parent), (None, None), "a modulator is in no lane");
        assert_eq!(node(&graph, lfo).name, "LFO");
        assert_eq!(node(&graph, env).name, "ADSR");
        assert_eq!(
            node(&graph, lfo).kind,
            NodeKind::Modulator { source: ModulatorSource::Lfo { waveform: Waveform::Saw, rate: 2.0, retrigger: false, fade: 0.0, sync: 0 }, hears: Some(osc) }
        );
        assert_eq!(
            node(&graph, env).kind,
            NodeKind::Modulator { source: ModulatorSource::Envelope { attack: 0.02, decay: 0.4, sustain: 0.5, release: 1.0 }, hears: Some(osc) }
        );

        let modulation_edges: Vec<&Edge> = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).collect();
        let expected = [
            // The oscillator's detune is its parameter 1, the filter's cutoff its parameter 0.
            modulation(lfo, param(osc, 1), lfo_edges[0], 0.1),
            modulation(lfo, param(filter, 0), lfo_edges[1], -0.3),
            // The envelope drives the LFO's rate and the depth of the LFO's second edge. (Its third cable, to its own sustain, which does nothing, is not made.)
            modulation(env, at(lfo, Port::Rate), env_edges[0], 0.5),
            modulation(env, at(lfo, Port::Depth { edge: lfo_edges[1] }), env_edges[1], 0.2),
        ];
        assert_eq!(env_edges.len(), 2);
        assert_eq!(modulation_edges, expected.iter().collect::<Vec<_>>());
    }

    #[test]
    fn a_target_is_named_by_the_endpoint_that_the_graph_draws_its_cable_to_and_what_is_not_there_is_not_named() {
        use crate::app::testutil::{CABLES_THAT_DRIVE_CABLES, CROSS_MOD, CROSS_MOD_RACKS, LANES_IN_GROUPS};
        use crate::plugin::chain::{ModTargetKind, ModulatorLocation};
        let mut checked = 0;
        for text in [GROUPS, CROSS_MOD, CROSS_MOD_RACKS, LANES_IN_GROUPS, CABLES_THAT_DRIVE_CABLES] {
            let model = load(text).model;
            let graph = model.graph();
            let at = ModulatorLocation::Free;
            for (index, target) in model.free_modulators.iter().enumerate().flat_map(|(index, modulator)| modulator.targets.iter().map(move |target| (index, target))) {
                let drawn = graph.edges.iter().find(|edge| matches!(edge.kind, EdgeKind::Modulation { id, .. } if id == target.edge)).map(|edge| edge.to);
                // The place that the rack names what the cable drives by, and the endpoint that the place is: the same as the node and the port that the cable has.
                let kind = model.kind_of(at, index, target.aim).expect("a cable drives something that is in reach");
                assert_eq!(model.endpoint_of(at, &kind), drawn, "{at:?} {kind:?}");
                assert_eq!(model.endpoint_of_aim(target.aim), drawn, "{at:?} {:?}", target.aim);
                checked += usize::from(drawn.is_some());
            }
        }
        assert!(checked > 20, "the sessions have cables enough to say something: {checked}");
        let model = load(GROUPS).model;
        let nowhere = ModTargetKind::FreeParam { plugin: NodeId(9_999), param_index: 0 };
        assert_eq!(model.endpoint_of(ModulatorLocation::Free, &nowhere), None, "no such plugin");
        assert_eq!(model.endpoint_of(ModulatorLocation::Free, &ModTargetKind::ModulatorRate { mod_index: 99 }), None, "no such modulator");
    }

    #[test]
    fn the_audio_edges_come_before_the_modulation_edges() {
        let model = load(MODULATED).model;
        let graph = model.graph();
        let first_modulation = graph.edges.iter().position(|e| matches!(e.kind, EdgeKind::Modulation { .. })).unwrap();
        assert!(graph.edges[..first_modulation].iter().all(|e| e.kind == EdgeKind::Audio));
        assert!(graph.edges[first_modulation..].iter().all(|e| matches!(e.kind, EdgeKind::Modulation { .. })));
    }

    #[test]
    fn every_kind_of_modulator_source_is_described() {
        let model = load(MIDI).model;
        let graph = model.graph();
        let sources: Vec<&ModulatorSource> = model
            .free_modulators
            .iter()
            .map(|m| match &node(&graph, m.node).kind {
                NodeKind::Modulator { source, .. } => source,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            sources,
            [
                &ModulatorSource::MidiLearn { control: Some(MidiControl::Cc { number: 74 }), learning: false },
                &ModulatorSource::MidiLearn { control: None, learning: true },
                &ModulatorSource::MidiLearn { control: Some(MidiControl::PitchBend), learning: false },
                &ModulatorSource::MidiLearn { control: Some(MidiControl::Aftertouch), learning: false },
                &ModulatorSource::MidiLearn { control: Some(MidiControl::Velocity), learning: false },
                &ModulatorSource::MidiLearn { control: Some(MidiControl::Key), learning: false },
            ]
        );
        let names: Vec<&str> = model.free_modulators.iter().map(|m| node(&graph, m.node).name.as_str()).collect();
        assert_eq!(names, ["CC 74", "MIDI (learning…)", "Pitch Bend", "Aftertouch", "Velocity", "Key"]);
    }

    #[test]
    fn every_waveform_is_named() {
        for (name, waveform) in [
            ("sine", Waveform::Sine),
            ("triangle", Waveform::Triangle),
            ("saw", Waveform::Saw),
            ("square", Waveform::Square),
            ("random", Waveform::Random),
            ("smooth", Waveform::Smooth),
        ] {
            let model = load(&format!("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nwaveform = \"{name}\"\n")).model;
            let graph = model.graph();
            let m = model.free_modulators[0].node;
            assert_eq!(
                node(&graph, m).kind,
                NodeKind::Modulator { source: ModulatorSource::Lfo { waveform, rate: 1.0, retrigger: false, fade: 0.0, sync: 0 }, hears: Some(model.instruments[0].node) },
                "{name}"
            );
        }
    }

    #[test]
    fn a_modulator_that_hears_a_group_reaches_into_its_members_and_its_bus() {
        let model = load(GROUPS).model;
        let group = &model.groups[0];
        let (pad, bus) = (group.node, group.effects[0].node);
        let (lane0, lane1) = (model.instruments[0].node, model.instruments[1].node);
        let filter = model.instruments[1].effects[0].node;
        // The lanes' modulators come first in the session's own list, and then the group's two.
        let own = &model.free_modulators;
        let (lfo, env) = (own[own.len() - 2].node, own[own.len() - 1].node);
        let lane1_lfo = own.iter().find(|m| m.ear == Some(lane1)).expect("the second lane's modulator").node;
        let lfo_edges: Vec<EdgeId> = own[own.len() - 2].targets.iter().map(|t| t.edge).collect();
        let env_edges: Vec<EdgeId> = own[own.len() - 1].targets.iter().map(|t| t.edge).collect();
        let graph = model.graph();

        assert_eq!((node(&graph, lfo).parent, node(&graph, env).parent), (None, None));
        assert_eq!((own[own.len() - 2].ear, own[own.len() - 1].ear), (Some(pad), Some(pad)), "they hear the group");
        let edges: Vec<&Edge> = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).collect();
        let expected = [
            // Member 0's detune (the oscillator's parameter 1).
            modulation(lfo, param(lane0, 1), lfo_edges[0], 0.4),
            // Member 1's effect in slot 1: the filter's cutoff, parameter 0.
            modulation(lfo, param(filter, 0), lfo_edges[1], -0.3),
            // The bus effect's damping: the reverb's parameter 2.
            modulation(lfo, param(bus, 2), lfo_edges[2], 0.25),
            // Member 1's own LFO's rate.
            modulation(lfo, at(lane1_lfo, Port::Rate), lfo_edges[3], 0.2),
            // The second group modulator drives the first one's rate, and the depth of its edge 2.
            modulation(env, at(lfo, Port::Rate), env_edges[0], 0.3),
            modulation(env, at(lfo, Port::Depth { edge: lfo_edges[2] }), env_edges[1], 0.1),
        ];
        assert_eq!(edges, expected.iter().collect::<Vec<_>>());
        assert_ne!(lane0, lane1);
    }

    #[test]
    fn a_target_whose_aim_is_gone_has_no_edge() {
        use crate::app::model::Driven;
        let mut model = load(MODULATED).model;
        let lfo = &mut model.free_modulators[0];
        let real = lfo.targets.len();
        let own = lfo.node;
        let template = |aim: Driven| crate::app::model::ModTargetSlot {
            edge: EdgeId(1_000),
            param_name: "x".to_string(),
            aim,
            depth: 0.5,
            param_min: 0.0,
            param_max: 1.0,
            effective: None,
        };
        for (n, aim) in [
            // A plugin that is not there, a node that is not a plugin, a modulator that is not there and a cable that is not there.
            Driven::Param { plugin: crate::app::model::NodeId(9_999), index: 0 },
            Driven::Param { plugin: own, index: 0 },
            Driven::Setting { modulator: crate::app::model::NodeId(9_998), field: crate::app::model::ModSetting::Rate },
            Driven::Depth(EdgeId(9_997)),
            // The modulator's own settings: a cable like that does nothing.
            Driven::Setting { modulator: own, field: crate::app::model::ModSetting::Rate },
        ]
        .into_iter()
        .enumerate()
        {
            lfo.targets.push(ModTargetSlot { edge: EdgeId(1_000 + n as u32), ..template(aim) });
        }
        let graph = model.graph();
        let edges = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).count();
        assert_eq!(edges, 4, "only the {real} real targets of the LFO and the envelope's two");
    }

    /// The modulation edges of `graph` as `(from, port, amount)`: the modulator
    /// they start at, the port they end at, and their amount.
    fn modulation_of(graph: &Graph) -> Vec<(NodeId, Endpoint, f32)> {
        graph
            .edges
            .iter()
            .filter_map(|e| match e.kind {
                EdgeKind::Modulation { amount, .. } => Some((e.from.node, e.to, amount)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_modulator_can_drive_every_field_of_another() {
        let model = load(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
param = "volume"
[[instrument.modulator]]
type = "envelope"
[[instrument.modulator]]
type = "lfo"
[[instrument.modulator.target]]
mod_rate = 0
depth = 0.1
[[instrument.modulator.target]]
mod_attack = 1
depth = 0.2
[[instrument.modulator.target]]
mod_decay = 1
depth = 0.3
[[instrument.modulator.target]]
mod_sustain = 1
depth = 0.4
[[instrument.modulator.target]]
mod_release = 1
depth = 0.5
[[instrument.modulator.target]]
mod_depth = [0, 1]
depth = 0.6
"#,
        )
        .model;
        let own = &model.free_modulators;
        let (lfo, env, driver) = (own[0].node, own[1].node, own[2].node);
        let second_edge = own[0].targets[1].edge;
        let graph = model.graph();
        let got = modulation_of(&graph);
        let driven = |port: Port, node: NodeId| at(node, port);
        assert_eq!(
            got[2..],
            [
                (driver, driven(Port::Rate, lfo), 0.1),
                (driver, driven(Port::Attack, env), 0.2),
                (driver, driven(Port::Decay, env), 0.3),
                (driver, driven(Port::Sustain, env), 0.4),
                (driver, driven(Port::Release, env), 0.5),
                (driver, driven(Port::Depth { edge: second_edge }, lfo), 0.6),
            ]
        );
        assert_closed(&graph, "every field");
    }

    #[test]
    fn a_target_of_a_missing_member_is_not_taken_for_another() {
        let model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 1
param = "detune"
depth = 0.1
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument]]
plugin = "builtin:osc"
"#,
        )
        .model;
        // The loader may keep the target; it points at a member that is not there, and a
        // lane that is not in the group is not that member.
        assert_eq!(modulation_of(&model.graph()), []);
    }

    #[test]
    fn the_edges_of_the_modulators_come_in_the_order_of_the_sessions_own_rack_the_lanes_first_and_then_the_groups() {
        let model = load(
            r#"
[[group]]
[[group.modulator]]
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.1
[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "detune"
depth = 0.2
"#,
        )
        .model;
        let amounts: Vec<f32> = modulation_of(&model.graph()).iter().map(|(_, _, amount)| *amount).collect();
        assert_eq!(amounts, [0.2, 0.1]);
    }

    #[test]
    fn a_row_that_is_not_a_parameter_is_left_out_and_a_preset_is_described() {
        use crate::app::model::{ParamKind, ParamSlot};
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n").model;
        let reverb = &mut model.instruments[0].effects[0];
        reverb.params.push(ParamSlot { name: "rule".to_string(), index: 99, kind: ParamKind::Separator, ..Default::default() });
        reverb.current_preset = Some("Cathedral".to_string());
        let id = reverb.node;
        let graph = model.graph();
        let plugin = plugin_of(node(&graph, id));
        assert!(plugin.params.iter().all(|p| p.index != 99), "the separator is not a parameter");
        assert_eq!(plugin.params.len(), 10, "and the ten real ones are");
        assert_eq!(plugin.current_preset.as_deref(), Some("Cathedral"));
        let presets: Vec<(&str, &str)> = plugin.presets.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
        assert_eq!(presets[..2], [("arcadia", "Arcadia Dream Hall"), ("cathedral", "Cathedral")]);
    }

    // -- The whole graph --

    /// What has to hold of any graph: ids are unique, every edge joins ports that
    /// exist, and every parent is a node.
    fn assert_closed(graph: &Graph, what: &str) {
        let mut ids = std::collections::HashSet::new();
        for n in &graph.nodes {
            assert!(ids.insert(n.id), "{what}: {:?} twice", n.id);
        }
        assert_eq!(graph.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Master { .. })).count(), 1, "{what}: one master");
        for n in &graph.nodes {
            if let Some(parent) = n.parent {
                assert!(ids.contains(&parent), "{what}: the parent of {:?}", n.id);
            }
        }
        let modulation_ids: std::collections::HashSet<EdgeId> = graph
            .edges
            .iter()
            .filter_map(|e| match e.kind {
                EdgeKind::Modulation { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        let count = graph.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Modulation { .. })).count();
        assert_eq!(modulation_ids.len(), count, "{what}: modulation edge ids are unique");
        for e in &graph.edges {
            for end in [e.from, e.to] {
                let n = graph.nodes.iter().find(|n| n.id == end.node).unwrap_or_else(|| panic!("{what}: {e:?} has no node"));
                match (end.port, &n.kind) {
                    (Port::AudioOut, NodeKind::Instrument { .. } | NodeKind::Effect { .. } | NodeKind::Group { .. }) => {}
                    (Port::AudioIn, NodeKind::Effect { .. } | NodeKind::Group { .. } | NodeKind::Master { .. }) => {}
                    (Port::AudioInSide, NodeKind::Effect { side: Some(_), .. }) => {}
                    (Port::ModOut, NodeKind::Modulator { .. }) => {}
                    (Port::EventsOut, NodeKind::Source { .. }) => {}
                    (Port::EventsIn, NodeKind::Instrument { .. }) => {}
                    (Port::Param { index }, NodeKind::Instrument { plugin: Some(p), .. } | NodeKind::Effect { plugin: p, .. }) => {
                        assert!(p.params.iter().any(|q| q.index == index), "{what}: {e:?} names a parameter {index} the plugin lacks");
                    }
                    // (The host-side values: a lane that has an instrument and a group have a volume, an effect a mix, a group a pan.)
                    (Port::Volume, NodeKind::Instrument { plugin: Some(_), .. } | NodeKind::Group { .. }) => {}
                    (Port::Mix, NodeKind::Effect { .. }) => {}
                    (Port::Pan, NodeKind::Group { .. }) => {}
                    (Port::Rate | Port::Attack | Port::Decay | Port::Sustain | Port::Release, NodeKind::Modulator { .. }) => {}
                    (Port::Depth { edge }, NodeKind::Modulator { .. }) => {
                        assert!(modulation_ids.contains(&edge), "{what}: {e:?} names an edge that is not there");
                    }
                    (port, kind) => panic!("{what}: {port:?} on {kind:?}"),
                }
            }
            match e.kind {
                EdgeKind::Modulation { .. } => assert_eq!(e.from.port, Port::ModOut, "{what}"),
                EdgeKind::Audio => {
                    assert_eq!((e.from.port, e.to.port), (Port::AudioOut, Port::AudioIn), "{what}");
                }
                EdgeKind::Member => {
                    assert!(matches!(node(graph, e.to.node).kind, NodeKind::Group { .. }), "{what}: members go to groups");
                    assert_eq!((e.from.port, e.to.port), (Port::AudioOut, Port::AudioIn), "{what}");
                }
                EdgeKind::Cable { id, .. } => {
                    assert_eq!((e.from.port, e.to.port), (Port::AudioOut, Port::AudioIn), "{what}");
                    assert!(!modulation_ids.contains(&id), "{what}: a cable has an id of its own");
                }
                EdgeKind::Events { id } => {
                    assert_eq!((e.from.port, e.to.port), (Port::EventsOut, Port::EventsIn), "{what}");
                    assert!(!modulation_ids.contains(&id), "{what}: a cable has an id of its own");
                }
                EdgeKind::Hears => assert_eq!((e.from.port, e.to.port), (Port::EventsOut, Port::EventsIn), "{what}"),
            }
        }
    }

    #[test]
    fn every_scene_projects_into_a_closed_graph() {
        for (what, scene) in [
            ("OSC", OSC),
            ("GROUPED", GROUPED),
            ("MODULATED", MODULATED),
            ("GROUPS", GROUPS),
            ("LEGACY", LEGACY),
            ("MIDI", MIDI),
            ("SETTINGS", SETTINGS),
            ("DISABLED_PATTERN", DISABLED_PATTERN),
            ("split", include_str!("../../examples/split.toml")),
            ("tracer", include_str!("../../examples/tracer.toml")),
        ] {
            let model = load(scene).model;
            assert_closed(&model.graph(), what);
        }
    }

    #[test]
    fn every_node_of_the_model_is_a_node_of_the_graph() {
        for scene in [GROUPED, MODULATED, GROUPS, LEGACY] {
            let model = load(scene).model;
            let graph = model.graph();
            let in_model: std::collections::HashSet<NodeId> = model.nodes().map(|(id, _)| id).collect();
            let in_graph: std::collections::HashSet<NodeId> =
                graph.nodes.iter().map(|n| n.id).filter(|id| *id != NodeId::MASTER).collect();
            assert_eq!(in_graph, in_model);
            assert_eq!(graph.nodes.len(), in_model.len() + 1, "and once each, with the master");
        }
    }

    #[test]
    fn the_master_is_not_a_node_of_the_model() {
        let model = load(GROUPS).model;
        assert_eq!(model.locate(NodeId::MASTER), None);
        assert!(model.nodes().all(|(id, _)| id != NodeId::MASTER));
    }

    #[test]
    fn a_session_with_nothing_in_it_is_just_the_master() {
        let graph = SessionModel::default().graph();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id, NodeId::MASTER);
        assert!(graph.edges.is_empty());
    }

    // -- The wire --

    #[test]
    fn an_effect_that_has_a_second_input_says_what_it_calls_its_inputs_and_the_others_say_nothing_on_the_wire() {
        let (sided, plain) = (
            NodeKind::Effect { plugin: Plugin { id: "builtin:math".to_string(), format: "Built-in".to_string(), is_instrument: false, params: vec![], presets: vec![], current_preset: None, pinned: vec![], takes_notes: true }, mix: 1.0, feed: Feed::Chain, side: Some(SideInfo { main: "A".to_string(), side: "B".to_string() }) },
            NodeKind::Effect { plugin: Plugin { id: "builtin:filter".to_string(), format: "Built-in".to_string(), is_instrument: false, params: vec![], presets: vec![], current_preset: None, pinned: vec![], takes_notes: true }, mix: 1.0, feed: Feed::Chain, side: None },
        );
        assert_eq!(serde_json::to_value(&sided).unwrap()["side"], json!({"main": "A", "side": "B"}));
        assert!(serde_json::to_value(&plain).unwrap().get("side").is_none(), "an effect with one input has no key for it: the older nodes are the same JSON");
        let back: NodeKind = serde_json::from_value(serde_json::to_value(&sided).unwrap()).unwrap();
        assert_eq!(back, sided);
        // A node that a client wrote before there was a second input has none.
        let mut old = serde_json::to_value(&plain).unwrap();
        old.as_object_mut().unwrap().remove("side");
        assert_eq!(serde_json::from_value::<NodeKind>(old).unwrap(), plain);
        // And the port that a cable ends at there is a port of its own on the wire.
        assert_eq!(serde_json::to_value(Port::AudioInSide).unwrap(), json!({"type": "audioInSide"}));
    }

    #[test]
    fn a_parameter_that_has_an_effect_only_sometimes_says_when_on_the_wire_and_the_others_say_nothing() {
        let model = load(crate::app::testutil::SIDED).model;
        let graph = model.graph();
        let math = plugin_of(graph.nodes.iter().find(|node| node.id == model.instruments[0].effects[1].node).unwrap());
        let filter = plugin_of(graph.nodes.iter().find(|node| node.id == model.instruments[0].effects[0].node).unwrap());
        let wire = |plugin: &Plugin, name: &str| serde_json::to_value(the_param(plugin, name)).unwrap();
        assert_eq!(
            wire(math, "bits")["usedWhen"],
            json!({"condition": {"type": "choice", "param": 0, "values": [7, 8, 9]}, "why": "only in the modes xor, and, or"})
        );
        assert_eq!(wire(math, "pitch")["usedWhen"], json!({"condition": {"type": "sideUnpatched"}, "why": "only while nothing is patched at B"}));
        // A parameter that always has an effect has no key for it, so that every older plugin's node is the same JSON.
        for name in ["mode", "level"] {
            assert!(wire(math, name).get("usedWhen").is_none(), "{name}");
        }
        assert!(filter.params.iter().all(|p| p.used_when.is_none()));
        assert!(wire(filter, "cutoff").get("usedWhen").is_none());
        // It is read back, and a node that a client wrote before there was such a thing has none.
        let back: Param = serde_json::from_value(wire(math, "wave")).unwrap();
        assert_eq!(&back, the_param(math, "wave"));
        let mut old = wire(math, "wave");
        old.as_object_mut().unwrap().remove("usedWhen");
        assert_eq!(serde_json::from_value::<Param>(old).unwrap().used_when, None);
    }

    #[test]
    fn nodes_edges_and_ports_are_tagged_objects_in_camel_case() {
        let node = Node {
            id: NodeId(3),
            parent: Some(NodeId(1)),
            name: "Hall".to_string(),
            ui: None,
            kind: NodeKind::Effect {
                plugin: Plugin {
                    id: "builtin:reverb".to_string(),
                    format: "Built-in".to_string(),
                    is_instrument: false,
                    pinned: vec![2],
                    takes_notes: true,
                    params: vec![Param {
                        index: 2,
                        name: "decay".to_string(),
                        min: 0.0,
                        max: 1.0,
                        default: 0.5,
                        value: 0.75,
                        labels: None,
                        unit: Some("s".to_string()),
                        group: vec!["Tail".to_string()],
                        step_count: 0,
                        used_when: None,
                    }],
                    presets: vec![Preset { id: "room".to_string(), name: "Small Room".to_string() }],
                    current_preset: None,
                },
                mix: 0.5,
                feed: Feed::Chain,
                side: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&node).unwrap(),
            json!({
                "id": 3,
                "parent": 1,
                "name": "Hall",
                "kind": "effect",
                "plugin": {
                    "id": "builtin:reverb",
                    "format": "Built-in",
                    "isInstrument": false,
                    "params": [{
                        "index": 2, "name": "decay", "min": 0.0, "max": 1.0, "default": 0.5, "value": 0.75,
                        "labels": null, "unit": "s", "group": ["Tail"], "stepCount": 0
                    }],
                    "presets": [{"id": "room", "name": "Small Room"}],
                    "currentPreset": null,
                    "pinned": [2],
                },
                "mix": 0.5,
                "feed": "chain",
            })
        );

        let edge = modulation(NodeId(7), at(NodeId(3), Port::Depth { edge: EdgeId(4) }), EdgeId(9), 0.5);
        assert_eq!(
            serde_json::to_value(&edge).unwrap(),
            json!({
                "from": {"node": 7, "port": {"type": "modOut"}},
                "to": {"node": 3, "port": {"type": "depth", "edge": 4}},
                "kind": "modulation",
                "id": 9,
                "amount": 0.5,
            })
        );
        assert_eq!(
            serde_json::to_value(audio(out(NodeId(1)), into(NodeId(2)))).unwrap(),
            json!({"from": {"node": 1, "port": {"type": "audioOut"}}, "to": {"node": 2, "port": {"type": "audioIn"}}, "kind": "audio"})
        );
        assert_eq!(
            serde_json::to_value(member(out(NodeId(1)), into(NodeId(2)))).unwrap()["kind"],
            json!("member")
        );
        assert_eq!(serde_json::to_value(Port::Param { index: 5 }).unwrap(), json!({"type": "param", "index": 5}));
        for (port, name) in [
            (Port::Rate, "rate"),
            (Port::Attack, "attack"),
            (Port::Decay, "decay"),
            (Port::Sustain, "sustain"),
            (Port::Release, "release"),
        ] {
            assert_eq!(serde_json::to_value(port).unwrap(), json!({ "type": name }));
        }
    }

    #[test]
    fn an_lfos_key_is_in_the_graph_as_the_nodes_source() {
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nrate = 2.0\nretrigger = true\nfade = 0.75\n[[instrument.modulator]]\n").model;
        let graph = model.graph();
        let (keyed, free) = (model.free_modulators[0].node, model.free_modulators[1].node);
        assert_eq!(
            node(&graph, keyed).kind,
            NodeKind::Modulator { source: ModulatorSource::Lfo { waveform: Waveform::Sine, rate: 2.0, retrigger: true, fade: 0.75, sync: 0 }, hears: Some(model.instruments[0].node) }
        );
        assert_eq!(
            node(&graph, free).kind,
            NodeKind::Modulator { source: ModulatorSource::Lfo { waveform: Waveform::Sine, rate: 1.0, retrigger: false, fade: 0.0, sync: 0 }, hears: Some(model.instruments[0].node) },
            "one that says nothing of it runs free"
        );
    }

    #[test]
    fn the_note_length_an_lfo_and_a_step_sequencer_follow_the_tempo_by_is_in_the_graph_as_a_place_in_the_list_and_none_as_0() {
        let model = load(
            "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\nrate = 2.0\nsync = \"1/8\"\n[[instrument.modulator]]\ntype = \"steps\"\nsync = \"1/16\"\n[[instrument.modulator]]\n",
        )
        .model;
        let graph = model.graph();
        let sync_of = |index: usize| match &node(&graph, model.free_modulators[index].node).kind {
            NodeKind::Modulator { source: ModulatorSource::Lfo { sync, rate, .. } | ModulatorSource::Steps { sync, rate, .. }, .. } => (*sync, *rate),
            other => panic!("{other:?}"),
        };
        // (The rate is told too, whether or not it is followed: it is what the knob shows, and what it is when the modulator is free again.)
        assert_eq!([sync_of(0), sync_of(1), sync_of(2)], [(11, 2.0), (14, 8.0), (0, 1.0)]);
    }

    #[test]
    fn modulator_sources_and_controls_are_tagged_objects() {
        assert_eq!(
            serde_json::to_value(ModulatorSource::Lfo { waveform: Waveform::Saw, rate: 2.0, retrigger: false, fade: 0.0, sync: 0 }).unwrap(),
            json!({"type": "lfo", "waveform": "saw", "rate": 2.0, "retrigger": false, "fade": 0.0, "sync": 0})
        );
        assert_eq!(
            serde_json::to_value(ModulatorSource::Lfo { waveform: Waveform::Sine, rate: 1.0, retrigger: true, fade: 0.5, sync: 11 }).unwrap(),
            json!({"type": "lfo", "waveform": "sine", "rate": 1.0, "retrigger": true, "fade": 0.5, "sync": 11})
        );
        assert_eq!(
            serde_json::to_value(ModulatorSource::Steps { values: vec![0.5, -0.5], length: 2, rate: 8.0, glide: 0.0, retrigger: true, sync: 14 }).unwrap(),
            json!({"type": "steps", "values": [0.5, -0.5], "length": 2, "rate": 8.0, "glide": 0.0, "retrigger": true, "sync": 14})
        );
        assert_eq!(
            serde_json::to_value(ModulatorSource::Envelope { attack: 0.5, decay: 0.25, sustain: 1.0, release: 2.0 }).unwrap(),
            json!({"type": "envelope", "attack": 0.5, "decay": 0.25, "sustain": 1.0, "release": 2.0})
        );
        assert_eq!(
            serde_json::to_value(ModulatorSource::MidiLearn { control: Some(MidiControl::Cc { number: 74 }), learning: false }).unwrap(),
            json!({"type": "midiLearn", "control": {"type": "cc", "number": 74}, "learning": false})
        );
        assert_eq!(serde_json::to_value(ModulatorSource::Macro { value: 0.25 }).unwrap(), json!({"type": "macro", "value": 0.25}));
        assert_eq!(serde_json::to_value(MidiControl::PitchBend).unwrap(), json!({"type": "pitchBend"}));
        assert_eq!(serde_json::to_value(MidiControl::Velocity).unwrap(), json!({"type": "velocity"}));
        assert_eq!(serde_json::to_value(MidiControl::Key).unwrap(), json!({"type": "key"}));
        assert_eq!(
            serde_json::to_value(NodeKind::Instrument { plugin: None, range: Some(NoteRange { low: 36, high: 83 }), transpose: -12, volume: 0.5, muted: true, solo: false, route: Route::None, pattern: None })
                .unwrap(),
            json!({"kind": "instrument", "plugin": null, "range": {"low": 36, "high": 83}, "transpose": -12, "volume": 0.5, "muted": true, "solo": false, "route": "none", "pattern": null})
        );
        assert_eq!(
            serde_json::to_value(NodeKind::Master { volume: 0.5, limiter: false, ceiling: -3.0 }).unwrap(),
            json!({"kind": "master", "volume": 0.5, "limiter": false, "ceiling": -3.0})
        );
        assert_eq!(
            serde_json::to_value(NodeKind::Group { volume: 1.0, pan: 0.0, muted: false, solo: true, route: Route::Out }).unwrap(),
            json!({"kind": "group", "volume": 1.0, "pan": 0.0, "muted": false, "solo": true, "route": "out"})
        );
        // An effect says whether the link into it is there, in the word that the file uses.
        let model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n").model;
        let graph = model.graph();
        let wire: Vec<serde_json::Value> = model.instruments[0].effects.iter().map(|fx| serde_json::to_value(&node(&graph, fx.node).kind).unwrap()).collect();
        assert_eq!((wire[0]["kind"].clone(), wire[0]["feed"].clone(), wire[1]["feed"].clone()), (json!("effect"), json!("chain"), json!("none")));
    }

    #[test]
    fn a_module_is_a_node_with_the_nodes_that_came_with_it_after_the_modulators_and_before_the_master_and_says_so_on_the_wire() {
        use crate::app::model::ModuleInstance;
        let mut model = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[modulator]]\nhears = \"instrument[1]\"\n").model;
        let (lane, modulator) = (model.instruments[1].node, model.free_modulators[0].node);
        let id = model.alloc_node();
        model.modules.push(ModuleInstance { node: id, name: "Room".to_string(), members: vec![lane, modulator] });
        model.ui.insert(id, NodeUi { x: 12.0, y: 34.5 });
        let graph = model.graph();
        let order: Vec<NodeId> = graph.nodes.iter().map(|node| node.id).collect();
        assert_eq!(order.iter().rev().take(3).copied().collect::<Vec<_>>(), [NodeId::MASTER, id, modulator], "the box comes after the modulators and before the master");
        let found = node(&graph, id);
        assert_eq!((found.name.as_str(), found.parent, found.ui), ("Room", None, Some(NodeUi { x: 12.0, y: 34.5 })));
        assert_eq!(found.kind, NodeKind::Module { members: vec![lane, modulator] });
        let wire = serde_json::to_value(found).unwrap();
        assert_eq!(wire, json!({"id": id.0, "parent": null, "name": "Room", "ui": {"x": 12.0, "y": 34.5}, "kind": "module", "members": [lane.0, modulator.0]}));
        // The nodes that came with it are in the graph as any are, and so are their cables: the box is the record of what came together, and no part of the sound.
        assert!(graph.nodes.iter().any(|node| node.id == lane) && graph.nodes.iter().any(|node| node.id == modulator));
        assert_eq!(graph.edges, SessionModel::graph(&{
            let mut without = load("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[modulator]]\nhears = \"instrument[1]\"\n").model;
            without.alloc_node();
            without
        })
        .edges);
        assert_eq!(serde_json::from_value::<Graph>(serde_json::to_value(&graph).unwrap()).unwrap(), graph);
    }

    #[test]
    fn a_graph_comes_back_from_json_as_it_went() {
        for scene in [GROUPED, MODULATED, GROUPS, MIDI] {
            let graph = load(scene).model.graph();
            let json = serde_json::to_string(&graph).unwrap();
            assert_eq!(serde_json::from_str::<Graph>(&json).unwrap(), graph);
        }
    }
}
