//! What a front-end can ask of the controller, and what comes back; see
//! `docs/web-ui.md` §5.
//!
//! An `Intent` names the node it concerns by `NodeId`, so it stays valid while
//! other nodes come and go. `Controller::apply` carries it out on the model and
//! the audio thread and answers with the `Event`s that describe what changed, or
//! says why it could not. This is the first family; the rest of §5 follows.
//!
//! Intents and events are also what the browser sends and receives, as JSON: an
//! object tagged with its camel-case name, as in
//! `{"type":"setParam","node":3,"param":2,"value":0.5}`; a gain is a string
//! (`"groupPan"`); a node is a bare number. The TypeScript is generated from the
//! same types (`app::protocol`).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::app::graph::{Endpoint, NoteRange, PatternInfo, SourceInfo};
use crate::app::model::{EdgeId, MAX_FADE, ModSourceSlot, NodeId, SYNC_DIVISIONS};
use crate::plugin::chain::{MAX_STEPS, MidiBindSource};
use crate::session::{Feed, NodeUi, Route};

/// What a front-end asks of the controller, naming the node it concerns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Intent {
    /// Set a parameter of the plugin in an instrument, effect or bus-effect node.
    /// `param` is the plugin's own index for it. The controller clamps `value` to
    /// the parameter's range and rounds an enum parameter to a whole value.
    SetParam { node: NodeId, param: u32, value: f32 },
    /// Set a host-side gain: an instrument's volume, a group's volume or pan, or
    /// the dry/wet mix of an effect or bus effect. `gain` says which, and the node
    /// has to be one it applies to. The controller clamps `value` to the gain's
    /// range.
    SetGain { node: NodeId, gain: Gain, value: f32 },
    /// Put the limiter on the output mix in the sound, or take it out (step 102). The limiter is a look-ahead peak limiter that holds the mix under its ceiling (`Gain::LimiterCeiling` of the master)
    /// instead of letting it clip; it is on in a session unless the file says it is not. What is as it is asked to be changes nothing. A step of the history ("turn the limiter off"), never merged
    /// with another; told as `LimiterSet`.
    SetLimiter { on: bool },
    /// Load one of the plugin's presets, by its id, into an instrument or an
    /// effect node. The parameters take the preset's values.
    LoadPreset { node: NodeId, preset: String },
    /// Set one setting of a modulator: an LFO's rate or waveform, an
    /// envelope's stage. `value` is clamped to the setting's range, and a waveform is
    /// rounded to a whole value, an index (see `ModulatorField`). A modulator has no
    /// setting that is not its kind's: an LFO has no attack.
    SetModulatorField { node: NodeId, field: ModulatorField, value: f32 },
    /// Set how deep a modulation cable pulls, -1 to 1 (a negative depth pulls the
    /// other way). The controller clamps it. An audio cable has a level and no depth (`SetCableGain`): it is refused.
    SetEdgeDepth { edge: EdgeId, depth: f32 },
    /// Take a cable out. A modulation cable: the modulator stays, no longer driving what the cable led
    /// to; the cables that drive the depth of this one go with it (and theirs, in turn), since
    /// what they drove is gone. An audio cable: the nodes stay, and the audio no longer goes from one to the other
    /// by it (it goes down to nothing over one buffer). A cable of notes: the lane no longer hears the source by it, and hears
    /// every source if it was the last cable of notes that ended at the lane.
    RemoveEdge { edge: EdgeId },
    /// Add a modulator (the session's own, since step 96 the only kind: it is in no lane and no group), at the end of the session's own rack. It drives nothing yet. It hears every note and every
    /// source (it is in no lane to take a range or a source from) unless `hears` names a lane (its instrument's node) or a group, whose notes it hears then (P5c-1, the ear: what the
    /// "+" of a lane or a group asks since step 94, so that the modulator is the lane's or the group's by what it hears, and not by the rack it is in), and a cable from it can end at a
    /// parameter of any plugin of the session, an instrument's or an effect's, a lane's or a bus's, and at a setting of another free modulator (`AddModEdge`, `Modulate`). A step of the
    /// history, in the words of the button that adds a modulator ("add LFO"): undo takes it out, and redo makes it again under its id, hearing what it heard. Refused, with nothing
    /// changed, for a `hears` that is not a node of the session or is neither a lane nor a group.
    AddFreeModulator { kind: ModulatorKind, hears: Option<NodeId> },
    /// Draw a modulation cable from the modulator `modulator` to the port `to` of a node: a plugin's parameter,
    /// or a setting of another modulator. A parameter has to be one that modulator reaches: any plugin of the session
    /// (an instrument, an effect of a lane's chain or of a group's bus). A setting (`rate` of an LFO, `attack`, `decay`,
    /// `sustain` or `release` of an envelope) or the `depth` of one of the cables of a modulator has to be that of another
    /// modulator, and none reaches itself. The cable starts at half depth (`NEW_CABLE_DEPTH`;
    /// `SetEdgeDepth` changes it). Refused, with nothing changed, for a port that is none of these (an output, an
    /// audio port), a node or a plugin that is not there or out of reach, a node that is no modulator when the port
    /// is a modulator's, a setting its modulator does not have, a depth that is not one of its cables, and a
    /// modulator that drives that port already.
    AddModEdge { modulator: NodeId, to: Endpoint },
    /// Make a modulator of `kind` for what the port `to` of a node is, and draw a cable from it to the port, as one thing: what
    /// the TUI's `m` (a new LFO or envelope for a parameter) and `l` (a MIDI control that learns the next controller that is moved)
    /// do. The modulator is **the session's own** (step 94: a free modulator, at the end of its rack, which any plugin of the session
    /// can be drawn to): it hears the notes of the lane for a plugin of the lane (its instrument or an effect of its chain), of the
    /// group for a bus effect, and what the modulator hears for a setting of one that is free (its rate, a stage of its envelope, the
    /// depth of one of its cables), where the new one is a sibling that drives it. The cable
    /// starts at half depth, or at `depth` (clamped as `SetEdgeDepth` clamps it) if one is given. Refused, with nothing changed (nothing is made,
    /// and no id is used), for what `AddModEdge` refuses of a cable to `to` from a modulator that is not there yet, for a group (it
    /// has no parameter) and for a depth that is not a number.
    Modulate { to: Endpoint, kind: ModulatorKind, depth: Option<f32> },
    /// Draw an audio cable, a send, from the output of audio `from` to the input of audio `to` (P3c of `docs/patch-graph.md` §11.6): `from` is the `audioOut` port of
    /// a lane (what its instrument puts out, after its volume and before its chain), of an effect of a lane's chain or of a group's bus (after its mix) or of a group (after its
    /// effects, its pan and its fade), and `to` the `audioIn` port of an effect (added to what its chain gives it), of a group (its bus: added to its members) or of the master
    /// (`NodeId::MASTER`: the mix). It is in addition to the way the audio goes by itself, down a lane's chain and into its group or the mix. What comes out of `from` goes to `to`
    /// at `gain` (0 to 2, clamped; 1, as it is, if none is given; `SetCableGain` changes it), is heard at what its lane or its group is heard at (muting a lane mutes its sends),
    /// and comes in over one buffer. The cable has an id from the series that the modulation cables have. Refused, with nothing changed (nothing is made, and no id is used), for a port
    /// that is not an output of audio (`from`) or an input of audio (`to`), a node that is not there or has no such port (a modulator, the master as a start, a lane as an end), a
    /// cable between the two ports that is there already, a cable that would make the audio go round, and a level that is not a number.
    ///
    /// **A cable of notes** (P4b): `from` is the `eventsOut` port of a source of notes (the keys, or a device: `AddSource`) and `to` the `eventsIn` port of a lane (its instrument's
    /// node, which an empty lane has too). A lane that no cable of notes ends at hears every source; one that has some hears the sources they start at and no others. The cable has
    /// an id from the same series, and no level: a `gain` is refused. Refused, with nothing changed (nothing is made, and no id is used), for a start that is no source of
    /// notes, an end that is no lane, one of the two ports that is not the one it should be, a node that is not there, a `gain`, and a cable between the two that is there already.
    AddCable { from: Endpoint, to: Endpoint, gain: Option<f32> },
    /// Put the keys, or a MIDI device, in the session as a source of notes: a node with an output of events that cables of notes start at (P4b; `AddCable`). `source` says which:
    /// the keys (the computer's keys, the page's piano, and every browser's) or a device, by its name or as much of it as tells it (`Launchkey` is every port of a Launchkey Mini; it is
    /// held to the text without the spaces round it). The node goes at the end of the sources, with an id of its own. Refused, with nothing changed (nothing is made, and no id is
    /// used), for the keys when they are a source already, for a device whose name is blank, is longer than `MAX_NAME` characters or has a control character in it, and for a device of
    /// a name that is a source already.
    AddSource { source: SourceInfo },
    /// Set the level of an audio cable, 0 to 2 (`CABLE_GAIN_MAX`): the controller clamps it. The cable goes to the new level over one buffer. Refused, with nothing changed, for a
    /// level that is not a number, a cable that is not there and one that is a modulation cable (which has a depth: `SetEdgeDepth`).
    SetCableGain { edge: EdgeId, gain: f32 },
    /// Put a plugin at the end of the effect chain of a lane or of a group's bus: `parent` is the node of the lane
    /// (its instrument's, which an empty lane has too) or of the group, and `plugin` the `id` of an entry of the
    /// catalog the server told (`ServerMessage::Catalog`): it is loaded by the server, which loads only what its own
    /// catalog names, and not an instrument. It starts at the mix that suits it (a filter fully wet, the rest half
    /// wet). Refused, with nothing changed, for a parent that is not a lane or a group, a plugin the catalog does
    /// not name, an instrument, one that does not load, and one whose outputs are not the chain's channels.
    AddEffect { parent: NodeId, plugin: String },
    /// Add an effect that stands by itself (step 87, the page's "+ Effect"): `plugin` is as for `AddEffect`, and it goes in a lane of its own at the end of the session, a lane
    /// that has no instrument, is full range, in no group and has a meter, so that what the effect is fed is what cables bring (the sound of an instrument is cabled to it, and
    /// its own end goes to the mix, or where its way out is cut and a cable is drawn from it). What a front-end shows of the lane is the effect: the lane is its holder, and
    /// the mute, the solo and the way out of the lane are the effect's. The two nodes, the lane's and then the effect's, are made in that order (what a redo gives back under
    /// the ids they had). A step of the history, in the words of the button that adds an effect ("add Reverb"): undo takes the lane out with the effect in it. Refused, with
    /// nothing changed, no lane made and no id used, for what `AddEffect` refuses of a plugin: one the catalog does not name, an instrument, one that does not load and one whose
    /// outputs are not the chain's channels.
    AddFreeEffect { plugin: String },
    /// Add a lane at the end with an instrument in it: `plugin` is the `id` of an entry of the catalog the server told, which has
    /// to be an instrument with an audio output; it is loaded by the server, which loads only what its own catalog names. The lane
    /// is full range, in no group, and has a meter of its own (and the master's meter hears it). Refused, with nothing changed and
    /// no lane made, for a plugin the catalog does not name, an effect, one that does not load, and one with no audio output.
    AddInstrument { plugin: String },
    /// Put an instrument in a lane, replacing the one it has (an empty lane is filled): `node` is the node of the lane, which the
    /// instrument keeps, and `plugin` is as for `AddInstrument`. The lane keeps its effects, modulators, range, volume and group.
    /// The cables that drove the old plugin's parameters (a lane's and a group's, with what drives their depths) are taken out,
    /// since the new plugin has parameters of its own, and a note remap goes with the old plugin. Refused, with nothing changed,
    /// for a node that is not a lane, and for what `AddInstrument` refuses.
    ReplaceInstrument { node: NodeId, plugin: String },
    /// Take the instrument out of a lane (the TUI's `d` on an instrument that has one, the page's "Remove only the instrument"): `node` is the node of the lane. The lane
    /// keeps what else it has, its effects (which go on running, on what is cabled into them), modulators, pattern, range, volume, transpose, group, way out, mute and solo, and
    /// is an empty lane until an instrument is put in it (`ReplaceInstrument`). The cables that drove the plugin's parameters (a lane's and a group's, with what drives their
    /// depths) are taken out, since there is nothing left for them to drive, and a note remap goes with the plugin. A step of the history: undo puts the very plugin back, with its
    /// cables. Refused, with nothing changed, for a node that is not a lane and for a lane that has no instrument.
    ClearInstrument { node: NodeId },
    /// Set the keys a lane plays: `node` is the node of the lane (its instrument's, which an empty lane has too), and
    /// `range` the notes it takes, both ends included, or `None` for every note. A lane that is given a range
    /// plays no note outside it (a held note that was counted under the old range is let go). The ends are notes,
    /// 0 to 127, and the low one is not above the high one. Refused, with nothing changed, for a node that is not
    /// a lane and for a range that is not a range of notes.
    SetRange { node: NodeId, range: Option<NoteRange> },
    /// Mute or unmute a lane or a group (the TUI's `M`, the page's M button): `node` is the node of the lane (its instrument's, which an empty lane has
    /// too) or of the group. A muted lane or group is never heard, and a mute wins over a solo; its notes, modulators and pattern go on (and a muted group's
    /// members and bus effects), so that what is heard when it is brought back is where it would be. It is let go of, and brought back, over one buffer.
    /// Saved with the session (`mute = true`). Refused, with nothing changed, for a node that is neither a lane nor a group.
    SetMute { node: NodeId, muted: bool },
    /// Solo or unsolo a lane or a group (the TUI's `S`, the page's S button): while anything is soloed only what is soloed is heard, with the group a soloed
    /// lane is in (its bus is heard for it) and every member of a soloed group. Saved with the session (`solo = true`). Refused, with nothing changed, for a node
    /// that is neither a lane nor a group.
    SetSolo { node: NodeId, solo: bool },
    /// Cut or give back the way out of a lane or a group (the page's choice on the node; P3c-2 of `docs/patch-graph.md` §11.6): `node` is the node of the lane (its instrument's,
    /// which an empty lane has too) or of the group. A lane's end goes to the bus of its group or to the mix (`out`, which every lane and group has to begin with) or, cut (`none`),
    /// nowhere but the audio cables that start at it, so that an oscillator that is sent to a reverb can be heard only through it; a group's end goes to the mix, or is cut the same
    /// way. The wiring changes at once, as a lane's group does, and the intent is told as a snapshot (the links of the graph change). Saved with the session (`route = "none"`). Refused,
    /// with nothing changed, for a node that is neither a lane nor a group, and for a way out that is given back where the audio would go round, by the cables there are.
    SetRoute { node: NodeId, route: Route },
    /// Set whose notes a modulator hears (step 93, P5c-1 of `docs/patch-graph.md` §11.6: the page's choice on the modulator, the terminal's row): `node` is the
    /// modulator's and `ear` the node of a lane (its instrument's, which an empty lane has too) or of a group, or none for every event, whatever its source, which is what a modulator hears
    /// to begin with. A modulator that hears a lane is given the notes that the lane's instrument is given (after the sources the lane hears, its range, its remap, its pattern and its
    /// transpose), so that an envelope follows a pattern that the lane plays and an LFO that restarts at a note restarts at the lane's; one that hears a group is given those of its members.
    /// Told as a snapshot (the graph says whom a modulator hears) and saved with the session (`hears = "instrument[0]"`). Refused, with nothing changed, for a node that is not a modulator
    /// and for an ear that is neither a lane nor a group.
    SetModulatorEar { node: NodeId, ear: Option<NodeId> },
    /// Cut or give back the link into an effect from the stage before it in its chain (step 86: the page's × on a link of a chain and its button on the effect's node, the TUI's `f`): `node`
    /// is the node of the effect, a lane's or a group's bus. An effect is fed by the stage before it (the lane's instrument, the bus's sum or the effect before it: `chain`, which every
    /// effect has to begin with) and, cut (`none`), by the audio cables that end at it and by nothing else, so that a link of a chain is taken apart as a cable is. The wiring changes
    /// at once, as a way out does, and the intent is told as a snapshot (the links of the graph change). Saved with the session (`feed = "none"` on the effect). Refused, with nothing
    /// changed, for a node that is not an effect, and for a feed that is given back where the audio would go round, by the cables there are.
    SetFeed { node: NodeId, feed: Feed },
    /// Set the scale of the piano (the Piano tab's): `scale` is a root and a kind of scale as the session file
    /// says it, `C major` or `F# dorian`, where the root may be a sharp or a flat, and the name of the kind
    /// short or long, in any case. The piano colours the notes of the scale, and drops the others when it is
    /// locked (`SetLocked`). The scale is saved with the session. Refused, with nothing changed, for text that is
    /// not a scale.
    SetScale { scale: String },
    /// Lock the piano to its scale, so that a note that is not in it is dropped where it is sent (from the page,
    /// a MIDI keyboard or the computer's keys), or let every note through, with the ones outside the scale only
    /// coloured. It is saved with the session.
    SetLocked { locked: bool },
    /// Set the tempo that patterns are recorded at, in beats a minute (the TUI's `b`, the page's tempo field): the metronome's count-in and the length of the
    /// next recording are counted in it. It is the session's (`bpm` in the file) and the audio thread's; a pattern that has been recorded keeps the tempo it
    /// was recorded at. `bpm` is held to `BPM_MIN` to `BPM_MAX`. Refused, with nothing changed, for a value that is not a number.
    SetBpm { bpm: f32 },
    /// Start recording a pattern on the lane `node`, or stop the recording that is going on. A recording is counted in by the metronome, four beats of
    /// the session's tempo, and then records the notes that are played for the pattern's length in beats; it ends by itself when that has gone by, and
    /// `recording: false` ends it at once (a pattern that was being recorded is what has been played so far, as long as it was to be). The audio thread
    /// reports the pattern when it is made, and the controller puts it in the lane. A lane that has none is given an empty pattern (4 beats, looping, not
    /// in key); one that has keeps its settings and its notes until the new ones are made. Starting one that is going on, and stopping one that is not, is
    /// told as it is and changes nothing. Refused, with nothing changed, for a node that is not a lane.
    SetRecording { node: NodeId, recording: bool },
    /// Take the pattern of the lane `node` out: the audio thread forgets it, and a recording that was going on ends. Refused, with nothing changed, for a node
    /// that is not a lane and for a lane that has no pattern.
    ClearPattern { node: NodeId },
    /// Set a setting of the pattern of the lane `node` (`PatternField`: its length in beats, whether a held key plays it, whether it loops and how it is
    /// transposed): the value is held to the setting's range and a switch is rounded to 0 or 1. A lane that has no pattern is given an empty one with the
    /// setting, so that the length of its first recording can be chosen. Refused, with nothing changed, for a node that is not a lane and for a value that is
    /// not a number.
    SetPatternField { node: NodeId, field: PatternField, value: f32 },
    /// Say where a front-end draws a node, in the pixels of its canvas, or `None` to give the node back to the
    /// front-end's own layout. `node` is any node of the session, the master (`NodeId::MASTER`) included. A place is
    /// kept to a tenth of a pixel and to 100 000 either way, which the controller does for the client; the engine has
    /// no use for it, and the TUI does not show it, but it is saved with the session (the node's `ui` in the file) and
    /// told to every client. Refused, with nothing changed, for a node that is not there and for numbers that are not
    /// numbers.
    SetNodeUi { node: NodeId, ui: Option<NodeUi> },
    /// Say which parameters of a plugin a front-end shows as knobs on its node: `node` is the node of an instrument (a lane's) or of an
    /// effect or a bus effect, and `params` the plugin's own indices of the parameters (`Param::index`), in the order the front-end shows
    /// them; none gives the node back to the front-end's own choice (the page shows the first few). It replaces the list there was. The
    /// engine has no use for it, and the TUI does not show it, but it is saved with the session (the plugin's `pinned`, by name) and told
    /// to every client. It belongs to the plugin: an effect that is moved takes it along, and an instrument that is replaced has none.
    /// Refused, with nothing changed, for a node that has no plugin, a parameter the plugin does not have, a separator, a parameter
    /// listed twice and more than `MAX_PINNED` of them.
    SetPinned { node: NodeId, params: Vec<u32> },
    /// Set the values of a step sequencer, a modulator's: `node` is the node of the modulator and `values` all of its steps, from the
    /// first, `MAX_STEPS` of them (the ones past its length are kept for when it is longer again), each held to -1 to 1. It is one thing to the history, so
    /// that a stroke across the bars is one step to undo. Refused, with nothing changed, for a node that is not a step sequencer, a list that has not as many
    /// steps as there are, and a value that is not a number.
    SetSteps { node: NodeId, values: Vec<f32> },
    /// Add a group, a submix bus, at the end: it has no effects and no modulators, full volume and a centred pan, and a meter of
    /// its own. `members` are lanes (each the node of a lane, its instrument's, which an empty lane has too) that are put in it
    /// at once, as `SetLaneGroup` does for each (a lane in another group leaves it); none makes an empty group. Refused, with
    /// nothing changed and no group made, for a member that is not a lane.
    AddGroup { members: Vec<NodeId> },
    /// Put a lane in a group, or take it out of the one it is in (`None`): `node` is the node of the lane (its instrument's, which an
    /// empty lane has too) and `group` the node of a group. A lane is in one group at most: one that is in another leaves it. What it
    /// leaves is no longer driven by it: the cables of the group it leaves to the lane's plugins and its modulators are taken out (with
    /// the cables that drive their depths) and what they drove has the value that was set again, and the cables to the members after
    /// the lane keep driving the same plugins. A lane put in the group it is in is as it was. Refused, with nothing changed, for a
    /// node that is not a lane and for a group that is not a group.
    SetLaneGroup { node: NodeId, group: Option<NodeId> },
    /// Name a group: `name` is what it is called, without the spaces around it; a name of nothing gives the group back the name it has
    /// by default, "Group" and its number, counting from one. A name is at most `MAX_NAME` characters and has no control characters.
    /// The engine has no use for it, but it is saved with the session and told to every client. Refused, with nothing changed, for a
    /// node that is not a group and a name that is not one.
    RenameGroup { node: NodeId, name: String },
    /// Call a macro something (step 110: the user's "let a Macro have a name"): `name` is what it is called, without the spaces around it, so that the knob of it on the box of a module that it is in says what it
    /// does ("Brightness"); a name of nothing gives it back the name it has by default, "Macro". A name is at most `MAX_NAME` characters and has no control characters. The engine has no use for it, but it is saved with
    /// the session and told to every client (as a snapshot, since the graph names the node by it). A step of the history ("rename a macro"), which a name that it has already is not. Refused, with nothing changed, for a
    /// node that is not a macro and a name that is not one.
    RenameMacro { node: NodeId, name: String },
    /// Have a macro show its knob on the box of the module that it is in, or not (step 110: the user's "an expose option/flip, that will expose the macro-knob when used in a module"): on to begin with, which is what a
    /// macro that is in a module is for. It changes nothing of the sound, of the cables of the macro or of what the lane of a module shows when it is open; it is saved with the session and told to every client (as a snapshot, since
    /// the graph says it). A step of the history ("show a macro on its box", "hide a macro from its box"), which a macro that is so already is not. Refused, with nothing changed, for a node that is not a macro.
    ExposeMacro { node: NodeId, expose: bool },
    /// Take a node out of the session (a step of the history, which undo puts back whole). A modulator can be, with its own cables and the
    /// cables of the others that drove its settings (and what drove the depth of any of those), and so can an
    /// effect or a bus effect, with the cables that drive its parameters (and what drove their depths): what they
    /// were driving goes with it, and the audio path closes up. A lane goes with its instrument, its effects and its
    /// modulators, the cables of its group to it and the cables of notes that end at it; the last lane of a session is refused (a session keeps one).
    /// A group goes with its bus effects, its modulators and every cable of it: its members stay, in no group, and
    /// what its cables were driving in them has the value that was set. A source of notes goes with the cables of notes that start at it: the lanes that heard it
    /// hear what their other cables bring, and every source if it was their last. Any other node (the master) is refused.
    RemoveNode { node: NodeId },
    /// Collapse nodes that are in the session into one module, where they are (step 109: the user's "compact/collapse them into a module in place ... to clean up the ui a bit"): `nodes` are what is chosen on a canvas, the
    /// instrument of a lane (the lane's node), an effect or a modulator, and the lane is the unit, so that a lane that has any of its plugins chosen is in the module whole, with every plugin it has. The module is the record that a
    /// module that is added is (`ModuleInstance`: the lanes in the order of the session and then the modulators in the order of their rack, under a node of its own that is made last, so that no other id changes), which a front-end
    /// draws as one box and opens again; nothing about the sound changes and nothing is told to the audio thread. `name` is what the box is called (the spaces round it go; at most `MAX_NAME` characters and no control character;
    /// a name of nothing is `Module`), and `ui` where it is drawn, if a front-end says (it is the box's place, as `SetNodeUi` would say it, in the same step). A step of the history ("collapse into Room"), which undo takes the
    /// record out of (`UngroupModule`; the nodes stay) and redo makes again under the node it had. Refused, with nothing changed and no id used, for a node that is not there (the first, if more than one: `NotFound`), a node that is a group, a bus effect, a source of
    /// notes or a module (a module cannot hold a module yet), none at all, a lane or a modulator that is in a module already, a name that is not one and a place that is not a place.
    CollapseIntoModule { nodes: Vec<NodeId>, name: String, ui: Option<NodeUi> },
    /// Take the box away and keep what is in it (step 109): the module `node`, a record that a module that was added or collapsed is, is gone, and its lanes and modulators stay in the session as they are, each in none. A step of the
    /// history ("ungroup Room"), which undo puts back as the record was. It is not `RemoveNode`, which takes the module out with all that is in it. Refused, with nothing changed, for a node that is not a module.
    UngroupModule { node: NodeId },
    /// Move an effect to another place in its chain: `node` is the node of an effect in a lane's chain or on a group's bus, and `index`
    /// the place it is to have among the effects of that chain, from 0 (the first, the nearest the instrument or the group) to one less
    /// than the chain has. The effects between its old place and its new one move one place the other way, and the audio path goes
    /// through them in the new order. The cables that drive the effect's parameters, and those that drive the other plugins of the lane
    /// or of the bus, stay on the plugins they drove (the controller tells the audio thread the places they have now). Refused, with
    /// nothing changed, for a node that is not an effect (a lane, a group, a modulator), a place the chain has not, and the place the
    /// effect is at.
    MoveEffect { node: NodeId, index: u32 },
    /// Write the session to the file it came from. A client does not name a file: it is the
    /// session's own. Refused (`IntentError::Io`) if the session has no file or it cannot be written,
    /// and then nothing changes.
    Save,
    /// Write the session to a file called `name`, beside the one it has (where the program was started, if it has none), and
    /// have that file be the session's from then on: what `Save` writes, and what the clients are told it is called (`Saved`). The
    /// client gives a name and not a path: `session::plain_file_name` makes the file of it (`.toml` added, the spaces round it gone) and
    /// refuses what is not one, in its own words (`Invalid`): a folder in it, a character that a file cannot have on every system, a name
    /// that Windows keeps, one that is too long, none. A file that is there already is refused too (`Invalid`), unless it is the session's own
    /// (that is a `Save`): a client cannot see the directory, and a name that is typed twice must not be a file that is lost. One that cannot
    /// be written is `Io`. Refused, nothing changes: the session keeps its file and its mark, and no revision is used. It is no step of the
    /// history, and does not forget it.
    SaveAs { name: String },
    /// Save the chain of effects of a lane as a module in the library (step 104): `node` is the node of the lane, or of an effect in its chain, and `name` what the module is called, which
    /// `module::clean_name` makes a file name of (the spaces round it go, `.toml` is not part of it, and it has to be a name and not a path). The module is the effects as they are now, in
    /// order, with their presets, values, mixes, knobs and feeds, and the audio cables between them; what is not in the chain (the lane's instrument, its modulators, the cables that
    /// go into the chain and out of it) is not saved. A module that has the name already is replaced. It is added to a session by `AddFreeEffect` with the module's id from the catalog
    /// (`module:<name>`), which the clients are told again. It is no change of the session: no revision of it, no step of the history, and the session is not marked. Refused, with
    /// nothing written, for a node that is not a lane or an effect of one, a lane that has no effect, a name that is not one, and a program that has no library (`Io` for a write that fails).
    SaveModule { node: NodeId, name: String },
    /// Save the plugins in `nodes` as a module in the library (step 105: "save the graph of fx or instruments ... into its own module"): the instrument of a lane (the lane's node) or an effect, any of
    /// them, from any lanes. The module is each lane that has one of them, with its instrument if that was chosen and the effects that were, in the order of their chains, each as it is now (presets,
    /// values, mixes, volumes, knobs, feeds), and the audio cables that have both ends among them, with their levels. What is not chosen is left out, and so is what joins the chosen to it: the cables that
    /// go in and out, the lane's way out, a link from a stage that was not chosen (the effect after it is not fed by the chain: it is fed by the cables that are drawn). `name` is as in `SaveModule`, and the
    /// module is replaced if it is there; it is added by `AddFreeEffect` with its id from the catalog, as a chain is, in a lane of its own for each of the module's, with the cables between them.
    /// Like `SaveModule` it is no change of the session: no step of the history, the session is not marked, and a revision is used. Refused, with nothing written, for a node that is not there (the first, if
    /// more than one: `NotFound`), a name that is not one, a node that is a group, a group's bus effect, a modulator or a source (a module has none of those yet), a lane that has no instrument, none at all,
    /// and a program that has no library (`Io` for a write that fails).
    SaveSelectionAsModule { nodes: Vec<NodeId>, name: String },
    /// Open the plugin's own window (step 112, "a button to open the official UI"): the face that the plugin draws for itself, in a window of Tang's own on the machine that runs it, which takes the keyboard from the program's
    /// terminal; one that is open is brought to the front. `node` is a lane's instrument (the lane's node) or an effect, a bus effect included, and the values that the session has for the plugin's parameters are told to its controller
    /// first, so that the window shows them. What a person does to the plugin's knobs in the window comes back as `SetParam` (a drag is one step of the history); a window is no change of the session: no step of the history, the
    /// session is not marked, and a revision is used, as a save uses one (`EditorOpened`). Refused, with nothing opened, for a node that is not there, one that has no plugin (a group, a modulator, a source, a module, a lane that has
    /// no instrument), a plugin that has no window of its own (the built-in ones, the CLAP and LV2 plugins: only VST3 so far), a machine that has no screen to open one on, a plugin scan that is still going (the windows' events are not turned
    /// while it is: f179) and a plugin whose window did not open, each in its own words.
    OpenEditor { node: NodeId },
    /// Put back what the last step that can be undone did: a parameter, a gain, a modulator's setting, a cable's depth, the keys
    /// of a lane, the place of a node, the knobs a node shows, a group's name, the scale or the lock of the piano, the place of
    /// an effect. A change that goes on (a knob that is dragged) is one step. What it does is told as the intents it is made of
    /// are (their events), and what Undo and Redo would do now is told to the clients as `History`. What cannot be undone yet
    /// (what adds or takes out nodes or cables, a preset, an instrument that is replaced) empties the history. Refused, with nothing
    /// changed and no revision, if there is nothing to undo, or if what the step names is not there any more (the step is
    /// dropped, and the one before it is what Undo does next).
    Undo,
    /// Do again what the last step that was undone did. Refused if there is nothing to redo; a change made since an undo ended what
    /// could be redone.
    Redo,
}

/// What a new modulator is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ModulatorKind {
    /// A sine at 1 Hz.
    Lfo,
    /// An envelope of 10 ms attack, 0.3 s decay, 0.7 sustain and 0.5 s release, opened by the notes
    /// its lane (or its group's members) plays.
    Envelope,
    /// A hardware control, waiting for the next one that comes in to bind to.
    MidiLearn,
    /// A knob on the screen, at rest (0): a number that the person sets and nothing else moves, that drives every parameter with a cable from it.
    Macro,
    /// The velocity of the last note the lane plays (the group's members', for a group's), bound when it is made.
    Velocity,
    /// The number of the last note the lane plays, bound when it is made.
    Key,
    /// A step sequencer: sixteen steps that climb from the bottom in eighths, eight a second, started again by every note.
    Steps,
}

impl ModulatorKind {
    /// What the modulator is to begin with.
    pub fn source(self) -> ModSourceSlot {
        match self {
            ModulatorKind::Lfo => ModSourceSlot::new_lfo(),
            ModulatorKind::Envelope => ModSourceSlot::new_envelope(),
            ModulatorKind::MidiLearn => ModSourceSlot::new_midi_learn(),
            ModulatorKind::Macro => ModSourceSlot::new_macro(),
            ModulatorKind::Velocity => ModSourceSlot::new_note_control(MidiBindSource::Velocity),
            ModulatorKind::Key => ModSourceSlot::new_note_control(MidiBindSource::Key),
            ModulatorKind::Steps => ModSourceSlot::new_steps(),
        }
    }

    /// The kind that a modulator with `source` is.
    pub fn of(source: &ModSourceSlot) -> ModulatorKind {
        match source {
            ModSourceSlot::Lfo { .. } => ModulatorKind::Lfo,
            ModSourceSlot::Envelope { .. } => ModulatorKind::Envelope,
            ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), .. } => ModulatorKind::Velocity,
            ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), .. } => ModulatorKind::Key,
            ModSourceSlot::MidiLearn { .. } => ModulatorKind::MidiLearn,
            ModSourceSlot::Macro { .. } => ModulatorKind::Macro,
            ModSourceSlot::Steps { .. } => ModulatorKind::Steps,
        }
    }

    /// What a button that adds one says it adds: "an LFO".
    pub fn label(self) -> &'static str {
        match self {
            ModulatorKind::Lfo => "an LFO",
            ModulatorKind::Envelope => "an envelope",
            ModulatorKind::MidiLearn => "a MIDI control",
            ModulatorKind::Macro => "a macro",
            ModulatorKind::Velocity => "a velocity control",
            ModulatorKind::Key => "a key control",
            ModulatorKind::Steps => "a step sequencer",
        }
    }
}

/// A setting of the pattern of a lane, by name (the rows of the terminal's pattern pane).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum PatternField {
    /// How long the pattern is, and what the next recording is counted in, in beats: 1 to 32.
    Length,
    /// Whether a held key plays the pattern: 0 or 1.
    Enabled,
    /// Whether it goes round again while the key is held: 0 or 1.
    Looping,
    /// How it is transposed to the key that is held: 0 by semitones, 1 by the degrees of the piano's scale.
    Transpose,
}

impl PatternField {
    /// The lowest and the highest value.
    pub fn range(self) -> (f32, f32) {
        match self {
            PatternField::Length => (1.0, 32.0),
            PatternField::Enabled | PatternField::Looping | PatternField::Transpose => (0.0, 1.0),
        }
    }

    /// Whether it is a switch: its value is 0 or 1, and is rounded to one of them.
    pub fn is_switch(self) -> bool {
        !matches!(self, PatternField::Length)
    }
}

/// A setting of a modulator, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ModulatorField {
    /// An LFO's rate, in Hz: 0.01 to 50.
    Rate,
    /// An LFO's waveform, as an index: 0 sine, 1 triangle, 2 saw, 3 square, 4 random, 5 smooth.
    Waveform,
    /// An envelope's attack time, in seconds: 0.001 to 10.
    Attack,
    /// An envelope's decay time, in seconds: 0.001 to 10.
    Decay,
    /// An envelope's sustain level: 0 to 1.
    Sustain,
    /// An envelope's release time, in seconds: 0.001 to 10.
    Release,
    /// A macro's value: 0 to 1.
    Value,
    /// A step sequencer's glide: how much of each step is spent going to the value of the next, 0 (none: each is held) to 1 (all of it).
    Glide,
    /// How many of a step sequencer's steps are in use: 1 to 16, a whole number.
    Length,
    /// Whether a note starts a step sequencer again at its first step, or an LFO again at the start of its wave: 0 or 1.
    Retrigger,
    /// How many seconds an LFO takes to come in from nothing after a note: 0 (it does not) to 5.
    Fade,
    /// Whether an LFO or a step sequencer follows the tempo, and by what note length: 0 (it goes at its own rate) or the place of a note length in `SYNC_DIVISIONS`, from 1, a whole number.
    Sync,
}

impl ModulatorField {
    /// Every setting there is, for what goes through all of them (a table of the page's constants, a test of each: only tests do). One that is left out is a key missing in the `Record` of
    /// the page's constants, which its typecheck refuses.
    #[cfg(test)]
    pub const ALL: [ModulatorField; 12] = [
        ModulatorField::Rate,
        ModulatorField::Waveform,
        ModulatorField::Attack,
        ModulatorField::Decay,
        ModulatorField::Sustain,
        ModulatorField::Release,
        ModulatorField::Value,
        ModulatorField::Glide,
        ModulatorField::Length,
        ModulatorField::Retrigger,
        ModulatorField::Fade,
        ModulatorField::Sync,
    ];

    /// What it is called.
    pub fn label(self) -> &'static str {
        match self {
            ModulatorField::Rate => "rate",
            ModulatorField::Waveform => "waveform",
            ModulatorField::Attack => "attack",
            ModulatorField::Decay => "decay",
            ModulatorField::Sustain => "sustain",
            ModulatorField::Release => "release",
            ModulatorField::Value => "value",
            ModulatorField::Glide => "glide",
            ModulatorField::Length => "length",
            ModulatorField::Retrigger => "retrigger",
            ModulatorField::Fade => "fade",
            ModulatorField::Sync => "sync",
        }
    }

    /// The lowest and the highest value.
    pub fn range(self) -> (f32, f32) {
        match self {
            ModulatorField::Rate => (0.01, 50.0),
            ModulatorField::Waveform => (0.0, (crate::plugin::chain::LfoWaveform::ALL.len() - 1) as f32),
            ModulatorField::Attack | ModulatorField::Decay | ModulatorField::Release => (0.001, 10.0),
            ModulatorField::Sustain | ModulatorField::Value | ModulatorField::Glide | ModulatorField::Retrigger => (0.0, 1.0),
            ModulatorField::Length => (1.0, MAX_STEPS as f32),
            ModulatorField::Fade => (0.0, MAX_FADE),
            ModulatorField::Sync => (0.0, SYNC_DIVISIONS.len() as f32),
        }
    }

    /// Whether its value is a whole number, which the controller rounds it to: a place in a list (a waveform), a count (a length) or a switch.
    pub fn is_whole(self) -> bool {
        matches!(self, ModulatorField::Waveform | ModulatorField::Length | ModulatorField::Retrigger | ModulatorField::Sync)
    }

    /// How its values are spread over its range when it is turned: a rate and a time run over thousands, which a linear control would crowd into its first
    /// corner, so they are turned by ratios; a level is turned by distances.
    pub fn taper(self) -> Taper {
        match self {
            ModulatorField::Rate | ModulatorField::Attack | ModulatorField::Decay | ModulatorField::Release => Taper::Log,
            ModulatorField::Waveform
            | ModulatorField::Sustain
            | ModulatorField::Value
            | ModulatorField::Glide
            | ModulatorField::Length
            | ModulatorField::Retrigger
            | ModulatorField::Fade
            | ModulatorField::Sync => Taper::Linear,
        }
    }
}

/// How the values of a setting are spread over its range when it is turned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taper {
    /// Equal steps are equal distances: a level.
    Linear,
    /// Equal steps are equal ratios: what runs over decades. It has no zero, so its range starts above it.
    Log,
}

/// A host-side gain, outside the plugin's own parameter list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Gain {
    /// An instrument's output volume, applied before its effects: 0 to 4, 1 is
    /// unity.
    Volume,
    /// An effect's, or a bus effect's, dry/wet mix: 0 (dry) to 1 (wet).
    Mix,
    /// A group's output volume: 0 to 4.
    GroupVolume,
    /// A group's stereo balance: -1 (left) to 1 (right).
    GroupPan,
    /// The output mix's volume, set on `NodeId::MASTER` (step 102): 0 to 2, 1 is unity. It comes before the limiter.
    MasterVolume,
    /// Where the limiter on the output mix holds the sound under, set on `NodeId::MASTER`: -24 to 0 decibels below full scale.
    LimiterCeiling,
}

impl Gain {
    /// The lowest and the highest value.
    pub fn range(self) -> (f32, f32) {
        match self {
            Gain::Volume | Gain::GroupVolume => (0.0, 4.0),
            Gain::Mix => (0.0, 1.0),
            Gain::GroupPan => (-1.0, 1.0),
            Gain::MasterVolume => (0.0, crate::app::model::MASTER_VOLUME_MAX),
            Gain::LimiterCeiling => (crate::app::model::CEILING_MIN, crate::app::model::CEILING_MAX),
        }
    }

    /// What changing it is, for a button that undoes it.
    pub fn label(self) -> &'static str {
        match self {
            Gain::Volume => "set volume",
            Gain::Mix => "set mix",
            Gain::GroupVolume => "set the group's volume",
            Gain::GroupPan => "set pan",
            Gain::MasterVolume => "set the master volume",
            Gain::LimiterCeiling => "set the limiter's ceiling",
        }
    }
}

/// What happened, in terms a front-end can apply to its own copy of the session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    /// A parameter now has `value` (after clamping and rounding).
    ParamSet { node: NodeId, param: u32, value: f32 },
    /// A gain now has `value` (after clamping).
    GainSet { node: NodeId, gain: Gain, value: f32 },
    /// A preset is loaded, by name; these are the parameters it set, by the
    /// plugin's own index, with their new values.
    PresetLoaded { node: NodeId, preset: String, values: Vec<ParamValue> },
    /// A lane now takes the notes of `range`, or every note if it is `None`.
    RangeSet { node: NodeId, range: Option<NoteRange> },
    /// A lane or a group is now muted, or is not.
    MuteSet { node: NodeId, muted: bool },
    /// A lane or a group is now soloed, or is not.
    SoloSet { node: NodeId, solo: bool },
    /// The limiter on the output mix is now in the sound, or is not.
    LimiterSet { on: bool },
    /// The piano's scale is now this: `scale` as the session file says it (`F# dorian`), and what a client
    /// needs to draw it without knowing any scale: its `root` (a pitch class, 0 C to 11 B), the short name of
    /// its `kind` (`dorian`, `harm minor`) and the pitch classes of its `notes`, lowest first.
    ScaleSet { scale: String, root: u8, kind: String, notes: Vec<u8> },
    /// The piano is locked to its scale, or it is not.
    LockedSet { locked: bool },
    /// The tempo is now `bpm` (after it was held to the tempos there can be).
    BpmSet { bpm: f32 },
    /// The pattern of a lane is now this, or the lane has none (after a recording began or ended, a setting of it was set, or it was taken out).
    PatternSet { node: NodeId, pattern: Option<PatternInfo> },
    /// A node is now drawn at `ui` (after rounding and clamping), or wherever the front-end lays it out if it is `None`.
    NodeUiSet { node: NodeId, ui: Option<NodeUi> },
    /// The parameters now pinned to the node's plugin, in order.
    PinnedSet { node: NodeId, params: Vec<u32> },
    /// A group is now called `name`: what it was given, or "Group" and its number if it was given none.
    GroupRenamed { node: NodeId, name: String },
    /// A step sequencer now has these steps, all of them, held to -1 to 1.
    StepsSet { node: NodeId, values: Vec<f32> },
    /// A modulator's setting now has `value` (after clamping, and rounding a waveform).
    ModulatorFieldSet { node: NodeId, field: ModulatorField, value: f32 },
    /// A cable now pulls `depth` (after clamping).
    EdgeDepthSet { edge: EdgeId, depth: f32 },
    /// An audio cable now has the level `gain` (after clamping).
    CableGainSet { edge: EdgeId, gain: f32 },
    /// The session is written: it has nothing unsaved now. `file` is the name of the file it was
    /// written to (not its path), if the session has one.
    Saved { file: Option<String> },
    /// A module is written to the library under `name` (the name as `module::clean_name` made it): the session is as it was, and the catalog has the module.
    ModuleSaved { name: String },
    /// The window of the plugin in `node` is open on the machine that runs Tang (`OpenEditor`): the session is as it was. A window that the person closes is not told: it is theirs.
    EditorOpened { node: NodeId },
}

/// A parameter, by the plugin's own index, and the value it has.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct ParamValue {
    pub param: u32,
    pub value: f32,
}

/// An event as it goes out to the clients: the revision of the session it brought
/// about, then the event itself, in one object
/// (`{"rev":9,"type":"paramSet","node":3,...}`). A client that sees a gap in the
/// revisions has missed something and asks for a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends events"))]
pub struct StampedEvent {
    pub rev: u64,
    #[serde(flatten)]
    pub event: Event,
}

/// An intent that was carried out.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// The session's revision after the intent: one more than before, every time.
    pub rev: u64,
    pub events: Vec<Event>,
    /// The intent added or took out a node or a cable, which no event describes: a front-end
    /// that draws the structure has to draw it again, and a client is given a snapshot (the one
    /// projection of the graph is the controller's: a client that derived the audio path, the
    /// order of the nodes, from events would be a second one). It follows at once, at the revision
    /// after this one.
    pub restructured: bool,
}

impl Applied {
    /// An intent that changed values, as the `events` tell.
    pub fn of(rev: u64, events: Vec<Event>) -> Applied {
        Applied { rev, events, restructured: false }
    }

    /// An intent that changed the structure of the session.
    pub fn restructured(rev: u64) -> Applied {
        Applied { rev, events: Vec::new(), restructured: true }
    }

    /// The events, each stamped with the revision this intent brought about.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends events"))]
    pub fn stamped(&self) -> Vec<StampedEvent> {
        self.events.iter().map(|event| StampedEvent { rev: self.rev, event: event.clone() }).collect()
    }
}

/// An intent that was not carried out. Nothing changed.
#[derive(Clone, Debug, PartialEq)]
pub enum IntentError {
    /// No node has that id: it was removed, or never existed.
    NotFound(NodeId),
    /// No modulation cable has that id: it was removed, or never existed.
    EdgeNotFound(EdgeId),
    /// The intent does not make sense for what it names.
    Invalid(String),
    /// A file could not be written: the reason is in the words.
    Io(String),
}

impl fmt::Display for IntentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntentError::NotFound(node) => write!(f, "there is no node {}", node.0),
            IntentError::EdgeNotFound(edge) => write!(f, "there is no cable {}", edge.0),
            IntentError::Invalid(why) | IntentError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for IntentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::graph::{NoteRange, Port};
    use crate::session::NodeUi;
    use serde_json::{Value, json};

    fn wire<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    #[test]
    fn the_waveform_setting_runs_from_the_first_waveform_to_the_last() {
        use crate::plugin::chain::LfoWaveform;
        let (lowest, highest) = ModulatorField::Waveform.range();
        assert_eq!((lowest, highest), (0.0, 5.0));
        assert_eq!(LfoWaveform::ALL[lowest as usize], LfoWaveform::Sine);
        assert_eq!(LfoWaveform::ALL[highest as usize], LfoWaveform::Smooth);
        assert_eq!(LfoWaveform::ALL.len(), highest as usize + 1, "a waveform that is not in the range cannot be chosen");
    }

    #[test]
    fn a_macro_is_made_at_rest_and_is_the_kind_that_it_says() {
        let source = ModulatorKind::Macro.source();
        assert!(matches!(source, ModSourceSlot::Macro { value, .. } if value == 0.0), "{source:?}");
        assert_eq!(ModulatorKind::of(&source), ModulatorKind::Macro);
        assert_eq!(ModulatorKind::Macro.label(), "a macro");
    }

    #[test]
    fn a_velocity_and_a_key_are_made_bound_and_are_the_kinds_they_say() {
        for (kind, bound, label) in [(ModulatorKind::Velocity, MidiBindSource::Velocity, "a velocity control"), (ModulatorKind::Key, MidiBindSource::Key, "a key control")] {
            let source = kind.source();
            assert!(matches!(source, ModSourceSlot::MidiLearn { source: Some(s), learning: false } if s == bound), "{kind:?}: {source:?}");
            assert_eq!(ModulatorKind::of(&source), kind);
            assert_eq!(kind.label(), label);
        }
        // The controls of a hardware controller are the kind that learns, whatever they are bound to.
        for source in [Some(MidiBindSource::Cc(74)), Some(MidiBindSource::PitchBend), Some(MidiBindSource::Aftertouch), None] {
            assert_eq!(ModulatorKind::of(&ModSourceSlot::MidiLearn { source, learning: source.is_none() }), ModulatorKind::MidiLearn, "{source:?}");
        }
    }

    #[test]
    fn the_value_of_a_macro_runs_from_nothing_to_one() {
        assert_eq!(ModulatorField::Value.range(), (0.0, 1.0));
        assert_eq!(ModulatorField::Value.label(), "value");
        assert_eq!(wire(&ModulatorField::Value), json!("value"));
        assert_eq!(serde_json::from_value::<ModulatorField>(json!("value")).unwrap(), ModulatorField::Value);
    }

    #[test]
    fn a_step_sequencer_is_made_as_a_staircase_of_sixteen_steps_and_is_the_kind_that_it_says() {
        let source = ModulatorKind::Steps.source();
        let ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } = source else { panic!("{source:?}") };
        assert_eq!(
            values,
            [-1.0, -0.875, -0.75, -0.625, -0.5, -0.375, -0.25, -0.125, 0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875],
            "a staircase that climbs from the bottom, which shows at once that it goes round"
        );
        assert_eq!((length, rate, glide, retrigger), (16, 8.0, 0.0, true), "all sixteen, a sixteenth note at 120 beats a minute, no glide, started again by a note");
        assert_eq!(ModulatorKind::of(&source), ModulatorKind::Steps);
        assert_eq!(ModulatorKind::Steps.label(), "a step sequencer");
    }

    #[test]
    fn the_settings_of_a_step_sequencer_have_their_ranges_their_names_and_whether_they_are_whole() {
        use ModulatorField::{Glide, Length, Retrigger};
        assert_eq!(Glide.range(), (0.0, 1.0));
        assert_eq!(Length.range(), (1.0, 16.0));
        assert_eq!(Retrigger.range(), (0.0, 1.0));
        for (field, name) in [(Glide, "glide"), (Length, "length"), (Retrigger, "retrigger")] {
            assert_eq!(field.label(), name);
            assert_eq!(wire(&field), json!(name));
            assert_eq!(serde_json::from_value::<ModulatorField>(json!(name)).unwrap(), field);
        }
        // A waveform is a place in a list, a length a number of steps and a retrigger a switch: they are whole. The rest are not.
        for field in [ModulatorField::Waveform, Length, Retrigger] {
            assert!(field.is_whole(), "{field:?}");
        }
        for field in [ModulatorField::Rate, Glide, ModulatorField::Attack, ModulatorField::Decay, ModulatorField::Sustain, ModulatorField::Release, ModulatorField::Value] {
            assert!(!field.is_whole(), "{field:?}");
        }
    }

    #[test]
    fn the_fade_of_an_lfo_is_seconds_from_nothing_to_five_turned_on_a_linear_scale_and_is_not_whole() {
        let fade = ModulatorField::Fade;
        assert_eq!(fade.range(), (0.0, 5.0));
        assert_eq!(fade.label(), "fade");
        assert_eq!(wire(&fade), json!("fade"));
        assert_eq!(serde_json::from_value::<ModulatorField>(json!("fade")).unwrap(), fade);
        assert!(!fade.is_whole());
        assert_eq!(fade.taper(), Taper::Linear);
    }

    #[test]
    fn the_settings_that_run_over_decades_are_turned_on_a_log_scale_and_the_others_on_a_linear_one() {
        use ModulatorField::{Attack, Decay, Glide, Length, Rate, Release, Retrigger, Sustain, Value, Waveform};
        use Taper::{Linear, Log};
        for (field, taper) in [
            (Rate, Log),
            (Waveform, Linear),
            (Attack, Log),
            (Decay, Log),
            (Sustain, Linear),
            (Release, Log),
            (Value, Linear),
            (Glide, Linear),
            (Length, Linear),
            (Retrigger, Linear),
        ] {
            assert_eq!(field.taper(), taper, "{field:?}");
            // A log scale has no zero: what runs over decades has a lowest value above it.
            if taper == Log {
                assert!(field.range().0 > 0.0, "{field:?}");
            }
        }
    }

    /// Every intent, and what it looks like as JSON. The values are exact in a
    /// float, so that they read the same as f32 and as f64.
    fn intents() -> Vec<(Intent, Value)> {
        vec![
            (
                Intent::SetParam { node: NodeId(3), param: 2, value: 0.5 },
                json!({"type": "setParam", "node": 3, "param": 2, "value": 0.5}),
            ),
            (
                Intent::SetGain { node: NodeId(4), gain: Gain::GroupPan, value: -0.25 },
                json!({"type": "setGain", "node": 4, "gain": "groupPan", "value": -0.25}),
            ),
            (
                Intent::SetGain { node: NodeId::MASTER, gain: Gain::LimiterCeiling, value: -6.0 },
                json!({"type": "setGain", "node": 4_294_967_295u32, "gain": "limiterCeiling", "value": -6.0}),
            ),
            (
                Intent::SetGain { node: NodeId::MASTER, gain: Gain::MasterVolume, value: 0.5 },
                json!({"type": "setGain", "node": 4_294_967_295u32, "gain": "masterVolume", "value": 0.5}),
            ),
            (Intent::SetLimiter { on: false }, json!({"type": "setLimiter", "on": false})),
            (
                Intent::LoadPreset { node: NodeId(5), preset: "cathedral".to_string() },
                json!({"type": "loadPreset", "node": 5, "preset": "cathedral"}),
            ),
            (Intent::RemoveEdge { edge: EdgeId(6) }, json!({"type": "removeEdge", "edge": 6})),
            (Intent::AddFreeModulator { kind: ModulatorKind::Steps, hears: None }, json!({"type": "addFreeModulator", "kind": "steps", "hears": null})),
            (Intent::AddFreeModulator { kind: ModulatorKind::Envelope, hears: Some(NodeId(3)) }, json!({"type": "addFreeModulator", "kind": "envelope", "hears": 3})),
            (Intent::RemoveNode { node: NodeId(8) }, json!({"type": "removeNode", "node": 8})),
            (
                Intent::CollapseIntoModule { nodes: vec![NodeId(2), NodeId(5)], name: "Room".to_string(), ui: Some(NodeUi { x: 40.0, y: 120.5 }) },
                json!({"type": "collapseIntoModule", "nodes": [2, 5], "name": "Room", "ui": {"x": 40.0, "y": 120.5}}),
            ),
            (
                Intent::CollapseIntoModule { nodes: vec![NodeId(7)], name: String::new(), ui: None },
                json!({"type": "collapseIntoModule", "nodes": [7], "name": "", "ui": null}),
            ),
            (Intent::UngroupModule { node: NodeId(9) }, json!({"type": "ungroupModule", "node": 9})),
            (Intent::AddSource { source: SourceInfo::Keys }, json!({"type": "addSource", "source": {"type": "keys"}})),
            (
                Intent::AddSource { source: SourceInfo::Device { name: "Seaboard".to_string() } },
                json!({"type": "addSource", "source": {"type": "device", "name": "Seaboard"}}),
            ),
            (Intent::MoveEffect { node: NodeId(7), index: 0 }, json!({"type": "moveEffect", "node": 7, "index": 0})),
            (
                Intent::AddEffect { parent: NodeId(2), plugin: "builtin:filter".to_string() },
                json!({"type": "addEffect", "parent": 2, "plugin": "builtin:filter"}),
            ),
            (
                Intent::AddFreeEffect { plugin: "builtin:reverb".to_string() },
                json!({"type": "addFreeEffect", "plugin": "builtin:reverb"}),
            ),
            (
                Intent::AddInstrument { plugin: "builtin:osc".to_string() },
                json!({"type": "addInstrument", "plugin": "builtin:osc"}),
            ),
            (
                Intent::ReplaceInstrument { node: NodeId(2), plugin: "builtin:osc".to_string() },
                json!({"type": "replaceInstrument", "node": 2, "plugin": "builtin:osc"}),
            ),
            (Intent::ClearInstrument { node: NodeId(2) }, json!({"type": "clearInstrument", "node": 2})),
            (Intent::Save, json!({"type": "save"})),
            (Intent::SaveAs { name: "copy".to_string() }, json!({"type": "saveAs", "name": "copy"})),
            (Intent::SaveModule { node: NodeId(4), name: "Space".to_string() }, json!({"type": "saveModule", "node": 4, "name": "Space"})),
            (Intent::OpenEditor { node: NodeId(3) }, json!({"type": "openEditor", "node": 3})),
            (
                Intent::SaveSelectionAsModule { nodes: vec![NodeId(4), NodeId(7)], name: "Space".to_string() },
                json!({"type": "saveSelectionAsModule", "nodes": [4, 7], "name": "Space"}),
            ),
            (Intent::Undo, json!({"type": "undo"})),
            (Intent::Redo, json!({"type": "redo"})),
            (
                Intent::SetRange { node: NodeId(2), range: Some(NoteRange { low: 36, high: 84 }) },
                json!({"type": "setRange", "node": 2, "range": {"low": 36, "high": 84}}),
            ),
            (Intent::SetRange { node: NodeId(2), range: None }, json!({"type": "setRange", "node": 2, "range": null})),
            (Intent::SetMute { node: NodeId(2), muted: true }, json!({"type": "setMute", "node": 2, "muted": true})),
            (Intent::SetSolo { node: NodeId(4), solo: false }, json!({"type": "setSolo", "node": 4, "solo": false})),
            (Intent::SetRoute { node: NodeId(4), route: Route::None }, json!({"type": "setRoute", "node": 4, "route": "none"})),
            (Intent::SetModulatorEar { node: NodeId(7), ear: Some(NodeId(2)) }, json!({"type": "setModulatorEar", "node": 7, "ear": 2})),
            (Intent::SetModulatorEar { node: NodeId(7), ear: None }, json!({"type": "setModulatorEar", "node": 7, "ear": null})),
            (Intent::SetRoute { node: NodeId(2), route: Route::Out }, json!({"type": "setRoute", "node": 2, "route": "out"})),
            (Intent::SetFeed { node: NodeId(7), feed: Feed::None }, json!({"type": "setFeed", "node": 7, "feed": "none"})),
            (Intent::SetFeed { node: NodeId(3), feed: Feed::Chain }, json!({"type": "setFeed", "node": 3, "feed": "chain"})),
            (Intent::SetScale { scale: "F# dorian".to_string() }, json!({"type": "setScale", "scale": "F# dorian"})),
            (Intent::SetLocked { locked: true }, json!({"type": "setLocked", "locked": true})),
            (Intent::SetBpm { bpm: 96.5 }, json!({"type": "setBpm", "bpm": 96.5})),
            (Intent::SetRecording { node: NodeId(2), recording: true }, json!({"type": "setRecording", "node": 2, "recording": true})),
            (Intent::ClearPattern { node: NodeId(2) }, json!({"type": "clearPattern", "node": 2})),
            (
                Intent::SetPatternField { node: NodeId(2), field: PatternField::Transpose, value: 1.0 },
                json!({"type": "setPatternField", "node": 2, "field": "transpose", "value": 1.0}),
            ),
            (
                Intent::SetNodeUi { node: NodeId(2), ui: Some(NodeUi { x: 80.0, y: -12.5 }) },
                json!({"type": "setNodeUi", "node": 2, "ui": {"x": 80.0, "y": -12.5}}),
            ),
            (Intent::SetNodeUi { node: NodeId(2), ui: None }, json!({"type": "setNodeUi", "node": 2, "ui": null})),
            (Intent::SetPinned { node: NodeId(2), params: vec![3, 1] }, json!({"type": "setPinned", "node": 2, "params": [3, 1]})),
            (Intent::SetPinned { node: NodeId(2), params: vec![] }, json!({"type": "setPinned", "node": 2, "params": []})),
            (
                Intent::SetSteps { node: NodeId(9), values: vec![-1.0, 0.5, 0.25, 0.0, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.75] },
                json!({"type": "setSteps", "node": 9, "values": [-1.0, 0.5, 0.25, 0.0, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.75]}),
            ),
            (Intent::AddGroup { members: vec![NodeId(2), NodeId(5)] }, json!({"type": "addGroup", "members": [2, 5]})),
            (Intent::AddGroup { members: vec![] }, json!({"type": "addGroup", "members": []})),
            (
                Intent::SetLaneGroup { node: NodeId(2), group: Some(NodeId(7)) },
                json!({"type": "setLaneGroup", "node": 2, "group": 7}),
            ),
            (Intent::SetLaneGroup { node: NodeId(2), group: None }, json!({"type": "setLaneGroup", "node": 2, "group": null})),
            (
                Intent::RenameGroup { node: NodeId(7), name: "Pad".to_string() },
                json!({"type": "renameGroup", "node": 7, "name": "Pad"}),
            ),
            (
                Intent::RenameMacro { node: NodeId(7), name: "Brightness".to_string() },
                json!({"type": "renameMacro", "node": 7, "name": "Brightness"}),
            ),
            (Intent::ExposeMacro { node: NodeId(7), expose: false }, json!({"type": "exposeMacro", "node": 7, "expose": false})),
            (Intent::ExposeMacro { node: NodeId(7), expose: true }, json!({"type": "exposeMacro", "node": 7, "expose": true})),
            (
                Intent::AddModEdge { modulator: NodeId(4), to: Endpoint { node: NodeId(1), port: Port::Param { index: 3 } } },
                json!({"type": "addModEdge", "modulator": 4, "to": {"node": 1, "port": {"type": "param", "index": 3}}}),
            ),
            (
                Intent::AddCable {
                    from: Endpoint { node: NodeId(2), port: Port::AudioOut },
                    to: Endpoint { node: NodeId(5), port: Port::AudioIn },
                    gain: Some(0.5),
                },
                json!({"type": "addCable", "from": {"node": 2, "port": {"type": "audioOut"}}, "to": {"node": 5, "port": {"type": "audioIn"}}, "gain": 0.5}),
            ),
            (
                Intent::AddCable { from: Endpoint { node: NodeId(2), port: Port::AudioOut }, to: Endpoint { node: NodeId::MASTER, port: Port::AudioIn }, gain: None },
                json!({"type": "addCable", "from": {"node": 2, "port": {"type": "audioOut"}}, "to": {"node": NodeId::MASTER.0, "port": {"type": "audioIn"}}, "gain": null}),
            ),
            (Intent::SetCableGain { edge: EdgeId(9), gain: 1.5 }, json!({"type": "setCableGain", "edge": 9, "gain": 1.5})),
        ]
    }

    fn events() -> Vec<(Event, Value)> {
        vec![
            (
                Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 },
                json!({"type": "paramSet", "node": 3, "param": 2, "value": 0.5}),
            ),
            (
                Event::GainSet { node: NodeId(4), gain: Gain::Volume, value: 2.0 },
                json!({"type": "gainSet", "node": 4, "gain": "volume", "value": 2.0}),
            ),
            (
                Event::PresetLoaded {
                    node: NodeId(5),
                    preset: "Cathedral".to_string(),
                    values: vec![ParamValue { param: 0, value: 0.5 }, ParamValue { param: 3, value: 0.25 }],
                },
                json!({
                    "type": "presetLoaded",
                    "node": 5,
                    "preset": "Cathedral",
                    "values": [{"param": 0, "value": 0.5}, {"param": 3, "value": 0.25}],
                }),
            ),
            (
                Event::RangeSet { node: NodeId(2), range: Some(NoteRange { low: 36, high: 84 }) },
                json!({"type": "rangeSet", "node": 2, "range": {"low": 36, "high": 84}}),
            ),
            (Event::RangeSet { node: NodeId(2), range: None }, json!({"type": "rangeSet", "node": 2, "range": null})),
            (Event::MuteSet { node: NodeId(2), muted: true }, json!({"type": "muteSet", "node": 2, "muted": true})),
            (Event::SoloSet { node: NodeId(4), solo: false }, json!({"type": "soloSet", "node": 4, "solo": false})),
            (
                Event::ScaleSet { scale: "F# dorian".to_string(), root: 6, kind: "dorian".to_string(), notes: vec![1, 3, 4, 6, 8, 9, 11] },
                json!({"type": "scaleSet", "scale": "F# dorian", "root": 6, "kind": "dorian", "notes": [1, 3, 4, 6, 8, 9, 11]}),
            ),
            (Event::LockedSet { locked: false }, json!({"type": "lockedSet", "locked": false})),
            (Event::BpmSet { bpm: 96.5 }, json!({"type": "bpmSet", "bpm": 96.5})),
            (Event::LimiterSet { on: true }, json!({"type": "limiterSet", "on": true})),
            (
                Event::PatternSet {
                    node: NodeId(2),
                    pattern: Some(PatternInfo { recording: false, enabled: true, looping: true, in_key: false, length_beats: 4.0, bpm: 120.0, notes: 3 }),
                },
                json!({"type": "patternSet", "node": 2, "pattern": {"recording": false, "enabled": true, "looping": true, "inKey": false, "lengthBeats": 4.0, "bpm": 120.0, "notes": 3}}),
            ),
            (Event::PatternSet { node: NodeId(2), pattern: None }, json!({"type": "patternSet", "node": 2, "pattern": null})),
            (
                Event::NodeUiSet { node: NodeId(2), ui: Some(NodeUi { x: 80.0, y: -12.5 }) },
                json!({"type": "nodeUiSet", "node": 2, "ui": {"x": 80.0, "y": -12.5}}),
            ),
            (Event::NodeUiSet { node: NodeId(2), ui: None }, json!({"type": "nodeUiSet", "node": 2, "ui": null})),
            (Event::PinnedSet { node: NodeId(2), params: vec![3, 1] }, json!({"type": "pinnedSet", "node": 2, "params": [3, 1]})),
            (Event::PinnedSet { node: NodeId(2), params: vec![] }, json!({"type": "pinnedSet", "node": 2, "params": []})),
            (
                Event::StepsSet { node: NodeId(9), values: vec![-1.0, 0.5, 0.25, 0.0, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.75] },
                json!({"type": "stepsSet", "node": 9, "values": [-1.0, 0.5, 0.25, 0.0, 1.0, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.75]}),
            ),
            (
                Event::GroupRenamed { node: NodeId(7), name: "Pad".to_string() },
                json!({"type": "groupRenamed", "node": 7, "name": "Pad"}),
            ),
            (Event::CableGainSet { edge: EdgeId(9), gain: 1.5 }, json!({"type": "cableGainSet", "edge": 9, "gain": 1.5})),
            (Event::Saved { file: Some("song.toml".to_string()) }, json!({"type": "saved", "file": "song.toml"})),
            (Event::Saved { file: None }, json!({"type": "saved", "file": null})),
            (Event::ModuleSaved { name: "Space".to_string() }, json!({"type": "moduleSaved", "name": "Space"})),
            (Event::EditorOpened { node: NodeId(3) }, json!({"type": "editorOpened", "node": 3})),
        ]
    }

    #[test]
    fn an_intent_is_an_object_tagged_with_its_camel_case_name() {
        for (intent, json) in intents() {
            assert_eq!(wire(&intent), json, "{intent:?}");
        }
    }

    #[test]
    fn an_intent_comes_back_from_json_as_it_went() {
        for (intent, json) in intents() {
            assert_eq!(serde_json::from_value::<Intent>(json.clone()).unwrap(), intent, "{json}");
        }
    }

    #[test]
    fn every_gain_has_its_own_name() {
        let names: Vec<Value> = [Gain::Volume, Gain::Mix, Gain::GroupVolume, Gain::GroupPan].iter().map(wire).collect();
        assert_eq!(names, [json!("volume"), json!("mix"), json!("groupVolume"), json!("groupPan")]);
        for name in names {
            assert!(serde_json::from_value::<Gain>(name).is_ok());
        }
    }

    #[test]
    fn every_kind_of_modulator_has_its_own_name() {
        let names: Vec<Value> =
            [ModulatorKind::Lfo, ModulatorKind::Envelope, ModulatorKind::MidiLearn, ModulatorKind::Macro, ModulatorKind::Velocity, ModulatorKind::Key, ModulatorKind::Steps]
                .iter()
                .map(wire)
                .collect();
        assert_eq!(names, [json!("lfo"), json!("envelope"), json!("midiLearn"), json!("macro"), json!("velocity"), json!("key"), json!("steps")]);
        for name in names {
            assert!(serde_json::from_value::<ModulatorKind>(name).is_ok());
        }
    }

    #[test]
    fn what_a_client_sends_that_is_not_an_intent_is_an_error() {
        let bad = [
            ("no type", json!({"node": 3, "param": 2, "value": 0.5})),
            ("an unknown type", json!({"type": "explode", "node": 3})),
            ("a Rust name instead of the wire's", json!({"type": "SetParam", "node": 3, "param": 2, "value": 0.5})),
            ("a missing field", json!({"type": "setParam", "node": 3, "param": 2})),
            ("a value that is not a number (what NaN becomes)", json!({"type": "setParam", "node": 3, "param": 2, "value": null})),
            ("a negative node", json!({"type": "setParam", "node": -1, "param": 2, "value": 0.5})),
            ("a gain that does not exist", json!({"type": "setGain", "node": 3, "gain": "loud", "value": 1.0})),
            ("a node that is a string", json!({"type": "loadPreset", "node": "3", "preset": "x"})),
            ("a kind of modulator that does not exist", json!({"type": "addModulator", "parent": 3, "kind": "noise"})),
            ("a kind spelled the Rust way", json!({"type": "addModulator", "parent": 3, "kind": "MidiLearn"})),
            ("no kind", json!({"type": "addModulator", "parent": 3})),
            ("a scale that is a number", json!({"type": "setScale", "scale": 3})),
            ("no scale", json!({"type": "setScale"})),
            ("a lock that is not a boolean", json!({"type": "setLocked", "locked": "yes"})),
            ("no lock", json!({"type": "setLocked"})),
            ("a place with no y", json!({"type": "setNodeUi", "node": 3, "ui": {"x": 1.0}})),
            ("a place that is a number", json!({"type": "setNodeUi", "node": 3, "ui": 5})),
            ("a place whose x is a string", json!({"type": "setNodeUi", "node": 3, "ui": {"x": "1", "y": 2.0}})),
            ("a place whose x is null (what NaN becomes)", json!({"type": "setNodeUi", "node": 3, "ui": {"x": null, "y": 2.0}})),
            ("steps with no list", json!({"type": "setSteps", "node": 3})),
            ("steps that are strings", json!({"type": "setSteps", "node": 3, "values": ["0.5"]})),
            ("a step that is null (what NaN becomes)", json!({"type": "setSteps", "node": 3, "values": [null]})),
            ("pinned parameters with no list", json!({"type": "setPinned", "node": 3})),
            ("pinned parameters that are names", json!({"type": "setPinned", "node": 3, "params": ["cutoff"]})),
            ("a pinned parameter that is negative", json!({"type": "setPinned", "node": 3, "params": [-1]})),
            ("a pinned parameter that is not whole", json!({"type": "setPinned", "node": 3, "params": [1.5]})),
            ("a list of pinned parameters that is a number", json!({"type": "setPinned", "node": 3, "params": 4})),
            ("a group with no members", json!({"type": "addGroup"})),
            ("members that are not nodes", json!({"type": "addGroup", "members": ["a"]})),
            ("a lane's group that is a string", json!({"type": "setLaneGroup", "node": 3, "group": "7"})),
            ("a name that is a number", json!({"type": "renameGroup", "node": 3, "name": 5})),
            ("no name", json!({"type": "renameGroup", "node": 3})),
        ];
        for (what, json) in bad {
            assert!(serde_json::from_value::<Intent>(json).is_err(), "{what}");
        }
    }

    #[test]
    fn an_event_is_an_object_tagged_with_its_camel_case_name() {
        for (event, json) in events() {
            assert_eq!(wire(&event), json, "{event:?}");
            assert_eq!(serde_json::from_value::<Event>(json).unwrap(), event);
        }
    }

    #[test]
    fn the_events_of_an_applied_intent_go_out_stamped_with_its_revision() {
        let applied = Applied::of(
            9,
            vec![
                Event::ParamSet { node: NodeId(3), param: 2, value: 0.5 },
                Event::GainSet { node: NodeId(4), gain: Gain::Mix, value: 1.0 },
            ],
        );
        let stamped = applied.stamped();
        assert_eq!(
            wire(&stamped),
            json!([
                {"rev": 9, "type": "paramSet", "node": 3, "param": 2, "value": 0.5},
                {"rev": 9, "type": "gainSet", "node": 4, "gain": "mix", "value": 1.0},
            ])
        );
        let back: Vec<StampedEvent> = serde_json::from_value(wire(&stamped)).unwrap();
        assert_eq!(back, stamped);
        assert_eq!(back[1], StampedEvent { rev: 9, event: applied.events[1].clone() });
    }

    #[test]
    fn an_applied_intent_without_events_goes_out_as_nothing() {
        assert_eq!(wire(&Applied::of(1, vec![]).stamped()), json!([]));
        // (A change of structure is told as a snapshot, not as events.)
        assert_eq!(wire(&Applied::restructured(2).stamped()), json!([]));
    }
}
