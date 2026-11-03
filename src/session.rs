use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plugin::Plugin;
use crate::plugin::chain::MAX_STEPS;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RemapTarget {
    pub note: String,
    pub detune: f64,
}

/// The way out of a lane or a group (P3c-2 of `docs/patch-graph.md` §11.6): where what it makes goes when it has been through its chain. `Out` is the way a lane and a group are
/// wired by themselves: a lane's to the bus of its group or, with no group, to the mix, and a group's to the mix. `None` cuts it, so that a lane is heard only through the audio
/// cables that start at it (`[[cable]]`: its instrument's, or those of its effects), and a group only through its own. In the file it is `route = "none"` on the table of the lane or
/// the group, and `out`, which is what it is without, is not written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Route {
    #[default]
    Out,
    None,
}

impl Route {
    /// The word that the file and the page use for it.
    pub fn name(self) -> &'static str {
        match self {
            Route::Out => "out",
            Route::None => "none",
        }
    }

    /// The route that a word names, exactly as `name` writes it.
    pub fn parse(text: &str) -> Option<Route> {
        [Route::Out, Route::None].into_iter().find(|route| route.name() == text)
    }
}

/// The feed of an effect (step 86, `docs/web-ui.md`): whether the link into it from the stage before it in its chain (the lane's instrument, a bus's sum, or the effect before it) is there.
/// `Chain` is the way a chain is wired by itself. `None` cuts the link, so that the effect is fed by the audio cables that end at it (`[[cable]]`) and by nothing else: the chain
/// is a template and its links can be taken apart. In the file it is `feed = "none"` on the table of the effect, and `chain`, which is what it is without, is not written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Feed {
    #[default]
    Chain,
    None,
}

impl Feed {
    /// The word that the file and the page use for it.
    pub fn name(self) -> &'static str {
        match self {
            Feed::Chain => "chain",
            Feed::None => "none",
        }
    }

    /// The feed that a word names, exactly as `name` writes it.
    pub fn parse(text: &str) -> Option<Feed> {
        [Feed::Chain, Feed::None].into_iter().find(|feed| feed.name() == text)
    }

    /// Whether the link is there (`Chain`).
    pub fn is_fed(self) -> bool {
        self == Feed::Chain
    }
}

/// Where a front-end draws a node, in the pixels of its canvas. The engine and the TUI do not use it, and it is kept when
/// a session is saved: `ui = { x = 80, y = 120 }` on the node's table, the master's in a top-level `[ui]` table.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
pub struct NodeUi {
    pub x: f32,
    pub y: f32,
}

impl NodeUi {
    /// How far from the origin a node may be drawn, either way: a canvas has to be one a person can come back from.
    pub const LIMIT: f64 = 100_000.0;
    /// A place is kept to this many parts of a pixel (a tenth): it is read by people, in the file, and a drag is not
    /// that exact.
    pub const PARTS: f64 = 10.0;

    /// The place as it is kept: to a tenth of a pixel (half away from zero), no further than `LIMIT`, and never `-0`.
    /// `None` if a number of it is not one (NaN, infinite). Done in 64 bits, as a client's page does it, so that what
    /// the page says it is is what it is.
    pub fn tidy(self) -> Option<NodeUi> {
        if !self.x.is_finite() || !self.y.is_finite() {
            return None;
        }
        let tidy = |v: f32| ((f64::from(v).clamp(-Self::LIMIT, Self::LIMIT) * Self::PARTS).round() / Self::PARTS + 0.0) as f32;
        Some(NodeUi { x: tidy(self.x), y: tidy(self.y) })
    }
}

/// A node's `ui`, read leniently: a layout is not worth refusing a session for, so one that is not a pair of numbers
/// is left out (and logged) and the rest of the session loads.
fn lenient_ui<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<NodeUi>, D::Error> {
    let value = toml::Value::deserialize(deserializer)?;
    Ok(node_ui_of(value))
}

fn node_ui_of(value: toml::Value) -> Option<NodeUi> {
    match value.try_into::<NodeUi>() {
        Ok(ui) => Some(ui),
        Err(error) => {
            log::warn!("ignoring a position that is not {{ x, y }} numbers: {error}");
            None
        }
    }
}

/// A `pinned` list, read leniently like a place: which parameters a front-end shows as knobs is not worth refusing a session for,
/// so what is not a list is left out (and logged), and so is whatever in a list is not a name.
fn lenient_names<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    Ok(match toml::Value::deserialize(deserializer)? {
        toml::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                toml::Value::String(name) => Some(name),
                other => {
                    log::warn!("ignoring a pinned parameter that is not a name: {other}");
                    None
                }
            })
            .collect(),
        other => {
            log::warn!("ignoring a `pinned` that is not a list of names: {other}");
            Vec::new()
        }
    })
}

/// The `members` of a `[[module]]`, read leniently like a `pinned` list: what is not a list is left out (and logged), and so is whatever in a list is not a text.
fn lenient_members<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    Ok(match toml::Value::deserialize(deserializer)? {
        toml::Value::Array(items) => items
            .into_iter()
            .filter_map(|item| match item {
                toml::Value::String(place) => Some(place),
                other => {
                    log::warn!("ignoring a member of a module that is not a place: {other}");
                    None
                }
            })
            .collect(),
        other => {
            log::warn!("ignoring the `members` of a module that is not a list of places: {other}");
            Vec::new()
        }
    })
}

/// The top-level `[ui]` table, read leniently like a node's.
fn lenient_table<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<toml::Table>, D::Error> {
    Ok(match toml::Value::deserialize(deserializer)? {
        toml::Value::Table(table) => Some(table),
        _ => {
            log::warn!("ignoring a [ui] that is not a table");
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Instrument-centric config
// ---------------------------------------------------------------------------

/// Top-level session config: a flat list of instruments, optional submix
/// groups, and an optional Piano-tab tonality (root + scale).
pub struct SessionConfig {
    pub instruments: Vec<InstrumentSlotConfig>,
    pub groups: Vec<GroupConfig>,
    pub piano: Option<PianoConfig>,
    /// Where the master mix is drawn (`[ui] master = { x, y }`).
    pub master_ui: Option<NodeUi>,
    /// The tempo the session is at, in beats a minute (the top-level `bpm`); none for a file that has not one, which
    /// the loader gives the tempo of its first recorded pattern.
    pub bpm: Option<f32>,
    /// What the file says of the output mix (the `[master]` table): none for a file that has no such table, which is the master as it is without one (unity volume, the limiter in the sound at
    /// `DEFAULT_CEILING`).
    pub master: Option<MasterConfig>,
    /// The cables (`[[cable]]`), in the order of the file: audio cables, which are sends from one place of the audio to another besides the way that a lane and a group are wired by
    /// themselves, and event cables, which carry the notes of a source to a lane.
    pub cables: Vec<CableConfig>,
    /// The computer's keys as a source of notes that the session has a node for (`[keys]`; a cable that starts at `keys` makes it if the file has no such table).
    pub keys: Option<KeysConfig>,
    /// The MIDI devices that the session has a node for (`[[device]]`), in the order of the file, which is how a cable names them (`device[0]`).
    pub devices: Vec<DeviceConfig>,
    /// The modulators that belong to no lane and no group (the top-level `[[modulator]]`), in the order of the file, which is how the cables of one of them name another (`mod_rate = 1`).
    /// Each hears every note unless it says whose (`hears`), and its cables name the plugin they drive by its place in the session (`to = "instrument[0].effect[1]"`).
    /// `move_modulators_to_the_session` puts the modulators of the lanes and of the groups here too.
    pub free_modulators: Vec<ModulatorConfig>,
    /// The modules that were added to the session as one thing (`[[module]]`, step 108), in the order of the file: a front-end draws each as one box. What a member names that is not there is left out by the loader,
    /// and a module that has no member left is not made (a layout is not worth refusing a session for).
    pub modules: Vec<ModuleConfig>,
}

/// A `[[module]]` table (step 108): the name the module has in the library, the lanes and the modulators that came with it, and where its box is drawn.
pub struct ModuleConfig {
    pub name: String,
    pub members: Vec<MemberPlace>,
    /// Where the box is drawn.
    pub ui: Option<NodeUi>,
}

/// The `[master]` table (step 102): the output mix's volume, whether the limiter is in the sound and where its ceiling is. A key that is not there is what the master is without it: a volume of 1,
/// the limiter on (the user's "limiter default on": a session that was written before the limiter has one), and a ceiling of -1 dB. The loader holds the numbers to their ranges.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MasterConfig {
    /// The gain on the mix, 0 to 2 (`volume`).
    pub volume: Option<f32>,
    /// Whether the limiter is in the sound (`limiter`).
    pub limiter: Option<bool>,
    /// Where the limiter holds the sound under, in decibels below full scale, -24 to 0 (`ceiling`).
    pub ceiling: Option<f32>,
}

/// The keys as a source of notes: the computer's keys, the page's piano and keys and every browser's (`[keys]`).
pub struct KeysConfig {
    /// Where the node is drawn.
    pub ui: Option<NodeUi>,
}

/// A MIDI device as a source of notes (`[[device]]`): the name of its port as the system gives it, or as much of it as tells it ("Launchkey" is every port of a Launchkey).
pub struct DeviceConfig {
    pub name: String,
    /// Where the node is drawn.
    pub ui: Option<NodeUi>,
}

/// The most characters a device's name can have. The ports that ALSA names are "client:port client:port" and run long (`Arturia KeyLab mkII 61:Arturia KeyLab mkII 61 Arturia KeyLab mkII 61 MIDI 1 28:0`
/// is 78), so that the 64 of a group's name would turn a real device away.
pub const MAX_DEVICE_NAME: usize = 256;

/// The name that a device is called by, from the text that was given (`[[device]]` in a file, `AddSource` as an intent: one rule, so that a name the intent takes is a name that the file it is saved in
/// loads): the text without the spaces round it, which has to be something, at most `MAX_DEVICE_NAME` characters and with no control character in it. The case is the name's: two names that differ
/// by it are two devices. What is wrong is said as the end of a sentence about the name (`is blank: …`).
pub fn device_name(text: &str) -> Result<String, String> {
    let name = text.trim();
    if name.is_empty() {
        return Err("is blank: a device is called by its name, or as much of it as tells it".to_string());
    }
    if name.chars().count() > MAX_DEVICE_NAME {
        return Err(format!("is longer than {MAX_DEVICE_NAME} characters"));
    }
    if name.chars().any(char::is_control) {
        return Err("has a control character in it".to_string());
    }
    Ok(name.to_string())
}

/// A submix group: members are the instruments whose `group` index points here.
pub struct GroupConfig {
    pub name: Option<String>,
    pub volume: f32,
    pub pan: f32,
    /// Muted (`mute = true`): the bus is never heard.
    pub muted: bool,
    /// Soloed (`solo = true`): while anything is soloed, only what is soloed is heard.
    pub solo: bool,
    /// Where the bus goes (`route = "none"`: nowhere but its own cables).
    pub route: Route,
    pub effects: Vec<EffectConfig>,
    /// Group-scoped modulators. Their targets address member instruments
    /// (`member` + optional `slot`), bus effects (`bus`), or sibling
    /// group modulators (`mod_*`).
    pub modulators: Vec<ModulatorConfig>,
    /// Where the group is drawn.
    pub ui: Option<NodeUi>,
}

/// A place in a session that a cable starts or ends at, as the file names it (P3c and P4b of `docs/patch-graph.md` §11.6): a lane by its place among the `[[instrument]]`
/// tables, an effect of a lane or of a group's bus by its place in the chain, a group by its place among the `[[group]]` tables, the mix, or, for a cable of notes, the keys or a device
/// by its place among the `[[device]]` tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CablePlace {
    /// `instrument[0]`: a lane. A cable starts at what its instrument puts out (after the lane's volume, before its chain).
    Instrument(usize),
    /// `instrument[0].effect[1]`: an effect of a lane's chain.
    Effect { instrument: usize, effect: usize },
    /// `instrument[0].effect[1].side`: the second input of an effect of a lane's chain that has one (`Plugin::side_input`), where a cable ends.
    EffectSide { instrument: usize, effect: usize },
    /// `group[0]`: a group. A cable starts at what it puts out (after its effects, its pan and its fade), and ends at its bus.
    Group(usize),
    /// `group[0].effect[1]`: an effect of a group's bus.
    BusEffect { group: usize, effect: usize },
    /// `group[0].effect[1].side`: the second input of an effect of a group's bus that has one.
    BusEffectSide { group: usize, effect: usize },
    /// `master`: the output mix, where a cable ends.
    Master,
    /// `keys`: the computer's keys as a source of notes, where a cable of notes starts.
    Keys,
    /// `device[0]`: a MIDI device, the first of the `[[device]]` tables: where a cable of notes starts.
    Device(usize),
}

impl CablePlace {
    /// Whether it is a source of notes, which only a cable of notes starts at.
    pub fn is_source(self) -> bool {
        matches!(self, CablePlace::Keys | CablePlace::Device(_))
    }

    /// The place that `text` names, if it names one: `master`, `keys`, `device[0]`, `instrument[0]`, `instrument[0].effect[1]`, `instrument[0].effect[1].side`, `group[0]`, `group[0].effect[1]` or
    /// `group[0].effect[1].side`, the numbers whole and from 0.
    pub fn parse(text: &str) -> Option<CablePlace> {
        fn indexed(text: &str) -> Option<(&str, usize)> {
            let (word, rest) = text.split_once('[')?;
            let digits = rest.strip_suffix(']')?;
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            Some((word, digits.parse().ok()?))
        }
        let text = text.trim();
        if text == "master" {
            return Some(CablePlace::Master);
        }
        if text == "keys" {
            return Some(CablePlace::Keys);
        }
        let mut parts = text.split('.');
        let head = parts.next()?;
        let effect = match parts.next() {
            Some(rest) => match indexed(rest)? {
                ("effect", index) => Some(index),
                _ => return None,
            },
            None => None,
        };
        let side = match parts.next() {
            Some("side") if effect.is_some() => true,
            Some(_) => return None,
            None => false,
        };
        if parts.next().is_some() {
            return None;
        }
        match (indexed(head)?, effect, side) {
            (("instrument", instrument), None, _) => Some(CablePlace::Instrument(instrument)),
            (("instrument", instrument), Some(effect), false) => Some(CablePlace::Effect { instrument, effect }),
            (("instrument", instrument), Some(effect), true) => Some(CablePlace::EffectSide { instrument, effect }),
            (("group", group), None, _) => Some(CablePlace::Group(group)),
            (("group", group), Some(effect), false) => Some(CablePlace::BusEffect { group, effect }),
            (("group", group), Some(effect), true) => Some(CablePlace::BusEffectSide { group, effect }),
            (("device", device), None, _) => Some(CablePlace::Device(device)),
            _ => None,
        }
    }
}

impl std::fmt::Display for CablePlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CablePlace::Instrument(instrument) => write!(f, "instrument[{instrument}]"),
            CablePlace::Effect { instrument, effect } => write!(f, "instrument[{instrument}].effect[{effect}]"),
            CablePlace::EffectSide { instrument, effect } => write!(f, "instrument[{instrument}].effect[{effect}].side"),
            CablePlace::Group(group) => write!(f, "group[{group}]"),
            CablePlace::BusEffect { group, effect } => write!(f, "group[{group}].effect[{effect}]"),
            CablePlace::BusEffectSide { group, effect } => write!(f, "group[{group}].effect[{effect}].side"),
            CablePlace::Master => f.write_str("master"),
            CablePlace::Keys => f.write_str("keys"),
            CablePlace::Device(device) => write!(f, "device[{device}]"),
        }
    }
}

/// A node that came with a module that was added as one thing (`[[module]]`, step 108): a lane, by its place among the `[[instrument]]` tables (`instrument[2]`; its effects are in it), or a modulator, by its place
/// among the `[[modulator]]` tables (`modulator[0]`), the numbers whole and from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberPlace {
    Lane(usize),
    Modulator(usize),
}

impl MemberPlace {
    /// The place that `text` names, if it names one: `instrument[0]` or `modulator[0]`. A text with a sign, a space or a word that is none of the two names none.
    pub fn parse(text: &str) -> Option<MemberPlace> {
        let (word, rest) = text.trim().split_once('[')?;
        let digits = rest.strip_suffix(']')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let place = digits.parse().ok()?;
        match word {
            "instrument" => Some(MemberPlace::Lane(place)),
            "modulator" => Some(MemberPlace::Modulator(place)),
            _ => None,
        }
    }
}

impl std::fmt::Display for MemberPlace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemberPlace::Lane(lane) => write!(f, "instrument[{lane}]"),
            MemberPlace::Modulator(modulator) => write!(f, "modulator[{modulator}]"),
        }
    }
}

/// What a module that was added as one thing is called when the file has no name for it, or a blank one.
pub const DEFAULT_MODULE_NAME: &str = "Module";

/// The name that a module is called by, from the text a file has: the text without its control characters and the spaces round it, and `DEFAULT_MODULE_NAME` for one that is nothing then. (A name
/// is not cut short: the library's names run to a file name's length, and a module that was added under one must be saved and loaded as it is.)
pub fn module_name(text: &str) -> String {
    let name: String = text.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim();
    if name.is_empty() { DEFAULT_MODULE_NAME.to_string() } else { name.to_string() }
}

/// A cable of a session file (`[[cable]]`): where it starts, where it ends and its level, if the file says one. It is a cable of notes when it starts at a source (`keys`, `device[0]`), which has
/// no level (a file that gives it one is refused), and an audio cable otherwise, whose level is 1 if the file says none. Whether the places are in the
/// session, are what a cable can start or end at, and leave the audio going one way is for the loader to say: the file has only names.
pub struct CableConfig {
    pub from: CablePlace,
    pub to: CablePlace,
    pub gain: Option<f32>,
}

/// Optional `[piano]` section of the session TOML.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct PianoConfig {
    /// Scale spec like "C major" or "F# dorian".
    #[serde(default)]
    pub scale: Option<String>,
    /// If true, off-scale notes are dropped at the sender (Locked mode).
    #[serde(default)]
    pub locked: bool,
}

/// One instrument slot: an optional plugin with range, effects, and pattern.
pub struct InstrumentSlotConfig {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    /// Muted (`mute = true`): the lane is never heard; its notes, modulators and pattern go on.
    pub muted: bool,
    /// Soloed (`solo = true`): while anything is soloed, only what is soloed is heard.
    pub solo: bool,
    /// Where the lane goes (`route = "none"`: nowhere but its own cables).
    pub route: Route,
    pub instrument: Option<PluginConfig>,
    pub effects: Vec<EffectConfig>,
    /// The lane's modulators (their targets carry the chain slot they land on). They belong to the lane, not
    /// to its instrument plugin: a lane whose plugin was cleared has its effects and these still.
    pub modulators: Vec<ModulatorConfig>,
    pub pattern: Option<PatternConfig>,
    /// Submix group membership: index into the session's groups, or None.
    pub group: Option<usize>,
    /// Where the lane is drawn.
    pub ui: Option<NodeUi>,
}

/// Parsed pattern config for an instrument slot.
pub struct PatternConfig {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    /// Transpose playback by scale degrees of the piano scale instead of
    /// by semitones.
    pub in_key: bool,
}

#[derive(Deserialize)]
pub struct ModulatorConfig {
    #[serde(default = "default_mod_type", rename = "type")]
    pub mod_type: String,
    #[serde(default = "default_waveform")]
    pub waveform: String,
    /// An LFO's rate in Hz (1 if the file has none), a step sequencer's in steps a second (8): the loader says which.
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default = "default_attack")]
    pub attack: f64,
    #[serde(default = "default_decay")]
    pub decay: f64,
    #[serde(default = "default_sustain")]
    pub sustain: f64,
    #[serde(default = "default_release")]
    pub release: f64,
    /// For `type = "macro"`: the value the knob is at, 0 to 1 (held to it when the session is loaded). A macro that has none is at rest.
    #[serde(default)]
    pub value: f64,
    /// For `type = "steps"`: the values of the steps from the first, each -1 to 1 (held to it when the session is loaded), 1 to 16 of them; the steps that
    /// are not given are 0. None (no list at all) is the staircase that a new one starts as.
    #[serde(default)]
    pub steps: Option<Vec<f64>>,
    /// For `type = "steps"`: how many of the steps are in use, held to 1 to 16: as many as `steps` has values if it is not said (all sixteen if there are no values).
    #[serde(default)]
    pub length: Option<usize>,
    /// For `type = "steps"`: how much of each step is spent going to the value of the next, 0 to 1 (held to it when the session is loaded).
    #[serde(default)]
    pub glide: f64,
    /// For `type = "steps"` and `type = "lfo"`: whether a note starts it again, at its first step or at the start of its wave. If the file does not say, it is left to the loader:
    /// a note starts a step sequencer again, and does not start an LFO.
    #[serde(default)]
    pub retrigger: Option<bool>,
    /// For `type = "lfo"`: the seconds it takes to come in from nothing after a note, held to 0 to 5 when the session is loaded (0, none, if the file does not say).
    #[serde(default)]
    pub fade: f64,
    /// For `type = "lfo"` and `type = "steps"`: the note length that a cycle (an LFO's) or a step follows the tempo by, as the screens name it (`"1/8"`, `"1/8 triplet"`, `"4 bars"`), in place of
    /// the rate, which is kept for when this is `"free"` or is not there. The loader reads the name: one that is no note length is no sync, and the log says so.
    #[serde(default)]
    pub sync: Option<String>,
    /// For `type = "midi"`: the bound CC number (mutually exclusive with
    /// `midi_pitch_bend` / `midi_aftertouch`). Absent = unbound (armed for
    /// learning on load).
    #[serde(default)]
    pub midi_cc: Option<u8>,
    /// For `type = "midi"`: bound to the pitch-bend wheel.
    #[serde(default)]
    pub midi_pitch_bend: bool,
    /// For `type = "midi"`: bound to key pressure / aftertouch (channel
    /// pressure or poly aftertouch).
    #[serde(default)]
    pub midi_aftertouch: bool,
    /// For `type = "midi"`: follows the velocity of the notes the lane plays (the last one's, which stays when it ends).
    #[serde(default)]
    pub midi_velocity: bool,
    /// For `type = "midi"`: follows the number of the notes the lane plays (the last one's).
    #[serde(default)]
    pub midi_key: bool,
    /// (Free modulators) Whose notes the modulator hears, as the file names a place of the session: `instrument[0]` (the notes that the lane's instrument is given) or `group[0]` (those of its
    /// members). None is every event, whatever its source. A place that is not a lane or a group of the session is no ear: the modulator hears every event, and the log says so.
    #[serde(default)]
    pub hears: Option<String>,
    #[serde(default, rename = "target")]
    pub targets: Vec<ModTargetConfig>,
    /// Where the modulator is drawn.
    #[serde(default, deserialize_with = "lenient_ui")]
    pub ui: Option<NodeUi>,
}

#[derive(Deserialize)]
pub struct ModTargetConfig {
    /// Plugin parameter name (mutually exclusive with mod_* fields).
    #[serde(default)]
    pub param: Option<String>,
    /// (Free modulators) The plugin a `param` target drives, as the file names a place of the session: `instrument[0]` (the lane's instrument), `instrument[0].effect[1]` or
    /// `group[0].effect[1]`. A free modulator is in no lane, so its cable names the plugin and not a chain slot, a member or a bus.
    #[serde(default)]
    pub to: Option<String>,
    /// (Free modulators) A host-side value that the cable drives instead of a plugin's parameter, `volume`, `mix` or `pan`; `to` says whose, as it does for a `param`: `instrument[0]` (a lane's volume),
    /// `instrument[0].effect[1]` or `group[0].effect[0]` (an effect's mix) or `group[0]` (a group's volume or pan). Mutually exclusive with `param` and the `mod_*` fields.
    #[serde(default)]
    pub gain: Option<String>,
    /// Chain slot a `param` target lives on: 0 = instrument (default),
    /// 1..N = effects in order.
    #[serde(default)]
    pub slot: usize,
    /// (Group modulators) member ordinal a `param` target lands on. Present
    /// only for group-scoped modulator targets that drive a member instrument.
    #[serde(default)]
    pub member: Option<usize>,
    /// (Group modulators) bus-effect index a `param` target lands on. Present
    /// only for group-scoped modulator targets that drive a bus effect.
    #[serde(default)]
    pub bus: Option<usize>,
    /// Target a sibling modulator's LFO rate (by mod index).
    #[serde(default)]
    pub mod_rate: Option<usize>,
    /// Target a sibling modulator's target depth as [mod_index, target_index].
    #[serde(default)]
    pub mod_depth: Option<Vec<usize>>,
    /// Target a sibling modulator's envelope attack (by mod index).
    #[serde(default)]
    pub mod_attack: Option<usize>,
    /// Target a sibling modulator's envelope decay (by mod index).
    #[serde(default)]
    pub mod_decay: Option<usize>,
    /// Target a sibling modulator's envelope sustain (by mod index).
    #[serde(default)]
    pub mod_sustain: Option<usize>,
    /// Target a sibling modulator's envelope release (by mod index).
    #[serde(default)]
    pub mod_release: Option<usize>,
    #[serde(default = "default_depth")]
    pub depth: f64,
}

#[derive(Deserialize)]
pub struct PluginConfig {
    pub plugin: String,
    pub preset: Option<String>,
    #[serde(default = "default_volume")]
    pub volume: f64,
    #[serde(default = "default_pitch_bend_range")]
    pub pitch_bend_range: f64,
    #[serde(default)]
    pub remap: HashMap<String, RemapTarget>,
    #[serde(default)]
    pub params: HashMap<String, f64>,
    /// The parameters a front-end shows as knobs on the instrument's node, by name, in order (`pinned = [...]`). The engine and
    /// the TUI do not use them, and they are kept when a session is saved.
    #[serde(default, deserialize_with = "lenient_names")]
    pub pinned: Vec<String>,
}

fn default_volume() -> f64 {
    1.0
}

fn default_pitch_bend_range() -> f64 {
    2.0
}

#[derive(Deserialize)]
pub struct EffectConfig {
    pub plugin: String,
    pub preset: Option<String>,
    #[serde(default = "default_mix")]
    pub mix: f64,
    #[serde(default)]
    pub params: HashMap<String, f64>,
    #[serde(default, rename = "modulator")]
    pub modulators: Vec<ModulatorConfig>,
    /// The parameters a front-end shows as knobs on the effect's node, by name, in order.
    #[serde(default, deserialize_with = "lenient_names")]
    pub pinned: Vec<String>,
    /// Where the effect is drawn.
    #[serde(default, deserialize_with = "lenient_ui")]
    pub ui: Option<NodeUi>,
    /// The word that the file says for the feed (`feed = "none"`), as it was read: `parse` turns it into `feed`, and refuses a word that is not one with the place of the table.
    #[serde(default, rename = "feed")]
    feed_word: Option<String>,
    /// Whether the link into the effect from the stage before it is there (`Feed::None` cuts it).
    #[serde(skip)]
    pub feed: Feed,
}

fn default_mix() -> f64 {
    1.0
}

// ---------------------------------------------------------------------------
// TOML deserialization helpers (intermediate structs)
// ---------------------------------------------------------------------------

/// New flat format: [[instrument]] at top level.
/// Instrument plugin fields are directly on the [[instrument]] table.
#[derive(Deserialize)]
struct SessionRaw {
    #[serde(default, rename = "instrument")]
    instruments: Vec<InstrumentSlotRaw>,
    #[serde(default, rename = "group")]
    groups: Vec<GroupRaw>,
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
    #[serde(default, rename = "module")]
    modules: Vec<ModuleRaw>,
    #[serde(default, rename = "cable")]
    cables: Vec<CableRaw>,
    #[serde(default)]
    keys: Option<KeysRaw>,
    #[serde(default, rename = "device")]
    devices: Vec<DeviceRaw>,
    #[serde(default)]
    piano: Option<PianoConfig>,
    #[serde(default, deserialize_with = "lenient_table")]
    ui: Option<toml::Table>,
    #[serde(default)]
    bpm: Option<f64>,
    #[serde(default)]
    master: Option<MasterRaw>,
}

/// The `[master]` table as the file has it.
#[derive(Deserialize)]
struct MasterRaw {
    #[serde(default)]
    volume: Option<f64>,
    #[serde(default)]
    limiter: Option<bool>,
    #[serde(default)]
    ceiling: Option<f64>,
}

/// A `[[cable]]` as the file has it: its two places as text, and a level.
#[derive(Deserialize)]
struct CableRaw {
    from: String,
    to: String,
    #[serde(default)]
    gain: Option<f64>,
}

/// A `[[module]]` table as the file has it: its members are read leniently, as a place is (a layout is not worth refusing a session for).
#[derive(Deserialize)]
struct ModuleRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient_members")]
    members: Vec<String>,
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
}

/// A `[keys]` table as the file has it.
#[derive(Deserialize)]
struct KeysRaw {
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
}

/// A `[[device]]` table as the file has it.
#[derive(Deserialize)]
struct DeviceRaw {
    name: String,
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
}

#[derive(Deserialize)]
struct GroupRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default)]
    pan: f64,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
    #[serde(default)]
    route: Option<String>,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
}

#[derive(Deserialize)]
struct InstrumentSlotRaw {
    // Instrument plugin fields (flat on the table).
    plugin: Option<String>,
    #[serde(default)]
    preset: Option<String>,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default = "default_pitch_bend_range")]
    pitch_bend_range: f64,
    #[serde(default)]
    remap: HashMap<String, RemapTarget>,
    #[serde(default)]
    params: HashMap<String, f64>,
    #[serde(default, deserialize_with = "lenient_names")]
    pinned: Vec<String>,
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
    // Slot-level fields.
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    transpose: i8,
    #[serde(default)]
    group: Option<usize>,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
    #[serde(default)]
    route: Option<String>,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    #[serde(default)]
    pattern: Option<PatternRaw>,
    #[serde(default, deserialize_with = "lenient_ui")]
    ui: Option<NodeUi>,
}

#[derive(Deserialize)]
struct PatternRaw {
    #[serde(default = "default_pattern_bpm")]
    bpm: f64,
    #[serde(default = "default_pattern_length")]
    length_beats: f64,
    #[serde(default = "default_true")]
    looping: bool,
    #[serde(default)]
    base_note: Option<String>,
    #[serde(default)]
    events: Vec<PatternEventRaw>,
    #[serde(default)]
    enabled: bool,
    /// "chromatic" (default) or "in_key".
    #[serde(default)]
    transpose: Option<String>,
}

fn default_true() -> bool { true }

#[derive(Deserialize)]
struct PatternEventRaw {
    frame: u64,
    status: String, // "on" or "off"
    note: String,   // e.g. "C4"
    #[serde(default)]
    velocity: u8,
}

fn default_pattern_bpm() -> f64 {
    120.0
}

fn default_pattern_length() -> f64 {
    4.0
}

fn default_mod_type() -> String {
    "lfo".into()
}

fn default_waveform() -> String {
    "sine".into()
}

fn default_depth() -> f64 {
    0.5
}

fn default_attack() -> f64 {
    0.01
}

fn default_decay() -> f64 {
    0.3
}

fn default_sustain() -> f64 {
    0.7
}

fn default_release() -> f64 {
    0.5
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

pub fn load(path: &str) -> anyhow::Result<SessionConfig> {
    let content = std::fs::read_to_string(path)?;
    parse(&content)
}

/// Refuse a `nan` or an `inf` anywhere in a session file (TOML has both), and a number too
/// big for the 32-bit floats the model and the audio thread hold (`1e39` is finite to TOML's
/// 64 bits and `inf` once narrowed): as a volume, a rate or a depth it would be in the model
/// and in the audio thread, and a snapshot with one in it is not even JSON. The error says
/// where it is.
fn reject_non_finite(table: &toml::Table) -> anyhow::Result<()> {
    fn walk(value: &toml::Value, path: &str) -> anyhow::Result<()> {
        match value {
            toml::Value::Float(number) if !number.is_finite() => anyhow::bail!("{path} is not a finite number ({number})"),
            toml::Value::Float(number) if !(*number as f32).is_finite() => {
                anyhow::bail!("{path} is too big for the 32-bit numbers the audio engine holds ({number})")
            }
            toml::Value::Array(items) => items.iter().enumerate().try_for_each(|(i, item)| walk(item, &format!("{path}[{i}]"))),
            toml::Value::Table(table) => table.iter().try_for_each(|(key, item)| walk(item, &format!("{path}.{key}"))),
            _ => Ok(()),
        }
    }
    table.iter().try_for_each(|(key, value)| walk(value, key))
}

impl ModulatorConfig {
    /// Whether the loader makes an LFO of it: the types it knows that are not one are `envelope`, `midi`, `macro` and `steps`, and a type that it does not know is one too.
    pub fn is_lfo(&self) -> bool {
        !matches!(self.mod_type.as_str(), "envelope" | "midi" | "macro" | "steps")
    }

    /// Whether the file's `rate` is a speed that is read (an LFO's or a step sequencer's), and not left unread (an envelope's, a control's, a macro's).
    fn has_a_rate(&self) -> bool {
        self.is_lfo() || self.mod_type == "steps"
    }
}

/// Refuse an LFO or a step sequencer whose rate is below 0, in a lane, in a group, among the free modulators and in an effect of a lane (where older sessions kept a lane's modulators): the phase
/// of an LFO counts down from 0 towards -1 and the shapes leave the -1 to 1 that a cable's law assumes (a saw is from -3 to -1), where the knob
/// stops at 0.01 and the controller holds to it, and a step sequencer would play its steps backwards. Zero is one that stands still, and a rate above the knob's end
/// (50 Hz) is the file's to say. The error says where it is, as that of a number that is not one does.
fn reject_backwards_lfos(raw: &SessionRaw) -> anyhow::Result<()> {
    fn check(modulators: &[ModulatorConfig], place: &str) -> anyhow::Result<()> {
        let backwards = modulators.iter().enumerate().find_map(|(i, m)| m.rate.filter(|rate| m.has_a_rate() && *rate < 0.0).map(|rate| (i, rate, m.is_lfo())));
        match backwards {
            Some((i, rate, lfo)) => {
                let what = if lfo { "an LFO" } else { "a step sequencer" };
                anyhow::bail!("{}.rate is below 0 ({rate}): {what} cannot go round backwards", modulator_place(place, i))
            }
            None => Ok(()),
        }
    }
    each_rack(raw, check)
}

/// Refuse a step sequencer that has no values or more than it can have (`MAX_STEPS`), where the file has a list of them: which of them would be left out is
/// not for the loader to say. The error says where it is.
fn reject_bad_steps(raw: &SessionRaw) -> anyhow::Result<()> {
    fn check(modulators: &[ModulatorConfig], place: &str) -> anyhow::Result<()> {
        for (i, m) in modulators.iter().enumerate().filter(|(_, m)| m.mod_type == "steps") {
            match m.steps.as_ref().map(Vec::len) {
                Some(0) => anyhow::bail!("{}.steps has no values: a step sequencer has 1 to {MAX_STEPS} steps", modulator_place(place, i)),
                Some(n) if n > MAX_STEPS => anyhow::bail!("{}.steps has {n} values: a step sequencer has {MAX_STEPS} steps at most", modulator_place(place, i)),
                _ => {}
            }
        }
        Ok(())
    }
    each_rack(raw, check)
}

/// Where modulator `index` of the rack at `place` is in the file: `group[0].modulator[1]`, and `modulator[1]` for the free modulators, whose rack is the file's own (`place` is empty).
fn modulator_place(place: &str, index: usize) -> String {
    if place.is_empty() { format!("modulator[{index}]") } else { format!("{place}.modulator[{index}]") }
}

/// Run `check` on every rack of modulators that a session file can have, with the place of the rack in the file: the free modulators' (an empty place), a group's, a lane's and an
/// effect's of a lane (where older sessions kept a lane's modulators).
fn each_rack(raw: &SessionRaw, check: fn(&[ModulatorConfig], &str) -> anyhow::Result<()>) -> anyhow::Result<()> {
    check(&raw.modulators, "")?;
    for (g, group) in raw.groups.iter().enumerate() {
        check(&group.modulators, &format!("group[{g}]"))?;
    }
    for (i, slot) in raw.instruments.iter().enumerate() {
        check(&slot.modulators, &format!("instrument[{i}]"))?;
        for (e, effect) in slot.effects.iter().enumerate() {
            check(&effect.modulators, &format!("instrument[{i}].effect[{e}]"))?;
        }
    }
    Ok(())
}

/// The route that a table's `route` says (`out` for a table that has none), or the reason it is not one, with the place of the table: a lane that goes nowhere is silent, so a word
/// that is not a route is not read as the way a lane goes by itself, and the session is not loaded.
fn route_of(word: Option<String>, place: &str) -> anyhow::Result<Route> {
    match word {
        None => Ok(Route::Out),
        Some(word) => Route::parse(&word).ok_or_else(|| anyhow::anyhow!("{place}.route is \"{word}\": a way out is \"out\" or \"none\"")),
    }
}

/// The feed that the effects of a table say, resolved into each (`chain` for an effect that says none), or the reason that a word is not one, with the place of the effect: an effect that is
/// fed by nothing is silent, so a word that is not a feed is not read as the way a chain is wired by itself, and the session is not loaded.
fn resolve_feeds(effects: &mut [EffectConfig], owner: &str) -> anyhow::Result<()> {
    for (at, effect) in effects.iter_mut().enumerate() {
        effect.feed = match effect.feed_word.take() {
            None => Feed::Chain,
            Some(word) => Feed::parse(&word).ok_or_else(|| anyhow::anyhow!("{owner}.effect[{at}].feed is \"{word}\": a feed is \"chain\" or \"none\""))?,
        };
    }
    Ok(())
}

/// Parse session TOML text (the body of `load`, usable without a file), with every modulator made the session's own (`SessionConfig::move_modulators_to_the_session`).
pub fn parse(content: &str) -> anyhow::Result<SessionConfig> {
    let mut config = parse_raw(content)?;
    config.move_modulators_to_the_session();
    Ok(config)
}

/// Parse session TOML text as it is written: the modulators of a lane and of a group are in the lane and the group (the old form of the file).
pub fn parse_raw(content: &str) -> anyhow::Result<SessionConfig> {
    reject_non_finite(&content.parse::<toml::Table>()?)?;
    let mut raw: SessionRaw = toml::from_str(content)?;
    reject_backwards_lfos(&raw)?;
    reject_bad_steps(&raw)?;
    for (at, group) in raw.groups.iter_mut().enumerate() {
        resolve_feeds(&mut group.effects, &format!("group[{at}]"))?;
    }
    for (at, slot) in raw.instruments.iter_mut().enumerate() {
        resolve_feeds(&mut slot.effects, &format!("instrument[{at}]"))?;
    }
    let groups: Vec<GroupConfig> = raw
        .groups
        .into_iter()
        .enumerate()
        .map(|(at, g)| {
            Ok(GroupConfig {
                name: g.name,
                volume: g.volume as f32,
                pan: (g.pan as f32).clamp(-1.0, 1.0),
                muted: g.mute,
                solo: g.solo,
                route: route_of(g.route, &format!("group[{at}]"))?,
                effects: g.effects,
                modulators: g.modulators,
                ui: g.ui,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    let mut instruments = Vec::new();
    for (at, slot) in raw.instruments.into_iter().enumerate() {
        let route = route_of(slot.route, &format!("instrument[{at}]"))?;
        let range = slot.range.as_deref().map(parse_range).transpose()?;
        let pattern = slot.pattern.map(parse_pattern_raw).transpose()?;
        let instrument = slot.plugin.map(|plugin| PluginConfig {
            plugin,
            preset: slot.preset,
            volume: slot.volume,
            pitch_bend_range: slot.pitch_bend_range,
            remap: slot.remap,
            params: slot.params,
            pinned: slot.pinned,
        });
        // Drop out-of-range group references defensively.
        let group = slot.group.filter(|&g| g < groups.len());
        instruments.push(InstrumentSlotConfig {
            range,
            transpose: slot.transpose,
            muted: slot.mute,
            solo: slot.solo,
            route,
            instrument,
            effects: slot.effects,
            modulators: slot.modulators,
            pattern,
            group,
            ui: slot.ui,
        });
    }
    let master_ui = raw.ui.and_then(|mut table| table.remove("master")).and_then(node_ui_of);
    let mut devices: Vec<DeviceConfig> = Vec::new();
    for (at, device) in raw.devices.into_iter().enumerate() {
        let name = device_name(&device.name).map_err(|why| anyhow::anyhow!("device[{at}].name {why}"))?;
        if let Some(first) = devices.iter().position(|other| other.name == name) {
            anyhow::bail!("device[{at}].name is \"{name}\", as device[{first}]'s is: a device has one node");
        }
        devices.push(DeviceConfig { name, ui: device.ui });
    }
    let cables = raw
        .cables
        .into_iter()
        .enumerate()
        .map(|(i, cable)| {
            let place = |end: &str, text: &str| {
                CablePlace::parse(text).ok_or_else(|| {
                    anyhow::anyhow!("cable[{i}].{end}: \"{text}\" is not a place of the session (instrument[0], instrument[0].effect[1], group[0], group[0].effect[1], master, keys or device[0])")
                })
            };
            let (from, to) = (place("from", &cable.from)?, place("to", &cable.to)?);
            if from.is_source() && cable.gain.is_some() {
                anyhow::bail!("cable[{i}].gain: a cable of notes has no level: it carries the notes of {from} as they are");
            }
            Ok(CableConfig { from, to, gain: cable.gain.map(|gain| gain as f32) })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let modules = raw
        .modules
        .into_iter()
        .enumerate()
        .map(|(at, module)| ModuleConfig {
            name: module_name(module.name.as_deref().unwrap_or("")),
            members: module
                .members
                .iter()
                .filter_map(|text| {
                    let place = MemberPlace::parse(text);
                    if place.is_none() {
                        log::warn!("module[{at}].members: \"{text}\" is not a place of a lane or a modulator (instrument[0], modulator[0]): left out");
                    }
                    place
                })
                .collect(),
            ui: module.ui,
        })
        .collect();
    let keys = raw.keys.map(|keys| KeysConfig { ui: keys.ui });
    let master = raw.master.map(|master| MasterConfig { volume: master.volume.map(|volume| volume as f32), limiter: master.limiter, ceiling: master.ceiling.map(|ceiling| ceiling as f32) });
    Ok(SessionConfig { instruments, groups, piano: raw.piano, master_ui, bpm: raw.bpm.map(|bpm| bpm as f32), master, cables, keys, devices, free_modulators: raw.modulators, modules })
}

/// A place in a rack that no modulator is at: a cable that names it is left out by the loader, as one that names a modulator that is not there is.
const NOT_THERE: usize = usize::MAX;

/// Where the plugin in chain slot `slot` of lane `lane` is, as a free modulator's cable names it: `instrument[0]` for the instrument (slot 0) and `instrument[0].effect[1]` for the effect in slot 2.
pub(crate) fn lane_plugin_place(lane: usize, slot: usize) -> String {
    if slot == 0 { format!("instrument[{lane}]") } else { format!("instrument[{lane}].effect[{}]", slot - 1) }
}

/// Where modulator `index` of a rack of `count` that begins at `base` in the new list is: there, or nowhere if the rack has no such modulator.
fn place_in_rack(base: usize, count: usize, index: usize) -> usize {
    if index < count { base + index } else { NOT_THERE }
}

/// Put `map` of the index of every modulator that the cable `target` names (the rate of a sibling, a stage of its envelope, or the depth of one of its cables) in place of it.
fn remap_siblings(target: &mut ModTargetConfig, map: impl Fn(usize) -> usize) {
    for field in [&mut target.mod_rate, &mut target.mod_attack, &mut target.mod_decay, &mut target.mod_sustain, &mut target.mod_release].into_iter().flatten() {
        *field = map(*field);
    }
    if let Some(pair) = &mut target.mod_depth {
        // (The loader reads the modulator of a pair that has none as the first.)
        match pair.first_mut() {
            Some(first) => *first = map(*first),
            None => pair.push(map(0)),
        }
    }
}

impl SessionConfig {
    /// Make every modulator of the session one of its own (step 94 of `docs/web-ui.md`, P5c-2 of `docs/patch-graph.md` §11.6: "all modulators should be global"): the modulators of each lane (and,
    /// for older files, of each of its effects) and of each group are moved to `free_modulators`, in front of the ones that the file has there, each hearing what its rack heard (`hears = "instrument[N]"`
    /// for a lane's, `"group[N]"` for a group's), and every cable of theirs is written as a free modulator's is: a plugin by its place in the session (`to`) and a modulator by its place in the new list.
    /// They are in the order that the loader made them in (each lane's merged rack in the order of the lanes, then each group's), so that a random LFO has the seed that it had, and a place among
    /// the modulators is the modulator it was.
    ///
    /// A cable that the loader leaves out for what it names (a modulator that is not there, a member that is not, a slot that the chain has not) is left out after it as well: such a place is
    /// moved to one that no modulator is at, and not to the modulator that is at its number in the longer list. A group's cable to the depth of a cable of a member's modulator names the cable by
    /// the place it has among those that the loader made, and this by the place it has in the file: the two are the same unless a cable of that modulator before it is left out (a save writes none).
    pub fn move_modulators_to_the_session(&mut self) {
        // The merged rack of each lane, as the loader makes it: its own modulators, and then those that an older file nested in each of its effects (which drive their effect, whatever slot their cable
        // says), with the slot of that effect and the place that the modulator's own list begins at in the rack, which is what a cable between siblings counts from.
        let mut lane_racks: Vec<Vec<(ModulatorConfig, Option<usize>, usize)>> = Vec::new();
        for lane in &mut self.instruments {
            let mut lists = vec![(std::mem::take(&mut lane.modulators), None)];
            lists.extend(lane.effects.iter_mut().enumerate().map(|(at, effect)| (std::mem::take(&mut effect.modulators), Some(at + 1))));
            let mut rack = Vec::new();
            for (list, own_slot) in lists {
                let begins = rack.len();
                rack.extend(list.into_iter().map(|modulator| (modulator, own_slot, begins)));
            }
            lane_racks.push(rack);
        }
        let group_racks: Vec<Vec<ModulatorConfig>> = self.groups.iter_mut().map(|group| std::mem::take(&mut group.modulators)).collect();
        let members: Vec<Vec<usize>> = (0..self.groups.len()).map(|group| (0..self.instruments.len()).filter(|lane| self.instruments[*lane].group == Some(group)).collect()).collect();

        // Where each rack begins in the new list: the lanes' in order, then the groups', and then the modulators that the file has of its own.
        let lane_counts: Vec<usize> = lane_racks.iter().map(Vec::len).collect();
        let lane_at: Vec<usize> = lane_counts.iter().scan(0, |next, count| Some(std::mem::replace(next, *next + count))).collect();
        let group_start = lane_counts.iter().sum::<usize>();
        let group_at: Vec<usize> = group_racks.iter().scan(group_start, |next, rack| Some(std::mem::replace(next, *next + rack.len()))).collect();
        let moved = group_start + group_racks.iter().map(Vec::len).sum::<usize>();

        let mut free: Vec<ModulatorConfig> = Vec::with_capacity(moved + self.free_modulators.len());
        for (lane, rack) in lane_racks.into_iter().enumerate() {
            for (mut modulator, own_slot, begins) in rack {
                for target in &mut modulator.targets {
                    if target.param.is_some() {
                        target.to = Some(lane_plugin_place(lane, own_slot.unwrap_or(target.slot)));
                    } else {
                        remap_siblings(target, |index| place_in_rack(lane_at[lane], lane_counts[lane], index.saturating_add(begins)));
                    }
                }
                modulator.hears = Some(format!("instrument[{lane}]"));
                free.push(modulator);
            }
        }
        for (group, rack) in group_racks.into_iter().enumerate() {
            let count = rack.len();
            for mut modulator in rack {
                for target in &mut modulator.targets {
                    if let Some(member) = target.member {
                        // A member's plugin, or a field of one of the member's modulators (by its place in the member's rack): a member that is not there has no plugin and no modulator.
                        let lane = members[group].get(member).copied();
                        if target.param.is_some() {
                            target.to = lane.map(|lane| lane_plugin_place(lane, target.slot));
                        } else {
                            remap_siblings(target, |index| lane.map_or(NOT_THERE, |lane| place_in_rack(lane_at[lane], lane_counts[lane], index)));
                        }
                    } else if let Some(bus) = target.bus {
                        if target.param.is_some() {
                            target.to = Some(format!("group[{group}].effect[{bus}]"));
                        } else {
                            // (A bus with no parameter is a cable that the loader leaves out, whatever else it says.)
                            remap_siblings(target, |_| NOT_THERE);
                        }
                    } else {
                        remap_siblings(target, |index| place_in_rack(group_at[group], count, index));
                    }
                }
                modulator.hears = Some(format!("group[{group}]"));
                free.push(modulator);
            }
        }
        // The file's own are after them, and name each other by their places in the file, which are `moved` places further on now.
        for mut modulator in std::mem::take(&mut self.free_modulators) {
            for target in &mut modulator.targets {
                remap_siblings(target, |index| index.saturating_add(moved));
            }
            free.push(modulator);
        }
        self.free_modulators = free;
    }
}

/// Parse a note range like "C0-B3" into (low, high) MIDI note numbers
/// (inclusive). Either bound may be omitted for an open-ended range: "C4-"
/// means C4 and up (to MIDI 127), "-C4" means up to and including C4 (from
/// MIDI 0). Octave -1 note names are accepted (e.g. "C-1-B3").
pub fn parse_range(s: &str) -> anyhow::Result<(u8, u8)> {
    let (low_str, high_str) = split_range_bounds(s.trim()).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid range format '{}', expected 'NOTE-NOTE' (e.g. 'C0-B3'); \
             open-ended 'C4-' or '-C4' are also allowed",
            s
        )
    })?;
    let low_str = low_str.trim();
    let high_str = high_str.trim();
    if low_str.is_empty() && high_str.is_empty() {
        anyhow::bail!("invalid range '{}': at least one bound is required", s);
    }
    let low = if low_str.is_empty() { 0 } else { parse_note_name(low_str)? };
    let high = if high_str.is_empty() { 127 } else { parse_note_name(high_str)? };
    if low > high {
        anyhow::bail!("range '{}' has low ({}) > high ({})", s, low, high);
    }
    Ok((low, high))
}

/// Split a range string at its bounds separator '-'. A '-' right after the
/// low bound's note letter (or accidental) that is followed by a digit is the
/// negative-octave sign, not the separator (e.g. "C-1-B3" → ("C-1", "B3")).
fn split_range_bounds(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0] == b'-' {
        return Some(("", &s[1..]));
    }
    let mut i = 1; // note letter
    if matches!(b.get(i), Some(b'#' | b'b')) {
        i += 1; // accidental
    }
    if b.get(i) == Some(&b'-') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        i += 1; // negative-octave sign
    }
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1; // octave digits
    }
    while b.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1; // whitespace before separator
    }
    if b.get(i) == Some(&b'-') {
        Some((&s[..i], &s[i + 1..]))
    } else {
        None
    }
}

/// Format a note range as a string `parse_range` accepts, preserving
/// open-ended forms: (0, hi) → "-B3", (lo, 127) → "C4-", else "C0-B3".
pub fn format_range((low, high): (u8, u8)) -> String {
    match (low, high) {
        (0, hi) => format!("-{}", note_name(hi)),
        (lo, 127) => format!("{}-", note_name(lo)),
        (lo, hi) => format!("{}-{}", note_name(lo), note_name(hi)),
    }
}

/// Parse a raw pattern from TOML into a PatternConfig.
fn parse_pattern_raw(raw: PatternRaw) -> anyhow::Result<PatternConfig> {
    let base_note = raw
        .base_note
        .as_deref()
        .map(parse_note_name)
        .transpose()?;
    let in_key = match raw.transpose.as_deref() {
        None | Some("chromatic") => false,
        Some("in_key") | Some("in-key") | Some("in key") => true,
        Some(other) => anyhow::bail!(
            "invalid pattern transpose '{other}', expected 'chromatic' or 'in_key'"
        ),
    };
    let mut events = Vec::with_capacity(raw.events.len());
    for ev in &raw.events {
        let note = parse_note_name(&ev.note)?;
        let status = match ev.status.as_str() {
            "on" => 0x90,
            "off" => 0x80,
            other => anyhow::bail!("invalid pattern event status '{other}', expected 'on' or 'off'"),
        };
        events.push((ev.frame, status, note, ev.velocity));
    }
    Ok(PatternConfig {
        bpm: raw.bpm as f32,
        length_beats: raw.length_beats as f32,
        looping: raw.looping,
        base_note,
        events,
        enabled: raw.enabled,
        in_key,
    })
}

/// Resolve a plugin path relative to the session file's directory.
pub fn resolve_plugin_path(plugin_source: &str, session_dir: &Path) -> String {
    // URI-style references (lv2:..., clap:...) pass through as-is
    if plugin_source.contains(':') {
        return plugin_source.to_string();
    }
    // Absolute paths pass through
    let p = Path::new(plugin_source);
    if p.is_absolute() {
        return plugin_source.to_string();
    }
    // Relative paths are resolved against the session file's directory
    session_dir
        .join(plugin_source)
        .to_string_lossy()
        .to_string()
}

/// Parse a note name like "C4", "G#3", "Bb5" into a MIDI note number.
/// C4 = 60, A0 = 21. Formula: (octave + 1) * 12 + semitone.
pub fn parse_note_name(name: &str) -> anyhow::Result<u8> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        anyhow::bail!("empty note name");
    }

    let letter = bytes[0].to_ascii_uppercase();
    let semitone_base = match letter {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => anyhow::bail!("invalid note letter '{}'", bytes[0] as char),
    };

    let (accidental, rest) = if bytes.len() > 1 && bytes[1] == b'#' {
        (1i8, &name[2..])
    } else if bytes.len() > 1 && bytes[1] == b'b' {
        (-1i8, &name[2..])
    } else {
        (0i8, &name[1..])
    };

    let octave: i8 = rest
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid octave in note name '{name}'"))?;

    let note = (octave as i16 + 1) * 12 + semitone_base as i16 + accidental as i16;
    if !(0..=127).contains(&note) {
        anyhow::bail!("note '{name}' is out of MIDI range (0-127)");
    }
    Ok(note as u8)
}

/// Apply a preset to a loaded plugin (no parameter overrides).
/// Accepts either the preset's display `name` or its `id`.
pub fn apply_preset(plugin: &mut Box<dyn Plugin>, preset_ref: &str) {
    let presets = plugin.presets();
    let found = presets
        .iter()
        .find(|p| p.name == *preset_ref)
        .or_else(|| presets.iter().find(|p| p.id == *preset_ref));
    match found {
        Some(preset_info) => {
            let id = preset_info.id.clone();
            match plugin.load_preset(&id) {
                Ok(()) => log::info!("Loaded preset '{}' on {}", preset_info.name, plugin.name()),
                Err(e) => log::warn!(
                    "Failed to load preset '{}' on {}: {}",
                    preset_info.name,
                    plugin.name(),
                    e
                ),
            }
        }
        None => {
            log::warn!(
                "Preset '{}' not found for {} (available: {})",
                preset_ref,
                plugin.name(),
                presets
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

/// Data needed to serialize one instrument slot for saving.
pub struct SaveInstrumentSlot {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    pub muted: bool,
    pub solo: bool,
    pub route: Route,
    pub instrument: Option<SaveInstrument>,
    pub effects: Vec<SaveEffect>,
    pub pattern: Option<SavePattern>,
    /// Submix group membership: index into the saved groups, or None.
    pub group: Option<usize>,
    /// Where the lane is drawn.
    pub ui: Option<NodeUi>,
}

/// Data needed to serialize a submix group for saving.
pub struct SaveGroup {
    pub name: Option<String>,
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub route: Route,
    pub effects: Vec<SaveEffect>,
    /// Where the group is drawn.
    pub ui: Option<NodeUi>,
}

/// Data needed to serialize a cable for saving: its two places in the file's terms, and its level (a cable of notes, which starts at a source, has none: its level is not written).
pub struct SaveCable {
    pub from: CablePlace,
    pub to: CablePlace,
    pub gain: f32,
}

/// The nodes that stand outside the lanes and the groups, as a save writes them: the sources of notes (the keys, if it has them, and its devices in the order that a cable names them in)
/// and the session's modulators.
#[derive(Default)]
pub struct SaveSources {
    pub keys: Option<SaveKeys>,
    pub devices: Vec<SaveDevice>,
    /// The modulators, in order: each cable names the plugin it drives by its place (`SaveModTarget::to`).
    pub modulators: Vec<SaveModulator>,
    /// The modules that were added as one thing (`[[module]]`, step 108), in order, each naming the lanes and the modulators that came with it by their places.
    pub modules: Vec<SaveModule>,
    /// The `[master]` table, or none when the master is what it is without one (the model's `master_to_save`). It rides with the rest of what stands outside the lanes and the groups, so that
    /// the writers that take the session in parts take no part more.
    pub master: Option<MasterConfig>,
}

/// A module that was added as one thing (`[[module]]`).
pub struct SaveModule {
    pub name: String,
    pub members: Vec<MemberPlace>,
    /// Where the box is drawn.
    pub ui: Option<NodeUi>,
}

/// The keys as a source (`[keys]`).
pub struct SaveKeys {
    /// Where the node is drawn.
    pub ui: Option<NodeUi>,
}

/// A device as a source (`[[device]]`).
pub struct SaveDevice {
    pub name: String,
    /// Where the node is drawn.
    pub ui: Option<NodeUi>,
}

/// Data needed to serialize a pattern for saving.
pub struct SavePattern {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    pub in_key: bool,
}

/// Data needed to serialize a modulator for saving.
pub enum SaveModSource {
    /// An LFO: its waveform and rate, whether a note starts it again at the start of its wave, the seconds it takes to come in after a note (0 for none) and the name of the
    /// note length that a cycle of it follows the tempo by (`None` for none: it goes at its rate).
    Lfo { waveform: String, rate: f32, retrigger: bool, fade: f32, sync: Option<String> },
    Envelope { attack: f32, decay: f32, sustain: f32, release: f32 },
    /// A MIDI-learned control. `cc = Some(n)` = CC n, else `pitch_bend` /
    /// `aftertouch` select the wheel or key pressure, and `velocity` / `key` follow the notes
    /// of the lane. All unset = unbound (still learning).
    MidiLearn { cc: Option<u8>, pitch_bend: bool, aftertouch: bool, velocity: bool, key: bool },
    /// A macro: the number it is set to.
    Macro { value: f32 },
    /// A step sequencer: all its steps, how many are in use, how fast it goes, how it glides, whether a note starts it again and the name of the note length that a step of it
    /// follows the tempo by (`None` for none).
    Steps { values: Vec<f32>, length: usize, rate: f32, glide: f32, retrigger: bool, sync: Option<String> },
}

pub struct SaveModulator {
    pub source: SaveModSource,
    pub targets: Vec<SaveModTarget>,
    /// Where the modulator is drawn.
    pub ui: Option<NodeUi>,
    /// The place of the lane or the group whose notes it hears.
    pub hears: Option<CablePlace>,
}

/// Data needed to serialize a modulation target for saving.
pub struct SaveModTarget {
    pub kind: crate::plugin::chain::ModTargetKind,
    pub label: String,
    pub depth: f32,
    /// The place of the plugin the target drives, which is how the file names it.
    pub to: Option<CablePlace>,
}

/// Data needed to serialize an instrument plugin for saving.
pub struct SaveInstrument {
    pub plugin: String,
    pub volume: f32,
    pub preset: Option<String>,
    pub params: Vec<(String, f32)>,
    /// The parameters a front-end shows as knobs on the node, by name, in order.
    pub pinned: Vec<String>,
    pub pitch_bend_range: f64,
    pub remap: HashMap<String, RemapTarget>,
}

/// Data needed to serialize an effect slot for saving.
pub struct SaveEffect {
    pub plugin: String,
    pub mix: f32,
    /// Whether the link into the effect from the stage before it is there (written only when it is cut).
    pub feed: Feed,
    pub preset: Option<String>,
    pub params: Vec<(String, f32)>,
    /// The parameters a front-end shows as knobs on the node, by name, in order.
    pub pinned: Vec<String>,
    /// Where the effect is drawn.
    pub ui: Option<NodeUi>,
}

/// Serialization output: flat [[instrument]] format.
#[derive(Serialize)]
struct SessionOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    bpm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    master: Option<MasterOut>,
    /// The sources of notes come first: they are what the lanes below them hear.
    #[serde(skip_serializing_if = "Option::is_none")]
    keys: Option<KeysOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "device")]
    devices: Vec<DeviceOut>,
    #[serde(rename = "instrument")]
    instruments: Vec<InstrumentSlotOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "group")]
    groups: Vec<GroupOut>,
    /// The modulators that belong to no lane and no group come after the lanes and the groups that their cables drive.
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
    /// The modules that were added as one thing name the lanes and the modulators above them.
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "module")]
    modules: Vec<ModuleOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "cable")]
    cables: Vec<CableOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    piano: Option<PianoOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<UiOut>,
}

/// The `[master]` table in the output: only what is not what the master is without it.
#[derive(Serialize)]
struct MasterOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limiter: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ceiling: Option<f64>,
}

/// The `[keys]` table in the output.
#[derive(Serialize)]
struct KeysOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

/// One `[[device]]` table in the output.
#[derive(Serialize)]
struct DeviceOut {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

/// One `[[module]]` table in the output.
#[derive(Serialize)]
struct ModuleOut {
    name: String,
    members: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

/// One `[[cable]]` table in the output: where it starts and ends, as text, and its level when it is not 1.
#[derive(Serialize)]
struct CableOut {
    from: String,
    to: String,
    #[serde(skip_serializing_if = "is_default_volume_f32", serialize_with = "f32_clean")]
    gain: f32,
}

/// The top-level `[ui]` table: where the master mix is drawn.
#[derive(Serialize)]
struct UiOut {
    master: NodeUiOut,
}

/// A position as it is written: `x` and `y`, a whole number without a point.
#[derive(Serialize)]
struct NodeUiOut {
    #[serde(serialize_with = "coordinate")]
    x: f32,
    #[serde(serialize_with = "coordinate")]
    y: f32,
}

impl From<NodeUi> for NodeUiOut {
    fn from(ui: NodeUi) -> Self {
        NodeUiOut { x: ui.x, y: ui.y }
    }
}

/// A coordinate the way a person writes it: a whole number without a point, anything else as it was typed
/// (an f32 widened to f64 would print `0.10000000149011612`).
fn coordinate<S: serde::Serializer>(v: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        serializer.serialize_i64(*v as i64)
    } else {
        serializer.serialize_f64(clean_f64(*v))
    }
}

#[derive(Serialize)]
struct GroupOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "is_default_volume_f32", serialize_with = "f32_clean")]
    volume: f32,
    #[serde(skip_serializing_if = "is_default_pan_f32", serialize_with = "f32_clean")]
    pan: f32,
    /// Only written when on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    mute: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    solo: bool,
    /// Only written when it is `none`.
    #[serde(skip_serializing_if = "is_out")]
    route: Route,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

fn is_out(route: &Route) -> bool {
    *route == Route::Out
}

fn is_chain(feed: &Feed) -> bool {
    *feed == Feed::Chain
}

fn is_default_volume_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

/// Write an f32 the way it was typed: widening 0.8_f32 to f64 would print
/// `0.800000011920929`.
fn f32_clean<S: serde::Serializer>(v: &f32, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(clean_f64(*v))
}

fn f32_clean_opt<S: serde::Serializer>(v: &Option<f32>, serializer: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => serializer.serialize_f64(clean_f64(*v)),
        None => serializer.serialize_none(),
    }
}

fn is_default_pan_f32(v: &f32) -> bool {
    v.abs() < f32::EPSILON
}

#[derive(Serialize)]
struct PianoOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<String>,
    /// Only written when on — highlight mode is the default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    locked: bool,
}

/// One [[instrument]] table in the output. Plugin fields are at the top level.
#[derive(Serialize)]
struct InstrumentSlotOut {
    // Instrument plugin fields (top-level).
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    #[serde(skip_serializing_if = "is_default_volume_f32_opt", serialize_with = "f32_clean_opt")]
    volume: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pinned: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "is_default_pitch_bend_range_opt")]
    pitch_bend_range: Option<f64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    remap: BTreeMap<String, RemapTarget>,
    // Slot-level fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<String>,
    #[serde(skip_serializing_if = "is_zero_i8")]
    transpose: i8,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<usize>,
    /// Only written when on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    mute: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    solo: bool,
    /// Only written when it is `none`.
    #[serde(skip_serializing_if = "is_out")]
    route: Route,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<PatternOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

#[derive(Serialize)]
struct PatternOut {
    bpm: f64,
    length_beats: f64,
    #[serde(skip_serializing_if = "is_true")]
    looping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_note: Option<String>,
    /// Only written when "in_key" — chromatic is the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    transpose: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    events: Vec<PatternEventOut>,
    enabled: bool,
}

fn is_true(v: &bool) -> bool { *v }
fn is_zero_i8(v: &i8) -> bool { *v == 0 }

#[derive(Serialize)]
struct PatternEventOut {
    frame: u64,
    status: String,
    note: String,
    velocity: u8,
}

#[derive(Serialize, Default)]
struct ModulatorOut {
    #[serde(rename = "type", skip_serializing_if = "is_lfo_type")]
    mod_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    waveform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attack: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decay: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sustain: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    release: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    steps: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    glide: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retrigger: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fade: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    midi_cc: Option<u8>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_pitch_bend: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_aftertouch: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_velocity: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_key: bool,
    /// The place of the lane or the group whose notes it hears.
    #[serde(skip_serializing_if = "Option::is_none")]
    hears: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "target")]
    targets: Vec<ModTargetOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

fn is_lfo_type(s: &String) -> bool {
    s == "lfo"
}

#[derive(Serialize)]
struct ModTargetOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<String>,
    /// The place of the plugin a `param` target drives, or of the lane, the effect or the group whose volume, mix or pan a `gain` target drives.
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    /// Which host-side value a cable drives, `volume`, `mix` or `pan`.
    #[serde(skip_serializing_if = "Option::is_none")]
    gain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_rate: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_depth: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_attack: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_decay: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_sustain: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mod_release: Option<usize>,
    depth: f64,
}

#[derive(Serialize)]
struct EffectOut {
    plugin: String,
    #[serde(skip_serializing_if = "is_default_mix_f32", serialize_with = "f32_clean")]
    mix: f32,
    /// Only written when it is `none`.
    #[serde(skip_serializing_if = "is_chain")]
    feed: Feed,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pinned: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui: Option<NodeUiOut>,
}

fn save_mod_target_to_out(t: &SaveModTarget) -> ModTargetOut {
    use crate::plugin::chain::ModTargetKind;
    let mut out = ModTargetOut {
        param: None,
        to: None,
        gain: None,
        mod_rate: None,
        mod_depth: None,
        mod_attack: None,
        mod_decay: None,
        mod_sustain: None,
        mod_release: None,
        depth: clean_f64(t.depth),
    };
    match &t.kind {
        ModTargetKind::FreeParam { .. } => {
            out.param = Some(t.label.clone());
            out.to = t.to.map(|place| place.to_string());
        }
        ModTargetKind::Gain { gain, .. } => {
            out.gain = Some(gain.name().to_string());
            out.to = t.to.map(|place| place.to_string());
        }
        ModTargetKind::ModulatorRate { mod_index } => {
            out.mod_rate = Some(*mod_index);
        }
        ModTargetKind::ModulatorDepth { mod_index, target_index } => {
            out.mod_depth = Some(vec![*mod_index, *target_index]);
        }
        ModTargetKind::ModulatorAttack { mod_index } => {
            out.mod_attack = Some(*mod_index);
        }
        ModTargetKind::ModulatorDecay { mod_index } => {
            out.mod_decay = Some(*mod_index);
        }
        ModTargetKind::ModulatorSustain { mod_index } => {
            out.mod_sustain = Some(*mod_index);
        }
        ModTargetKind::ModulatorRelease { mod_index } => {
            out.mod_release = Some(*mod_index);
        }
    }
    out
}

fn is_default_volume_f32_opt(v: &Option<f32>) -> bool {
    match v {
        Some(v) => (*v - 1.0).abs() < f32::EPSILON,
        None => true,
    }
}

fn is_default_pitch_bend_range_opt(v: &Option<f64>) -> bool {
    match v {
        Some(v) => (*v - default_pitch_bend_range()).abs() < f64::EPSILON,
        None => true,
    }
}

/// Widen f32 to f64 without exposing the f32 binary mantissa as decimal noise.
/// e.g. 0.55_f32 → 0.55_f64 (not 0.550000011920929).
fn clean_f64(v: f32) -> f64 {
    v.to_string().parse::<f64>().unwrap_or(v as f64)
}

fn is_default_mix_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

/// Format a MIDI note number as a note name (e.g. 60 → "C4").
fn note_name(note: u8) -> String {
    crate::note_name(note)
}

fn mods_to_out(mods: &[SaveModulator]) -> Vec<ModulatorOut> {
    mods.iter()
        .map(|m| {
            let targets: Vec<ModTargetOut> = m
                .targets
                .iter()
                .map(save_mod_target_to_out)
                .collect();
            let ui = m.ui.map(NodeUiOut::from);
            let hears = m.hears.map(|place| place.to_string());
            match &m.source {
                // (An LFO that runs free writes neither of its key's keys, so that a file that was written before it had a key is as it was.)
                SaveModSource::Lfo { waveform, rate, retrigger, fade, sync } => ModulatorOut {
                    mod_type: "lfo".into(),
                    waveform: Some(waveform.clone()),
                    rate: Some(clean_f64(*rate)),
                    retrigger: retrigger.then_some(true),
                    fade: (*fade > 0.0).then(|| clean_f64(*fade)),
                    sync: sync.clone(),
                    hears: hears.clone(),
                    targets,
                    ui,
                    ..Default::default()
                },
                SaveModSource::Envelope { attack, decay, sustain, release } => ModulatorOut {
                    mod_type: "envelope".into(),
                    attack: Some(clean_f64(*attack)),
                    decay: Some(clean_f64(*decay)),
                    sustain: Some(clean_f64(*sustain)),
                    release: Some(clean_f64(*release)),
                    hears: hears.clone(),
                    targets,
                    ui,
                    ..Default::default()
                },
                SaveModSource::MidiLearn { cc, pitch_bend, aftertouch, velocity, key } => ModulatorOut {
                    mod_type: "midi".into(),
                    midi_cc: *cc,
                    midi_pitch_bend: *pitch_bend,
                    midi_aftertouch: *aftertouch,
                    midi_velocity: *velocity,
                    midi_key: *key,
                    hears: hears.clone(),
                    targets,
                    ui,
                    ..Default::default()
                },
                SaveModSource::Macro { value } => ModulatorOut {
                    mod_type: "macro".into(),
                    value: Some(clean_f64(*value)),
                    hears: hears.clone(),
                    targets,
                    ui,
                    ..Default::default()
                },
                SaveModSource::Steps { values, length, rate, glide, retrigger, sync } => ModulatorOut {
                    mod_type: "steps".into(),
                    steps: Some(values.iter().map(|value| clean_f64(*value)).collect()),
                    length: Some(*length),
                    rate: Some(clean_f64(*rate)),
                    glide: Some(clean_f64(*glide)),
                    retrigger: Some(*retrigger),
                    sync: sync.clone(),
                    hears: hears.clone(),
                    targets,
                    ui,
                    ..Default::default()
                },
            }
        })
        .collect()
}

fn effect_to_out(fx: &SaveEffect) -> EffectOut {
    EffectOut {
        plugin: fx.plugin.clone(),
        mix: fx.mix,
        feed: fx.feed,
        preset: fx.preset.clone(),
        pinned: fx.pinned.clone(),
        params: fx.params.iter().map(|(k, v)| (k.clone(), clean_f64(*v))).collect(),
        ui: fx.ui.map(NodeUiOut::from),
    }
}

/// The session as TOML text, which is what `save` writes to the file.
pub fn to_toml(
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    cables: &[SaveCable],
    sources: &SaveSources,
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
    bpm: Option<f32>,
) -> anyhow::Result<String> {
    let session = SessionOut {
        bpm: bpm.map(clean_f64),
        master: sources.master.map(|master| MasterOut { volume: master.volume.map(clean_f64), limiter: master.limiter, ceiling: master.ceiling.map(clean_f64) }),
        keys: sources.keys.as_ref().map(|keys| KeysOut { ui: keys.ui.map(NodeUiOut::from) }),
        devices: sources.devices.iter().map(|device| DeviceOut { name: device.name.clone(), ui: device.ui.map(NodeUiOut::from) }).collect(),
        cables: cables.iter().map(|cable| CableOut { from: cable.from.to_string(), to: cable.to.to_string(), gain: cable.gain }).collect(),
        piano: piano.map(|p| PianoOut { scale: p.scale.clone(), locked: p.locked }),
        ui: master_ui.map(|ui| UiOut { master: ui.into() }),
        modulators: mods_to_out(&sources.modulators),
        modules: sources.modules.iter().map(|module| ModuleOut { name: module.name.clone(), members: module.members.iter().map(ToString::to_string).collect(), ui: module.ui.map(NodeUiOut::from) }).collect(),
        groups: groups
            .iter()
            .map(|g| GroupOut {
                name: g.name.clone(),
                volume: g.volume,
                pan: g.pan,
                mute: g.muted,
                solo: g.solo,
                route: g.route,
                effects: g.effects.iter().map(effect_to_out).collect(),
                ui: g.ui.map(NodeUiOut::from),
            })
            .collect(),
        instruments: instruments
            .iter()
            .map(|slot| {
                let (plugin, volume, preset, params, pitch_bend_range, remap) = match &slot.instrument {
                    Some(inst) => {
                        let params: BTreeMap<String, f64> = inst
                            .params
                            .iter()
                            .map(|(k, v)| (k.clone(), clean_f64(*v)))
                            .collect();
                        let remap: BTreeMap<String, RemapTarget> = inst
                            .remap
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    RemapTarget {
                                        note: v.note.clone(),
                                        detune: clean_f64(v.detune as f32),
                                    },
                                )
                            })
                            .collect();
                        (
                            Some(inst.plugin.clone()),
                            Some(inst.volume),
                            inst.preset.clone(),
                            params,
                            Some(inst.pitch_bend_range),
                            remap,
                        )
                    }
                    None => (None, None, None, BTreeMap::new(), None, BTreeMap::new()),
                };
                InstrumentSlotOut {
                    plugin,
                    volume,
                    preset,
                    pinned: slot.instrument.as_ref().map(|inst| inst.pinned.clone()).unwrap_or_default(),
                    params,
                    pitch_bend_range,
                    remap,
                    range: slot.range.map(format_range),
                    transpose: slot.transpose,
                    group: slot.group,
                    mute: slot.muted,
                    solo: slot.solo,
                    route: slot.route,
                    effects: slot.effects.iter().map(effect_to_out).collect(),
                    pattern: slot.pattern.as_ref().map(|p| {
                        PatternOut {
                            bpm: clean_f64(p.bpm),
                            length_beats: clean_f64(p.length_beats),
                            looping: p.looping,
                            base_note: p.base_note.map(note_name),
                            transpose: p.in_key.then(|| "in_key".to_string()),
                            events: p.events.iter().map(|&(frame, status, note, vel)| {
                                PatternEventOut {
                                    frame,
                                    status: if status == 0x90 { "on".into() } else { "off".into() },
                                    note: note_name(note),
                                    velocity: vel,
                                }
                            }).collect(),
                            enabled: p.enabled,
                        }
                    }),
                    ui: slot.ui.map(NodeUiOut::from),
                }
            })
            .collect(),
    };

    // Not `to_string_pretty`: it writes arrays one element per line.
    let content = toml::to_string(&session)?;
    inline_remap_entries(&content)
}

/// The key of the master mix in the top-level `[ui]` table.
const MASTER_KEY: &str = "master";

/// Rewrite every node's `ui` table as an inline one, `ui = { x = 80, y = 120 }` on the node's own table rather than a
/// table of its own below it, and the master's in the top-level `[ui]` as `master = { x, y }`.
fn inline_ui_tables(table: &mut toml_edit::Table, root: bool) {
    use toml_edit::{InlineTable, Item, Value};

    fn inlined(table: &toml_edit::Table) -> Item {
        let mut inline = InlineTable::new();
        for (key, value) in table.iter() {
            if let Some(value) = value.as_value() {
                inline.insert(key, value.clone());
            }
        }
        inline.fmt();
        Item::Value(Value::InlineTable(inline))
    }

    let keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    for key in keys {
        let Some(item) = table.get_mut(&key) else { continue };
        match item {
            // (The master's position is one level down, in the table of the session's own settings.)
            Item::Table(child) if key == "ui" && root => {
                if let Some(Item::Table(position)) = child.get(MASTER_KEY) {
                    let inline = inlined(position);
                    child.insert(MASTER_KEY, inline);
                    // (A key that was a table's header is not written as `key = value` until it is told to be.)
                    if let Some(mut key) = child.key_mut(MASTER_KEY) {
                        key.fmt();
                    }
                }
            }
            Item::Table(child) if key == "ui" => {
                *item = inlined(child);
                if let Some(mut key) = table.key_mut(&key) {
                    key.fmt();
                }
            }
            // (The keys have a table of their own, and a place in it.)
            Item::Table(child) if key == "keys" && root => inline_ui_tables(child, false),
            // (Every node is an element of an array of tables, the instruments and what is under them.)
            Item::ArrayOfTables(array) => {
                for child in array.iter_mut() {
                    inline_ui_tables(child, false);
                }
            }
            _ => {}
        }
    }
}

/// Where the session given as `given` on the command line is, as a path that keeps pointing there. A
/// relative one would not: plugins that are loaded or scanned can change the working directory of the
/// whole process (some do), and the session would be saved to wherever that led.
pub fn absolute_path(given: &str) -> PathBuf {
    std::path::absolute(given).unwrap_or_else(|_| PathBuf::from(given))
}

/// The names that Windows keeps for devices: a file cannot be called one of them there, with any extension after it.
const WINDOWS_DEVICE_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The most bytes a file name has on the file systems that matter (ext4, APFS, NTFS count 255 of one thing or another).
const MAX_FILE_NAME_BYTES: usize = 255;

/// The file that a session is saved as when it is given the name `given`, if that is a plain file name; or the words for what is wrong with it. The
/// spaces round it go, `.toml` is added unless it ends in it, and what is left is a name and not a place: no `/` or `\` (whichever system it is
/// written for, so a folder cannot be got into a name), none of the characters that a file cannot have on one system or another (`:*?"<>|`, and the
/// controls), nothing before the extension that is not a name, none of the names that Windows keeps for itself, and at most 255 bytes. It is the one
/// rule for every front-end that lets a person name a file: a name that a page sends is a name and not a path (the design, §8), and `dir.join(name)` would
/// let an absolute one replace the directory.
pub fn plain_file_name(given: &str) -> Result<String, String> {
    let name = given.trim();
    if name.contains(['/', '\\']) {
        return Err("a file name has no folder in it: `/` and `\\` are not allowed".to_string());
    }
    if name.chars().any(|c| c.is_control() || ":*?\"<>|".contains(c)) {
        return Err("a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character".to_string());
    }
    let file = if name.ends_with(".toml") { name.to_string() } else { format!("{name}.toml") };
    // (Nothing before the extension, which is what no name at all comes to as well.)
    if file == ".toml" {
        return Err("a file name is needed".to_string());
    }
    let stem = file.split('.').next().unwrap_or_default();
    if WINDOWS_DEVICE_NAMES.contains(&stem.to_ascii_lowercase().as_str()) {
        return Err(format!("{stem} is a name that Windows keeps for itself"));
    }
    if file.len() > MAX_FILE_NAME_BYTES {
        return Err("a file name is at most 255 bytes long".to_string());
    }
    Ok(file)
}

/// Write the session to `path`, creating its directory first (a new session's
/// default home is `~/.config/tang/sessions/`). A file that is there is written over.
#[allow(clippy::too_many_arguments)]
pub fn save(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    cables: &[SaveCable],
    sources: &SaveSources,
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
    bpm: Option<f32>,
) -> anyhow::Result<()> {
    write_session(path, instruments, groups, cables, sources, piano, master_ui, bpm, false)
}

/// As `save`, for a file that is not there yet: the write itself fails, with an `io::Error` of kind `AlreadyExists` in the error, if the path is taken
/// (by a file, a folder or a link, one that leads nowhere included, which it does not follow). Nothing looked at before the write can be out of date by the
/// time of it (finding f128).
#[allow(clippy::too_many_arguments)]
pub fn save_new(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    cables: &[SaveCable],
    sources: &SaveSources,
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
    bpm: Option<f32>,
) -> anyhow::Result<()> {
    write_session(path, instruments, groups, cables, sources, piano, master_ui, bpm, true)
}

#[allow(clippy::too_many_arguments)]
fn write_session(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    cables: &[SaveCable],
    sources: &SaveSources,
    piano: Option<&PianoConfig>,
    master_ui: Option<NodeUi>,
    bpm: Option<f32>,
    create_new: bool,
) -> anyhow::Result<()> {
    let content = to_toml(instruments, groups, cables, sources, piano, master_ui, bpm)?;
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create directory {}: {e}", parent.display()))?;
        }
    }
    if create_new {
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(file) => file,
            // Where the path is a folder, Windows says that access is denied and not, as the other systems do, that the path is taken: whatever is there is taken, whatever the error says.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied && std::fs::symlink_metadata(path).is_ok() => {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, error).into());
            }
            Err(error) => return Err(error.into()),
        };
        std::io::Write::write_all(&mut file, content.as_bytes())?;
    } else {
        std::fs::write(path, content)?;
    }
    Ok(())
}

/// Walk every `[[instrument]]` and rewrite each `remap` child so its entries
/// are inline tables — `"F#2" = { note = "F2", detune = 1.0 }` rather than the
/// default toml-rs block-table form. Non-instrument tables are untouched.
fn inline_remap_entries(input: &str) -> anyhow::Result<String> {
    use toml_edit::{DocumentMut, InlineTable, Item, Value};

    let mut doc: DocumentMut = input.parse()?;
    inline_ui_tables(doc.as_table_mut(), true);
    if let Some(Item::ArrayOfTables(arr)) = doc.get_mut("instrument") {
        for inst in arr.iter_mut() {
            if let Some(Item::Table(remap)) = inst.get_mut("remap") {
                // Drain block-table entries into inline tables paired with their MIDI pitch.
                let keys: Vec<String> = remap.iter().map(|(k, _)| k.to_string()).collect();
                let mut entries: Vec<(u8, String, InlineTable)> = Vec::new();
                for key in keys {
                    let Some(Item::Table(entry)) = remap.remove(&key) else { continue };
                    let mut inline = InlineTable::new();
                    for (k, v) in entry.iter() {
                        if let Some(val) = v.as_value() {
                            inline.insert(k, val.clone());
                        }
                    }
                    inline.fmt();
                    let pitch = parse_note_name(&key).unwrap_or(0);
                    entries.push((pitch, key, inline));
                }
                // Re-insert in pitch order so the file reads musically.
                entries.sort_by_key(|(pitch, _, _)| *pitch);
                for (_, key, inline) in entries {
                    remap.insert(&key, Item::Value(Value::InlineTable(inline)));
                }
            }
        }
    }
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_given_by_a_relative_path_is_known_by_an_absolute_one() {
        let here = std::env::current_dir().unwrap();
        for given in ["jam.toml", "./jam.toml", "sub/jam.toml", "../jam.toml"] {
            let path = absolute_path(given);
            assert!(path.is_absolute(), "{given}: {path:?}");
            assert_eq!(path.file_name(), Path::new(given).file_name(), "{given}: it is the same file");
            // (Windows takes a `..` out of the path where the other systems leave it in, so that `../jam.toml` is a file of the folder above there.)
            let anchor = if given.starts_with("..") { here.parent().unwrap() } else { here.as_path() };
            assert!(path.starts_with(anchor), "{given}: relative to where the program was started: {path:?}");
        }
        assert_eq!(absolute_path("sub/jam.toml"), here.join("sub/jam.toml"));
        // One that is absolute already is left as it is.
        let given = here.join("jam.toml");
        assert_eq!(absolute_path(given.to_str().unwrap()), given);
    }

    /// The words of the refusals of a file name (what `plain_file_name` says), as the page's tests have them.
    const NEEDED: &str = "a file name is needed";
    const FOLDER: &str = "a file name has no folder in it: `/` and `\\` are not allowed";
    const CHARACTERS: &str = "a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character";
    const TOO_LONG: &str = "a file name is at most 255 bytes long";

    #[test]
    fn a_plain_file_name_gets_toml_once_and_loses_the_spaces_round_it() {
        for (given, file) in [
            ("mix", "mix.toml"),
            ("mix.toml", "mix.toml"),
            ("  mix.toml  ", "mix.toml"),
            ("a.b", "a.b.toml"),
            ("my sound", "my sound.toml"),
            (".hidden", ".hidden.toml"),
            ("Ünïcode ♪", "Ünïcode ♪.toml"),
            ("MIX.TOML", "MIX.TOML.toml"),
            ("a..b", "a..b.toml"),
            ("..", "...toml"),
            ("console", "console.toml"),
            ("com0", "com0.toml"),
            ("nul2", "nul2.toml"),
        ] {
            assert_eq!(plain_file_name(given).as_deref(), Ok(file), "{given:?}");
        }
    }

    #[test]
    fn a_name_that_has_nothing_before_the_extension_is_a_name_that_is_needed() {
        for given in ["", "   ", "\t\n", ".toml", "  .toml ", " \u{a0} "] {
            assert_eq!(plain_file_name(given), Err(NEEDED.to_string()), "{given:?}");
        }
    }

    #[test]
    fn a_name_with_a_folder_in_it_is_refused_whatever_system_it_is_written_for() {
        for given in ["a/b", "/etc/passwd", "../x", "..\\x", "C:\\x", "sub/", "/", "a\\b", "./x", "x/../y"] {
            assert_eq!(plain_file_name(given), Err(FOLDER.to_string()), "{given:?}");
        }
    }

    #[test]
    fn a_name_with_a_character_that_a_file_cannot_have_on_every_system_is_refused() {
        for given in ["c:x", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\0b", "a\nb", "a\tb", "a\u{7f}b", "a\u{85}b", "\u{1b}[31m"] {
            assert_eq!(plain_file_name(given), Err(CHARACTERS.to_string()), "{given:?}");
        }
    }

    #[test]
    fn a_name_that_windows_keeps_for_itself_is_refused_whatever_follows_its_first_dot_and_in_any_case() {
        for (given, stem) in [("con", "con"), ("CON", "CON"), ("Nul.txt", "Nul"), ("prn", "prn"), ("aux.toml", "aux"), ("com1", "com1"), ("COM9", "COM9"), ("lpt1", "lpt1"), ("Lpt9.x.y", "Lpt9")] {
            assert_eq!(plain_file_name(given), Err(format!("{stem} is a name that Windows keeps for itself")), "{given:?}");
        }
    }

    #[test]
    fn a_name_is_at_most_as_long_as_a_file_system_takes_with_its_extension_counted() {
        let longest = "a".repeat(250);
        assert_eq!(plain_file_name(&longest), Ok(format!("{longest}.toml")));
        assert_eq!(plain_file_name(&format!("{longest}a")), Err(TOO_LONG.to_string()));
        // (Bytes, not characters: 'é' is two.)
        assert!(plain_file_name(&"é".repeat(125)).is_ok());
        assert_eq!(plain_file_name(&"é".repeat(126)), Err(TOO_LONG.to_string()));
        // The extension is not added twice, so a name that has it is counted with it.
        assert_eq!(plain_file_name(&format!("{longest}.toml")).map(|name| name.len()), Ok(255));
    }

    #[test]
    fn a_plain_file_name_is_one_part_of_a_path_that_is_not_a_prefix_or_a_way_up() {
        use std::path::{Component, Path};
        // What is let through is what `Path` says is one ordinary component: it joins to a directory and stays in it.
        for given in ["mix", "a.b", ".hidden", "..", "a..b", "my sound", "x.toml"] {
            let file = plain_file_name(given).unwrap();
            let parts: Vec<Component> = Path::new(&file).components().collect();
            assert!(matches!(parts.as_slice(), [Component::Normal(_)]), "{given:?} -> {file:?}: {parts:?}");
            assert_eq!(Path::new("/songs").join(&file).parent(), Some(Path::new("/songs")), "{given:?}");
        }
    }

    /// What each of some names comes to, as the web client's mock engine reads it (`web/src/client/fixtures/filenames.json`): the file it is saved as, or
    /// the words for what is wrong with it. They pin the mock's rule to this one. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_file_names_is_current() {
        let mut given: Vec<String> = [
            "mix", "mix.toml", "  mix.toml  ", "a.b", "my sound", ".hidden", "Ünïcode ♪", "MIX.TOML", "a..b", "..", "console", "com0", "nul2", "", "   ", "\t\n", ".toml", "  .toml ", "a/b", "/etc/passwd", "../x",
            "..\\x", "C:\\x", "sub/", "/", "a\\b", "./x", "x/../y", "c:x", "a*b", "a?b", "a\"b", "a<b", "a>b", "a|b", "a\0b", "a\nb", "a\tb", "a\u{7f}b", "a\u{85}b", "con", "CON", "Nul.txt", "prn", "aux.toml", "com1",
            "COM9", "lpt1", "Lpt9.x.y", "\u{85}x", "\u{feff}x", " \u{3000}x\u{2003} ", "x\u{a0}", "toml", "atoml",
            // (The ends of the controls, which the page's rule has to write out: the last of the first block and the ends of the second, and what is next to each.)
            "a\u{1f}b", "a\u{20}b", "a\u{7e}b", "a\u{80}b", "a\u{9e}b", "a\u{9f}b", "a\u{a0}b",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();
        given.extend([250, 251].map(|count| "a".repeat(count)));
        given.extend([125, 126].map(|count| "é".repeat(count)));
        given.push(format!("{}.toml", "a".repeat(250)));
        let names: Vec<serde_json::Value> = given
            .iter()
            .map(|given| match plain_file_name(given) {
                Ok(file) => serde_json::json!({ "given": given, "file": file }),
                Err(error) => serde_json::json!({ "given": given, "error": error }),
            })
            .collect();
        let fresh = serde_json::to_string_pretty(&serde_json::json!({ "names": names })).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/filenames.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what a file name comes to now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn a_number_that_is_not_a_number_is_refused_wherever_it_is_and_the_error_says_where() {
        // TOML has `nan` and `inf`. As a volume, a rate or a depth they would be in the
        // model and in the audio thread, and a snapshot with a NaN in it is not even JSON.
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for (text, place) in [
            (format!("{osc}volume = nan\n"), "instrument[0].volume"),
            (format!("bpm = inf\n{osc}"), "bpm"),
            (format!("{osc}[instrument.params]\ncutoff = -inf\n"), "instrument[0].params.cutoff"),
            (format!("{osc}[[instrument.effect]]\nplugin = \"builtin:reverb\"\nmix = nan\n"), "instrument[0].effect[0].mix"),
            (format!("{osc}[[instrument.modulator]]\nrate = nan\n"), "instrument[0].modulator[0].rate"),
            (format!("[[group]]\npan = nan\n{osc}"), "group[0].pan"),
            (format!("{osc}[instrument.pattern]\nbpm = nan\n"), "instrument[0].pattern.bpm"),
            (format!("{osc}[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\ngain = nan\n"), "cable[0].gain"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert!(error.contains(place) && error.contains("is not a finite number"), "{error}\nfor:\n{text}");
        }
    }

    #[test]
    fn a_number_too_big_for_the_audio_thread_is_refused_like_one_that_is_not_a_number() {
        // TOML's floats are 64 bits and the model's are 32: 1e39 is finite to the one and `inf` to the other, and
        // it would be in the model as that, and in a snapshot as `null`.
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for (text, place) in [
            (format!("{osc}volume = 1e39\n"), "instrument[0].volume"),
            (format!("{osc}[[instrument.modulator]]\nrate = 1e39\n"), "instrument[0].modulator[0].rate"),
            (format!("{osc}[[instrument.modulator]]\n[[instrument.modulator.target]]\nparam = \"detune\"\ndepth = -1e39\n"), "instrument[0].modulator[0].target[0].depth"),
            (format!("[[group]]\nvolume = 4e38\n{osc}"), "group[0].volume"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert!(error.contains(place) && error.contains("32-bit"), "{error}\nfor:\n{text}");
        }
        // The largest the audio thread holds is as good as any other number, and so is a very small one.
        assert!(parse(&format!("{osc}volume = 3.4e38\n")).is_ok());
        assert!(parse(&format!("{osc}volume = 1e-50\n")).is_ok());
    }

    #[test]
    fn an_lfo_with_a_rate_below_zero_is_refused_wherever_it_is_and_the_error_says_where() {
        // A rate below 0 counts the phase down from 0 towards -1, where the shapes go out of the -1 to 1 that a cable's law
        // assumes (a saw from -3 to -1). The knob stops at 0.01 and the controller holds to it: a hand-written file is told.
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let reverb = "[[instrument.effect]]\nplugin = \"builtin:reverb\"\n";
        for (text, place, rate) in [
            (format!("{osc}[[instrument.modulator]]\nrate = -1.0\n"), "instrument[0].modulator[0].rate", "-1"),
            (format!("{osc}[[instrument.modulator]]\ntype = \"lfo\"\nwaveform = \"saw\"\nrate = -0.5\n"), "instrument[0].modulator[0].rate", "-0.5"),
            (format!("{osc}[[instrument.modulator]]\n[[instrument.modulator]]\nrate = -1e-3\n"), "instrument[0].modulator[1].rate", "-0.001"),
            (format!("{osc}{osc}[[instrument.modulator]]\nrate = -2.0\n"), "instrument[1].modulator[0].rate", "-2"),
            // (The loader makes an LFO of a type it does not know, too.)
            (format!("{osc}[[instrument.modulator]]\ntype = \"wobble\"\nrate = -2.0\n"), "instrument[0].modulator[0].rate", "-2"),
            (format!("[[group]]\n[[group.modulator]]\nrate = -2.5\n{osc}"), "group[0].modulator[0].rate", "-2.5"),
            (format!("[[group]]\n[[group]]\n[[group.modulator]]\n[[group.modulator]]\nrate = -4.0\n{osc}"), "group[1].modulator[1].rate", "-4"),
            // The free modulators, which are the file's own (no lane and no group in their place).
            (format!("[[modulator]]\nrate = -1.5\n{osc}"), "modulator[0].rate", "-1.5"),
            (format!("[[modulator]]\n[[modulator]]\nwaveform = \"square\"\nrate = -2.0\n{osc}"), "modulator[1].rate", "-2"),
            // Older sessions nested the modulators of an effect in it, and the loader moves them into the lane.
            (format!("{osc}{reverb}[[instrument.effect.modulator]]\nrate = -3.0\n"), "instrument[0].effect[0].modulator[0].rate", "-3"),
            (format!("{osc}{reverb}{reverb}[[instrument.effect.modulator]]\n[[instrument.effect.modulator]]\nrate = -3.5\n"), "instrument[0].effect[1].modulator[1].rate", "-3.5"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert_eq!(error, format!("{place} is below 0 ({rate}): an LFO cannot go round backwards"), "for:\n{text}");
        }
    }

    #[test]
    fn the_free_modulators_of_a_file_are_read_with_the_places_their_cables_name_and_a_session_with_none_has_none() {
        let config = parse(
            "[[modulator]]\ntype = \"envelope\"\n[[modulator.target]]\nto = \"instrument[0].effect[1]\"\nparam = \"cutoff\"\ndepth = 0.25\n[[modulator.target]]\nmod_attack = 1\n[[modulator]]\nrate = 2.0\nui = { x = 3, y = 4 }\n[[instrument]]\nplugin = \"builtin:osc\"\n",
        )
        .unwrap();
        assert_eq!(config.free_modulators.len(), 2);
        assert_eq!(config.free_modulators[0].mod_type, "envelope");
        assert_eq!(config.free_modulators[0].targets[0].to.as_deref(), Some("instrument[0].effect[1]"));
        assert_eq!(config.free_modulators[0].targets[0].param.as_deref(), Some("cutoff"));
        assert_eq!(config.free_modulators[0].targets[1].mod_attack, Some(1));
        assert_eq!(config.free_modulators[1].rate, Some(2.0));
        assert_eq!(config.free_modulators[1].ui, Some(NodeUi { x: 3.0, y: 4.0 }));
        assert!(config.instruments[0].modulators.is_empty(), "the lane has none of them");
        assert!(parse("[[instrument]]\nplugin = \"builtin:osc\"\n").unwrap().free_modulators.is_empty());
    }

    #[test]
    fn a_step_sequencer_with_a_rate_below_zero_is_refused_wherever_it_is_and_the_error_says_where() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for (text, place, rate) in [
            (format!("{osc}[[instrument.modulator]]\ntype = \"steps\"\nrate = -1.0\n"), "instrument[0].modulator[0].rate", "-1"),
            (format!("{osc}[[instrument.modulator]]\n[[instrument.modulator]]\ntype = \"steps\"\nrate = -0.25\n"), "instrument[0].modulator[1].rate", "-0.25"),
            (format!("[[group]]\n[[group.modulator]]\ntype = \"steps\"\nrate = -2.5\n{osc}"), "group[0].modulator[0].rate", "-2.5"),
            (format!("[[modulator]]\ntype = \"steps\"\nrate = -3.0\n{osc}"), "modulator[0].rate", "-3"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert_eq!(error, format!("{place} is below 0 ({rate}): a step sequencer cannot go round backwards"), "for:\n{text}");
        }
    }

    #[test]
    fn a_step_sequencer_with_no_steps_or_with_more_than_there_are_is_refused_and_the_error_says_where() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let seventeen = vec!["0.5"; 17].join(", ");
        for (text, place, what) in [
            (format!("{osc}[[instrument.modulator]]\ntype = \"steps\"\nsteps = []\n"), "instrument[0].modulator[0].steps", "has no values: a step sequencer has 1 to 16 steps"),
            (format!("{osc}[[instrument.modulator]]\ntype = \"steps\"\nsteps = [{seventeen}]\n"), "instrument[0].modulator[0].steps", "has 17 values: a step sequencer has 16 steps at most"),
            (format!("[[group]]\n[[group.modulator]]\ntype = \"steps\"\nsteps = []\n{osc}"), "group[0].modulator[0].steps", "has no values: a step sequencer has 1 to 16 steps"),
            (format!("[[modulator]]\ntype = \"steps\"\nsteps = []\n{osc}"), "modulator[0].steps", "has no values: a step sequencer has 1 to 16 steps"),
            (format!("[[modulator]]\n[[modulator]]\ntype = \"steps\"\nsteps = [{seventeen}]\n{osc}"), "modulator[1].steps", "has 17 values: a step sequencer has 16 steps at most"),
        ] {
            let error = parse(&text).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert_eq!(error, format!("{place} {what}"), "for:\n{text}");
        }
        // Sixteen is as many as there are, and one is as few.
        let sixteen = vec!["0.5"; 16].join(", ");
        for list in [sixteen.as_str(), "-1.0"] {
            let text = format!("{osc}[[instrument.modulator]]\ntype = \"steps\"\nsteps = [{list}]\n");
            assert!(parse(&text).is_ok(), "refused:\n{text}");
        }
    }

    #[test]
    fn a_step_sequencer_is_read_with_what_the_file_says_and_what_it_leaves_out_is_left_to_the_loader() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let config = parse_raw(&format!("{osc}[[instrument.modulator]]\ntype = \"steps\"\nsteps = [0.5, -0.25, 1.0]\nlength = 2\nrate = 4.5\nglide = 0.25\nretrigger = false\n")).unwrap();
        let m = &config.instruments[0].modulators[0];
        assert_eq!(m.mod_type, "steps");
        assert_eq!((m.steps.as_deref(), m.length, m.rate, m.glide, m.retrigger), (Some(&[0.5, -0.25, 1.0][..]), Some(2), Some(4.5), 0.25, Some(false)));
        // None of them: no values, no length, no rate (an LFO's is 1 Hz and its is 8), no glide, and a note starts it again.
        let config = parse_raw(&format!("{osc}[[instrument.modulator]]\ntype = \"steps\"\n")).unwrap();
        let m = &config.instruments[0].modulators[0];
        // (A switch that is not there is left to the loader, which says what it is by the kind of modulator: a note starts a step sequencer again and does not start an LFO.)
        assert_eq!((m.steps.as_deref(), m.length, m.rate, m.glide, m.retrigger), (None, None, None, 0.0, None));
        // A step sequencer is not an LFO, whatever the loader makes of a type it does not know.
        assert!(!m.is_lfo());
    }

    #[test]
    fn an_lfo_is_read_with_its_key_and_what_the_file_leaves_out_is_left_to_the_loader() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let config = parse_raw(&format!("{osc}[[instrument.modulator]]\ntype = \"lfo\"\nrate = 2.0\nretrigger = true\nfade = 0.5\n")).unwrap();
        let m = &config.instruments[0].modulators[0];
        assert_eq!((m.mod_type.as_str(), m.retrigger, m.fade), ("lfo", Some(true), 0.5));
        let config = parse_raw(&format!("{osc}[[instrument.modulator]]\n")).unwrap();
        let m = &config.instruments[0].modulators[0];
        assert_eq!((m.mod_type.as_str(), m.retrigger, m.fade), ("lfo", None, 0.0), "none of it: the loader makes it an LFO that runs free");
    }

    #[test]
    fn an_lfo_that_is_keyed_is_written_with_its_key_and_read_back_the_same_and_one_that_is_free_has_neither_key() {
        let keyed = SaveModulator { source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0, retrigger: true, fade: 0.3, sync: None }, targets: vec![], ui: None, hears: None };
        let (text, config) = saved_with(vec![keyed]);
        for line in ["retrigger = true", "fade = 0.3"] {
            assert!(text.contains(line), "{line} (as typed, and not what its 32 bits are): {text}");
        }
        let m = &config.free_modulators[0];
        assert_eq!((m.mod_type.as_str(), m.retrigger, m.fade), ("lfo", Some(true), 0.3));
        // One that retriggers and does not fade has the one key, and the other way about.
        let only = |retrigger: bool, fade: f32| {
            let lfo = SaveModulator { source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0, retrigger, fade, sync: None }, targets: vec![], ui: None, hears: None };
            saved_with(vec![lfo]).0
        };
        let (a, b) = (only(true, 0.0), only(false, 2.0));
        assert!(a.contains("retrigger = true") && !a.contains("fade"), "{a}");
        assert!(b.contains("fade = 2.0") && !b.contains("retrigger"), "{b}");
        // A free one has neither.
        let text = only(false, 0.0);
        assert!(!text.contains("retrigger") && !text.contains("fade"), "{text}");
    }

    #[test]
    fn an_lfo_and_a_step_sequencer_are_read_with_the_note_length_they_follow_the_tempo_by_and_none_if_the_file_says_none() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let config = parse_raw(&format!(
            "{osc}[[instrument.modulator]]\nsync = \"1/8 triplet\"\nrate = 2.0\n[[instrument.modulator]]\ntype = \"steps\"\nsync = \"1/16\"\n[[instrument.modulator]]\ntype = \"lfo\"\nsync = \"free\"\n[[instrument.modulator]]\n"
        ))
        .unwrap();
        let synced = |i: usize| config.instruments[0].modulators[i].sync.as_deref();
        assert_eq!([synced(0), synced(1), synced(2), synced(3)], [Some("1/8 triplet"), Some("1/16"), Some("free"), None]);
        // (The rate that goes with it is kept.)
        assert_eq!(config.instruments[0].modulators[0].rate, Some(2.0));
    }

    #[test]
    fn a_modulator_that_follows_the_tempo_is_written_with_the_name_of_its_note_length_and_read_back_the_same_and_one_that_is_free_has_no_sync_key() {
        let lfo = |sync: Option<&str>| SaveModulator {
            source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0, retrigger: false, fade: 0.0, sync: sync.map(str::to_string) },
            targets: vec![],
            ui: None,
            hears: None,
        };
        let steps = |sync: Option<&str>| SaveModulator {
            source: SaveModSource::Steps { values: vec![0.5; 4], length: 4, rate: 8.0, glide: 0.0, retrigger: true, sync: sync.map(str::to_string) },
            targets: vec![],
            ui: None,
            hears: None,
        };
        let (text, config) = saved_with(vec![lfo(Some("1/8 triplet")), steps(Some("1/16")), lfo(None), steps(None)]);
        assert_eq!(text.matches("sync = ").count(), 2, "only the two that follow the tempo: {text}");
        for line in ["sync = \"1/8 triplet\"", "sync = \"1/16\""] {
            assert!(text.contains(line), "{line}: {text}");
        }
        let synced = |i: usize| config.free_modulators[i].sync.as_deref();
        assert_eq!([synced(0), synced(1), synced(2), synced(3)], [Some("1/8 triplet"), Some("1/16"), None, None]);
    }

    #[test]
    fn a_rate_that_is_no_rate_below_zero_is_not_refused() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        // Zero is an LFO that stands still, 100 Hz is above the knob's end and may be wanted, and a rate on a modulator
        // that is not an LFO is not read.
        for modulator in [
            "rate = 0.0",
            "rate = -0.0",
            "rate = 100.0",
            "type = \"envelope\"\nrate = -1.0",
            "type = \"midi\"\nrate = -1.0",
            "type = \"macro\"\nrate = -1.0",
        ] {
            let text = format!("{osc}[[instrument.modulator]]\n{modulator}\n");
            assert!(parse(&text).is_ok(), "refused:\n{text}");
        }
    }

    #[test]
    fn a_step_sequencer_is_written_with_all_that_it_has_and_read_back_the_same_and_the_others_have_none_of_it() {
        let mut values: Vec<f32> = (0..16).map(|i| i as f32 / 8.0 - 1.0).collect();
        values[3] = 0.3;
        let steps = SaveModulator { source: SaveModSource::Steps { values: values.clone(), length: 5, rate: 6.5, glide: 0.25, retrigger: false, sync: None }, targets: vec![], ui: None, hears: None };
        let (text, config) = saved_with(vec![steps]);
        assert!(text.contains("type = \"steps\""), "{text}");
        assert!(text.contains("steps = [-1.0, -0.875, -0.75, 0.3, -0.5, -0.375, -0.25, -0.125, 0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875]"), "every value, as typed: {text}");
        for line in ["length = 5", "rate = 6.5", "glide = 0.25", "retrigger = false"] {
            assert!(text.contains(line), "{line}: {text}");
        }
        let m = &config.free_modulators[0];
        assert_eq!(m.mod_type, "steps");
        // (A value is written as it was typed: 0.3, and not the 0.30000001192092896 that its 32 bits are.)
        assert_eq!(m.steps.as_deref(), Some(&values.iter().map(|v| clean_f64(*v)).collect::<Vec<_>>()[..]));
        assert_eq!(m.steps.as_ref().unwrap()[3], 0.3);
        assert_eq!((m.length, m.rate, m.glide, m.retrigger), (Some(5), Some(6.5), 0.25, Some(false)));
        // The rest of the modulators have none of it (an LFO that is free has neither of its key's keys, so that a file that was written before it had a key is as it was).
        let lfo = SaveModulator { source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0, retrigger: false, fade: 0.0, sync: None }, targets: vec![], ui: None, hears: None };
        let envelope = SaveModulator { source: SaveModSource::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 }, targets: vec![], ui: None, hears: None };
        let knob = SaveModulator { source: SaveModSource::Macro { value: 0.5 }, targets: vec![], ui: None, hears: None };
        let (text, _) = saved_with(vec![lfo, envelope, knob]);
        for key in ["steps", "length", "glide", "retrigger"] {
            assert!(!text.contains(key), "{key}: {text}");
        }
    }

    #[test]
    fn mute_and_solo_are_read_from_a_lane_and_a_group_and_are_off_when_a_file_has_none() {
        let config = parse(
            "[[group]]\nmute = true\n[[group]]\nsolo = true\n[[group]]\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nmute = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nsolo = true\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n",
        )
        .unwrap();
        assert_eq!(config.instruments.iter().map(|lane| (lane.muted, lane.solo)).collect::<Vec<_>>(), [(true, false), (false, true), (false, false)]);
        assert_eq!(config.groups.iter().map(|group| (group.muted, group.solo)).collect::<Vec<_>>(), [(true, false), (false, true), (false, false)]);
    }

    #[test]
    fn mute_and_solo_are_written_by_to_toml_only_when_on() {
        let lane = |muted: bool, solo: bool| SaveInstrumentSlot { muted, solo, ..placed_lane(None) };
        let group = |muted: bool, solo: bool| SaveGroup { name: None, volume: 1.0, pan: 0.0, muted, solo, effects: vec![], ui: None, route: Route::Out };
        let text = to_toml(&[lane(true, false), lane(false, true), lane(true, true), lane(false, false)], &[group(true, false), group(false, true), group(false, false)], &[], &Default::default(), None, None, None).unwrap();
        let config = parse(&text).unwrap();
        assert_eq!(config.instruments.iter().map(|l| (l.muted, l.solo)).collect::<Vec<_>>(), [(true, false), (false, true), (true, true), (false, false)]);
        assert_eq!(config.groups.iter().map(|g| (g.muted, g.solo)).collect::<Vec<_>>(), [(true, false), (false, true), (false, false)]);
        assert_eq!(text.matches("mute = true").count(), 3, "{text}");
        assert_eq!(text.matches("solo = true").count(), 3, "{text}");
        assert!(!text.contains("false"), "nothing that is off is written: {text}");
    }

    #[test]
    fn a_route_is_read_from_a_lane_and_a_group_and_is_out_when_a_file_has_none() {
        let config = parse(
            "[[group]]\nroute = \"none\"\n[[group]]\nroute = \"out\"\n[[group]]\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nroute = \"none\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\nroute = \"out\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n",
        )
        .unwrap();
        assert_eq!(config.instruments.iter().map(|lane| lane.route).collect::<Vec<_>>(), [Route::None, Route::Out, Route::Out]);
        assert_eq!(config.groups.iter().map(|group| group.route).collect::<Vec<_>>(), [Route::None, Route::Out, Route::Out]);
    }

    #[test]
    fn a_route_that_is_not_one_refuses_the_session_and_says_where() {
        // (A lane that goes nowhere is silent, so a word that is nearly "none" is not read as "out": the session is not loaded.)
        for (text, place) in [
            ("[[instrument]]\nplugin = \"builtin:osc\"\nroute = \"nowhere\"\n", "instrument[0].route"),
            ("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\nroute = \"None\"\n", "instrument[1].route"),
            ("[[group]]\n[[group]]\nroute = \"mix\"\n[[instrument]]\nplugin = \"builtin:osc\"\n", "group[1].route"),
        ] {
            let error = parse(text).err().unwrap_or_else(|| panic!("{text} loads")).to_string();
            assert!(error.contains(place) && error.contains("\"out\" or \"none\""), "{error}");
        }
        // A route that is not a word at all is the parser's to refuse.
        assert!(parse("[[instrument]]\nplugin = \"builtin:osc\"\nroute = true\n").is_err());
    }

    #[test]
    fn a_route_is_written_by_to_toml_only_when_it_is_none() {
        let lane = |route: Route| SaveInstrumentSlot { route, ..placed_lane(None) };
        let group = |route: Route| SaveGroup { name: None, volume: 1.0, pan: 0.0, muted: false, solo: false, route, effects: vec![], ui: None };
        let text = to_toml(&[lane(Route::None), lane(Route::Out)], &[group(Route::Out), group(Route::None)], &[], &Default::default(), None, None, None).unwrap();
        let config = parse(&text).unwrap();
        assert_eq!(config.instruments.iter().map(|l| l.route).collect::<Vec<_>>(), [Route::None, Route::Out]);
        assert_eq!(config.groups.iter().map(|g| g.route).collect::<Vec<_>>(), [Route::Out, Route::None]);
        assert_eq!(text.matches("route = \"none\"").count(), 2, "{text}");
        assert_eq!(text.matches("route").count(), 2, "nothing that is the way it always was is written: {text}");
    }

    #[test]
    fn a_route_is_named_as_the_file_names_it() {
        assert_eq!((Route::Out.name(), Route::None.name()), ("out", "none"));
        assert_eq!((Route::parse("out"), Route::parse("none"), Route::parse("None"), Route::parse("")), (Some(Route::Out), Some(Route::None), None, None));
    }

    #[test]
    fn a_feed_is_read_from_the_effects_of_a_lane_and_of_a_bus_and_is_chain_when_a_file_has_none() {
        let config = parse(
            "[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\nfeed = \"none\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\nfeed = \"chain\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\n\
             [[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n",
        )
        .unwrap();
        assert_eq!(config.groups[0].effects.iter().map(|effect| effect.feed).collect::<Vec<_>>(), [Feed::None, Feed::Chain, Feed::Chain]);
        assert_eq!(config.instruments[0].effects.iter().map(|effect| effect.feed).collect::<Vec<_>>(), [Feed::Chain, Feed::None]);
    }

    #[test]
    fn a_feed_that_is_not_one_refuses_the_session_and_says_where() {
        // (An effect that is fed by nothing is silent, so a word that is nearly "none" is not read as "chain": the session is not loaded.)
        for (text, place) in [
            ("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"off\"\n", "instrument[0].effect[0].feed"),
            (
                "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"None\"\n",
                "instrument[1].effect[1].feed",
            ),
            ("[[group]]\n[[group]]\n[[group.effect]]\nplugin = \"builtin:reverb\"\nfeed = \"cut\"\n[[instrument]]\nplugin = \"builtin:osc\"\n", "group[1].effect[0].feed"),
        ] {
            let error = parse(text).err().unwrap_or_else(|| panic!("{text} loads")).to_string();
            assert!(error.contains(place) && error.contains("\"chain\" or \"none\""), "{error}");
        }
        // A feed that is not a word at all is the parser's to refuse.
        assert!(parse("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = 1\n").is_err());
    }

    #[test]
    fn a_feed_is_written_by_to_toml_only_when_it_is_none_and_read_back_on_the_effect_it_belongs_to() {
        let mut lane = placed_lane(None);
        lane.effects = vec![SaveEffect { feed: Feed::None, ..bare_effect("builtin:filter") }, bare_effect("builtin:delay"), SaveEffect { feed: Feed::None, ..bare_effect("builtin:drive") }];
        let group = SaveGroup {
            name: None,
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            route: Route::Out,
            effects: vec![bare_effect("builtin:reverb"), SaveEffect { feed: Feed::None, ..bare_effect("builtin:filter") }],
            ui: None,
        };
        let text = to_toml(&[lane], &[group], &[], &Default::default(), None, None, None).unwrap();
        let config = parse(&text).unwrap();
        assert_eq!(config.instruments[0].effects.iter().map(|effect| effect.feed).collect::<Vec<_>>(), [Feed::None, Feed::Chain, Feed::None], "{text}");
        assert_eq!(config.groups[0].effects.iter().map(|effect| effect.feed).collect::<Vec<_>>(), [Feed::Chain, Feed::None]);
        assert_eq!(text.matches("feed = \"none\"").count(), 3, "{text}");
        assert_eq!(text.matches("feed").count(), 3, "nothing that is the way a chain always was is written: {text}");
    }

    #[test]
    fn a_feed_is_named_as_the_file_names_it() {
        assert_eq!((Feed::Chain.name(), Feed::None.name()), ("chain", "none"));
        assert_eq!((Feed::parse("chain"), Feed::parse("none"), Feed::parse("None"), Feed::parse("")), (Some(Feed::Chain), Some(Feed::None), None, None));
        assert_eq!((Feed::default(), Feed::Chain.is_fed(), Feed::None.is_fed()), (Feed::Chain, true, false));
    }

    #[test]
    fn a_word_that_looks_like_a_number_that_is_not_one_is_only_a_word() {
        let config = parse("[[instrument]]\nplugin = \"builtin:osc\"\npreset = \"nan\"\nvolume = 1e2\n").unwrap();
        assert_eq!(config.instruments[0].instrument.as_ref().map(|i| (i.preset.as_deref(), i.volume)), Some((Some("nan"), 100.0)));
    }

    #[test]
    fn parse_range_valid() {
        assert_eq!(parse_range("C0-B3").unwrap(), (12, 59));
    }

    #[test]
    fn parse_range_single_octave() {
        assert_eq!(parse_range("C4-B4").unwrap(), (60, 71));
    }

    #[test]
    fn parse_range_same_note() {
        assert_eq!(parse_range("C4-C4").unwrap(), (60, 60));
    }

    #[test]
    fn parse_range_with_accidentals() {
        assert_eq!(parse_range("C#2-Bb5").unwrap(), (37, 82));
    }

    #[test]
    fn parse_range_invalid_low_gt_high() {
        assert!(parse_range("C4-C3").is_err());
    }

    #[test]
    fn parse_range_open_high() {
        // "C4-" means C4 and up, to the top of the MIDI range.
        assert_eq!(parse_range("C4-").unwrap(), (60, 127));
    }

    #[test]
    fn parse_range_open_low() {
        // "-B3" means everything up to and including B3.
        assert_eq!(parse_range("-B3").unwrap(), (0, 59));
    }

    #[test]
    fn parse_range_invalid_format() {
        assert!(parse_range("C4").is_err());
        assert!(parse_range("C4-B3-C5").is_err());
        // Both bounds omitted is not a range.
        assert!(parse_range("-").is_err());
    }

    #[test]
    fn parse_range_negative_octave() {
        // A '-' that is part of an octave -1 note name is not the separator.
        assert_eq!(parse_range("C-1-B3").unwrap(), (0, 59));
        assert_eq!(parse_range("D-1-B-1").unwrap(), (2, 11));
        assert_eq!(parse_range("B-1-").unwrap(), (11, 127));
        assert_eq!(parse_range("-C-1").unwrap(), (0, 0));
    }

    #[test]
    fn format_range_round_trips() {
        // Open-ended forms are preserved on save.
        assert_eq!(format_range((0, 59)), "-B3");
        assert_eq!(format_range((60, 127)), "C4-");
        assert_eq!(format_range((12, 59)), "C0-B3");
        // Bounds in octave -1 survive a save/load cycle.
        for range in [(0, 59), (2, 11), (60, 127), (0, 127), (12, 59)] {
            assert_eq!(parse_range(&format_range(range)).unwrap(), range);
        }
    }

    #[test]
    fn load_new_format() {
        let toml = r#"
[[instrument]]
plugin = "builtin:sine"
range = "C0-B3"

[[instrument]]
plugin = "builtin:sine"
range = "C4-C8"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        std::fs::write(&path, toml).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 2);
        assert_eq!(config.instruments[0].range, Some((12, 59)));
        assert_eq!(config.instruments[1].range, Some((60, 108)));
    }

    #[test]
    fn save_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.toml");

        let instruments = vec![
            SaveInstrumentSlot {
                muted: false,
                solo: false,
                range: Some((12, 59)), // C0-B3
                transpose: 0,
                group: None,
                instrument: Some(SaveInstrument {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    volume: 0.8,
                    preset: None,
                    params: vec![("cutoff".into(), 0.75)],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![SaveEffect {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    mix: 0.5,
                    feed: Feed::Chain,
                    preset: None,
                    params: vec![],
                    ui: None,
                }],
                pattern: None,
                ui: None,
                route: Route::Out,
            },
            SaveInstrumentSlot {
                muted: false,
                solo: false,
                range: None,
                transpose: 0,
                group: None,
                instrument: Some(SaveInstrument {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    volume: 1.0,
                    preset: None,
                    params: vec![],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![],
                pattern: None,
                ui: None,
                route: Route::Out,
            },
        ];

        save(&path, &instruments, &[], &[], &Default::default(), None, None, None).unwrap();

        // Reload and verify
        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 2);
        assert_eq!(config.instruments[0].range, Some((12, 59)));
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.plugin, "builtin:sine");
        assert!((inst.volume - 0.8).abs() < 0.01);
        assert_eq!(config.instruments[0].effects.len(), 1);
        assert!((config.instruments[0].effects[0].mix - 0.5).abs() < 0.01);
        assert!(config.instruments[1].range.is_none());
    }

    #[test]
    fn save_and_reload_with_preset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preset_test.toml");

        let instruments = vec![SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: Some("Lead".into()),
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![SaveEffect {
                pinned: vec![],
                plugin: "builtin:reverb".into(),
                mix: 0.4,
                feed: Feed::Chain,
                preset: Some("Arcadia Dream Hall".into()),
                params: vec![],
                ui: None,
            }],
            pattern: None,
            ui: None,
            route: Route::Out,
        }];

        save(&path, &instruments, &[], &[], &Default::default(), None, None, None).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 1);
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.preset.as_deref(), Some("Lead"));
        assert_eq!(config.instruments[0].effects.len(), 1);
        assert_eq!(
            config.instruments[0].effects[0].preset.as_deref(),
            Some("Arcadia Dream Hall")
        );
    }

    #[test]
    fn save_and_reload_with_modulators() {
        let lfo = SaveModulator {
            source: SaveModSource::Lfo { waveform: "sine".into(), rate: 2.5, retrigger: false, fade: 0.0, sync: None },
            targets: vec![cutoff_cable(0.75)],
            ui: None,
            hears: Some(CablePlace::Instrument(0)),
        };
        let (_, config) = saved_with(vec![lfo]);
        assert!(config.instruments[0].modulators.is_empty(), "none is written in a lane");
        assert_eq!(config.free_modulators.len(), 1);
        let m = &config.free_modulators[0];
        assert_eq!(m.waveform, "sine");
        assert!((m.rate.unwrap() - 2.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
        assert!((m.targets[0].depth - 0.75).abs() < 0.01);
        // The plugin it drives is named by its place, and it hears the lane.
        assert_eq!(m.targets[0].to.as_deref(), Some("instrument[0].effect[0]"));
        assert_eq!(m.hears.as_deref(), Some("instrument[0]"));
    }

    #[test]
    fn save_and_reload_midi_learn_modulator() {
        // A MIDI-learned modulator (bound to CC 74) must round-trip as
        // `type = "midi"` + `midi_cc = 74`.
        let control = SaveModulator {
            source: SaveModSource::MidiLearn { cc: Some(74), pitch_bend: false, aftertouch: false, velocity: false, key: false },
            targets: vec![cutoff_cable(0.5)],
            ui: None,
            hears: None,
        };
        let (raw, config) = saved_with(vec![control]);
        assert!(raw.contains("type = \"midi\""), "expected midi modulator:\n{raw}");
        assert!(raw.contains("midi_cc = 74"), "expected bound CC:\n{raw}");

        assert_eq!(config.free_modulators.len(), 1);
        let m = &config.free_modulators[0];
        assert_eq!(m.mod_type, "midi");
        assert_eq!(m.midi_cc, Some(74));
        assert!(!m.midi_pitch_bend);
        assert!(!m.midi_aftertouch);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
    }

    #[test]
    fn save_and_reload_aftertouch_modulator() {
        // A MIDI-learned modulator bound to key pressure must round-trip as
        // `type = "midi"` + `midi_aftertouch = true`.
        let control = SaveModulator {
            source: SaveModSource::MidiLearn { cc: None, pitch_bend: false, aftertouch: true, velocity: false, key: false },
            targets: vec![cutoff_cable(0.5)],
            ui: None,
            hears: None,
        };
        let (raw, config) = saved_with(vec![control]);
        assert!(raw.contains("midi_aftertouch = true"), "expected aftertouch binding:\n{raw}");

        let m = &config.free_modulators[0];
        assert_eq!(m.mod_type, "midi");
        assert!(m.midi_aftertouch);
        assert!(m.midi_cc.is_none());
        assert!(!m.midi_pitch_bend);
    }

    #[test]
    fn save_and_reload_velocity_and_key_modulators() {
        // A control that follows the notes round-trips as `type = "midi"` + `midi_velocity = true` or `midi_key = true`, and only one of them is written for each.
        let modulator = |velocity, key| SaveModulator {
            source: SaveModSource::MidiLearn { cc: None, pitch_bend: false, aftertouch: false, velocity, key },
            targets: vec![],
            ui: None,
            hears: None,
        };
        let (raw, config) = saved_with(vec![modulator(true, false), modulator(false, true)]);
        assert_eq!(raw.matches("midi_velocity = true").count(), 1, "{raw}");
        assert_eq!(raw.matches("midi_key = true").count(), 1, "{raw}");
        assert!(!raw.contains("midi_cc") && !raw.contains("midi_pitch_bend") && !raw.contains("midi_aftertouch"), "{raw}");

        let [first, second] = &config.free_modulators[..] else { panic!("two modulators") };
        assert_eq!((first.mod_type.as_str(), first.midi_velocity, first.midi_key), ("midi", true, false));
        assert_eq!((second.mod_type.as_str(), second.midi_velocity, second.midi_key), ("midi", false, true));
        assert!(first.midi_cc.is_none() && !first.midi_pitch_bend && !first.midi_aftertouch);
    }

    #[test]
    fn save_and_reload_group() {
        // A group with a bus effect + volume, and two member instruments,
        // must round-trip (membership index + group fields).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("group.toml");

        let member = |group: Option<usize>| SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group,
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
            route: Route::Out,
        };
        let instruments = vec![member(Some(0)), member(Some(0)), member(None)];
        let groups = vec![SaveGroup {
            muted: false,
            solo: false,
            name: Some("Pad".into()),
            volume: 0.5,
            pan: -0.5,
            effects: vec![SaveEffect {
                pinned: vec![],
                plugin: "builtin:reverb".into(),
                mix: 1.0,
                feed: Feed::Chain,
                preset: None,
                params: vec![],
                ui: None,
            }],
            ui: None,
            route: Route::Out,
        }];
        save(&path, &instruments, &groups, &[], &Default::default(), None, None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[[group]]"), "expected a [[group]] section:\n{raw}");
        assert!(raw.contains("group = 0"), "members should record group index:\n{raw}");
        assert!(raw.contains("pan = -0.5"), "group pan should serialize:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.groups.len(), 1);
        assert_eq!(config.groups[0].name.as_deref(), Some("Pad"));
        assert!((config.groups[0].volume - 0.5).abs() < 1e-6);
        assert!((config.groups[0].pan + 0.5).abs() < 1e-6);
        assert_eq!(config.groups[0].effects.len(), 1);
        assert_eq!(config.instruments[0].group, Some(0));
        assert_eq!(config.instruments[1].group, Some(0));
        assert_eq!(config.instruments[2].group, None);
    }

    #[test]
    fn save_and_reload_with_remap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remap_test.toml");

        let mut remap = HashMap::new();
        // Insert out of pitch order on purpose; saved file should still be in pitch order.
        remap.insert(
            "G#6".into(),
            RemapTarget { note: "G6".into(), detune: 1.0 },
        );
        remap.insert(
            "F#2".into(),
            RemapTarget { note: "F2".into(), detune: 1.0 },
        );
        remap.insert(
            "C#6".into(),
            RemapTarget { note: "C6".into(), detune: 1.0 },
        );

        let instruments = vec![SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap,
            }),
            effects: vec![],
            pattern: None,
            ui: None,
            route: Route::Out,
        }];

        save(&path, &instruments, &[], &[], &Default::default(), None, None, None).unwrap();

        // Output should use inline tables and stable alphabetical ordering.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(r#""F#2" = { note = "F2", detune = 1.0 }"#)
                && !raw.contains(r#"[instrument.remap."F#2"]"#),
            "expected inline table for F#2, got:\n{raw}"
        );
        // Saved file should be ordered by MIDI pitch: F#2 < C#6 < G#6.
        let f_pos = raw.find("F#2").unwrap();
        let c_pos = raw.find("C#6").unwrap();
        let g_pos = raw.find("G#6").unwrap();
        assert!(
            f_pos < c_pos && c_pos < g_pos,
            "remap entries not in pitch order:\n{raw}"
        );

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.remap.len(), 3);
        assert_eq!(inst.remap["F#2"].note, "F2");
        assert!((inst.remap["F#2"].detune - 1.0).abs() < f64::EPSILON);
        assert_eq!(inst.remap["G#6"].note, "G6");
    }

    /// A session with a position on every kind of node, as a person or the page writes them: whole numbers or decimals.
    const PLACED: &str = r#"
[ui]
master = { x = 1200, y = 300.5 }

[[group]]
name = "Pad"
ui = { x = 800, y = 40 }

[[group.effect]]
plugin = "builtin:reverb"
ui = { x = 1100, y = 40 }

[[group.modulator]]
ui = { x = 800, y = -120 }

[[instrument]]
plugin = "builtin:osc"
group = 0
ui = { x = 0, y = 0 }

[[instrument.effect]]
plugin = "builtin:filter"
ui = { x = 376, y = 0.25 }

[[instrument.modulator]]
type = "envelope"
ui = { x = 376, y = -200 }

[[instrument]]
plugin = "builtin:osc"
"#;

    #[test]
    fn a_position_is_read_on_every_kind_of_node_and_a_node_without_one_has_none() {
        let config = parse_raw(PLACED).unwrap();
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        assert_eq!(config.master_ui, at(1200.0, 300.5));
        assert_eq!(config.groups[0].ui, at(800.0, 40.0));
        assert_eq!(config.groups[0].effects[0].ui, at(1100.0, 40.0));
        assert_eq!(config.groups[0].modulators[0].ui, at(800.0, -120.0));
        assert_eq!(config.instruments[0].ui, at(0.0, 0.0));
        assert_eq!(config.instruments[0].effects[0].ui, at(376.0, 0.25));
        assert_eq!(config.instruments[0].modulators[0].ui, at(376.0, -200.0));
        assert_eq!(config.instruments[1].ui, None);
        // (A session from before there were positions has none.)
        let old = parse_raw("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").unwrap();
        assert_eq!((old.master_ui, old.instruments[0].ui, old.instruments[0].effects[0].ui), (None, None, None));
    }

    #[test]
    fn a_position_that_is_not_a_pair_of_numbers_is_ignored_and_the_rest_of_the_session_loads() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for text in ["ui = { x = 1 }", "ui = 5", "ui = { x = \"a\", y = 2 }", "ui = []", "ui = {}"] {
            let config = parse(&format!("{osc}{text}\n")).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!((config.instruments.len(), config.instruments[0].ui), (1, None), "{text}");
        }
        let config = parse(&format!("{osc}[[instrument.effect]]\nplugin = \"builtin:filter\"\nui = \"here\"\n[ui]\nmaster = 3\n")).unwrap();
        assert_eq!((config.instruments[0].effects[0].ui, config.master_ui), (None, None));
        // (Numbers that are not numbers are refused wherever they are, as every number is.)
        assert!(parse(&format!("{osc}ui = {{ x = nan, y = 1 }}\n")).is_err());
    }

    /// An effect with nothing set, for the tests of what is written for one thing.
    fn bare_effect(plugin: &str) -> SaveEffect {
        SaveEffect { plugin: plugin.into(), mix: 1.0, feed: Feed::Chain, preset: None, params: vec![], pinned: vec![], ui: None }
    }

    /// A lane with an effect and a modulator, each with this position, for the tests of what is written.
    fn placed_lane(ui: Option<NodeUi>) -> SaveInstrumentSlot {
        SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: Some(SaveInstrument { pinned: vec![], plugin: "builtin:osc".into(), volume: 1.0, preset: None, params: vec![], pitch_bend_range: 2.0, remap: HashMap::new() }),
            effects: vec![SaveEffect { pinned: vec![], plugin: "builtin:filter".into(), mix: 1.0, feed: Feed::Chain, preset: None, params: vec![], ui }],
            pattern: None,
            ui,
            route: Route::Out,
        }
    }

    /// The text of a session of one lane (`placed_lane`) and these modulators of the session's own, as `to_toml` writes it, and what `parse` makes of it.
    fn saved_with(modulators: Vec<SaveModulator>) -> (String, SessionConfig) {
        let sources = SaveSources { modulators, ..Default::default() };
        let text = to_toml(&[placed_lane(None)], &[], &[], &sources, None, None, None).unwrap();
        let config = parse(&text).unwrap();
        (text, config)
    }

    /// A cable of a modulator to the cutoff of the filter of the first lane, as the saver is given it.
    fn cutoff_cable(depth: f32) -> SaveModTarget {
        SaveModTarget { kind: crate::plugin::chain::ModTargetKind::FreeParam { plugin: crate::ids::NodeId(1), param_index: 0 }, label: "cutoff".into(), depth, to: Some(CablePlace::Effect { instrument: 0, effect: 0 }) }
    }

    #[test]
    fn positions_are_written_on_their_nodes_as_inline_tables_the_master_in_a_table_of_its_own_and_read_back() {
        let at = |x: f32, y: f32| Some(NodeUi { x, y });
        let group = SaveGroup {
            muted: false,
            solo: false,
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: vec![SaveEffect { pinned: vec![], plugin: "builtin:reverb".into(), mix: 1.0, feed: Feed::Chain, preset: None, params: vec![], ui: at(1100.0, 40.0) }],
            ui: at(800.0, 40.0),
            route: Route::Out,
        };
        let lfo = |ui, hears| SaveModulator { source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0, retrigger: false, fade: 0.0, sync: None }, targets: vec![], ui, hears: Some(hears) };
        let sources = SaveSources { modulators: vec![lfo(at(80.0, 120.0), CablePlace::Instrument(0)), lfo(at(800.0, -120.5), CablePlace::Group(0))], ..Default::default() };
        let text = to_toml(&[placed_lane(at(80.0, 120.0))], &[group], &[], &sources, None, at(1200.0, 300.0), None).unwrap();
        // Whole numbers are written without a point, and it is one line for a node, in the table of the node.
        assert!(text.contains("ui = { x = 80, y = 120 }"), "{text}");
        assert_eq!(text.matches("ui = { x = 80, y = 120 }").count(), 3, "the lane, its effect and its modulator: {text}");
        assert!(text.contains("ui = { x = 800, y = -120.5 }"), "{text}");
        assert!(text.contains("[ui]\nmaster = { x = 1200, y = 300 }"), "the master has a table of its own: {text}");
        // And what is written is read back, where it was.
        let config = parse(&text).unwrap();
        assert_eq!(config.master_ui, at(1200.0, 300.0));
        assert_eq!(config.groups[0].ui, at(800.0, 40.0));
        assert_eq!(config.groups[0].effects[0].ui, at(1100.0, 40.0));
        assert_eq!(config.free_modulators[1].ui, at(800.0, -120.5), "the group's modulator is the session's own");
        assert_eq!(config.instruments[0].ui, at(80.0, 120.0));
        assert_eq!(config.instruments[0].effects[0].ui, at(80.0, 120.0));
        assert_eq!(config.free_modulators[0].ui, at(80.0, 120.0), "and so is the lane's");
    }

    #[test]
    fn a_pinned_list_is_read_on_an_instrument_an_effect_and_a_bus_effect_and_a_node_without_one_has_none() {
        let config = parse(crate::app::testutil::PINNED).unwrap();
        fn names(list: &[String]) -> Vec<&str> {
            list.iter().map(String::as_str).collect()
        }
        assert_eq!(names(&config.instruments[0].instrument.as_ref().unwrap().pinned), ["sustain", "detune", "waveform"]);
        assert_eq!(names(&config.instruments[0].effects[0].pinned), ["resonance", "no such parameter", "resonance", "cutoff"]);
        assert_eq!(names(&config.groups[0].effects[0].pinned), ["drive", "cutoff"]);
        assert!(config.instruments[1].instrument.as_ref().unwrap().pinned.is_empty());
        // (A session from before there were pins has none.)
        let old = parse("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n").unwrap();
        assert!(old.instruments[0].instrument.as_ref().unwrap().pinned.is_empty() && old.instruments[0].effects[0].pinned.is_empty());
    }

    #[test]
    fn a_pinned_list_that_is_not_a_list_of_names_is_ignored_where_it_is_wrong_and_the_rest_of_the_session_loads() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        let cases: [(&str, &[&str]); 7] = [
            ("pinned = 5", &[]),
            ("pinned = \"cutoff\"", &[]),
            ("pinned = {}", &[]),
            ("pinned = [1, 2]", &[]),
            ("pinned = [\"a\", 2, \"b\"]", &["a", "b"]),
            ("pinned = [[\"a\"]]", &[]),
            ("pinned = []", &[]),
        ];
        for (text, expected) in cases {
            let config = parse(&format!("{osc}{text}\n")).unwrap_or_else(|e| panic!("{text}: {e}"));
            let pinned: Vec<&str> = config.instruments[0].instrument.as_ref().unwrap().pinned.iter().map(String::as_str).collect();
            assert_eq!((config.instruments.len(), pinned), (1, expected.to_vec()), "{text}");
        }
        let config = parse(&format!("{osc}[[instrument.effect]]\nplugin = \"builtin:filter\"\npinned = false\n")).unwrap();
        assert_eq!((config.instruments.len(), config.instruments[0].effects[0].pinned.len()), (1, 0));
    }

    #[test]
    fn pinned_parameters_are_written_by_name_on_the_plugin_they_are_for_and_read_back_and_none_are_written_for_none() {
        let mut lane = placed_lane(None);
        lane.instrument.as_mut().unwrap().pinned = vec!["sustain".into(), "detune".into()];
        lane.effects[0].pinned = vec!["cutoff".into()];
        let group = SaveGroup {
            muted: false,
            solo: false,
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: vec![SaveEffect { pinned: vec!["mix".into(), "width".into()], ..bare_effect("builtin:reverb") }],
            ui: None,
            route: Route::Out,
        };
        let text = to_toml(&[lane], &[group], &[], &Default::default(), None, None, None).unwrap();
        for line in [r#"pinned = ["sustain", "detune"]"#, r#"pinned = ["cutoff"]"#, r#"pinned = ["mix", "width"]"#] {
            assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{line} in\n{text}");
        }
        assert_eq!(text.lines().filter(|l| l.starts_with("pinned")).count(), 3, "{text}");
        // And what is written is read back, on the same plugins and in the same order.
        let config = parse(&text).unwrap();
        assert_eq!(config.instruments[0].instrument.as_ref().unwrap().pinned, ["sustain", "detune"]);
        assert_eq!(config.instruments[0].effects[0].pinned, ["cutoff"]);
        assert_eq!(config.groups[0].effects[0].pinned, ["mix", "width"]);
        // With none pinned there is no line: a file is as it always was.
        let plain = to_toml(&[placed_lane(None)], &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!plain.contains("pinned"), "{plain}");
    }

    #[test]
    fn a_place_is_kept_to_a_tenth_of_a_pixel_half_away_from_zero_and_within_the_limits_of_the_canvas() {
        let tidy = |x: f32, y: f32| NodeUi { x, y }.tidy().map(|ui| (ui.x, ui.y));
        assert_eq!(tidy(376.382, -40.04), Some((376.4, -40.0)));
        assert_eq!(tidy(0.25, -0.25), Some((0.3, -0.3)));
        assert_eq!(tidy(0.05, -0.05), Some((0.1, -0.1)));
        assert_eq!(tidy(80.0, 120.0), Some((80.0, 120.0)), "a place that is tidy is as it was");
        assert_eq!(tidy(1.0e9, -1.0e9), Some((NodeUi::LIMIT as f32, -(NodeUi::LIMIT as f32))));
        assert_eq!(tidy(f32::MAX, f32::MIN), Some((100_000.0, -100_000.0)));
        // (No `-0`: it would be written, to a file and to a client, as it is.)
        let (x, y) = tidy(-0.04, -0.0).unwrap();
        assert!(x.is_sign_positive() && y.is_sign_positive(), "{x} {y}");
        // Numbers that are not numbers are not a place.
        for (x, y) in [(f32::NAN, 1.0), (1.0, f32::NAN), (f32::INFINITY, 1.0), (1.0, f32::NEG_INFINITY)] {
            assert_eq!(tidy(x, y), None, "{x} {y}");
        }
    }

    #[test]
    fn a_node_without_a_position_is_written_as_it_always_was() {
        let text = to_toml(&[placed_lane(None)], &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!text.lines().any(|line| line.starts_with("ui ") || line == "[ui]"), "{text}");
        // (The same session with positions has the same lines besides the positions.)
        let placed = to_toml(&[placed_lane(Some(NodeUi { x: 1.0, y: 2.0 }))], &[], &[], &Default::default(), None, None, None).unwrap();
        let without: Vec<&str> = placed.lines().filter(|line| !line.starts_with("ui = ")).collect();
        assert_eq!(without.join("\n") + "\n", text);
    }

    #[test]
    fn a_position_with_decimals_is_written_the_way_it_was_typed() {
        // (Not `0.10000000149011612`: an f32 is written as the shortest number that is it.)
        let text = to_toml(&[placed_lane(Some(NodeUi { x: 12.3, y: -0.1 }))], &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(text.contains("ui = { x = 12.3, y = -0.1 }"), "{text}");
    }

    #[test]
    fn save_and_reload_with_piano_scale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("piano.toml");

        let instruments = vec![SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
            route: Route::Out,
        }];
        let piano = PianoConfig { scale: Some("F# dorian".into()), locked: false };
        save(&path, &instruments, &[], &[], &Default::default(), Some(&piano), None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[piano]"));
        assert!(raw.contains("scale = \"F# dorian\""));

        let config = load(path.to_str().unwrap()).unwrap();
        let piano_cfg = config.piano.expect("piano section");
        assert_eq!(piano_cfg.scale.as_deref(), Some("F# dorian"));
    }

    #[test]
    fn save_creates_the_sessions_directory() {
        // A new session's home (~/.config/tang/sessions/) does not exist before the first save.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tang").join("sessions").join("session-1.toml");
        let lane = SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: None,
            effects: vec![],
            pattern: None,
            ui: None,
            route: Route::Out,
        };
        save(&path, &[lane], &[], &[], &Default::default(), None, None, None).unwrap();
        assert_eq!(load(path.to_str().unwrap()).unwrap().instruments.len(), 1);
    }

    /// A session of one lane, for the tests of what a write does to the file system.
    fn one_lane() -> Vec<SaveInstrumentSlot> {
        vec![SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
            ui: None,
            route: Route::Out,
        }]
    }

    /// Whether an error that a write gave is the file system saying that the path is taken.
    fn path_is_taken(error: &anyhow::Error) -> bool {
        error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
    }

    #[test]
    fn a_session_saved_as_a_new_file_is_what_a_save_writes_and_is_made_where_the_directory_is_not_yet() {
        let dir = tempfile::tempdir().unwrap();
        save_new(&dir.path().join("new.toml"), &one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        save(&dir.path().join("same.toml"), &one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("new.toml")).unwrap(), std::fs::read_to_string(dir.path().join("same.toml")).unwrap());
        // (A new session's home is a directory that may not be there yet.)
        let nested = dir.path().join("sessions").join("new.toml");
        save_new(&nested, &one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        assert_eq!(load(nested.to_str().unwrap()).unwrap().instruments.len(), 1);
    }

    #[test]
    fn a_session_saved_as_a_new_file_does_not_write_over_a_file_that_is_there_and_the_write_is_what_finds_it() {
        // (Finding f128: a look at the path and then a write could be out of date by the time of the write; the write itself refuses a path that is taken.)
        let dir = tempfile::tempdir().unwrap();
        let taken = dir.path().join("taken.toml");
        std::fs::write(&taken, "another session").unwrap();
        let error = save_new(&taken, &one_lane(), &[], &[], &Default::default(), None, None, None).expect_err("the file is there");
        assert!(path_is_taken(&error), "{error:#}");
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "another session", "what was there is as it was");

        // A folder is a path that is taken.
        std::fs::create_dir(dir.path().join("folder.toml")).unwrap();
        assert!(path_is_taken(&save_new(&dir.path().join("folder.toml"), &one_lane(), &[], &[], &Default::default(), None, None, None).expect_err("a folder is there")));

        // A save, which is for the session's own file, writes over it.
        save(&taken, &one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(std::fs::read_to_string(&taken).unwrap().contains("builtin:sine"));
    }

    #[cfg(unix)]
    #[test]
    fn a_session_saved_as_a_new_file_does_not_follow_a_link_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        // A link to nowhere, and one to a file that is there: the write would follow either (and make the first one's file, or write over the second's).
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("dangling.toml")).unwrap();
        std::fs::write(dir.path().join("precious.toml"), "keep me").unwrap();
        std::os::unix::fs::symlink(dir.path().join("precious.toml"), dir.path().join("link.toml")).unwrap();
        for link in ["dangling.toml", "link.toml"] {
            let error = save_new(&dir.path().join(link), &one_lane(), &[], &[], &Default::default(), None, None, None).expect_err("a link is a path that is taken");
            assert!(path_is_taken(&error), "{link}: {error:#}");
        }
        assert!(!dir.path().join("nowhere").exists(), "what the link leads to was not made");
        assert_eq!(std::fs::read_to_string(dir.path().join("precious.toml")).unwrap(), "keep me");
    }

    #[test]
    fn save_and_reload_piano_lock() {
        // The Piano tab's lock toggle is session state: `[piano] locked = true` is
        // read at startup, so a save has to write it back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.toml");
        let instruments = || {
            vec![SaveInstrumentSlot {
                muted: false,
                solo: false,
                range: None,
                transpose: 0,
                group: None,
                instrument: Some(SaveInstrument {
                    pinned: vec![],
                    plugin: "builtin:sine".into(),
                    volume: 1.0,
                    preset: None,
                    params: vec![],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![],
                pattern: None,
                ui: None,
                route: Route::Out,
            }]
        };

        let locked = PianoConfig { scale: Some("C major".into()), locked: true };
        save(&path, &instruments(), &[], &[], &Default::default(), Some(&locked), None, None).unwrap();
        let config = load(path.to_str().unwrap()).unwrap();
        assert!(config.piano.expect("piano section").locked);

        // Highlight mode is the default and stays out of the file.
        let open = PianoConfig { scale: Some("C major".into()), locked: false };
        save(&path, &instruments(), &[], &[], &Default::default(), Some(&open), None, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("locked"), "{raw}");
        assert!(!load(path.to_str().unwrap()).unwrap().piano.expect("piano section").locked);
    }

    #[test]
    fn the_tempo_is_a_top_level_key_that_a_save_writes_and_a_load_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tempo.toml");
        save(&path, &one_lane(), &[], &[], &Default::default(), None, None, Some(96.5)).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with("bpm = 96.5\n"), "it comes before every table: {raw}");
        assert_eq!(load(path.to_str().unwrap()).unwrap().bpm, Some(96.5));

        // A session that has no tempo has no key, and reads as having none.
        save(&path, &one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("bpm"), "{raw}");
        assert_eq!(load(path.to_str().unwrap()).unwrap().bpm, None);
    }

    #[test]
    fn a_tempo_is_written_as_it_was_typed() {
        // (100.1 as the 32-bit number it is would be 100.0999984741211.)
        let text = to_toml(&one_lane(), &[], &[], &Default::default(), None, None, Some(100.1)).unwrap();
        assert!(text.starts_with("bpm = 100.1\n"), "{text}");
    }

    // -- the output mix (step 102) --

    #[test]
    fn a_master_table_is_read_key_by_key_and_a_file_that_has_none_says_nothing_of_it() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        assert_eq!(parse(osc).unwrap().master, None, "no table: the master is what it is without one");
        assert_eq!(parse(&format!("[master]\n{osc}")).unwrap().master, Some(MasterConfig::default()), "an empty table says nothing either");
        let all = parse(&format!("[master]\nvolume = 0.5\nlimiter = false\nceiling = -3.5\n{osc}")).unwrap().master;
        assert_eq!(all, Some(MasterConfig { volume: Some(0.5), limiter: Some(false), ceiling: Some(-3.5) }));
        let some = parse(&format!("{osc}[master]\nlimiter = true\n")).unwrap().master;
        assert_eq!(some, Some(MasterConfig { volume: None, limiter: Some(true), ceiling: None }), "a key that is not there is not said");
    }

    #[test]
    fn a_master_with_a_number_that_is_not_one_or_a_switch_that_is_not_one_refuses_the_session_with_its_place() {
        let osc = "[[instrument]]\nplugin = \"builtin:osc\"\n";
        for (table, place) in [("volume = nan", "master.volume"), ("ceiling = inf", "master.ceiling"), ("volume = 1e39", "master.volume"), ("ceiling = -inf", "master.ceiling")] {
            let error = parse(&format!("[master]\n{table}\n{osc}")).err().expect("refused").to_string();
            assert!(error.contains(place), "{error}");
        }
        for table in ["volume = \"loud\"", "limiter = 1", "limiter = \"on\"", "ceiling = true"] {
            assert!(parse(&format!("[master]\n{table}\n{osc}")).is_err(), "{table}");
        }
    }

    #[test]
    fn a_master_is_written_with_the_keys_that_are_set_and_a_session_that_has_none_has_no_table() {
        let master = |volume, limiter, ceiling| SaveSources { master: Some(MasterConfig { volume, limiter, ceiling }), ..Default::default() };
        let text = to_toml(&one_lane(), &[], &[], &master(Some(0.8), Some(false), Some(-3.0)), None, None, None).unwrap();
        assert!(text.contains("[master]\nvolume = 0.8\nlimiter = false\nceiling = -3.0\n"), "{text}");
        let text = to_toml(&one_lane(), &[], &[], &master(None, Some(false), None), None, None, None).unwrap();
        assert!(text.contains("[master]\nlimiter = false\n") && !text.contains("volume") && !text.contains("ceiling"), "{text}");
        // (Written as it was typed: a volume of 0.7 is not 0.699999988079071.)
        let text = to_toml(&one_lane(), &[], &[], &master(Some(0.7), None, Some(-1.1)), None, None, None).unwrap();
        assert!(text.contains("volume = 0.7\n") && text.contains("ceiling = -1.1\n"), "{text}");
        // None is no table, and the file is the file that there was.
        let plain = to_toml(&one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!plain.contains("master"), "{plain}");
        // It comes before the tables of the nodes, and reads back as it was written.
        let text = to_toml(&one_lane(), &[], &[], &master(Some(0.5), Some(false), Some(-6.0)), None, None, Some(100.0)).unwrap();
        assert!(text.find("[master]").unwrap() < text.find("[[instrument]]").unwrap() && text.starts_with("bpm = 100.0\n"), "{text}");
        assert_eq!(parse(&text).unwrap().master, Some(MasterConfig { volume: Some(0.5), limiter: Some(false), ceiling: Some(-6.0) }));
    }

    // -- audio cables (P3c of docs/patch-graph.md §11.6) --

    const OSC: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    #[test]
    fn a_place_of_a_cable_is_a_lane_an_effect_a_group_a_bus_effect_or_the_mix_and_is_written_as_it_is_read() {
        for (text, place) in [
            ("instrument[0]", CablePlace::Instrument(0)),
            ("instrument[12]", CablePlace::Instrument(12)),
            ("instrument[1].effect[0]", CablePlace::Effect { instrument: 1, effect: 0 }),
            ("group[3]", CablePlace::Group(3)),
            ("group[0].effect[2]", CablePlace::BusEffect { group: 0, effect: 2 }),
            ("instrument[1].effect[0].side", CablePlace::EffectSide { instrument: 1, effect: 0 }),
            ("group[2].effect[1].side", CablePlace::BusEffectSide { group: 2, effect: 1 }),
            ("master", CablePlace::Master),
            ("  master ", CablePlace::Master),
            ("keys", CablePlace::Keys),
            (" keys ", CablePlace::Keys),
            ("device[0]", CablePlace::Device(0)),
            ("device[12]", CablePlace::Device(12)),
        ] {
            assert_eq!(CablePlace::parse(text), Some(place), "{text:?}");
            assert_eq!(CablePlace::parse(&place.to_string()), Some(place), "{text:?} is written and read back the same");
        }
        assert_eq!(CablePlace::Instrument(0).to_string(), "instrument[0]");
        assert_eq!(CablePlace::Effect { instrument: 2, effect: 1 }.to_string(), "instrument[2].effect[1]");
        assert_eq!(CablePlace::Group(1).to_string(), "group[1]");
        assert_eq!(CablePlace::BusEffect { group: 1, effect: 0 }.to_string(), "group[1].effect[0]");
        assert_eq!(CablePlace::EffectSide { instrument: 2, effect: 1 }.to_string(), "instrument[2].effect[1].side");
        assert_eq!(CablePlace::BusEffectSide { group: 1, effect: 0 }.to_string(), "group[1].effect[0].side");
        assert_eq!(CablePlace::Master.to_string(), "master");
        assert_eq!(CablePlace::Keys.to_string(), "keys");
        assert_eq!(CablePlace::Device(3).to_string(), "device[3]");
    }

    #[test]
    fn only_the_keys_and_the_devices_are_sources_of_notes() {
        for (place, source) in [
            (CablePlace::Keys, true),
            (CablePlace::Device(0), true),
            (CablePlace::Instrument(0), false),
            (CablePlace::Effect { instrument: 0, effect: 0 }, false),
            (CablePlace::Group(0), false),
            (CablePlace::BusEffect { group: 0, effect: 0 }, false),
            (CablePlace::EffectSide { instrument: 0, effect: 0 }, false),
            (CablePlace::BusEffectSide { group: 0, effect: 0 }, false),
            (CablePlace::Master, false),
        ] {
            assert_eq!(place.is_source(), source, "{place}");
        }
    }

    #[test]
    fn a_text_that_is_no_place_is_none() {
        for text in [
            "", "instrument", "instrument[]", "instrument[-1]", "instrument[1.5]", "instrument[a]", "instrument[ 1 ]", "instrument[0", "instrument 0", "Instrument[0]", "Master", "mix", "group", "group[0].effect",
            "group[0].effect[]", "group[0].effects[1]", "instrument[0].effect[0].effect[0]", "instrument[0].group[0]", "master[0]", "instrument[0]x", "[0]", "effect[0]", "instrument[99999999999999999999]",
            "instrument[0].", ".effect[0]", "instrument[+1]", "group[0].effect[+0]", "keys[0]", "Keys", "key", "device", "device[]", "device[x]", "device[-1]", "Device[0]", "device[0].effect[0]",
            "keys.effect[0]", "devices[0]", "instrument[0].side", "group[0].side", "instrument[0].effect[0].sides", "instrument[0].effect[0].Side", "instrument[0].effect[0].side.x", "instrument[0].effect[0].",
            "instrument[0].effect[0].side[0]", "device[0].effect[0].side", "master.side", "keys.side", "instrument[0].side.effect[0]",
        ] {
            assert_eq!(CablePlace::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_cable_is_read_with_its_two_places_and_a_level_only_when_the_file_says_one() {
        let config = parse(&format!(
            "{OSC}[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[group]]\n[[cable]]\nfrom = \"instrument[0]\"\nto = \"instrument[0].effect[0]\"\n[[cable]]\nfrom = \"group[0]\"\nto = \"master\"\ngain = 0.5\n"
        ))
        .unwrap();
        assert_eq!(config.cables.len(), 2);
        assert_eq!((config.cables[0].from, config.cables[0].to, config.cables[0].gain), (CablePlace::Instrument(0), CablePlace::Effect { instrument: 0, effect: 0 }, None));
        assert_eq!((config.cables[1].from, config.cables[1].to, config.cables[1].gain), (CablePlace::Group(0), CablePlace::Master, Some(0.5)));
        assert!(parse(OSC).unwrap().cables.is_empty());
    }

    #[test]
    fn a_cable_with_a_place_that_is_none_is_refused_and_the_error_says_which_of_the_cable() {
        for (text, why) in [
            ("[[cable]]\nfrom = \"instrument[x]\"\nto = \"master\"\n", "cable[0].from: \"instrument[x]\" is not a place of the session"),
            ("[[cable]]\nfrom = \"instrument[0]\"\nto = \"mix\"\n", "cable[0].to: \"mix\" is not a place of the session"),
            ("[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\n[[cable]]\nfrom = \"\"\nto = \"master\"\n", "cable[1].from: \"\" is not a place of the session"),
        ] {
            let error = parse(&format!("{OSC}{text}")).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert!(error.contains(why), "{error}\nfor:\n{text}");
        }
        // A cable that has no start or no end is not a table that fits.
        assert!(parse(&format!("{OSC}[[cable]]\nto = \"master\"\n")).is_err());
        assert!(parse(&format!("{OSC}[[cable]]\nfrom = \"instrument[0]\"\n")).is_err());
    }

    fn cable(from: CablePlace, to: CablePlace, gain: f32) -> SaveCable {
        SaveCable { from, to, gain }
    }

    #[test]
    fn cables_are_written_after_the_groups_with_a_level_only_when_it_is_not_one_and_are_read_back_the_same() {
        let cables = [
            cable(CablePlace::Instrument(0), CablePlace::Effect { instrument: 0, effect: 1 }, 1.0),
            cable(CablePlace::BusEffect { group: 0, effect: 0 }, CablePlace::Master, 0.3),
            cable(CablePlace::Group(0), CablePlace::Group(1), 2.0),
        ];
        let bus = SaveGroup { name: None, volume: 1.0, pan: 0.0, muted: false, solo: false, effects: vec![], ui: None, route: Route::Out };
        let text = to_toml(&one_lane(), &[bus], &cables, &Default::default(), None, None, None).unwrap();
        assert!(text.contains("[[cable]]\nfrom = \"instrument[0]\"\nto = \"instrument[0].effect[1]\"\n\n"), "no level of one: {text}");
        assert!(text.contains("[[cable]]\nfrom = \"group[0].effect[0]\"\nto = \"master\"\ngain = 0.3\n"), "a level as it was typed: {text}");
        assert!(text.find("[[group]]").unwrap() < text.find("[[cable]]").unwrap(), "after the groups: {text}");
        assert_eq!(text.matches("[[cable]]").count(), 3);
        let read = parse(&text).unwrap();
        let again: Vec<_> = read.cables.iter().map(|c| (c.from, c.to, c.gain.unwrap_or(1.0))).collect();
        assert_eq!(again, cables.iter().map(|c| (c.from, c.to, c.gain)).collect::<Vec<_>>());
        // A session with none writes none (its file is what it was).
        let plain = to_toml(&one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!plain.contains("cable"), "{plain}");
    }

    /// Two lanes, the keys with a place, and two devices, one with a place, with a cable from each source.
    const ROUTED: &str = r#"
[keys]
ui = { x = 0, y = 40 }

[[device]]
name = "Launchkey Mini MK4 37"
ui = { x = 0, y = 300.5 }

[[device]]
name = "Seaboard"

[[instrument]]
plugin = "builtin:osc"

[[instrument]]
plugin = "builtin:osc"

[[cable]]
from = "keys"
to = "instrument[0]"

[[cable]]
from = "device[0]"
to = "instrument[1]"

[[cable]]
from = "device[1]"
to = "instrument[1]"
"#;

    #[test]
    fn the_keys_and_the_devices_a_session_has_a_node_for_are_read_with_their_places_and_the_cables_of_notes_have_no_level() {
        let config = parse(ROUTED).unwrap();
        assert_eq!(config.keys.as_ref().map(|keys| keys.ui), Some(Some(NodeUi { x: 0.0, y: 40.0 })));
        assert_eq!(config.devices.iter().map(|device| (device.name.as_str(), device.ui)).collect::<Vec<_>>(), [("Launchkey Mini MK4 37", Some(NodeUi { x: 0.0, y: 300.5 })), ("Seaboard", None)]);
        assert_eq!(
            config.cables.iter().map(|cable| (cable.from, cable.to, cable.gain)).collect::<Vec<_>>(),
            [
                (CablePlace::Keys, CablePlace::Instrument(0), None),
                (CablePlace::Device(0), CablePlace::Instrument(1), None),
                (CablePlace::Device(1), CablePlace::Instrument(1), None),
            ]
        );
    }

    #[test]
    fn a_session_that_has_no_source_has_none_and_the_keys_are_there_for_a_table_even_an_empty_one() {
        let plain = parse(OSC).unwrap();
        assert!(plain.keys.is_none() && plain.devices.is_empty());
        let empty = parse(&format!("[keys]\n{OSC}")).unwrap();
        assert_eq!(empty.keys.map(|keys| keys.ui), Some(None), "a table with nothing in it is the keys, with no place");
    }

    #[test]
    fn a_device_is_called_by_a_name_that_is_not_blank_and_a_name_has_one_node() {
        let config = parse(&format!("[[device]]\nname = \"  Launchkey  \"\n{OSC}")).unwrap();
        assert_eq!(config.devices[0].name, "Launchkey", "the spaces round a name go");
        for (text, why) in [
            ("[[device]]\nname = \"\"\n", "device[0].name is blank"),
            ("[[device]]\nname = \"   \"\n", "device[0].name is blank"),
            ("[[device]]\nname = \"A\"\n[[device]]\nname = \"B\"\n[[device]]\nname = \" A \"\n", "device[2].name is \"A\", as device[0]'s is"),
        ] {
            let error = parse(&format!("{text}{OSC}")).err().unwrap_or_else(|| panic!("accepted:\n{text}")).to_string();
            assert!(error.contains(why), "{error}\nfor:\n{text}");
        }
        // A device with no name is not a table that fits, and the case of a name is a name's: two devices that differ by it are two nodes.
        assert!(parse(&format!("[[device]]\nui = {{ x = 1, y = 2 }}\n{OSC}")).is_err());
        assert_eq!(parse(&format!("[[device]]\nname = \"a\"\n[[device]]\nname = \"A\"\n{OSC}")).unwrap().devices.len(), 2);
    }

    #[test]
    fn a_device_name_is_the_text_without_the_spaces_round_it_and_is_refused_blank_too_long_or_with_a_control_character() {
        assert_eq!(device_name("  Launchkey Mini \t"), Ok("Launchkey Mini".to_string()));
        for blank in ["", "   ", "\t \n"] {
            assert!(device_name(blank).unwrap_err().starts_with("is blank"), "{blank:?}");
        }
        // The longest is a name, counted in characters and not bytes; one more is not.
        let longest = "é".repeat(MAX_DEVICE_NAME);
        assert_eq!(device_name(&longest), Ok(longest.clone()));
        assert_eq!(device_name(&format!("{longest}x")), Err(format!("is longer than {MAX_DEVICE_NAME} characters")));
        // (A control character at the edge that is a space goes with the spaces; one that is not, and one inside the name, does not.)
        assert_eq!(device_name("\nA"), Ok("A".to_string()));
        for name in ["A\nB", "\u{7}A", "A\u{0}"] {
            assert_eq!(device_name(name), Err("has a control character in it".to_string()), "{name:?}");
        }
    }

    #[test]
    fn a_session_whose_device_name_is_too_long_or_has_a_control_character_is_refused_with_the_place() {
        let long = "x".repeat(MAX_DEVICE_NAME + 1);
        for (name, why) in [(long.as_str(), "device[1].name is longer than 256 characters"), ("A\\nB", "device[1].name has a control character in it")] {
            let error = parse(&format!("[[device]]\nname = \"fine\"\n[[device]]\nname = \"{name}\"\n{OSC}")).err().unwrap_or_else(|| panic!("accepted {name:?}")).to_string();
            assert!(error.contains(why), "{error}");
        }
    }

    #[test]
    fn a_cable_of_notes_that_has_a_level_refuses_the_session_with_its_place() {
        for from in ["keys", "device[0]"] {
            let error = parse(&format!("[[device]]\nname = \"A\"\n{OSC}[[cable]]\nfrom = \"{from}\"\nto = \"instrument[0]\"\ngain = 0.5\n")).err().unwrap_or_else(|| panic!("accepted a level on {from}")).to_string();
            assert!(error.contains("cable[0].gain: a cable of notes has no level"), "{error}");
        }
        // (The audio cable that is drawn after it has one, as it always had, and the place that a cable names is told in the words of the file.)
        parse(&format!("{OSC}[[cable]]\nfrom = \"instrument[0]\"\nto = \"master\"\ngain = 0.5\n")).unwrap();
        let error = parse(&format!("{OSC}[[cable]]\nfrom = \"mic\"\nto = \"instrument[0]\"\n")).err().unwrap().to_string();
        assert!(error.contains("cable[0].from: \"mic\" is not a place of the session") && error.contains("keys or device[0]"), "{error}");
    }

    fn sources(keys: Option<Option<NodeUi>>, devices: &[(&str, Option<NodeUi>)]) -> SaveSources {
        SaveSources { keys: keys.map(|ui| SaveKeys { ui }), devices: devices.iter().map(|(name, ui)| SaveDevice { name: name.to_string(), ui: *ui }).collect(), modulators: Vec::new(), modules: Vec::new(), master: None }
    }

    #[test]
    fn the_sources_are_written_before_the_lanes_and_their_cables_have_no_level_and_all_of_it_is_read_back_the_same() {
        let cables = [cable(CablePlace::Keys, CablePlace::Instrument(0), 1.0), cable(CablePlace::Device(1), CablePlace::Instrument(0), 1.0)];
        let saved = sources(Some(Some(NodeUi { x: 0.0, y: 40.0 })), &[("Launchkey", Some(NodeUi { x: -20.0, y: 300.5 })), ("Seaboard", None)]);
        let text = to_toml(&one_lane(), &[], &cables, &saved, None, None, None).unwrap();
        assert!(text.starts_with("[keys]\nui = { x = 0, y = 40 }\n\n[[device]]\nname = \"Launchkey\"\nui = { x = -20, y = 300.5 }\n\n[[device]]\nname = \"Seaboard\"\n\n[[instrument]]"), "{text}");
        assert!(text.contains("[[cable]]\nfrom = \"keys\"\nto = \"instrument[0]\"\n\n[[cable]]\nfrom = \"device[1]\"\nto = \"instrument[0]\"\n"), "{text}");
        assert!(!text.contains("gain"), "a cable of notes has no level: {text}");
        let read = parse(&text).unwrap();
        assert_eq!(read.keys.map(|keys| keys.ui), Some(Some(NodeUi { x: 0.0, y: 40.0 })));
        assert_eq!(read.devices.iter().map(|device| (device.name.as_str(), device.ui)).collect::<Vec<_>>(), [("Launchkey", Some(NodeUi { x: -20.0, y: 300.5 })), ("Seaboard", None)]);
        assert_eq!(read.cables.iter().map(|cable| (cable.from, cable.to)).collect::<Vec<_>>(), [(CablePlace::Keys, CablePlace::Instrument(0)), (CablePlace::Device(1), CablePlace::Instrument(0))]);
    }

    #[test]
    fn the_keys_are_written_as_a_table_with_no_place_and_a_session_with_no_source_writes_none() {
        let text = to_toml(&one_lane(), &[], &[], &sources(Some(None), &[]), None, None, None).unwrap();
        assert!(text.starts_with("[keys]\n\n[[instrument]]"), "{text}");
        assert_eq!(parse(&text).unwrap().keys.map(|keys| keys.ui), Some(None));
        let plain = to_toml(&one_lane(), &[], &[], &Default::default(), None, None, None).unwrap();
        assert!(!plain.contains("keys") && !plain.contains("device"), "a session with none is the file it was: {plain}");
    }

    #[test]
    fn a_tempo_that_is_not_a_number_is_refused() {
        for text in ["bpm = \"fast\"", "bpm = true", "bpm = nan", "bpm = inf"] {
            assert!(parse(&format!("{text}\n[[instrument]]\nplugin = \"builtin:osc\"\n")).is_err(), "{text}");
        }
    }

    #[test]
    fn save_and_reload_pattern_transpose_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pattern.toml");

        let instruments = vec![SaveInstrumentSlot {
            muted: false,
            solo: false,
            range: None,
            transpose: 0,
            group: None,
            instrument: Some(SaveInstrument {
                pinned: vec![],
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: Some(SavePattern {
                bpm: 120.0,
                length_beats: 4.0,
                looping: true,
                base_note: Some(60),
                events: vec![(0, 0x90, 60, 100), (24000, 0x80, 60, 0)],
                enabled: true,
                in_key: true,
            }),
            ui: None,
            route: Route::Out,
        }];
        save(&path, &instruments, &[], &[], &Default::default(), None, None, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("transpose = \"in_key\""));

        let config = load(path.to_str().unwrap()).unwrap();
        let pattern = config.instruments[0].pattern.as_ref().expect("pattern");
        assert!(pattern.in_key);

        // Chromatic (default) round-trips without writing the field.
        let mut instruments = instruments;
        instruments[0].pattern.as_mut().unwrap().in_key = false;
        save(&path, &instruments, &[], &[], &Default::default(), None, None, None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("transpose"));
        let config = load(path.to_str().unwrap()).unwrap();
        assert!(!config.instruments[0].pattern.as_ref().unwrap().in_key);
    }

    #[test]
    fn pattern_transpose_rejects_unknown_value() {
        let toml = r#"
[[instrument]]
plugin = "builtin:sine"

[instrument.pattern]
transpose = "dorian"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad_transpose.toml");
        std::fs::write(&path, toml).unwrap();
        let err = match load(path.to_str().unwrap()) {
            Err(e) => e,
            Ok(_) => panic!("expected load to fail on unknown transpose value"),
        };
        assert!(err.to_string().contains("transpose"));
    }

    #[test]
    fn load_session_with_modulators() {
        let toml = r#"
[[instrument]]
plugin = "builtin:sine"

[[instrument.modulator]]
waveform = "triangle"
rate = 0.5

[[instrument.modulator.target]]
param = "frequency"
depth = 0.3
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_mod.toml");
        std::fs::write(&path, toml).unwrap();

        // What `load` gives is the session as the program has it: the lane's modulator is the session's own, and hears the lane.
        let config = load(path.to_str().unwrap()).unwrap();
        assert!(config.instruments[0].modulators.is_empty());
        assert_eq!(config.free_modulators.len(), 1);
        let m = &config.free_modulators[0];
        assert_eq!(m.waveform, "triangle");
        assert!((m.rate.unwrap() - 0.5).abs() < 0.01);
        assert_eq!(m.hears.as_deref(), Some("instrument[0]"));
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("frequency"));
        assert!((m.targets[0].depth - 0.3).abs() < 0.01);
        // What the file says is what `parse_raw` reads, in the lane.
        let raw = parse_raw(toml).unwrap();
        assert_eq!(raw.instruments[0].modulators.len(), 1);
        assert_eq!(raw.instruments[0].modulators[0].targets[0].param.as_deref(), Some("frequency"));
    }

    // -- Every modulator the session's own (step 94) --

    const RACKS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:reverb"

[[group.modulator]]
[[group.modulator.target]]
member = 1
slot = 1
param = "cutoff"
[[group.modulator.target]]
member = 5
param = "detune"
[[group.modulator.target]]
bus = 0
param = "damping"
[[group.modulator.target]]
member = 0
mod_rate = 1
[[group.modulator.target]]
member = 9
mod_attack = 0
[[group.modulator.target]]
mod_rate = 1
[[group.modulator.target]]
mod_rate = 2
[[group.modulator.target]]
bus = 0
mod_rate = 0
[[group.modulator]]
type = "envelope"
[[group.modulator.target]]
mod_depth = []
[[group.modulator.target]]
mod_depth = [0, 3]

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
[[instrument.modulator.target]]
param = "detune"
[[instrument.modulator.target]]
param = "cutoff"
slot = 2
[[instrument.modulator]]
[[instrument.modulator.target]]
mod_rate = 0
[[instrument.modulator.target]]
mod_rate = 7
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect.modulator]]
[[instrument.effect.modulator.target]]
param = "cutoff"
slot = 9
[[instrument.effect.modulator.target]]
mod_rate = 0

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
hears = "group[0]"
ui = { x = 4, y = 5 }
[[instrument.modulator.target]]
param = "cutoff"
slot = 1
[[instrument.modulator.target]]
mod_release = 0

[[modulator]]
hears = "instrument[1]"
[[modulator.target]]
to = "instrument[0]"
param = "detune"
[[modulator.target]]
mod_rate = 1
[[modulator]]
[[modulator.target]]
mod_rate = 0
[[modulator.target]]
mod_depth = [0, 1]
[[modulator.target]]
mod_rate = 2
"#;

    fn moved() -> SessionConfig {
        let mut config = parse(RACKS).unwrap();
        config.move_modulators_to_the_session();
        config
    }

    #[test]
    fn the_modulators_of_the_lanes_and_the_groups_are_moved_in_front_of_the_sessions_own_each_hearing_the_rack_it_was_in() {
        let config = moved();
        // The first lane has three (its own two, and the one that an older file nested in its filter), the second has one, the group has two, and the file has two of its own.
        assert!(config.instruments.iter().all(|lane| lane.modulators.is_empty() && lane.effects.iter().all(|effect| effect.modulators.is_empty())));
        assert!(config.groups.iter().all(|group| group.modulators.is_empty()));
        let hears: Vec<Option<&str>> = config.free_modulators.iter().map(|m| m.hears.as_deref()).collect();
        assert_eq!(hears, [Some("instrument[0]"), Some("instrument[0]"), Some("instrument[0]"), Some("instrument[1]"), Some("group[0]"), Some("group[0]"), Some("instrument[1]"), None]);
        // (A lane's modulator is read as the lane's: the `hears` that this one says is not read.)
        assert_eq!(config.free_modulators[3].ui.map(|ui| (ui.x, ui.y)), Some((4.0, 5.0)), "it keeps its place on the canvas");
    }

    #[test]
    fn a_lanes_cables_name_the_plugins_of_the_lane_by_their_places_and_its_siblings_by_their_places_in_the_new_list() {
        let config = moved();
        let target = |m: usize, t: usize| &config.free_modulators[m].targets[t];
        assert_eq!((target(0, 0).to.as_deref(), target(0, 0).param.as_deref()), (Some("instrument[0]"), Some("detune")));
        assert_eq!(target(0, 1).to.as_deref(), Some("instrument[0].effect[1]"), "slot 2 is the second effect");
        // The second modulator of the lane drives the first; the one after the end of the rack is where no modulator is.
        assert_eq!((target(1, 0).mod_rate, target(1, 1).mod_rate), (Some(0), Some(NOT_THERE)));

        // The one that an older file nested in the filter drives the filter (slot 1, the effect, whatever its cable says), and its sibling is its own first.
        assert_eq!(target(2, 0).to.as_deref(), Some("instrument[0].effect[0]"));
        assert_eq!(target(2, 1).mod_rate, Some(2), "the third of the new list, which is the one that the cable is in: a cable to itself, which is left out");
        // The second lane's modulator drives its own filter, and (every field of an envelope that a cable can drive is a place, as the rate is) the stage of its own first: the fourth of the new list.
        assert_eq!(target(3, 0).to.as_deref(), Some("instrument[1].effect[0]"));
        assert_eq!(target(3, 1).mod_release, Some(3));
    }

    #[test]
    fn a_groups_cables_name_the_members_plugins_and_the_bus_and_the_modulators_of_the_members_by_their_places() {
        let config = moved();
        let target = |t: usize| &config.free_modulators[4].targets[t];
        assert_eq!(target(0).to.as_deref(), Some("instrument[1].effect[0]"), "member 1 is the second lane of the group, slot 1 its filter");
        assert_eq!(target(1).to, None, "member 5 is not there: the cable names no plugin and is left out");
        assert_eq!(target(2).to.as_deref(), Some("group[0].effect[0]"));
        // A member's modulator (the first lane's second, which is the second of the new list), and one that the member does not have, and a member that is not there.
        assert_eq!(target(3).mod_rate, Some(1));
        assert_eq!(target(4).mod_attack, Some(NOT_THERE));
        // A sibling of the group (the group's second, which is the sixth of the list), and one that is not there.
        assert_eq!((target(5).mod_rate, target(6).mod_rate), (Some(5), Some(NOT_THERE)));
        // A bus with no parameter is left out whatever else it says.
        assert_eq!(target(7).mod_rate, Some(NOT_THERE));
        // A pair that has no modulator in it names the first, as the loader reads it.
        let envelope = &config.free_modulators[5];
        assert_eq!(envelope.targets[0].mod_depth, Some(vec![4]));
        assert_eq!(envelope.targets[1].mod_depth, Some(vec![4, 3]), "the place of the cable is the file's, which is the same");
    }

    #[test]
    fn the_modulators_the_file_has_of_its_own_name_each_other_by_their_places_in_the_file_which_are_further_on_now() {
        let config = moved();
        let (first, second) = (&config.free_modulators[6], &config.free_modulators[7]);
        assert_eq!(first.targets[1].mod_rate, Some(7), "the file's second one, which is the last of eight");
        assert_eq!((second.targets[0].mod_rate, second.targets[1].mod_depth.clone(), second.targets[2].mod_rate), (Some(6), Some(vec![6, 1]), Some(8)));
        // (A cable of its own to its plugin is as it was.)
        assert_eq!(first.targets[0].to.as_deref(), Some("instrument[0]"));
    }

    #[test]
    fn a_groups_member_is_counted_among_the_lanes_of_that_group_and_not_among_the_lanes_that_are_in_some_group() {
        // The second group's modulator drives member 0 of that group, which is the second lane: the first lane is in the first group.
        let text = "[[group]]\n[[group]]\n[[group.modulator]]\n[[group.modulator.target]]\nmember = 0\nparam = \"detune\"\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n";
        let mut config = parse(text).unwrap();
        config.move_modulators_to_the_session();
        assert_eq!(config.free_modulators.len(), 1);
        assert_eq!(config.free_modulators[0].hears.as_deref(), Some("group[1]"));
        assert_eq!(config.free_modulators[0].targets[0].to.as_deref(), Some("instrument[1]"));
    }

    #[test]
    fn a_session_with_no_modulator_in_a_lane_or_a_group_is_moved_to_itself() {
        let text = "[[instrument]]\nplugin = \"builtin:osc\"\n[[modulator]]\nhears = \"instrument[0]\"\n[[modulator.target]]\nmod_rate = 0\n[[modulator]]\n[[modulator.target]]\nmod_rate = 0\n";
        let mut config = parse(text).unwrap();
        config.move_modulators_to_the_session();
        assert_eq!(config.free_modulators.len(), 2);
        assert_eq!(config.free_modulators[0].hears.as_deref(), Some("instrument[0]"));
        assert_eq!((config.free_modulators[0].targets[0].mod_rate, config.free_modulators[1].targets[0].mod_rate), (Some(0), Some(0)));
    }

    // -- the modules that were added as one thing (`[[module]]`, step 108) --

    #[test]
    fn a_member_of_a_module_is_a_lane_or_a_modulator_by_its_place_and_nothing_else_is_a_place() {
        for (text, place) in [("instrument[0]", MemberPlace::Lane(0)), ("instrument[12]", MemberPlace::Lane(12)), ("modulator[3]", MemberPlace::Modulator(3)), ("  modulator[0] ", MemberPlace::Modulator(0))] {
            assert_eq!(MemberPlace::parse(text), Some(place), "{text:?}");
            assert_eq!(MemberPlace::parse(&place.to_string()), Some(place), "what is written is read back");
        }
        assert_eq!((MemberPlace::Lane(2).to_string(), MemberPlace::Modulator(7).to_string()), ("instrument[2]".to_string(), "modulator[7]".to_string()));
        for text in [
            "", "instrument", "instrument[]", "instrument[x]", "instrument[-1]", "instrument[+1]", "instrument[ 1]", "instrument[1", "instrument 1]", "Instrument[0]", "modulator", "modulator[0].effect[0]", "instrument[0].effect[0]", "group[0]", "master", "keys",
            "device[0]", "effect[0]", "instrument[99999999999999999999999]",
        ] {
            assert_eq!(MemberPlace::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_module_is_called_by_its_name_without_control_characters_and_the_spaces_round_it_and_by_a_default_if_that_is_nothing() {
        assert_eq!(module_name("  Room  "), "Room");
        assert_eq!(module_name("Big\tRoom\n"), "BigRoom");
        assert_eq!(module_name("a b  c"), "a b  c", "the spaces inside a name are its own");
        for blank in ["", "   ", "\t\n", "\u{7}"] {
            assert_eq!(module_name(blank), DEFAULT_MODULE_NAME, "{blank:?}");
        }
        let long = "x".repeat(300);
        assert_eq!(module_name(&long), long, "a name is not cut short: the library's run to a file name's length, and one that was added must be saved and loaded as it is");
    }

    #[test]
    fn a_session_file_has_modules_that_name_their_lanes_and_modulators_by_place_and_are_written_after_the_modulators_and_read_back_the_same() {
        let text = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[modulator]]\n[[module]]\nname = \"  Room \"\nmembers = [\"instrument[1]\", \"modulator[0]\"]\nui = { x = 10, y = 20.5 }\n[[module]]\nmembers = [\"instrument[0]\"]\n";
        let config = parse(text).unwrap();
        assert_eq!(config.modules.len(), 2);
        assert_eq!((config.modules[0].name.as_str(), config.modules[0].members.clone(), config.modules[0].ui), ("Room", vec![MemberPlace::Lane(1), MemberPlace::Modulator(0)], Some(NodeUi { x: 10.0, y: 20.5 })));
        assert_eq!((config.modules[1].name.as_str(), config.modules[1].members.clone(), config.modules[1].ui), (DEFAULT_MODULE_NAME, vec![MemberPlace::Lane(0)], None), "a module that has no name is called by the default");
        // Written: after the modulators and before the cables, a table for each, with the name, the members as text and the place where there is one.
        let saved = SaveSources {
            modules: vec![
                SaveModule { name: "Room".to_string(), members: vec![MemberPlace::Lane(1), MemberPlace::Modulator(0)], ui: Some(NodeUi { x: 10.0, y: 20.5 }) },
                SaveModule { name: "Plain".to_string(), members: vec![MemberPlace::Lane(0)], ui: None },
            ],
            ..SaveSources::default()
        };
        let written = to_toml(&one_lane(), &[], &[cable(CablePlace::Instrument(0), CablePlace::Master, 1.0)], &saved, None, None, None).unwrap();
        let (modules, cables) = (written.find("[[module]]").expect("the modules are written"), written.find("[[cable]]").expect("and the cable"));
        assert!(written.find("[[instrument]]").unwrap() < modules && modules < cables, "{written}");
        assert!(written.contains("[[module]]\nname = \"Room\"\nmembers = [\"instrument[1]\", \"modulator[0]\"]\nui = { x = 10, y = 20.5 }\n\n[[module]]\nname = \"Plain\"\nmembers = [\"instrument[0]\"]\n"), "{written}");
        let again = parse(&written).unwrap();
        assert_eq!(again.modules.iter().map(|module| (module.name.clone(), module.members.clone(), module.ui)).collect::<Vec<_>>(), [("Room".to_string(), vec![MemberPlace::Lane(1), MemberPlace::Modulator(0)], Some(NodeUi { x: 10.0, y: 20.5 })), ("Plain".to_string(), vec![MemberPlace::Lane(0)], None)]);
        // A session that has none writes none, and is the file it was.
        let none = to_toml(&one_lane(), &[], &[], &SaveSources::default(), None, None, None).unwrap();
        assert!(!none.contains("module"), "{none}");
    }

    #[test]
    fn what_a_module_says_that_is_not_a_place_is_left_out_and_logged_and_does_not_refuse_the_session_a_layout_is_not_worth_it() {
        // A member that is a number, a place that is none, a members that is no list, a name that is a number, and a position that is no pair: each is left out, and the session is read.
        let text = "[[instrument]]\nplugin = \"builtin:osc\"\n[[module]]\nname = \"A\"\nmembers = [1, \"instrument[0]\", \"nowhere\", \"modulator[x]\"]\nui = 5\n[[module]]\nname = \"B\"\nmembers = \"instrument[0]\"\n";
        let config = parse(text).unwrap();
        assert_eq!(config.modules.len(), 2);
        assert_eq!((config.modules[0].members.clone(), config.modules[0].ui), (vec![MemberPlace::Lane(0)], None));
        assert!(config.modules[1].members.is_empty(), "a members that is no list is no members");
        // (A name that is not text is a mistake of the file's types, which refuses it as any key of the wrong type does.)
        assert!(parse("[[instrument]]\nplugin = \"builtin:osc\"\n[[module]]\nname = 5\n").is_err());
    }
}
