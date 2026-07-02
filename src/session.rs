use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::plugin::Plugin;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RemapTarget {
    pub note: String,
    pub detune: f64,
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
}

/// A submix group: members are the instruments whose `group` index points here.
pub struct GroupConfig {
    pub name: Option<String>,
    pub volume: f32,
    pub pan: f32,
    pub effects: Vec<EffectConfig>,
    /// Group-scoped modulators. Their targets address member instruments
    /// (`member` + optional `slot`), bus effects (`bus`), or sibling
    /// group modulators (`mod_*`).
    pub modulators: Vec<ModulatorConfig>,
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
    pub instrument: Option<PluginConfig>,
    pub effects: Vec<EffectConfig>,
    pub pattern: Option<PatternConfig>,
    /// Submix group membership: index into the session's groups, or None.
    pub group: Option<usize>,
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
    #[serde(default = "default_rate")]
    pub rate: f64,
    #[serde(default = "default_attack")]
    pub attack: f64,
    #[serde(default = "default_decay")]
    pub decay: f64,
    #[serde(default = "default_sustain")]
    pub sustain: f64,
    #[serde(default = "default_release")]
    pub release: f64,
    /// For `type = "midi"`: the bound CC number (mutually exclusive with
    /// `midi_pitch_bend`). Absent = unbound (armed for learning on load).
    #[serde(default)]
    pub midi_cc: Option<u8>,
    /// For `type = "midi"`: bound to the pitch-bend wheel.
    #[serde(default)]
    pub midi_pitch_bend: bool,
    #[serde(default, rename = "target")]
    pub targets: Vec<ModTargetConfig>,
}

#[derive(Deserialize)]
pub struct ModTargetConfig {
    /// Plugin parameter name (mutually exclusive with mod_* fields).
    #[serde(default)]
    pub param: Option<String>,
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
    #[serde(default, rename = "modulator")]
    pub modulators: Vec<ModulatorConfig>,
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
    #[serde(default)]
    piano: Option<PianoConfig>,
}

#[derive(Deserialize)]
struct GroupRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_volume")]
    volume: f64,
    #[serde(default)]
    pan: f64,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
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
    #[serde(default, rename = "modulator")]
    modulators: Vec<ModulatorConfig>,
    // Slot-level fields.
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    transpose: i8,
    #[serde(default)]
    group: Option<usize>,
    #[serde(default, rename = "effect")]
    effects: Vec<EffectConfig>,
    #[serde(default)]
    pattern: Option<PatternRaw>,
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

fn default_rate() -> f64 {
    1.0
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
    let raw: SessionRaw = toml::from_str(&content)?;
    let groups: Vec<GroupConfig> = raw
        .groups
        .into_iter()
        .map(|g| GroupConfig {
            name: g.name,
            volume: g.volume as f32,
            pan: (g.pan as f32).clamp(-1.0, 1.0),
            effects: g.effects,
            modulators: g.modulators,
        })
        .collect();
    let mut instruments = Vec::new();
    for slot in raw.instruments {
        let range = slot.range.as_deref().map(parse_range).transpose()?;
        let pattern = slot.pattern.map(parse_pattern_raw).transpose()?;
        let instrument = slot.plugin.map(|plugin| PluginConfig {
            plugin,
            preset: slot.preset,
            volume: slot.volume,
            pitch_bend_range: slot.pitch_bend_range,
            remap: slot.remap,
            params: slot.params,
            modulators: slot.modulators,
        });
        // Drop out-of-range group references defensively.
        let group = slot.group.filter(|&g| g < groups.len());
        instruments.push(InstrumentSlotConfig {
            range,
            transpose: slot.transpose,
            instrument,
            effects: slot.effects,
            pattern,
            group,
        });
    }
    Ok(SessionConfig { instruments, groups, piano: raw.piano })
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
    /// Lane-scoped modulators (targets carry their chain slot).
    pub modulators: Vec<SaveModulator>,
    pub instrument: Option<SaveInstrument>,
    pub effects: Vec<SaveEffect>,
    pub pattern: Option<SavePattern>,
    /// Submix group membership: index into the saved groups, or None.
    pub group: Option<usize>,
}

/// Data needed to serialize a submix group for saving.
pub struct SaveGroup {
    pub name: Option<String>,
    pub volume: f32,
    pub pan: f32,
    pub effects: Vec<SaveEffect>,
    /// Group-scoped modulators (targets address members / bus effects).
    pub modulators: Vec<SaveModulator>,
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
    Lfo { waveform: String, rate: f32 },
    Envelope { attack: f32, decay: f32, sustain: f32, release: f32 },
    /// A MIDI-learned control. `cc = Some(n)` = CC n, `None` with
    /// `pitch_bend = true` = pitch bend, `None`/false = unbound (still learning).
    MidiLearn { cc: Option<u8>, pitch_bend: bool },
}

pub struct SaveModulator {
    pub source: SaveModSource,
    pub targets: Vec<SaveModTarget>,
}

/// Data needed to serialize a modulation target for saving.
pub struct SaveModTarget {
    pub kind: crate::plugin::chain::ModTargetKind,
    pub label: String,
    pub depth: f32,
    /// Chain slot the target lives on (0 = instrument, 1..N = effect).
    pub slot: usize,
}

/// Data needed to serialize an instrument plugin for saving.
pub struct SaveInstrument {
    pub plugin: String,
    pub volume: f32,
    pub preset: Option<String>,
    pub params: Vec<(String, f32)>,
    pub pitch_bend_range: f64,
    pub remap: HashMap<String, RemapTarget>,
}

/// Data needed to serialize an effect slot for saving.
pub struct SaveEffect {
    pub plugin: String,
    pub mix: f32,
    pub preset: Option<String>,
    pub params: Vec<(String, f32)>,
}

/// Serialization output: flat [[instrument]] format.
#[derive(Serialize)]
struct SessionOut {
    #[serde(rename = "instrument")]
    instruments: Vec<InstrumentSlotOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "group")]
    groups: Vec<GroupOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    piano: Option<PianoOut>,
}

#[derive(Serialize)]
struct GroupOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "is_default_volume_f32")]
    volume: f32,
    #[serde(skip_serializing_if = "is_default_pan_f32")]
    pan: f32,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
}

fn is_default_volume_f32(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

fn is_default_pan_f32(v: &f32) -> bool {
    v.abs() < f32::EPSILON
}

#[derive(Serialize)]
struct PianoOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<String>,
}

/// One [[instrument]] table in the output. Plugin fields are at the top level.
#[derive(Serialize)]
struct InstrumentSlotOut {
    // Instrument plugin fields (top-level).
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin: Option<String>,
    #[serde(skip_serializing_if = "is_default_volume_f32_opt")]
    volume: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "is_default_pitch_bend_range_opt")]
    pitch_bend_range: Option<f64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    remap: BTreeMap<String, RemapTarget>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "modulator")]
    modulators: Vec<ModulatorOut>,
    // Slot-level fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<String>,
    #[serde(skip_serializing_if = "is_zero_i8")]
    transpose: i8,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "effect")]
    effects: Vec<EffectOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pattern: Option<PatternOut>,
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

#[derive(Serialize)]
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
    midi_cc: Option<u8>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    midi_pitch_bend: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "target")]
    targets: Vec<ModTargetOut>,
}

fn is_lfo_type(s: &String) -> bool {
    s == "lfo"
}

#[derive(Serialize)]
struct ModTargetOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<String>,
    /// Chain slot for a plugin-param target (omitted when 0 = instrument).
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<usize>,
    /// (Group modulators) member ordinal a `param` target lands on.
    #[serde(skip_serializing_if = "Option::is_none")]
    member: Option<usize>,
    /// (Group modulators) bus-effect index a `param` target lands on.
    #[serde(skip_serializing_if = "Option::is_none")]
    bus: Option<usize>,
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
    #[serde(skip_serializing_if = "is_default_mix_f32")]
    mix: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    params: BTreeMap<String, f64>,
}

fn save_mod_target_to_out(t: &SaveModTarget) -> ModTargetOut {
    use crate::plugin::chain::ModTargetKind;
    let mut out = ModTargetOut {
        param: None,
        slot: None,
        member: None,
        bus: None,
        mod_rate: None,
        mod_depth: None,
        mod_attack: None,
        mod_decay: None,
        mod_sustain: None,
        mod_release: None,
        depth: t.depth as f64,
    };
    match &t.kind {
        ModTargetKind::PluginParam { .. } => {
            out.param = Some(t.label.clone());
            // 0 (instrument) is the default and omitted.
            if t.slot != 0 {
                out.slot = Some(t.slot);
            }
        }
        ModTargetKind::GroupMember { member, slot, .. } => {
            out.param = Some(t.label.clone());
            out.member = Some(*member);
            // 0 (member's instrument) is the default and omitted.
            if *slot != 0 {
                out.slot = Some(*slot);
            }
        }
        ModTargetKind::GroupBus { effect_index, .. } => {
            out.param = Some(t.label.clone());
            out.bus = Some(*effect_index);
        }
        ModTargetKind::GroupMemberMod { member, mod_index, field } => {
            use crate::plugin::chain::CrossModField;
            // Serialized as `member` (discriminator) + the matching mod_* field
            // carrying the member modulator's index.
            out.member = Some(*member);
            match field {
                CrossModField::Rate => out.mod_rate = Some(*mod_index),
                CrossModField::Attack => out.mod_attack = Some(*mod_index),
                CrossModField::Decay => out.mod_decay = Some(*mod_index),
                CrossModField::Sustain => out.mod_sustain = Some(*mod_index),
                CrossModField::Release => out.mod_release = Some(*mod_index),
                CrossModField::Depth(ti) => out.mod_depth = Some(vec![*mod_index, *ti]),
            }
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
            match &m.source {
                SaveModSource::Lfo { waveform, rate } => ModulatorOut {
                    mod_type: "lfo".into(),
                    waveform: Some(waveform.clone()),
                    rate: Some(*rate as f64),
                    attack: None,
                    decay: None,
                    sustain: None,
                    release: None,
                    midi_cc: None,
                    midi_pitch_bend: false,
                    targets,
                },
                SaveModSource::Envelope { attack, decay, sustain, release } => ModulatorOut {
                    mod_type: "envelope".into(),
                    waveform: None,
                    rate: None,
                    attack: Some(*attack as f64),
                    decay: Some(*decay as f64),
                    sustain: Some(*sustain as f64),
                    release: Some(*release as f64),
                    midi_cc: None,
                    midi_pitch_bend: false,
                    targets,
                },
                SaveModSource::MidiLearn { cc, pitch_bend } => ModulatorOut {
                    mod_type: "midi".into(),
                    waveform: None,
                    rate: None,
                    attack: None,
                    decay: None,
                    sustain: None,
                    release: None,
                    midi_cc: *cc,
                    midi_pitch_bend: *pitch_bend,
                    targets,
                },
            }
        })
        .collect()
}

/// Save the current session state to a TOML file.
fn effect_to_out(fx: &SaveEffect) -> EffectOut {
    EffectOut {
        plugin: fx.plugin.clone(),
        mix: fx.mix,
        preset: fx.preset.clone(),
        params: fx.params.iter().map(|(k, v)| (k.clone(), clean_f64(*v))).collect(),
    }
}

pub fn save(
    path: &Path,
    instruments: &[SaveInstrumentSlot],
    groups: &[SaveGroup],
    piano: Option<&PianoConfig>,
) -> anyhow::Result<()> {
    let session = SessionOut {
        piano: piano.map(|p| PianoOut { scale: p.scale.clone() }),
        groups: groups
            .iter()
            .map(|g| GroupOut {
                name: g.name.clone(),
                volume: g.volume,
                pan: g.pan,
                effects: g.effects.iter().map(effect_to_out).collect(),
                modulators: mods_to_out(&g.modulators),
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
                    params,
                    pitch_bend_range,
                    remap,
                    // Lane modulators serialize at the instrument level.
                    modulators: mods_to_out(&slot.modulators),
                    range: slot.range.map(format_range),
                    transpose: slot.transpose,
                    group: slot.group,
                    effects: slot.effects.iter().map(effect_to_out).collect(),
                    pattern: slot.pattern.as_ref().map(|p| {
                        PatternOut {
                            bpm: p.bpm as f64,
                            length_beats: p.length_beats as f64,
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
                }
            })
            .collect(),
    };

    let content = toml::to_string_pretty(&session)?;
    let content = inline_remap_entries(&content)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Walk every `[[instrument]]` and rewrite each `remap` child so its entries
/// are inline tables — `"F#2" = { note = "F2", detune = 1.0 }` rather than the
/// default toml-rs block-table form. Non-instrument tables are untouched.
fn inline_remap_entries(input: &str) -> anyhow::Result<String> {
    use toml_edit::{DocumentMut, InlineTable, Item, Value};

    let mut doc: DocumentMut = input.parse()?;
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
                range: Some((12, 59)), // C0-B3
                transpose: 0,
                group: None,
                modulators: vec![],
                instrument: Some(SaveInstrument {
                    plugin: "builtin:sine".into(),
                    volume: 0.8,
                    preset: None,
                    params: vec![("cutoff".into(), 0.75)],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![SaveEffect {
                    plugin: "builtin:sine".into(),
                    mix: 0.5,
                    preset: None,
                    params: vec![],
                }],
                pattern: None,
            },
            SaveInstrumentSlot {
                range: None,
                transpose: 0,
                group: None,
                modulators: vec![],
                instrument: Some(SaveInstrument {
                    plugin: "builtin:sine".into(),
                    volume: 1.0,
                    preset: None,
                    params: vec![],
                    pitch_bend_range: 2.0,
                    remap: HashMap::new(),
                }),
                effects: vec![],
                pattern: None,
            },
        ];

        save(&path, &instruments, &[], None).unwrap();

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
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: Some("Lead".into()),
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![SaveEffect {
                plugin: "builtin:reverb".into(),
                mix: 0.4,
                preset: Some("Arcadia Dream Hall".into()),
                params: vec![],
            }],
            pattern: None,
        }];

        save(&path, &instruments, &[], None).unwrap();

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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod_test.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::Lfo {
                    waveform: "sine".into(),
                    rate: 2.5,
                },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 0.75,
                    slot: 0,
                }],
            }],
            instrument: Some(SaveInstrument {
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
        }];

        save(&path, &instruments, &[], None).unwrap();

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.instruments.len(), 1);
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.waveform, "sine");
        assert!((m.rate - 2.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
        assert!((m.targets[0].depth - 0.75).abs() < 0.01);
        // Slot defaults to instrument (0).
        assert_eq!(m.targets[0].slot, 0);
    }

    #[test]
    fn save_and_reload_midi_learn_modulator() {
        // A MIDI-learned modulator (bound to CC 74) must round-trip as
        // `type = "midi"` + `midi_cc = 74`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("midi_learn.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::MidiLearn { cc: Some(74), pitch_bend: false },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 1.0,
                    slot: 0,
                }],
            }],
            instrument: Some(SaveInstrument {
                plugin: "builtin:filter".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
        }];

        save(&path, &instruments, &[], None).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("type = \"midi\""), "expected midi modulator:\n{raw}");
        assert!(raw.contains("midi_cc = 74"), "expected bound CC:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.mod_type, "midi");
        assert_eq!(m.midi_cc, Some(74));
        assert!(!m.midi_pitch_bend);
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
    }

    #[test]
    fn save_and_reload_cross_slot_modulator() {
        // A lane modulator targeting an effect (slot 1) must round-trip the slot.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cross_slot.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![SaveModulator {
                source: SaveModSource::Lfo { waveform: "sine".into(), rate: 1.0 },
                targets: vec![SaveModTarget {
                    kind: crate::plugin::chain::ModTargetKind::PluginParam { slot: 1, param_index: 0 },
                    label: "cutoff".into(),
                    depth: 0.5,
                    slot: 1,
                }],
            }],
            instrument: Some(SaveInstrument {
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![SaveEffect {
                plugin: "builtin:filter".into(),
                mix: 1.0,
                preset: None,
                params: vec![],
            }],
            pattern: None,
        }];
        save(&path, &instruments, &[], None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("slot = 1"), "saved target should record its slot:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.modulators.len(), 1);
        assert_eq!(inst.modulators[0].targets[0].slot, 1);
        assert_eq!(inst.modulators[0].targets[0].param.as_deref(), Some("cutoff"));
    }

    #[test]
    fn save_and_reload_group() {
        // A group with a bus effect + volume, and two member instruments,
        // must round-trip (membership index + group fields).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("group.toml");

        let member = |group: Option<usize>| SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
        };
        let instruments = vec![member(Some(0)), member(Some(0)), member(None)];
        let groups = vec![SaveGroup {
            name: Some("Pad".into()),
            volume: 0.5,
            pan: -0.5,
            effects: vec![SaveEffect {
                plugin: "builtin:reverb".into(),
                mix: 1.0,
                preset: None,
                params: vec![],
            }],
            modulators: vec![],
        }];
        save(&path, &instruments, &groups, None).unwrap();

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
    fn save_and_reload_group_modulator() {
        // A group-scoped modulator with a member target and a bus target must
        // round-trip through `[[group.modulator]]` (member/bus + param name).
        use crate::plugin::chain::ModTargetKind;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("group_mod.toml");

        let member = SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: Some(0),
            modulators: vec![],
            instrument: Some(SaveInstrument {
                plugin: "builtin:osc".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
        };
        let groups = vec![SaveGroup {
            name: None,
            volume: 1.0,
            pan: 0.0,
            effects: vec![SaveEffect {
                plugin: "builtin:filter".into(),
                mix: 1.0,
                preset: None,
                params: vec![],
            }],
            modulators: vec![SaveModulator {
                source: SaveModSource::Lfo { waveform: "sine".into(), rate: 2.0 },
                targets: vec![
                    SaveModTarget {
                        kind: ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 },
                        label: "cutoff".into(),
                        depth: 0.5,
                        slot: 0,
                    },
                    SaveModTarget {
                        kind: ModTargetKind::GroupBus { effect_index: 0, param_index: 0 },
                        label: "cutoff".into(),
                        depth: 0.25,
                        slot: 0,
                    },
                    // Group→member-modulator cross-mod: member 0's modulator 0 rate.
                    SaveModTarget {
                        kind: ModTargetKind::GroupMemberMod {
                            member: 0,
                            mod_index: 0,
                            field: crate::plugin::chain::CrossModField::Rate,
                        },
                        label: "M0 Mod0 rate".into(),
                        depth: 0.3,
                        slot: 0,
                    },
                ],
            }],
        }];
        save(&path, &[member], &groups, None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[[group.modulator]]"), "expected group modulator:\n{raw}");
        assert!(raw.contains("member = 0"), "member target should record member:\n{raw}");
        assert!(raw.contains("bus = 0"), "bus target should record bus index:\n{raw}");

        let config = load(path.to_str().unwrap()).unwrap();
        assert_eq!(config.groups[0].modulators.len(), 1);
        let m = &config.groups[0].modulators[0];
        assert_eq!(m.targets.len(), 3);
        assert_eq!(m.targets[0].member, Some(0));
        assert_eq!(m.targets[0].param.as_deref(), Some("cutoff"));
        assert_eq!(m.targets[1].bus, Some(0));
        assert_eq!(m.targets[1].param.as_deref(), Some("cutoff"));
        // The member-modulator target: member set, mod_rate carries the index,
        // and no param name.
        assert_eq!(m.targets[2].member, Some(0));
        assert_eq!(m.targets[2].mod_rate, Some(0));
        assert_eq!(m.targets[2].param, None);
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
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap,
            }),
            effects: vec![],
            pattern: None,
        }];

        save(&path, &instruments, &[], None).unwrap();

        // Output should use inline tables and stable alphabetical ordering.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(r#""F#2" = { note = "F2", detune = 1.0}"#)
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

    #[test]
    fn save_and_reload_with_piano_scale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("piano.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
                plugin: "builtin:sine".into(),
                volume: 1.0,
                preset: None,
                params: vec![],
                pitch_bend_range: 2.0,
                remap: HashMap::new(),
            }),
            effects: vec![],
            pattern: None,
        }];
        let piano = PianoConfig { scale: Some("F# dorian".into()), locked: false };
        save(&path, &instruments, &[], Some(&piano)).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("[piano]"));
        assert!(raw.contains("scale = \"F# dorian\""));

        let config = load(path.to_str().unwrap()).unwrap();
        let piano_cfg = config.piano.expect("piano section");
        assert_eq!(piano_cfg.scale.as_deref(), Some("F# dorian"));
    }

    #[test]
    fn save_and_reload_pattern_transpose_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pattern.toml");

        let instruments = vec![SaveInstrumentSlot {
            range: None,
            transpose: 0,
            group: None,
            modulators: vec![],
            instrument: Some(SaveInstrument {
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
        }];
        save(&path, &instruments, &[], None).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("transpose = \"in_key\""));

        let config = load(path.to_str().unwrap()).unwrap();
        let pattern = config.instruments[0].pattern.as_ref().expect("pattern");
        assert!(pattern.in_key);

        // Chromatic (default) round-trips without writing the field.
        let mut instruments = instruments;
        instruments[0].pattern.as_mut().unwrap().in_key = false;
        save(&path, &instruments, &[], None).unwrap();
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

        let config = load(path.to_str().unwrap()).unwrap();
        let inst = config.instruments[0].instrument.as_ref().unwrap();
        assert_eq!(inst.modulators.len(), 1);
        let m = &inst.modulators[0];
        assert_eq!(m.waveform, "triangle");
        assert!((m.rate - 0.5).abs() < 0.01);
        assert_eq!(m.targets.len(), 1);
        assert_eq!(m.targets[0].param.as_deref(), Some("frequency"));
        assert!((m.targets[0].depth - 0.3).abs() < 0.01);
    }
}
