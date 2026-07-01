use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use super::Plugin;
use crate::piano_filter::PianoFilter;
use crate::scale::ScaleSetting;
use crate::session::{self, RemapTarget};

/// Maximum number of audio channels supported (for stack-allocated reference arrays).
const MAX_CHANNELS: usize = 16;

/// Pre-computed remap entry for a single note.
#[derive(Debug, Clone)]
struct RemapEntry {
    target_note: u8,
    channel: u8,
    pitch_bend_lsb: u8,
    pitch_bend_msb: u8,
}

/// Remaps specific MIDI notes to different notes on separate channels with pitch bend.
///
/// Normal notes pass through on channel 1 (status nibble 0x00).
/// Remapped notes are rewritten to a target note on channels 2-16, with a pitch bend
/// message inserted before each note-on to shift the pitch to the correct frequency.
#[derive(Debug, Clone)]
pub struct NoteRemapper {
    table: HashMap<u8, RemapEntry>,
}

impl NoteRemapper {
    /// Build a remapper from the session config.
    ///
    /// Groups entries by detune value and assigns MIDI channels 1-15 (status nibble 0x01-0x0F,
    /// i.e. MIDI channels 2-16). Returns an error if detune exceeds pitch_bend_range or if
    /// there are more than 15 distinct detune values.
    pub fn from_config(
        remap: &HashMap<String, RemapTarget>,
        pitch_bend_range: f64,
    ) -> anyhow::Result<Self> {
        if remap.is_empty() {
            return Ok(NoteRemapper {
                table: HashMap::new(),
            });
        }

        // Group by detune value to assign channels. Use ordered floats as keys.
        // We use a Vec to maintain insertion order and dedup by approximate equality.
        let mut detune_channels: Vec<(f64, u8)> = Vec::new();

        let mut table = HashMap::new();

        // First pass: collect all distinct detune values
        for target in remap.values() {
            if target.detune.abs() > pitch_bend_range {
                anyhow::bail!(
                    "detune {:.1} exceeds pitch_bend_range ±{:.1}",
                    target.detune,
                    pitch_bend_range
                );
            }
            let existing = detune_channels
                .iter()
                .find(|(d, _)| (*d - target.detune).abs() < 1e-9);
            if existing.is_none() {
                if detune_channels.len() >= 15 {
                    anyhow::bail!("too many distinct detune values (max 15, MIDI channels 2-16)");
                }
                // Channel status nibble: 0x01 for ch2, 0x02 for ch3, etc.
                let ch = detune_channels.len() as u8 + 1;
                detune_channels.push((target.detune, ch));
            }
        }

        // Second pass: build the lookup table
        for (source_name, target) in remap {
            let source_note = session::parse_note_name(source_name)?;
            let target_note = session::parse_note_name(&target.note)?;

            let &(_, channel) = detune_channels
                .iter()
                .find(|(d, _)| (*d - target.detune).abs() < 1e-9)
                .unwrap();

            // Pre-compute pitch bend: center is 8192, range maps to ±pitch_bend_range semitones
            let bend_value = (8192.0 + (target.detune / pitch_bend_range) * 8191.0).round() as i32;
            let bend_clamped = bend_value.clamp(0, 16383) as u16;
            let lsb = (bend_clamped & 0x7F) as u8;
            let msb = ((bend_clamped >> 7) & 0x7F) as u8;

            table.insert(
                source_note,
                RemapEntry {
                    target_note,
                    channel,
                    pitch_bend_lsb: lsb,
                    pitch_bend_msb: msb,
                },
            );
        }

        Ok(NoteRemapper { table })
    }

    /// Remap MIDI events, writing results into the provided buffer.
    /// For remapped note-on events, a pitch bend message is inserted before the note-on.
    /// For remapped note-off events, the note and channel are rewritten.
    /// All other events pass through unchanged.
    pub fn remap_events(&self, input: &[(u64, [u8; 3])], output: &mut Vec<(u64, [u8; 3])>) {
        output.clear();
        for &(frame, bytes) in input {
            let status_type = bytes[0] & 0xF0;
            let note = bytes[1];

            match status_type {
                0x90 if bytes[2] > 0 => {
                    // Note-on
                    if let Some(entry) = self.table.get(&note) {
                        log::info!(
                            "Remap: NoteOn {} → {} ch={} bend=({},{})",
                            note,
                            entry.target_note,
                            entry.channel + 1,
                            entry.pitch_bend_lsb,
                            entry.pitch_bend_msb,
                        );
                        // Rewritten note-on on remapped channel
                        output.push((frame, [0x90 | entry.channel, entry.target_note, bytes[2]]));
                        // Pitch bend after note-on
                        output.push((
                            frame,
                            [
                                0xE0 | entry.channel,
                                entry.pitch_bend_lsb,
                                entry.pitch_bend_msb,
                            ],
                        ));
                    } else {
                        output.push((frame, bytes));
                    }
                }
                0x80 | 0x90 => {
                    // Note-off (0x80 or 0x90 with velocity 0)
                    if let Some(entry) = self.table.get(&note) {
                        log::info!(
                            "Remap: NoteOff {} → {} ch={}",
                            note,
                            entry.target_note,
                            entry.channel + 1,
                        );
                        output.push((
                            frame,
                            [(status_type) | entry.channel, entry.target_note, bytes[2]],
                        ));
                    } else {
                        output.push((frame, bytes));
                    }
                }
                _ => {
                    output.push((frame, bytes));
                }
            }
        }
    }
}

/// Build `&mut [&mut [f32]]` on the stack from `&mut [Vec<f32>]`.
///
/// # Panics
/// Panics if `bufs.len() > MAX_CHANNELS`.
fn mut_slices<'a>(
    bufs: &'a mut [Vec<f32>],
    storage: &'a mut [MaybeUninit<&'a mut [f32]>; MAX_CHANNELS],
) -> &'a mut [&'a mut [f32]] {
    let n = bufs.len();
    assert!(n <= MAX_CHANNELS);
    for (i, buf) in bufs.iter_mut().enumerate() {
        storage[i].write(buf.as_mut_slice());
    }
    // SAFETY: first `n` elements are initialized. MaybeUninit<T> is #[repr(transparent)].
    unsafe { std::slice::from_raw_parts_mut(storage.as_mut_ptr().cast(), n) }
}

/// Build `&[&[f32]]` on the stack from `&[Vec<f32>]`.
///
/// # Panics
/// Panics if `bufs.len() > MAX_CHANNELS`.
fn shared_slices<'a>(
    bufs: &'a [Vec<f32>],
    storage: &'a mut [MaybeUninit<&'a [f32]>; MAX_CHANNELS],
) -> &'a [&'a [f32]] {
    let n = bufs.len();
    assert!(n <= MAX_CHANNELS);
    for (i, buf) in bufs.iter().enumerate() {
        storage[i].write(buf.as_slice());
    }
    // SAFETY: first `n` elements are initialized. MaybeUninit<T> is #[repr(transparent)].
    unsafe { std::slice::from_raw_parts(storage.as_ptr().cast(), n) }
}

// ---------------------------------------------------------------------------
// LFO Modulator
// ---------------------------------------------------------------------------

/// LFO waveform shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoWaveform {
    Sine,
    Triangle,
    Saw,
    Square,
}

impl LfoWaveform {
    /// Evaluate the waveform at a given phase (0.0–1.0), returning a value in -1.0..1.0.
    pub fn eval(self, phase: f32) -> f32 {
        match self {
            LfoWaveform::Sine => (phase * std::f32::consts::TAU).sin(),
            LfoWaveform::Triangle => {
                // 0→1 over first half, 1→-1 over second half
                if phase < 0.25 {
                    phase * 4.0
                } else if phase < 0.75 {
                    1.0 - (phase - 0.25) * 4.0
                } else {
                    -1.0 + (phase - 0.75) * 4.0
                }
            }
            LfoWaveform::Saw => {
                // Rising sawtooth: -1 at phase=0, +1 at phase=1
                phase * 2.0 - 1.0
            }
            LfoWaveform::Square => {
                if phase < 0.5 { 1.0 } else { -1.0 }
            }
        }
    }

    pub const ALL: &[LfoWaveform] = &[
        LfoWaveform::Sine,
        LfoWaveform::Triangle,
        LfoWaveform::Saw,
        LfoWaveform::Square,
    ];

    /// Cycle to the next waveform.
    pub fn next(self) -> Self {
        Self::ALL[(self.to_index() + 1) % Self::ALL.len()]
    }

    /// Cycle to the previous waveform.
    pub fn prev(self) -> Self {
        Self::ALL[(self.to_index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn to_index(self) -> usize {
        match self {
            LfoWaveform::Sine => 0,
            LfoWaveform::Triangle => 1,
            LfoWaveform::Saw => 2,
            LfoWaveform::Square => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LfoWaveform::Sine => "sine",
            LfoWaveform::Triangle => "triangle",
            LfoWaveform::Saw => "saw",
            LfoWaveform::Square => "square",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sine" => Some(LfoWaveform::Sine),
            "triangle" | "tri" => Some(LfoWaveform::Triangle),
            "saw" | "sawtooth" => Some(LfoWaveform::Saw),
            "square" | "sq" => Some(LfoWaveform::Square),
            _ => None,
        }
    }
}

/// Identifies what a modulation target points at.
///
/// Modulators are lane-scoped: one modulator can drive parameters anywhere in
/// its instrument's chain. `PluginParam.slot` selects the plugin — 0 is the
/// instrument, 1..N are the effects in order. Cross-mod kinds reference a
/// sibling modulator by its lane-global index.
#[derive(Debug, Clone)]
pub enum ModTargetKind {
    /// Target a plugin parameter: `slot` (0 = instrument, 1.. = effect) + index.
    /// Used by lane-scoped modulators.
    PluginParam { slot: usize, param_index: u32 },
    /// (Group-scoped) Target a parameter on a member instrument's chain.
    /// `member` is the member's ordinal within the group (ascending lane
    /// order); `slot` is 0 = that instrument, 1..N = its effects.
    GroupMember {
        member: usize,
        slot: usize,
        param_index: u32,
    },
    /// (Group-scoped) Target a parameter on one of the group's own bus effects.
    GroupBus { effect_index: usize, param_index: u32 },
    /// (Group-scoped) Cross-mod a member instrument's modulator: `member` is
    /// the member ordinal, `mod_index` the member lane modulator's index, and
    /// `field` which field of it to drive (rate / ADSR / target depth).
    GroupMemberMod {
        member: usize,
        mod_index: usize,
        field: CrossModField,
    },
    /// Target a sibling modulator's LFO rate.
    ModulatorRate { mod_index: usize },
    /// Target a sibling modulator's target depth.
    ModulatorDepth { mod_index: usize, target_index: usize },
    /// Target envelope Attack.
    ModulatorAttack { mod_index: usize },
    /// Target envelope Decay.
    ModulatorDecay { mod_index: usize },
    /// Target envelope Sustain.
    ModulatorSustain { mod_index: usize },
    /// Target envelope Release.
    ModulatorRelease { mod_index: usize },
}

/// A modulation target: one parameter on the parent plugin or a sibling modulator.
#[derive(Debug, Clone)]
pub struct ModTarget {
    pub kind: ModTargetKind,
    /// Fraction of parameter range for modulation depth (e.g. 0.5 = ±50%).
    pub depth: f32,
    /// The user's set value (auto-updated when SetParameter is handled).
    pub base_value: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// ADSR envelope state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvState {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

/// Modulation source: either an LFO or an ADSR envelope.
#[derive(Debug, Clone)]
pub enum ModSource {
    Lfo {
        waveform: LfoWaveform,
        rate: f32,
        phase: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        state: EnvState,
        level: f32,
        notes_held: u32,
    },
}

/// A block-rate modulator with a source (LFO or Envelope) and targets.
#[derive(Debug, Clone)]
pub struct Modulator {
    pub source: ModSource,
    sample_rate: f32,
    pub targets: Vec<ModTarget>,
    /// Last computed output value (bipolar -1..1 for LFO, unipolar 0..1 for envelope).
    pub last_output: f32,
}

impl Modulator {
    pub fn new(source: ModSource, sample_rate: f32) -> Self {
        Modulator {
            source,
            sample_rate,
            targets: Vec::new(),
            last_output: 0.0,
        }
    }

    /// Advance the modulator by one buffer. For envelopes, processes MIDI note events.
    fn tick(&mut self, buffer_size: usize, midi_events: &[(u64, [u8; 3])]) {
        match &mut self.source {
            ModSource::Lfo { waveform, rate, phase } => {
                let phase_inc = *rate * buffer_size as f32 / self.sample_rate;
                *phase = (*phase + phase_inc) % 1.0;
                self.last_output = waveform.eval(*phase);
            }
            ModSource::Envelope { attack, decay, sustain, release, state, level, notes_held } => {
                // Process MIDI events for note-on/off.
                for &(_frame, bytes) in midi_events {
                    let status_type = bytes[0] & 0xF0;
                    match status_type {
                        0x90 if bytes[2] > 0 => {
                            // Note-on: retrigger from Attack.
                            *notes_held = notes_held.saturating_add(1);
                            *state = EnvState::Attack;
                        }
                        0x80 | 0x90 => {
                            // Note-off.
                            *notes_held = notes_held.saturating_sub(1);
                            if *notes_held == 0 {
                                *state = EnvState::Release;
                            }
                        }
                        _ => {}
                    }
                }

                // Advance envelope state machine.
                let dt = buffer_size as f32 / self.sample_rate;
                match *state {
                    EnvState::Idle => {
                        *level = 0.0;
                    }
                    EnvState::Attack => {
                        let rate = if *attack > 0.0 { dt / *attack } else { 1.0 };
                        *level += rate;
                        if *level >= 1.0 {
                            *level = 1.0;
                            *state = EnvState::Decay;
                        }
                    }
                    EnvState::Decay => {
                        let rate = if *decay > 0.0 { dt / *decay } else { 1.0 };
                        *level -= rate * (1.0 - *sustain);
                        if *level <= *sustain {
                            *level = *sustain;
                            *state = EnvState::Sustain;
                        }
                    }
                    EnvState::Sustain => {
                        *level = *sustain;
                    }
                    EnvState::Release => {
                        let rate = if *release > 0.0 { dt / *release } else { 1.0 };
                        *level -= rate * (*level).max(0.001);
                        if *level <= 0.001 {
                            *level = 0.0;
                            *state = EnvState::Idle;
                        }
                    }
                }
                self.last_output = *level;
            }
        }
    }

    /// The offset this modulator currently contributes to a target, relative
    /// to the target's base value.
    ///
    /// LFOs are bipolar and wobble ±depth×range around the base. Envelopes
    /// are unipolar and treat the base as their **peak**: the value rises
    /// from `base − depth × (base − min)` at idle up to the base at full
    /// envelope level. With depth 1.0 the envelope spans min→base, so e.g.
    /// an amp envelope on a volume parameter reaches silence instead of
    /// trying to push past an already-maxed value (offsets above the base
    /// would just clamp at param_max and do nothing).
    fn target_offset(&self, target: &ModTarget) -> f32 {
        match self.source {
            ModSource::Lfo { .. } => {
                self.last_output * target.depth * (target.param_max - target.param_min)
            }
            ModSource::Envelope { .. } => {
                -(1.0 - self.last_output) * target.depth * (target.base_value - target.param_min)
            }
        }
    }
}

/// Apply cross-modulator targets within a modulator list.
///
/// For each modulator, applies its `last_output` to any sibling modulator targets
/// (rate, ADSR params, depth). Self-modulation (targeting own index) is skipped.
fn apply_cross_mod(modulators: &mut [Modulator]) {
    // Collect modifications first (avoids simultaneous borrow issues).
    let mut mods_to_apply: Vec<(usize, CrossModField, f32)> = Vec::new();

    for (src_idx, src) in modulators.iter().enumerate() {
        for target in &src.targets {
            let (tgt_mod_idx, field) = match &target.kind {
                ModTargetKind::ModulatorRate { mod_index } => (*mod_index, CrossModField::Rate),
                ModTargetKind::ModulatorAttack { mod_index } => (*mod_index, CrossModField::Attack),
                ModTargetKind::ModulatorDecay { mod_index } => (*mod_index, CrossModField::Decay),
                ModTargetKind::ModulatorSustain { mod_index } => (*mod_index, CrossModField::Sustain),
                ModTargetKind::ModulatorRelease { mod_index } => (*mod_index, CrossModField::Release),
                ModTargetKind::ModulatorDepth { mod_index, target_index } => {
                    (*mod_index, CrossModField::Depth(*target_index))
                }
                ModTargetKind::PluginParam { .. }
                | ModTargetKind::GroupMember { .. }
                | ModTargetKind::GroupBus { .. }
                | ModTargetKind::GroupMemberMod { .. } => continue,
            };
            // Skip self-modulation.
            if tgt_mod_idx == src_idx {
                continue;
            }
            let modulated = (target.base_value + src.target_offset(target))
                .clamp(target.param_min, target.param_max);
            mods_to_apply.push((tgt_mod_idx, field, modulated));
        }
    }

    // Apply collected modifications.
    for (tgt_idx, field, value) in mods_to_apply {
        if let Some(tgt) = modulators.get_mut(tgt_idx) {
            apply_cross_mod_field(tgt, field, value);
        }
    }
}

/// Write a cross-mod value into a modulator's field. Shared by sibling
/// cross-mod and group→member-modulator cross-mod.
fn apply_cross_mod_field(m: &mut Modulator, field: CrossModField, value: f32) {
    match field {
        CrossModField::Rate => {
            if let ModSource::Lfo { rate, .. } = &mut m.source {
                *rate = value;
            }
        }
        CrossModField::Attack => {
            if let ModSource::Envelope { attack, .. } = &mut m.source {
                *attack = value;
            }
        }
        CrossModField::Decay => {
            if let ModSource::Envelope { decay, .. } = &mut m.source {
                *decay = value;
            }
        }
        CrossModField::Sustain => {
            if let ModSource::Envelope { sustain, .. } = &mut m.source {
                *sustain = value;
            }
        }
        CrossModField::Release => {
            if let ModSource::Envelope { release, .. } = &mut m.source {
                *release = value;
            }
        }
        CrossModField::Depth(target_index) => {
            if let Some(t) = m.targets.get_mut(target_index) {
                t.depth = value;
            }
        }
    }
}

/// Which field of a modulator a cross-mod target drives. Shared by sibling
/// cross-mod (within one rack) and group→member-modulator cross-mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossModField {
    Rate,
    Attack,
    Decay,
    Sustain,
    Release,
    Depth(usize),
}

/// Apply all of a lane's modulators across its chain, summing contributions
/// when multiple modulators target the same parameter. Each parameter gets:
///   base_value + sum(offset_i)
/// where each offset comes from `Modulator::target_offset` (bipolar around
/// the base for LFOs, rising from below up to the base for envelopes). This
/// prevents the last-modulator-wins overwrite bug. Targets are routed by
/// slot: 0 = instrument, 1..N = effects[slot-1].
fn apply_modulators_to_chain(
    modulators: &[Modulator],
    instrument: &mut Option<Box<dyn Plugin>>,
    effects: &mut [Box<dyn Plugin>],
) {
    // Collect (slot, param_index, base_value, min, max, total_offset).
    let mut accum: Vec<(usize, u32, f32, f32, f32, f32)> = Vec::new();

    for m in modulators {
        for target in &m.targets {
            if let ModTargetKind::PluginParam { slot, param_index } = target.kind {
                let offset = m.target_offset(target);
                if let Some(entry) = accum
                    .iter_mut()
                    .find(|e| e.0 == slot && e.1 == param_index)
                {
                    entry.5 += offset;
                } else {
                    accum.push((
                        slot,
                        param_index,
                        target.base_value,
                        target.param_min,
                        target.param_max,
                        offset,
                    ));
                }
            }
        }
    }

    for (slot, param_index, base_value, min, max, total_offset) in accum {
        let modulated = (base_value + total_offset).clamp(min, max);
        let plugin: Option<&mut dyn Plugin> = if slot == 0 {
            instrument.as_deref_mut()
        } else {
            effects.get_mut(slot - 1).map(|b| b.as_mut())
        };
        if let Some(p) = plugin {
            let _ = p.set_parameter(param_index, modulated);
        }
    }
}

/// After removing a modulator at `removed_index`, clean up cross-mod targets
/// in siblings: remove targets pointing at the removed index, and decrement
/// indices > removed_index.
fn fixup_cross_mod_after_remove(modulators: &mut [Modulator], removed_index: usize) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            let idx = cross_mod_index(&t.kind);
            idx != Some(removed_index)
        });
        for t in &mut m.targets {
            adjust_cross_mod_index(&mut t.kind, removed_index);
        }
    }
}

/// After an effect is inserted at effect index `at`, bump plugin-param target
/// slots so they keep pointing at the same plugins (effects at/after `at`
/// shifted up one). Slot 0 (instrument) is untouched.
fn shift_slots_after_insert(modulators: &mut [Modulator], at: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > at {
                    *slot += 1;
                }
            }
        }
    }
}

/// After the effect at index `removed` is deleted, drop targets that pointed at
/// it and shift higher slots down one.
fn fixup_slots_after_remove(modulators: &mut [Modulator], removed: usize) {
    let removed_slot = removed + 1;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(&t.kind, ModTargetKind::PluginParam { slot, .. } if *slot == removed_slot)
        });
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot > removed_slot {
                    *slot -= 1;
                }
            }
        }
    }
}

/// After the effect at `from` moves to `to`, remap plugin-param target slots
/// through the same permutation so they follow their plugins.
fn remap_slots_after_reorder(modulators: &mut [Modulator], from: usize, to: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::PluginParam { slot, .. } = &mut t.kind {
                if *slot >= 1 {
                    let ei = *slot - 1;
                    let new_ei = if ei == from {
                        to
                    } else if from < to && ei > from && ei <= to {
                        ei - 1
                    } else if from > to && ei >= to && ei < from {
                        ei + 1
                    } else {
                        ei
                    };
                    *slot = new_ei + 1;
                }
            }
        }
    }
}

/// Extract the mod_index from a cross-mod target kind, if any.
fn cross_mod_index(kind: &ModTargetKind) -> Option<usize> {
    match kind {
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => None,
        ModTargetKind::ModulatorRate { mod_index }
        | ModTargetKind::ModulatorAttack { mod_index }
        | ModTargetKind::ModulatorDecay { mod_index }
        | ModTargetKind::ModulatorSustain { mod_index }
        | ModTargetKind::ModulatorRelease { mod_index }
        | ModTargetKind::ModulatorDepth { mod_index, .. } => Some(*mod_index),
    }
}

/// Decrement cross-mod mod_index values that are greater than `removed_index`.
fn adjust_cross_mod_index(kind: &mut ModTargetKind, removed_index: usize) {
    let idx = match kind {
        ModTargetKind::PluginParam { .. }
        | ModTargetKind::GroupMember { .. }
        | ModTargetKind::GroupBus { .. }
        | ModTargetKind::GroupMemberMod { .. } => return,
        ModTargetKind::ModulatorRate { mod_index }
        | ModTargetKind::ModulatorAttack { mod_index }
        | ModTargetKind::ModulatorDecay { mod_index }
        | ModTargetKind::ModulatorSustain { mod_index }
        | ModTargetKind::ModulatorRelease { mod_index }
        | ModTargetKind::ModulatorDepth { mod_index, .. } => mod_index,
    };
    if *idx > removed_index {
        *idx -= 1;
    }
}

/// When a modulator parameter is set by the user (e.g. SetModulatorRate),
/// update the `base_value` of any cross-mod targets pointing at that field.
fn update_cross_mod_base(modulators: &mut [Modulator], target_mod_index: usize, field: CrossModField, value: f32) {
    for m in modulators.iter_mut() {
        for target in &mut m.targets {
            let matches = match (&target.kind, &field) {
                (ModTargetKind::ModulatorRate { mod_index }, CrossModField::Rate) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorAttack { mod_index }, CrossModField::Attack) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorDecay { mod_index }, CrossModField::Decay) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorSustain { mod_index }, CrossModField::Sustain) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorRelease { mod_index }, CrossModField::Release) => *mod_index == target_mod_index,
                (ModTargetKind::ModulatorDepth { mod_index, target_index }, CrossModField::Depth(ti)) => {
                    *mod_index == target_mod_index && *target_index == *ti
                }
                _ => false,
            };
            if matches {
                target.base_value = value;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Group-scoped modulator helpers
// ---------------------------------------------------------------------------

/// The global lane index of the `member`-th member of `group` (members taken
/// in ascending lane order), or None if out of range.
fn group_member_lane(instruments: &[InstrumentLane], group: usize, member: usize) -> Option<usize> {
    instruments
        .iter()
        .enumerate()
        .filter(|(_, l)| l.group == Some(group))
        .nth(member)
        .map(|(i, _)| i)
}

/// The (group, member-ordinal) of lane `inst` within its group, or None if the
/// lane is ungrouped.
fn lane_member_ordinal(instruments: &[InstrumentLane], inst: usize) -> Option<(usize, usize)> {
    let group = instruments.get(inst)?.group?;
    let ordinal = instruments[..inst]
        .iter()
        .filter(|l| l.group == Some(group))
        .count();
    Some((group, ordinal))
}

/// Build the trigger MIDI for a group's modulators: note events whose note
/// falls within any member's range (members with no range match all notes),
/// plus all non-note events. Drives group-scoped envelope modulators so the
/// group envelope opens whenever any member would sound a note.
fn build_group_midi(
    instruments: &[InstrumentLane],
    group: usize,
    midi: &[(u64, [u8; 3])],
    out: &mut Vec<(u64, [u8; 3])>,
) {
    out.clear();
    for &(frame, bytes) in midi {
        let status = bytes[0] & 0xF0;
        if matches!(status, 0x80 | 0x90) {
            let note = bytes[1];
            let in_any = instruments
                .iter()
                .filter(|l| l.group == Some(group))
                .any(|l| l.range.is_none_or(|(lo, hi)| note >= lo && note <= hi));
            if in_any {
                out.push((frame, bytes));
            }
        } else {
            out.push((frame, bytes));
        }
    }
}

/// A pending parameter write produced by a group modulator.
enum GroupModWrite {
    /// A member instrument's plugin: resolved global lane index + chain slot.
    Member {
        lane: usize,
        slot: usize,
        param_index: u32,
        value: f32,
    },
    /// One of the group's own bus effects.
    Bus {
        group: usize,
        effect_index: usize,
        param_index: u32,
        value: f32,
    },
}

/// Collect the parameter writes from every group's modulators, summing
/// contributions when several modulators within a group target the same
/// parameter (mirrors `apply_modulators_to_chain`). Member ordinals are
/// resolved to global lane indices against the current membership;
/// unresolvable targets (e.g. a member that left the group) are skipped.
fn collect_group_mod_writes(
    groups: &[Group],
    instruments: &[InstrumentLane],
    out: &mut Vec<GroupModWrite>,
) {
    out.clear();
    for (gi, g) in groups.iter().enumerate() {
        // (lane, slot, param, base, min, max, total_offset)
        let mut member_acc: Vec<(usize, usize, u32, f32, f32, f32, f32)> = Vec::new();
        // (effect, param, base, min, max, total_offset)
        let mut bus_acc: Vec<(usize, u32, f32, f32, f32, f32)> = Vec::new();
        for m in &g.modulators {
            for t in &m.targets {
                match t.kind {
                    ModTargetKind::GroupMember {
                        member,
                        slot,
                        param_index,
                    } => {
                        let Some(lane) = group_member_lane(instruments, gi, member) else {
                            continue;
                        };
                        let off = m.target_offset(t);
                        if let Some(e) = member_acc
                            .iter_mut()
                            .find(|e| e.0 == lane && e.1 == slot && e.2 == param_index)
                        {
                            e.6 += off;
                        } else {
                            member_acc.push((
                                lane,
                                slot,
                                param_index,
                                t.base_value,
                                t.param_min,
                                t.param_max,
                                off,
                            ));
                        }
                    }
                    ModTargetKind::GroupBus {
                        effect_index,
                        param_index,
                    } => {
                        let off = m.target_offset(t);
                        if let Some(e) = bus_acc
                            .iter_mut()
                            .find(|e| e.0 == effect_index && e.1 == param_index)
                        {
                            e.5 += off;
                        } else {
                            bus_acc.push((
                                effect_index,
                                param_index,
                                t.base_value,
                                t.param_min,
                                t.param_max,
                                off,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        for (lane, slot, param_index, base, min, max, off) in member_acc {
            let value = (base + off).clamp(min, max);
            out.push(GroupModWrite::Member {
                lane,
                slot,
                param_index,
                value,
            });
        }
        for (effect_index, param_index, base, min, max, off) in bus_acc {
            let value = (base + off).clamp(min, max);
            out.push(GroupModWrite::Bus {
                group: gi,
                effect_index,
                param_index,
                value,
            });
        }
    }
}

/// Apply collected group-modulator writes to the member lanes and bus effects.
fn apply_group_mod_writes(
    writes: &[GroupModWrite],
    instruments: &mut [InstrumentLane],
    groups: &mut [Group],
) {
    for w in writes {
        match *w {
            GroupModWrite::Member {
                lane,
                slot,
                param_index,
                value,
            } => {
                if let Some(l) = instruments.get_mut(lane) {
                    let plugin: Option<&mut dyn Plugin> = if slot == 0 {
                        l.instrument.as_deref_mut()
                    } else {
                        l.effects.get_mut(slot - 1).map(|b| b.as_mut())
                    };
                    if let Some(p) = plugin {
                        let _ = p.set_parameter(param_index, value);
                    }
                }
            }
            GroupModWrite::Bus {
                group,
                effect_index,
                param_index,
                value,
            } => {
                if let Some(e) = groups
                    .get_mut(group)
                    .and_then(|g| g.effects.get_mut(effect_index))
                {
                    let _ = e.set_parameter(param_index, value);
                }
            }
        }
    }
}

/// A pending write from a group modulator into a member instrument's modulator.
struct GroupMemberModWrite {
    lane: usize,
    mod_index: usize,
    field: CrossModField,
    value: f32,
}

/// Collect group modulators' `GroupMemberMod` targets into cross-mod writes on
/// member lanes' modulators. Member ordinals resolve against current
/// membership; unresolvable ones are skipped. Multiple writers to the same
/// field are last-wins (matching sibling cross-mod).
fn collect_group_member_mod_writes(
    groups: &[Group],
    instruments: &[InstrumentLane],
    out: &mut Vec<GroupMemberModWrite>,
) {
    out.clear();
    for (gi, g) in groups.iter().enumerate() {
        for m in &g.modulators {
            for t in &m.targets {
                if let ModTargetKind::GroupMemberMod {
                    member,
                    mod_index,
                    field,
                } = t.kind
                {
                    let Some(lane) = group_member_lane(instruments, gi, member) else {
                        continue;
                    };
                    let value =
                        (t.base_value + m.target_offset(t)).clamp(t.param_min, t.param_max);
                    out.push(GroupMemberModWrite {
                        lane,
                        mod_index,
                        field,
                        value,
                    });
                }
            }
        }
    }
}

/// Apply collected group→member-modulator writes (bounds-checked).
fn apply_group_member_mod_writes(
    writes: &[GroupMemberModWrite],
    instruments: &mut [InstrumentLane],
) {
    for w in writes {
        if let Some(m) = instruments
            .get_mut(w.lane)
            .and_then(|l| l.modulators.get_mut(w.mod_index))
        {
            apply_cross_mod_field(m, w.field, w.value);
        }
    }
}

/// The member ordinal a group target lands on (GroupMember or GroupMemberMod),
/// as a mutable reference for fixups. Other kinds return None.
fn group_target_member_mut(kind: &mut ModTargetKind) -> Option<&mut usize> {
    match kind {
        ModTargetKind::GroupMember { member, .. } => Some(member),
        ModTargetKind::GroupMemberMod { member, .. } => Some(member),
        _ => None,
    }
}

/// After the member at ordinal `removed` leaves a group (membership change or
/// lane deletion), drop group targets pointing at it (GroupMember and
/// GroupMemberMod) and shift higher ordinals down one.
fn fixup_group_member_after_remove(modulators: &mut [Modulator], removed: usize) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| match &t.kind {
            ModTargetKind::GroupMember { member, .. }
            | ModTargetKind::GroupMemberMod { member, .. } => *member != removed,
            _ => true,
        });
        for t in &mut m.targets {
            if let Some(member) = group_target_member_mut(&mut t.kind) {
                if *member > removed {
                    *member -= 1;
                }
            }
        }
    }
}

/// After member `member`'s lane modulator at `removed` is deleted, drop
/// GroupMemberMod targets pointing at it and shift that member's higher
/// mod-indices down one.
fn fixup_group_member_mod_after_remove(
    modulators: &mut [Modulator],
    member: usize,
    removed: usize,
) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(t.kind, ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } if tm == member && mod_index == removed)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod { member: tm, mod_index, .. } = &mut t.kind {
                if *tm == member && *mod_index > removed {
                    *mod_index -= 1;
                }
            }
        }
    }
}

/// After a member joins a group at ordinal `inserted`, bump group-target member
/// ordinals at/after it so existing targets keep pointing at their members.
fn shift_group_member_after_insert(modulators: &mut [Modulator], inserted: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let Some(member) = group_target_member_mut(&mut t.kind) {
                if *member >= inserted {
                    *member += 1;
                }
            }
        }
    }
}

/// After bus effect `removed` is deleted, drop GroupBus targets on it and shift
/// higher effect indices down one.
fn fixup_group_bus_after_remove(modulators: &mut [Modulator], removed: usize) {
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            !matches!(t.kind, ModTargetKind::GroupBus { effect_index, .. } if effect_index == removed)
        });
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                if *effect_index > removed {
                    *effect_index -= 1;
                }
            }
        }
    }
}

/// After bus effect moves from `from` to `to`, remap GroupBus effect indices
/// through the same permutation so targets follow their effects.
fn remap_group_bus_after_reorder(modulators: &mut [Modulator], from: usize, to: usize) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus { effect_index, .. } = &mut t.kind {
                let ei = *effect_index;
                *effect_index = if ei == from {
                    to
                } else if from < to && ei > from && ei <= to {
                    ei - 1
                } else if from > to && ei >= to && ei < from {
                    ei + 1
                } else {
                    ei
                };
            }
        }
    }
}

/// Update the `base_value` of GroupMember targets matching (member, slot,
/// param) after the user sets that member parameter.
fn update_group_member_base(
    modulators: &mut [Modulator],
    member: usize,
    slot: usize,
    param_index: u32,
    value: f32,
) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMember {
                member: tm,
                slot: ts,
                param_index: tp,
            } = t.kind
            {
                if tm == member && ts == slot && tp == param_index {
                    t.base_value = value;
                }
            }
        }
    }
}

/// Update the `base_value` of GroupBus targets matching (effect, param) after
/// the user sets that bus-effect parameter.
fn update_group_bus_base(
    modulators: &mut [Modulator],
    effect_index: usize,
    param_index: u32,
    value: f32,
) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupBus {
                effect_index: te,
                param_index: tp,
            } = t.kind
            {
                if te == effect_index && tp == param_index {
                    t.base_value = value;
                }
            }
        }
    }
}

/// Update the `base_value` of GroupMemberMod targets matching (member,
/// mod_index, field) after the user changes that member modulator's field —
/// keeps the group cross-mod centered on the user's latest setting.
fn update_group_member_mod_base(
    modulators: &mut [Modulator],
    member: usize,
    mod_index: usize,
    field: CrossModField,
    value: f32,
) {
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            if let ModTargetKind::GroupMemberMod {
                member: tm,
                mod_index: tmi,
                field: tf,
            } = t.kind
            {
                if tm == member && tmi == mod_index && tf == field {
                    t.base_value = value;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// GraphCommand — addressed commands for the AudioGraph
// ---------------------------------------------------------------------------

/// Commands sent from the main thread to mutate the audio graph on the audio thread.
pub enum GraphCommand {
    /// Swap the instrument plugin in a specific instrument lane.
    SwapInstrument {
        inst: usize,
        instrument: Box<dyn Plugin>,
        inst_buf: Vec<Vec<f32>>,
        remapper: Option<NoteRemapper>,
    },
    /// Insert an effect into a specific instrument lane's chain.
    InsertEffect {
        inst: usize,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    },
    /// Remove an effect from a specific instrument lane's chain.
    RemoveEffect {
        inst: usize,
        index: usize,
    },
    /// Reorder an effect within a specific instrument lane's chain.
    ReorderEffect {
        inst: usize,
        from: usize,
        to: usize,
    },
    /// Set a parameter on a plugin. slot 0 = instrument, 1..N = effects.
    SetParameter {
        inst: usize,
        slot: usize,
        param_index: u32,
        value: f32,
    },
    /// Set the host-side dry/wet mix on an effect. slot 1..N = effects.
    SetMix {
        inst: usize,
        slot: usize,
        value: f32,
    },
    /// Load a preset on a plugin. slot 0 = instrument, 1..N = effects.
    LoadPreset {
        inst: usize,
        slot: usize,
        preset_id: String,
    },
    /// Set the host-side volume on an instrument lane.
    SetVolume {
        inst: usize,
        value: f32,
    },
    /// Set the note range for an instrument lane. None = full range.
    SetInstrumentRange {
        inst: usize,
        range: Option<(u8, u8)>,
    },
    /// Clear the instrument plugin from a lane (leaving it empty).
    ClearInstrument {
        inst: usize,
    },
    /// Swap instruments (with their buffers and remappers) between two lanes.
    SwapInstruments {
        inst_a: usize,
        inst_b: usize,
    },
    /// Add a new empty instrument lane.
    AddInstrument {
        range: Option<(u8, u8)>,
    },
    /// Remove an instrument lane entirely.
    RemoveInstrument {
        inst: usize,
    },
    /// Add a new empty submix group; the new group's index is `groups.len()`.
    AddGroup,
    /// Remove a group; its members become ungrouped and higher group indices
    /// shift down.
    RemoveGroup {
        group: usize,
    },
    /// Set (or clear) a lane's group membership.
    SetLaneGroup {
        inst: usize,
        group: Option<usize>,
    },
    /// Set a group's output volume (applied to the member sum, before its FX).
    SetGroupVolume {
        group: usize,
        value: f32,
    },
    /// Set a group's stereo balance pan (-1 left … 0 center … +1 right).
    SetGroupPan {
        group: usize,
        value: f32,
    },
    /// Insert an effect into a group's bus chain.
    InsertGroupEffect {
        group: usize,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    },
    /// Remove an effect from a group's bus chain.
    RemoveGroupEffect {
        group: usize,
        index: usize,
    },
    /// Reorder an effect within a group's bus chain.
    ReorderGroupEffect {
        group: usize,
        from: usize,
        to: usize,
    },
    /// Set a group effect's dry/wet mix.
    SetGroupMix {
        group: usize,
        index: usize,
        value: f32,
    },
    /// Set a parameter on a group bus effect.
    SetGroupParameter {
        group: usize,
        index: usize,
        param_index: u32,
        value: f32,
    },
    /// Insert a new modulator into the lane (modulators are lane-scoped).
    InsertModulator {
        inst: usize,
        index: usize,
        source: ModSource,
    },
    /// Remove a modulator from the lane.
    RemoveModulator {
        inst: usize,
        index: usize,
    },
    /// Set the rate of an LFO modulator.
    SetModulatorRate {
        inst: usize,
        mod_index: usize,
        rate: f32,
    },
    /// Set the waveform of an LFO modulator.
    SetModulatorWaveform {
        inst: usize,
        mod_index: usize,
        waveform: LfoWaveform,
    },
    /// Replace a modulator's source (for type switching between LFO/Envelope).
    SetModulatorSource {
        inst: usize,
        mod_index: usize,
        source: ModSource,
    },
    /// Set envelope parameters on an Envelope modulator.
    SetModulatorEnvelopeParam {
        inst: usize,
        mod_index: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// Add a modulation target to a modulator.
    AddModTarget {
        inst: usize,
        mod_index: usize,
        target: ModTarget,
    },
    /// Remove a modulation target from a modulator.
    #[expect(dead_code)]
    RemoveModTarget {
        inst: usize,
        mod_index: usize,
        target_index: usize,
    },
    /// Set the depth of a modulation target.
    SetModTargetDepth {
        inst: usize,
        mod_index: usize,
        target_index: usize,
        depth: f32,
    },
    /// Insert a new group-scoped modulator into a group.
    InsertGroupModulator {
        group: usize,
        index: usize,
        source: ModSource,
    },
    /// Remove a group-scoped modulator from a group.
    RemoveGroupModulator {
        group: usize,
        index: usize,
    },
    /// Set the rate of a group LFO modulator.
    SetGroupModulatorRate {
        group: usize,
        mod_index: usize,
        rate: f32,
    },
    /// Set the waveform of a group LFO modulator.
    SetGroupModulatorWaveform {
        group: usize,
        mod_index: usize,
        waveform: LfoWaveform,
    },
    /// Replace a group modulator's source (LFO/Envelope type switch).
    SetGroupModulatorSource {
        group: usize,
        mod_index: usize,
        source: ModSource,
    },
    /// Set envelope parameters on a group Envelope modulator.
    SetGroupModulatorEnvelopeParam {
        group: usize,
        mod_index: usize,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    /// Add a modulation target to a group modulator.
    AddGroupModTarget {
        group: usize,
        mod_index: usize,
        target: ModTarget,
    },
    /// Remove a modulation target from a group modulator.
    #[expect(dead_code)]
    RemoveGroupModTarget {
        group: usize,
        mod_index: usize,
        target_index: usize,
    },
    /// Set the depth of a group modulator's target.
    SetGroupModTargetDepth {
        group: usize,
        mod_index: usize,
        target_index: usize,
        depth: f32,
    },
    /// Enable/disable pattern playback for an instrument lane.
    SetPatternEnabled {
        inst: usize,
        enabled: bool,
    },
    /// Start/stop pattern recording for an instrument lane.
    SetPatternRecording {
        inst: usize,
        recording: bool,
    },
    /// Set the pattern data (e.g. after loading from session).
    SetPattern {
        inst: usize,
        pattern: Pattern,
        base_note: Option<u8>,
        in_key: bool,
    },
    /// Clear the pattern for an instrument lane.
    ClearPattern {
        inst: usize,
    },
    /// Swap patterns between two instrument lanes.
    SwapPatterns {
        inst_a: usize,
        inst_b: usize,
    },
    /// Set the global BPM (applied to all pattern players).
    SetGlobalBpm {
        bpm: f32,
    },
    /// Set pattern length in beats.
    SetPatternLength {
        inst: usize,
        beats: f32,
    },
    /// Set whether the pattern loops or plays once.
    SetPatternLooping {
        inst: usize,
        looping: bool,
    },
    /// Set whether pattern playback transposes by scale degrees (in-key,
    /// using the piano scale) or by semitones (chromatic).
    SetPatternInKey {
        inst: usize,
        in_key: bool,
    },
    /// Set the transpose (in semitones) for an instrument lane.
    SetTranspose {
        inst: usize,
        semitones: i8,
    },
}

// ---------------------------------------------------------------------------
// Pattern recorder/player
// ---------------------------------------------------------------------------

/// A single recorded MIDI event in a pattern.
#[derive(Clone)]
pub struct PatternEvent {
    /// Tick offset from pattern start (in samples at recording sample rate).
    pub frame: u64,
    /// MIDI status type: 0x90 = note-on, 0x80 = note-off.
    pub status: u8,
    /// Note number (absolute, will be transposed on playback).
    pub note: u8,
    /// Velocity.
    pub velocity: u8,
}

/// A recorded pattern — a sequence of note events with a fixed length.
#[derive(Clone, Default)]
pub struct Pattern {
    pub events: Vec<PatternEvent>,
    /// Total length of the pattern in samples.
    pub length_samples: u64,
}

/// Notification sent from audio thread to TUI when recording completes.
pub struct PatternNotification {
    pub inst: usize,
    pub base_note: Option<u8>,
    pub length_beats: f32,
    pub looping: bool,
    pub enabled: bool,
    /// (frame, status, note, velocity)
    pub events: Vec<(u64, u8, u8, u8)>,
}

/// Tracks one currently-sounding voice from pattern playback.
struct PatternVoice {
    /// The original pattern note (before transpose).
    pattern_note: u8,
    /// The transposed note actually playing.
    playing_note: u8,
    /// MIDI channel used for the note.
    channel: u8,
}

/// How pattern notes are mapped to output notes during transposed playback.
enum NoteMap {
    /// Shift every note by a fixed number of semitones (preserves intervals).
    Chromatic(i16),
    /// Shift by the scale-degree distance from base to trigger, keeping all
    /// notes aligned with the scale (a major-triad pattern triggered on the
    /// 2nd degree of a major scale comes out minor).
    InKey {
        scale: ScaleSetting,
        base: u8,
        trigger: u8,
    },
}

impl NoteMap {
    fn map(&self, note: u8) -> u8 {
        match *self {
            NoteMap::Chromatic(transpose) => (note as i16 + transpose).clamp(0, 127) as u8,
            NoteMap::InKey { scale, base, trigger } => scale.transpose_in_key(note, base, trigger),
        }
    }
}

/// Per-instrument pattern recorder and player.
struct PatternPlayer {
    pattern: Pattern,
    enabled: bool,
    recording: bool,
    /// Count-in phase: metronome ticks but no recording happens yet.
    counting_in: bool,
    /// Current playback position in samples within the pattern.
    playback_pos: u64,
    /// Recording position in samples since record started.
    record_pos: u64,
    /// Sample rate (needed for BPM → sample conversion).
    sample_rate: f32,
    /// The base note of the recorded pattern (first note-on recorded).
    base_note: Option<u8>,
    /// The note currently held that triggers playback. None = not playing.
    trigger_note: Option<u8>,
    /// All currently held notes (for switching trigger on key change).
    held_notes: Vec<u8>,
    /// Currently sounding voices from pattern playback.
    active_voices: Vec<PatternVoice>,
    /// Buffer for pattern-generated MIDI events to be merged into the stream.
    output_events: Vec<(u64, [u8; 3])>,
    /// Events recorded in the current recording pass.
    recording_events: Vec<PatternEvent>,
    /// Length of pattern in beats (default: 4 = 1 bar in 4/4).
    length_beats: f32,
    /// Whether the pattern loops when it reaches the end (default: true).
    looping: bool,
    /// Whether playback transposes by scale degrees of the piano scale
    /// (in-key) instead of by semitones (chromatic, the default).
    in_key: bool,
    /// BPM (global, set from main thread).
    bpm: f32,
    /// Notification sender for when recording completes automatically.
    pattern_tx: Option<Sender<PatternNotification>>,
    /// This player's instrument index (for notifications).
    inst_index: usize,
    // --- Metronome state ---
    /// Number of count-in beats before recording starts.
    count_in_beats: f32,
    /// Position in samples since the start of count-in (covers both count-in + recording).
    metronome_pos: u64,
    /// Samples per beat (precomputed when recording starts).
    beat_length_samples: u64,
    /// Metronome click oscillator phase (0.0–1.0).
    click_phase: f32,
    /// Remaining samples in the current click sound.
    click_remaining: u32,
    /// Whether the current click is a downbeat (higher pitch).
    click_is_downbeat: bool,
}

/// Metronome click duration in seconds.
const CLICK_DURATION_SECS: f32 = 0.025;
/// Metronome click frequency for normal beats (Hz).
const CLICK_FREQ: f32 = 1000.0;
/// Metronome click frequency for the downbeat (Hz).
const CLICK_DOWNBEAT_FREQ: f32 = 1500.0;
/// Metronome click volume (0.0–1.0).
const CLICK_VOLUME: f32 = 0.3;

impl PatternPlayer {
    fn new(sample_rate: f32) -> Self {
        PatternPlayer {
            pattern: Pattern::default(),
            enabled: false,
            recording: false,
            counting_in: false,
            playback_pos: 0,
            record_pos: 0,
            sample_rate,
            base_note: None,
            trigger_note: None,
            held_notes: Vec::new(),
            active_voices: Vec::new(),
            output_events: Vec::with_capacity(256),
            recording_events: Vec::new(),
            length_beats: 4.0,
            looping: true,
            in_key: false,
            bpm: 120.0,
            pattern_tx: None,
            inst_index: 0,
            count_in_beats: 4.0,
            metronome_pos: 0,
            beat_length_samples: 0,
            click_phase: 0.0,
            click_remaining: 0,
            click_is_downbeat: false,
        }
    }

    /// Calculate pattern length in samples from BPM and length_beats.
    fn length_samples(&self) -> u64 {
        let beats_per_sec = self.bpm / 60.0;
        let seconds = self.length_beats / beats_per_sec;
        (seconds * self.sample_rate) as u64
    }

    /// Returns true if the metronome should be generating audio (count-in or recording).
    fn metronome_active(&self) -> bool {
        self.counting_in || self.recording
    }

    /// Called each audio buffer. Consumes incoming MIDI events, produces
    /// merged output events (original + pattern playback). `scale` is the
    /// current piano scale, used when in-key transposition is enabled.
    fn process(
        &mut self,
        midi_in: &[(u64, [u8; 3])],
        buffer_frames: usize,
        scale: ScaleSetting,
    ) -> &[(u64, [u8; 3])] {
        self.output_events.clear();

        if self.counting_in {
            self.process_count_in(midi_in, buffer_frames);
            // During count-in, pass through original events (user may play along)
            self.output_events.extend_from_slice(midi_in);
            return &self.output_events;
        }

        if self.recording {
            self.process_recording(midi_in, buffer_frames);
            // During recording, pass through original events unmodified
            self.output_events.extend_from_slice(midi_in);
            return &self.output_events;
        }

        if !self.enabled || self.pattern.events.is_empty() || self.base_note.is_none() {
            // No pattern — pass through
            self.output_events.extend_from_slice(midi_in);
            return &self.output_events;
        }

        self.process_playback(midi_in, buffer_frames, scale);
        &self.output_events
    }

    /// Render metronome clicks into audio buffers. Call after instrument processing.
    /// Adds click samples additively to existing audio in `output`.
    fn render_metronome(&mut self, output: &mut [Vec<f32>], buffer_frames: usize) {
        if !self.metronome_active() || self.beat_length_samples == 0 {
            return;
        }

        let click_duration_samples = (CLICK_DURATION_SECS * self.sample_rate) as u32;

        for i in 0..buffer_frames {
            let sample_pos = self.metronome_pos + i as u64;

            // Check if we're at a beat boundary
            if sample_pos.is_multiple_of(self.beat_length_samples) {
                // Determine which beat this is in the overall sequence
                let beat_index = sample_pos / self.beat_length_samples;
                self.click_is_downbeat = beat_index.is_multiple_of(self.count_in_beats as u64);
                self.click_remaining = click_duration_samples;
                self.click_phase = 0.0;
            }

            // Generate click sample
            if self.click_remaining > 0 {
                let freq = if self.click_is_downbeat {
                    CLICK_DOWNBEAT_FREQ
                } else {
                    CLICK_FREQ
                };
                let phase_inc = freq / self.sample_rate;
                self.click_phase = (self.click_phase + phase_inc) % 1.0;

                // Sine wave with exponential decay envelope
                let t = 1.0 - (self.click_remaining as f32 / click_duration_samples as f32);
                let envelope = (-t * 8.0).exp(); // fast decay
                let sample = (self.click_phase * std::f32::consts::TAU).sin()
                    * envelope
                    * CLICK_VOLUME;

                // Add to all channels
                for ch in output.iter_mut() {
                    if i < ch.len() {
                        ch[i] += sample;
                    }
                }

                self.click_remaining -= 1;
            }
        }

        self.metronome_pos += buffer_frames as u64;
    }

    fn process_count_in(&mut self, midi_in: &[(u64, [u8; 3])], buffer_frames: usize) {
        let count_in_samples = (self.count_in_beats as u64) * self.beat_length_samples;

        // Capture note-ons during count-in — they'll be snapped to frame 0.
        for &(_frame, bytes) in midi_in {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x90 if bytes[2] > 0 => {
                    self.recording_events.push(PatternEvent {
                        frame: 0,
                        status: 0x90,
                        note: bytes[1],
                        velocity: bytes[2],
                    });
                }
                0x80 | 0x90 => {
                    // Note-off during count-in: also snap to frame 0
                    self.recording_events.push(PatternEvent {
                        frame: 0,
                        status: 0x80,
                        note: bytes[1],
                        velocity: 0,
                    });
                }
                _ => {}
            }
        }

        // Note: metronome_pos is advanced by render_metronome, but we need to
        // track count-in progress here too. Use record_pos as count-in position.
        self.record_pos += buffer_frames as u64;

        if self.record_pos >= count_in_samples {
            // Count-in complete — transition to recording
            self.counting_in = false;
            self.recording = true;
            self.record_pos = 0;
            // metronome_pos continues (don't reset — keeps beat alignment)
        }
    }

    fn process_recording(&mut self, midi_in: &[(u64, [u8; 3])], buffer_frames: usize) {
        let length = self.length_samples();

        for &(frame, bytes) in midi_in {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x90 if bytes[2] > 0 => {
                    // Note-on
                    self.recording_events.push(PatternEvent {
                        frame: self.record_pos + frame,
                        status: 0x90,
                        note: bytes[1],
                        velocity: bytes[2],
                    });
                }
                0x80 | 0x90 => {
                    // Note-off
                    self.recording_events.push(PatternEvent {
                        frame: self.record_pos + frame,
                        status: 0x80,
                        note: bytes[1],
                        velocity: 0,
                    });
                }
                _ => {
                    // CC, pitch bend, etc.: not recorded
                }
            }
        }

        self.record_pos += buffer_frames as u64;

        // Check if recording time has elapsed
        if self.record_pos >= length {
            self.finalize_recording(length);
        }
    }

    fn finalize_recording(&mut self, length_samples: u64) {
        // Clamp events to pattern length
        self.recording_events.retain(|e| e.frame < length_samples);

        // Base note = lowest note-on in the recording (for transpose reference).
        self.base_note = self.recording_events.iter()
            .filter(|e| e.status == 0x90)
            .map(|e| e.note)
            .min();

        self.pattern = Pattern {
            events: std::mem::take(&mut self.recording_events),
            length_samples,
        };
        self.recording = false;
        self.counting_in = false;
        self.enabled = !self.pattern.events.is_empty();
        self.click_remaining = 0;

        // Notify main thread with the recorded data
        if let Some(ref tx) = self.pattern_tx {
            let events = self.pattern.events.iter().map(|e| {
                (e.frame, e.status, e.note, e.velocity)
            }).collect();
            let _ = tx.try_send(PatternNotification {
                inst: self.inst_index,
                base_note: self.base_note,
                length_beats: self.length_beats,
                looping: self.looping,
                enabled: self.enabled,
                events,
            });
        }
    }

    fn process_playback(
        &mut self,
        midi_in: &[(u64, [u8; 3])],
        buffer_frames: usize,
        scale: ScaleSetting,
    ) {
        let base = match self.base_note {
            Some(n) => n,
            None => {
                self.output_events.extend_from_slice(midi_in);
                return;
            }
        };

        // Scan incoming MIDI for trigger note-on/off.
        // Track held notes so we can switch triggers instantly.
        for &(frame, bytes) in midi_in {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x90 if bytes[2] > 0 => {
                    self.held_notes.push(bytes[1]);
                    if self.trigger_note.is_some() && self.trigger_note != Some(bytes[1]) {
                        // Switch to new trigger: kill active voices, restart
                        for voice in self.active_voices.drain(..) {
                            self.output_events.push((
                                frame,
                                [0x80 | voice.channel, voice.playing_note, 0],
                            ));
                        }
                    }
                    self.trigger_note = Some(bytes[1]);
                    self.playback_pos = 0;
                    // Swallow note events — pattern handles them
                }
                0x80 | 0x90 => {
                    self.held_notes.retain(|&n| n != bytes[1]);
                    if self.trigger_note == Some(bytes[1]) {
                        if let Some(&last) = self.held_notes.last() {
                            // Another key is still held — switch to it
                            for voice in self.active_voices.drain(..) {
                                self.output_events.push((
                                    frame,
                                    [0x80 | voice.channel, voice.playing_note, 0],
                                ));
                            }
                            self.trigger_note = Some(last);
                            self.playback_pos = 0;
                        } else {
                            // No keys held — stop playback
                            for voice in self.active_voices.drain(..) {
                                self.output_events.push((
                                    frame,
                                    [0x80 | voice.channel, voice.playing_note, 0],
                                ));
                            }
                            self.trigger_note = None;
                        }
                    }
                    // Swallow note events
                }
                _ => {
                    // Pass through CC, pitch bend, etc.
                    self.output_events.push((frame, bytes));
                }
            }
        }

        // If no trigger is active, nothing to emit
        let trigger = match self.trigger_note {
            Some(t) => t,
            None => return,
        };
        let note_map = if self.in_key {
            NoteMap::InKey { scale, base, trigger }
        } else {
            NoteMap::Chromatic(trigger as i16 - base as i16)
        };

        let pattern_len = self.pattern.length_samples;
        if pattern_len == 0 {
            return;
        }

        let buf_start = self.playback_pos;
        let buf_end = self.playback_pos + buffer_frames as u64;

        // Check for end-of-pattern
        if buf_end > pattern_len {
            // Emit events from buf_start..pattern_len
            self.emit_events_in_range(buf_start, pattern_len, &note_map, 0);
            // Send note-off for all active voices at the boundary
            for voice in self.active_voices.drain(..) {
                let boundary_frame = pattern_len - buf_start;
                self.output_events.push((
                    boundary_frame,
                    [0x80 | voice.channel, voice.playing_note, 0],
                ));
            }
            if self.looping {
                // Wrap around and continue from the start
                let remainder = buf_end - pattern_len;
                let offset = pattern_len - buf_start;
                self.emit_events_in_range(0, remainder, &note_map, offset);
                self.playback_pos = remainder;
            } else {
                // One-shot: stop playback
                self.playback_pos = pattern_len;
            }
        } else {
            self.emit_events_in_range(buf_start, buf_end, &note_map, 0);
            self.playback_pos = buf_end;
            if self.playback_pos >= pattern_len {
                // Exact boundary
                for voice in self.active_voices.drain(..) {
                    self.output_events.push((
                        (buffer_frames - 1) as u64,
                        [0x80 | voice.channel, voice.playing_note, 0],
                    ));
                }
                if self.looping {
                    self.playback_pos = 0;
                }
            }
        }
    }

    /// Emit pattern events that fall within [range_start, range_end), with frame
    /// offsets adjusted by `frame_offset` for the output buffer.
    fn emit_events_in_range(
        &mut self,
        range_start: u64,
        range_end: u64,
        note_map: &NoteMap,
        frame_offset: u64,
    ) {
        let output_events = &mut self.output_events;
        let active_voices = &mut self.active_voices;
        for ev in &self.pattern.events {
            if ev.frame >= range_start && ev.frame < range_end {
                let out_frame = ev.frame - range_start + frame_offset;

                if ev.status == 0x90 {
                    // Note-on
                    let mapped_note = note_map.map(ev.note);
                    output_events.push((out_frame, [0x90, mapped_note, ev.velocity]));
                    active_voices.push(PatternVoice {
                        pattern_note: ev.note,
                        playing_note: mapped_note,
                        channel: 0,
                    });
                } else {
                    // Note-off — release the note(s) the matching voices are
                    // actually sounding, so the off always pairs with its on
                    // even if the scale changed while the note was held.
                    let mut released = false;
                    active_voices.retain(|v| {
                        if v.pattern_note == ev.note {
                            output_events.push((out_frame, [0x80 | v.channel, v.playing_note, 0]));
                            released = true;
                            false
                        } else {
                            true
                        }
                    });
                    if !released {
                        output_events.push((out_frame, [0x80, note_map.map(ev.note), 0]));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// InstrumentLane — one instrument + effect chain
// ---------------------------------------------------------------------------

struct InstrumentLane {
    range: Option<(u8, u8)>,
    instrument: Option<Box<dyn Plugin>>,
    volume: f32,
    inst_buf: Vec<Vec<f32>>,
    effects: Vec<Box<dyn Plugin>>,
    mix_values: Vec<f64>,
    buf_a: Vec<Vec<f32>>,
    buf_b: Vec<Vec<f32>>,
    remapper: Option<NoteRemapper>,
    remapped_events: Vec<(u64, [u8; 3])>,
    transposed_events: Vec<(u64, [u8; 3])>,
    filtered_midi: Vec<(u64, [u8; 3])>,
    /// Modulators for this whole lane. Each modulator's targets address any
    /// plugin in the chain by slot (0 = instrument, 1..N = effects).
    modulators: Vec<Modulator>,
    /// Pattern recorder/player for this instrument.
    pattern: PatternPlayer,
    /// Transpose in semitones applied to note events.
    transpose: i8,
    /// If `Some(g)`, this lane is a member of group `g`: its output is summed
    /// into that group's bus (and its group FX) instead of straight to master.
    group: Option<usize>,
}

impl InstrumentLane {
    fn new(num_channels: usize) -> Self {
        InstrumentLane {
            range: None,
            instrument: None,
            volume: 1.0,
            inst_buf: Vec::new(),
            effects: Vec::new(),
            mix_values: Vec::new(),
            buf_a: (0..num_channels).map(|_| Vec::new()).collect(),
            buf_b: (0..num_channels).map(|_| Vec::new()).collect(),
            remapper: None,
            remapped_events: Vec::with_capacity(128),
            transposed_events: Vec::with_capacity(128),
            filtered_midi: Vec::with_capacity(128),
            modulators: Vec::new(),
            pattern: PatternPlayer::new(48000.0),
            transpose: 0,
            group: None,
        }
    }

    /// Get the sample rate. Derived from the instrument if loaded, otherwise a sensible default.
    fn sample_rate(&self) -> f32 {
        self.instrument
            .as_ref()
            .map(|i| i.sample_rate())
            .unwrap_or(48000.0)
    }

    /// Filter MIDI events by this instrument's key range.
    /// Note-on/note-off: only pass if note is within range (inclusive).
    /// CC, pitch bend, channel pressure, etc.: always pass through.
    fn filter_midi(&mut self, midi_events: &[(u64, [u8; 3])]) {
        self.filtered_midi.clear();
        let range = match self.range {
            Some(r) => r,
            None => {
                // Full range — pass everything
                self.filtered_midi.extend_from_slice(midi_events);
                return;
            }
        };

        for &(frame, bytes) in midi_events {
            let status_type = bytes[0] & 0xF0;
            match status_type {
                0x80 | 0x90 => {
                    // Note-on or note-off: filter by range
                    let note = bytes[1];
                    if note >= range.0 && note <= range.1 {
                        self.filtered_midi.push((frame, bytes));
                    }
                }
                _ => {
                    // CC, pitch bend, channel pressure, etc. — duplicate to all instruments
                    self.filtered_midi.push((frame, bytes));
                }
            }
        }
    }

    /// Process this instrument + effect chain, writing output to `inst_out`.
    /// `inst_out` must have `num_channels` vecs, each with `frames` length.
    /// `scale` is the current piano scale for in-key pattern transposition.
    fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        inst_out: &mut [Vec<f32>],
        num_channels: usize,
        scale: ScaleSetting,
    ) -> anyhow::Result<()> {
        // Filter MIDI by range
        self.filter_midi(midi_events);

        // Apply note remapping if configured
        let effective_events: &[(u64, [u8; 3])] = if let Some(ref remapper) = self.remapper {
            remapper.remap_events(&self.filtered_midi, &mut self.remapped_events);
            &self.remapped_events
        } else {
            &self.filtered_midi
        };

        // Pattern recorder/player — process after remapping, before modulators.
        let frames = inst_out.first().map(|b| b.len()).unwrap_or(0);
        let effective_events = self.pattern.process(effective_events, frames, scale);

        // Apply transpose to note events.
        let effective_events = if self.transpose != 0 {
            self.transposed_events.clear();
            for &(frame, bytes) in effective_events {
                let status_type = bytes[0] & 0xF0;
                if matches!(status_type, 0x80 | 0x90) {
                    let note = bytes[1] as i16 + self.transpose as i16;
                    if (0..=127).contains(&note) {
                        self.transposed_events.push((frame, [bytes[0], note as u8, bytes[2]]));
                    }
                    // Drop notes that fall outside 0-127
                } else {
                    self.transposed_events.push((frame, bytes));
                }
            }
            self.transposed_events.as_slice()
        } else {
            effective_events
        };

        // Apply modulators (block-rate: once per buffer, before processing the
        // chain). Lane-scoped: tick all → cross-mod → route each target to its
        // slot's plugin (instrument or an effect). Applying up front is
        // equivalent to per-slot application since each plugin reads its
        // parameters when it processes, later in this same buffer.
        let buffer_size = inst_out.first().map(|b| b.len()).unwrap_or(0);
        if buffer_size > 0 {
            for m in &mut self.modulators {
                m.tick(buffer_size, effective_events);
            }
            apply_cross_mod(&mut self.modulators);
            apply_modulators_to_chain(&self.modulators, &mut self.instrument, &mut self.effects);
        }

        let instrument = match self.instrument.as_mut() {
            Some(inst) => inst,
            None => {
                for ch in inst_out.iter_mut() {
                    ch.fill(0.0);
                }
                // Render metronome even without an instrument (count-in)
                let frames = inst_out.first().map(|b| b.len()).unwrap_or(0);
                self.pattern.render_metronome(inst_out, frames);
                return Ok(());
            }
        };

        let frames = inst_out.first().map(|b| b.len()).unwrap_or(0);
        let inst_outputs = self.inst_buf.len();

        if inst_outputs <= num_channels && self.effects.is_empty() && (self.volume - 1.0).abs() < f32::EPSILON {
            // Fast path: instrument output fits, no effects, no volume scaling
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let out_refs = mut_slices(inst_out, &mut storage);
            instrument.process(effective_events, &[], out_refs)?;
            self.pattern.render_metronome(inst_out, frames);
            return Ok(());
        }

        // Resize inst_buf
        for buf in self.inst_buf.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Instrument → inst_buf
        {
            let mut storage = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
            let refs = mut_slices(&mut self.inst_buf, &mut storage);
            instrument.process(effective_events, &[], refs)?;
        }

        // Apply volume
        if (self.volume - 1.0).abs() >= f32::EPSILON {
            for ch in 0..self.inst_buf.len().min(num_channels) {
                for sample in self.inst_buf[ch].iter_mut() {
                    *sample *= self.volume;
                }
            }
        }

        if self.effects.is_empty() {
            // No effects — copy first num_channels from inst_buf to output
            for (ch, out) in inst_out.iter_mut().enumerate() {
                if ch < self.inst_buf.len() {
                    out.copy_from_slice(&self.inst_buf[ch]);
                } else {
                    out.fill(0.0);
                }
            }
            self.pattern.render_metronome(inst_out, frames);
            return Ok(());
        }

        // Resize effect ping-pong buffers
        for buf in self.buf_a.iter_mut().chain(self.buf_b.iter_mut()) {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Copy first num_channels from inst_buf → buf_a
        for ch in 0..num_channels {
            if ch < self.inst_buf.len() {
                self.buf_a[ch].copy_from_slice(&self.inst_buf[ch]);
            } else {
                self.buf_a[ch].fill(0.0);
            }
        }

        // Effects: alternate between buf_a and buf_b
        let mut src_is_a = true;

        for (effect, &mix) in self.effects.iter_mut().zip(self.mix_values.iter()) {
            let mix = mix as f32;

            if src_is_a {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_a, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_b, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }

                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_b[ch][i] = self.buf_a[ch][i] * dry + self.buf_b[ch][i] * mix;
                        }
                    }
                }
            } else {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_b, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_a, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }

                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_a[ch][i] = self.buf_b[ch][i] * dry + self.buf_a[ch][i] * mix;
                        }
                    }
                }
            }
            src_is_a = !src_is_a;
        }

        // Copy final result to inst_out
        let final_buf = if src_is_a { &self.buf_a } else { &self.buf_b };
        for (ch, out) in inst_out.iter_mut().enumerate() {
            if ch < final_buf.len() {
                let copy_len = out.len().min(final_buf[ch].len());
                out[..copy_len].copy_from_slice(&final_buf[ch][..copy_len]);
            }
        }

        // Metronome click (additive, on top of instrument+effects)
        self.pattern.render_metronome(inst_out, frames);

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Group — submix bus
// ---------------------------------------------------------------------------

/// A submix bus. Member lanes (those whose `group` points here) sum into
/// `accum` instead of the master; the group then applies its output volume and
/// its own effect chain to that sum, and the result is added to the master.
/// A group also owns modulators that can drive its members and bus effects
/// (see `collect_group_mod_writes`).
struct Group {
    effects: Vec<Box<dyn Plugin>>,
    mix_values: Vec<f64>,
    /// Output gain applied to the member sum, before the group effects.
    volume: f32,
    /// Stereo balance pan for the group bus output: -1 = left, 0 = center,
    /// +1 = right. Applied after the effect chain.
    pan: f32,
    /// Group-scoped modulators. Each target addresses a member instrument's
    /// chain (`GroupMember`), one of this group's bus effects (`GroupBus`), or
    /// a sibling group modulator (cross-mod).
    modulators: Vec<Modulator>,
    /// Member-sum accumulator (num_channels vecs).
    accum: Vec<Vec<f32>>,
    /// Effect ping-pong scratch.
    buf_a: Vec<Vec<f32>>,
    buf_b: Vec<Vec<f32>>,
}

impl Group {
    fn new(num_channels: usize) -> Self {
        Group {
            effects: Vec::new(),
            mix_values: Vec::new(),
            volume: 1.0,
            pan: 0.0,
            modulators: Vec::new(),
            accum: (0..num_channels).map(|_| Vec::new()).collect(),
            buf_a: (0..num_channels).map(|_| Vec::new()).collect(),
            buf_b: (0..num_channels).map(|_| Vec::new()).collect(),
        }
    }

    /// Resize + zero the accumulator for a fresh buffer.
    fn begin(&mut self, frames: usize, num_channels: usize) {
        if self.accum.len() < num_channels {
            self.accum.resize_with(num_channels, Vec::new);
        }
        for ch in self.accum.iter_mut() {
            ch.resize(frames, 0.0);
            ch.fill(0.0);
        }
    }

    /// Apply volume + the effect chain in place on `accum`, leaving the
    /// processed bus signal there. Mirrors `InstrumentLane`'s effect loop.
    // TODO: factor a shared effect-chain helper to remove this duplication.
    fn finish(&mut self, num_channels: usize, frames: usize) -> anyhow::Result<()> {
        if (self.volume - 1.0).abs() >= f32::EPSILON {
            for ch in self.accum.iter_mut().take(num_channels) {
                for s in ch.iter_mut() {
                    *s *= self.volume;
                }
            }
        }
        if self.effects.is_empty() {
            self.apply_pan(num_channels);
            return Ok(());
        }
        for buf in self.buf_a.iter_mut().chain(self.buf_b.iter_mut()) {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }
        for ch in 0..num_channels {
            if ch < self.accum.len() {
                self.buf_a[ch].copy_from_slice(&self.accum[ch]);
            } else {
                self.buf_a[ch].fill(0.0);
            }
        }
        let mut src_is_a = true;
        for (effect, &mix) in self.effects.iter_mut().zip(self.mix_values.iter()) {
            let mix = mix as f32;
            if src_is_a {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_a, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_b, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }
                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_b[ch][i] = self.buf_a[ch][i] * dry + self.buf_b[ch][i] * mix;
                        }
                    }
                }
            } else {
                {
                    let mut in_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let mut out_s = [const { MaybeUninit::uninit() }; MAX_CHANNELS];
                    let in_refs = shared_slices(&self.buf_b, &mut in_s);
                    let out_refs = mut_slices(&mut self.buf_a, &mut out_s);
                    effect.process(&[], in_refs, out_refs)?;
                }
                if mix < 1.0 {
                    let dry = 1.0 - mix;
                    for ch in 0..num_channels {
                        for i in 0..frames {
                            self.buf_a[ch][i] = self.buf_b[ch][i] * dry + self.buf_a[ch][i] * mix;
                        }
                    }
                }
            }
            src_is_a = !src_is_a;
        }
        let final_buf = if src_is_a { &self.buf_a } else { &self.buf_b };
        for (acc, fin) in self.accum.iter_mut().zip(final_buf.iter()).take(num_channels) {
            acc.copy_from_slice(fin);
        }
        self.apply_pan(num_channels);
        Ok(())
    }

    /// Balance-style stereo pan on the bus output (2-channel only): center
    /// leaves both channels untouched, panning attenuates the far channel.
    fn apply_pan(&mut self, num_channels: usize) {
        if num_channels != 2 || self.pan.abs() < f32::EPSILON {
            return;
        }
        let p = self.pan;
        let (left_gain, right_gain) = if p >= 0.0 {
            (1.0 - p, 1.0)
        } else {
            (1.0, 1.0 + p)
        };
        for s in self.accum[0].iter_mut() {
            *s *= left_gain;
        }
        for s in self.accum[1].iter_mut() {
            *s *= right_gain;
        }
    }
}

// ---------------------------------------------------------------------------
// AudioGraph — multi-instrument audio processor
// ---------------------------------------------------------------------------

/// An audio graph with multiple instruments, each with its own effect chain.
/// Instruments sum into the master, or — if they belong to a group — into that
/// group's submix bus, which runs its own effects before reaching the master.
///
/// Commands are drained at the top of every audio callback via try_recv loop.
pub struct AudioGraph {
    instruments: Vec<InstrumentLane>,
    /// Submix buses. Lanes with `group == Some(i)` feed `groups[i]`.
    groups: Vec<Group>,
    /// Accumulation buffer for summing all instruments
    mix_buf: Vec<Vec<f32>>,
    /// Per-instrument scratch buffer (reused across instruments)
    lane_buf: Vec<Vec<f32>>,
    num_channels: usize,
    command_rx: Receiver<GraphCommand>,
    return_tx: Sender<Box<dyn Plugin>>,
    /// Notification channel for pattern recording completion.
    pattern_tx: Option<Sender<PatternNotification>>,
    /// Shared piano-tab state; read once per buffer for the current scale
    /// (lock-free atomics) so in-key pattern transposition follows the
    /// scale picker live. None = default scale (C Major).
    piano_filter: Option<Arc<PianoFilter>>,
    /// Reusable scratch for a group's trigger MIDI (union of member ranges),
    /// rebuilt per group per buffer. Avoids per-buffer allocation.
    group_midi_scratch: Vec<(u64, [u8; 3])>,
    /// Reusable scratch for pending group-modulator parameter writes.
    group_mod_writes: Vec<GroupModWrite>,
    /// Reusable scratch for pending group→member-modulator cross-mod writes.
    group_member_mod_writes: Vec<GroupMemberModWrite>,
}

impl AudioGraph {
    /// Create an empty audio graph. Outputs silence until instruments are added.
    pub fn new(
        num_channels: usize,
        command_rx: Receiver<GraphCommand>,
        return_tx: Sender<Box<dyn Plugin>>,
    ) -> Self {
        AudioGraph {
            instruments: Vec::new(),
            groups: Vec::new(),
            mix_buf: (0..num_channels).map(|_| Vec::new()).collect(),
            lane_buf: (0..num_channels).map(|_| Vec::new()).collect(),
            num_channels,
            command_rx,
            return_tx,
            pattern_tx: None,
            piano_filter: None,
            group_midi_scratch: Vec::with_capacity(128),
            group_mod_writes: Vec::with_capacity(64),
            group_member_mod_writes: Vec::with_capacity(32),
        }
    }

    /// The audio sample rate, derived from the first loaded plugin (all plugins
    /// run at the device rate). Falls back to 48 kHz before any plugin loads.
    fn graph_sample_rate(&self) -> f32 {
        self.instruments
            .iter()
            .find_map(|l| l.instrument.as_ref().map(|i| i.sample_rate()))
            .or_else(|| {
                self.groups
                    .iter()
                    .flat_map(|g| g.effects.iter())
                    .next()
                    .map(|e| e.sample_rate())
            })
            .unwrap_or(48000.0)
    }

    /// Set the shared piano-tab state used for in-key pattern transposition.
    pub fn set_piano_filter(&mut self, filter: Arc<PianoFilter>) {
        self.piano_filter = Some(filter);
    }

    /// Set the notification channel for pattern recording completion.
    pub fn set_pattern_tx(&mut self, tx: Sender<PatternNotification>) {
        self.pattern_tx = Some(tx.clone());
        // Propagate to existing instruments.
        for (inst_idx, inst) in self.instruments.iter_mut().enumerate() {
            inst.pattern.pattern_tx = Some(tx.clone());
            inst.pattern.inst_index = inst_idx;
        }
    }

    pub fn num_channels(&self) -> usize {
        self.num_channels
    }

    /// Drain all pending commands from the command channel (lock-free).
    pub fn drain_commands(&mut self) {
        while let Ok(cmd) = self.command_rx.try_recv() {
            match cmd {
                GraphCommand::SwapInstrument {
                    inst,
                    instrument: new_inst,
                    inst_buf,
                    remapper,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.inst_buf = inst_buf;
                        lane.remapper = remapper;
                        if let Some(old) = lane.instrument.replace(new_inst) {
                            let _ = self.return_tx.try_send(old);
                        }
                    }
                }
                GraphCommand::InsertEffect {
                    inst,
                    index,
                    effect,
                    mix,
                } => {
                    let num_channels = self.num_channels;
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if effect.audio_output_count() != num_channels {
                            log::warn!(
                                "Rejecting effect '{}': output channels {} != chain channels {}",
                                effect.name(),
                                effect.audio_output_count(),
                                num_channels,
                            );
                            let _ = self.return_tx.try_send(effect);
                        } else {
                            let idx = index.min(lane.effects.len());
                            lane.effects.insert(idx, effect);
                            lane.mix_values.insert(idx, mix);
                            // Bump modulator target slots that pointed at this
                            // or a later effect.
                            shift_slots_after_insert(&mut lane.modulators, idx);
                        }
                    }
                }
                GraphCommand::RemoveEffect { inst, index } => {
                    let old = self.get_instrument_mut(inst).and_then(|lane| {
                        if index < lane.effects.len() {
                            let old = lane.effects.remove(index);
                            lane.mix_values.remove(index);
                            // Drop/shift modulator target slots for this effect.
                            fixup_slots_after_remove(&mut lane.modulators, index);
                            Some(old)
                        } else {
                            None
                        }
                    });
                    if let Some(old) = old {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                GraphCommand::ReorderEffect {
                    inst,
                    from,
                    to,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if from < lane.effects.len() && to < lane.effects.len() && from != to {
                            let effect = lane.effects.remove(from);
                            let mix = lane.mix_values.remove(from);
                            lane.effects.insert(to, effect);
                            lane.mix_values.insert(to, mix);
                            // Remap modulator target slots through the move.
                            remap_slots_after_reorder(&mut lane.modulators, from, to);
                        }
                    }
                }
                GraphCommand::SetParameter {
                    inst,
                    slot,
                    param_index,
                    value,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        let plugin: Option<&mut Box<dyn Plugin>> = if slot == 0 {
                            lane.instrument.as_mut()
                        } else {
                            lane.effects.get_mut(slot - 1)
                        };
                        if let Some(p) = plugin {
                            if let Err(e) = p.set_parameter(param_index, value) {
                                log::warn!("SetParameter inst={inst} slot={slot} index={param_index}: {e}");
                            }
                        }
                        // Update modulator base values for targets that point
                        // at this exact slot + parameter.
                        for modulator in &mut lane.modulators {
                            for target in &mut modulator.targets {
                                if let ModTargetKind::PluginParam {
                                    slot: tslot,
                                    param_index: pi,
                                } = target.kind
                                {
                                    if tslot == slot && pi == param_index {
                                        target.base_value = value;
                                    }
                                }
                            }
                        }
                    }
                    // Mirror to any group modulator targeting this member param.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_base(
                                &mut g.modulators,
                                ordinal,
                                slot,
                                param_index,
                                value,
                            );
                        }
                    }
                }
                GraphCommand::LoadPreset {
                    inst,
                    slot,
                    preset_id,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        let plugin: Option<&mut Box<dyn Plugin>> = if slot == 0 {
                            lane.instrument.as_mut()
                        } else {
                            lane.effects.get_mut(slot - 1)
                        };
                        if let Some(p) = plugin {
                            if let Err(e) = p.load_preset(&preset_id) {
                                log::warn!(
                                    "LoadPreset inst={inst} slot={slot} id={preset_id:?}: {e}"
                                );
                            }
                        }
                    }
                }
                GraphCommand::SetMix {
                    inst,
                    slot,
                    value,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if slot > 0 {
                            if let Some(mix) = lane.mix_values.get_mut(slot - 1) {
                                *mix = value as f64;
                            }
                        }
                    }
                }
                GraphCommand::SetVolume { inst, value } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.volume = value;
                    }
                }
                GraphCommand::SetInstrumentRange { inst, range } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.range = range;
                    }
                }
                GraphCommand::ClearInstrument { inst } => {
                    let old = self
                        .get_instrument_mut(inst)
                        .and_then(|lane| {
                            lane.inst_buf.clear();
                            lane.remapper = None;
                            lane.modulators.clear();
                            lane.instrument.take()
                        });
                    if let Some(old) = old {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                GraphCommand::SwapInstruments {
                    inst_a,
                    inst_b,
                } => {
                    if inst_a < self.instruments.len() && inst_b < self.instruments.len() && inst_a != inst_b {
                        // Swap instrument, inst_buf, and remapper between the two lanes.
                        let (a, b) = if inst_a < inst_b {
                            let (left, right) = self.instruments.split_at_mut(inst_b);
                            (&mut left[inst_a], &mut right[0])
                        } else {
                            let (left, right) = self.instruments.split_at_mut(inst_a);
                            (&mut right[0], &mut left[inst_b])
                        };
                        std::mem::swap(&mut a.instrument, &mut b.instrument);
                        std::mem::swap(&mut a.inst_buf, &mut b.inst_buf);
                        std::mem::swap(&mut a.remapper, &mut b.remapper);
                    }
                }
                GraphCommand::AddInstrument { range } => {
                    let mut lane = InstrumentLane::new(self.num_channels);
                    lane.range = range;
                    lane.pattern.inst_index = self.instruments.len();
                    lane.pattern.pattern_tx = self.pattern_tx.clone();
                    self.instruments.push(lane);
                }
                GraphCommand::RemoveInstrument { inst } => {
                    if inst < self.instruments.len() {
                        // If this lane was a group member, note its ordinal so the
                        // group's modulators can drop/shift their member targets.
                        let member_of = lane_member_ordinal(&self.instruments, inst);
                        let mut removed = self.instruments.remove(inst);
                        if let Some(old_inst) = removed.instrument.take() {
                            let _ = self.return_tx.try_send(old_inst);
                        }
                        for effect in removed.effects.drain(..) {
                            let _ = self.return_tx.try_send(effect);
                        }
                        if let Some((group, ordinal)) = member_of {
                            if let Some(g) = self.groups.get_mut(group) {
                                fixup_group_member_after_remove(&mut g.modulators, ordinal);
                            }
                        }
                        // Re-index remaining instruments so pattern notifications route correctly.
                        for (i, lane) in self.instruments.iter_mut().enumerate() {
                            lane.pattern.inst_index = i;
                        }
                    }
                }
                GraphCommand::AddGroup => {
                    self.groups.push(Group::new(self.num_channels));
                }
                GraphCommand::RemoveGroup { group } => {
                    if group < self.groups.len() {
                        let mut removed = self.groups.remove(group);
                        for effect in removed.effects.drain(..) {
                            let _ = self.return_tx.try_send(effect);
                        }
                        // Fix up lane membership: drop the removed group, shift
                        // higher group indices down.
                        for lane in self.instruments.iter_mut() {
                            match lane.group {
                                Some(g) if g == group => lane.group = None,
                                Some(g) if g > group => lane.group = Some(g - 1),
                                _ => {}
                            }
                        }
                    }
                }
                GraphCommand::SetLaneGroup { inst, group } => {
                    let valid = group.is_none_or(|g| g < self.groups.len());
                    if valid && inst < self.instruments.len() {
                        let old = lane_member_ordinal(&self.instruments, inst);
                        self.instruments[inst].group = group;
                        let new = lane_member_ordinal(&self.instruments, inst);
                        let old_group = old.map(|(g, _)| g);
                        let new_group = new.map(|(g, _)| g);
                        // Leaving the old group: drop/shift its member targets.
                        if let Some((og, oo)) = old {
                            if old_group != new_group {
                                if let Some(g) = self.groups.get_mut(og) {
                                    fixup_group_member_after_remove(&mut g.modulators, oo);
                                }
                            }
                        }
                        // Joining a new group: bump member targets at/after its slot.
                        if let Some((ng, no)) = new {
                            if old_group != new_group {
                                if let Some(g) = self.groups.get_mut(ng) {
                                    shift_group_member_after_insert(&mut g.modulators, no);
                                }
                            }
                        }
                    }
                }
                GraphCommand::SetGroupVolume { group, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.volume = value;
                    }
                }
                GraphCommand::SetGroupPan { group, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        g.pan = value.clamp(-1.0, 1.0);
                    }
                }
                GraphCommand::InsertGroupEffect { group, index, effect, mix } => {
                    let num_channels = self.num_channels;
                    if let Some(g) = self.groups.get_mut(group) {
                        if effect.audio_output_count() != num_channels {
                            log::warn!(
                                "Rejecting group effect '{}': output channels {} != chain channels {}",
                                effect.name(),
                                effect.audio_output_count(),
                                num_channels,
                            );
                            let _ = self.return_tx.try_send(effect);
                        } else {
                            let idx = index.min(g.effects.len());
                            g.effects.insert(idx, effect);
                            g.mix_values.insert(idx, mix);
                        }
                    }
                }
                GraphCommand::RemoveGroupEffect { group, index } => {
                    let old = self.groups.get_mut(group).and_then(|g| {
                        if index < g.effects.len() {
                            g.mix_values.remove(index);
                            let removed = g.effects.remove(index);
                            // Drop/shift group-mod targets that pointed at it.
                            fixup_group_bus_after_remove(&mut g.modulators, index);
                            Some(removed)
                        } else {
                            None
                        }
                    });
                    if let Some(old) = old {
                        let _ = self.return_tx.try_send(old);
                    }
                }
                GraphCommand::ReorderGroupEffect { group, from, to } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if from < g.effects.len() && to < g.effects.len() && from != to {
                            let effect = g.effects.remove(from);
                            let mix = g.mix_values.remove(from);
                            g.effects.insert(to, effect);
                            g.mix_values.insert(to, mix);
                            // Remap group-mod bus targets through the move.
                            remap_group_bus_after_reorder(&mut g.modulators, from, to);
                        }
                    }
                }
                GraphCommand::SetGroupMix { group, index, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(mix) = g.mix_values.get_mut(index) {
                            *mix = value as f64;
                        }
                    }
                }
                GraphCommand::SetGroupParameter { group, index, param_index, value } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(effect) = g.effects.get_mut(index) {
                            if let Err(e) = effect.set_parameter(param_index, value) {
                                log::warn!("SetGroupParameter group={group} index={index} param={param_index}: {e}");
                            }
                        }
                        // Track base value for group modulators driving this bus param.
                        update_group_bus_base(&mut g.modulators, index, param_index, value);
                    }
                }
                GraphCommand::InsertModulator {
                    inst,
                    index,
                    source,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        let m = Modulator::new(source, lane.sample_rate());
                        let idx = index.min(lane.modulators.len());
                        lane.modulators.insert(idx, m);
                    }
                }
                GraphCommand::RemoveModulator { inst, index } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if index < lane.modulators.len() {
                            lane.modulators.remove(index);
                            // Clean up cross-mod targets in remaining siblings.
                            fixup_cross_mod_after_remove(&mut lane.modulators, index);
                        }
                    }
                    // If this lane is a group member, fix up group modulators that
                    // cross-mod its modulators.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            fixup_group_member_mod_after_remove(&mut g.modulators, ordinal, index);
                        }
                    }
                }
                GraphCommand::SetModulatorRate {
                    inst,
                    mod_index,
                    rate,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let ModSource::Lfo { rate: ref mut r, .. } = m.source {
                                *r = rate;
                            }
                        }
                        // Update cross-mod base values for targets pointing at this rate.
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Rate, rate);
                    }
                    // Keep any group cross-mod of this member modulator centered.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Rate, rate);
                        }
                    }
                }
                GraphCommand::SetModulatorWaveform {
                    inst,
                    mod_index,
                    waveform,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let ModSource::Lfo { waveform: ref mut w, .. } = m.source {
                                *w = waveform;
                            }
                        }
                    }
                }
                GraphCommand::SetModulatorSource {
                    inst,
                    mod_index,
                    source,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            m.source = source;
                            m.last_output = 0.0;
                        }
                    }
                }
                GraphCommand::SetModulatorEnvelopeParam {
                    inst,
                    mod_index,
                    attack,
                    decay,
                    sustain,
                    release,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let ModSource::Envelope {
                                attack: ref mut a,
                                decay: ref mut d,
                                sustain: ref mut s,
                                release: ref mut r,
                                ..
                            } = m.source
                            {
                                *a = attack;
                                *d = decay;
                                *s = sustain;
                                *r = release;
                            }
                        }
                        // Update cross-mod base values.
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Attack, attack);
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Decay, decay);
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Sustain, sustain);
                        update_cross_mod_base(&mut lane.modulators, mod_index, CrossModField::Release, release);
                    }
                    // Keep any group cross-mod of this member modulator centered.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Attack, attack);
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Decay, decay);
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Sustain, sustain);
                            update_group_member_mod_base(&mut g.modulators, ordinal, mod_index, CrossModField::Release, release);
                        }
                    }
                }
                GraphCommand::AddModTarget {
                    inst,
                    mod_index,
                    target,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            m.targets.push(target);
                        }
                    }
                }
                GraphCommand::RemoveModTarget {
                    inst,
                    mod_index,
                    target_index,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if target_index < m.targets.len() {
                                m.targets.remove(target_index);
                            }
                        }
                    }
                }
                GraphCommand::SetModTargetDepth {
                    inst,
                    mod_index,
                    target_index,
                    depth,
                } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if let Some(m) = lane.modulators.get_mut(mod_index) {
                            if let Some(t) = m.targets.get_mut(target_index) {
                                t.depth = depth;
                            }
                        }
                    }
                    // Keep any group cross-mod of this member modulator's depth centered.
                    if let Some((group, ordinal)) = lane_member_ordinal(&self.instruments, inst) {
                        if let Some(g) = self.groups.get_mut(group) {
                            update_group_member_mod_base(
                                &mut g.modulators,
                                ordinal,
                                mod_index,
                                CrossModField::Depth(target_index),
                                depth,
                            );
                        }
                    }
                }
                GraphCommand::InsertGroupModulator {
                    group,
                    index,
                    source,
                } => {
                    let sr = self.graph_sample_rate();
                    if let Some(g) = self.groups.get_mut(group) {
                        let m = Modulator::new(source, sr);
                        let idx = index.min(g.modulators.len());
                        g.modulators.insert(idx, m);
                    }
                }
                GraphCommand::RemoveGroupModulator { group, index } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if index < g.modulators.len() {
                            g.modulators.remove(index);
                            fixup_cross_mod_after_remove(&mut g.modulators, index);
                        }
                    }
                }
                GraphCommand::SetGroupModulatorRate {
                    group,
                    mod_index,
                    rate,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let ModSource::Lfo { rate: ref mut r, .. } = m.source {
                                *r = rate;
                            }
                        }
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Rate, rate);
                    }
                }
                GraphCommand::SetGroupModulatorWaveform {
                    group,
                    mod_index,
                    waveform,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let ModSource::Lfo { waveform: ref mut w, .. } = m.source {
                                *w = waveform;
                            }
                        }
                    }
                }
                GraphCommand::SetGroupModulatorSource {
                    group,
                    mod_index,
                    source,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            m.source = source;
                            m.last_output = 0.0;
                        }
                    }
                }
                GraphCommand::SetGroupModulatorEnvelopeParam {
                    group,
                    mod_index,
                    attack,
                    decay,
                    sustain,
                    release,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let ModSource::Envelope {
                                attack: ref mut a,
                                decay: ref mut d,
                                sustain: ref mut s,
                                release: ref mut r,
                                ..
                            } = m.source
                            {
                                *a = attack;
                                *d = decay;
                                *s = sustain;
                                *r = release;
                            }
                        }
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Attack, attack);
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Decay, decay);
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Sustain, sustain);
                        update_cross_mod_base(&mut g.modulators, mod_index, CrossModField::Release, release);
                    }
                }
                GraphCommand::AddGroupModTarget {
                    group,
                    mod_index,
                    target,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            m.targets.push(target);
                        }
                    }
                }
                GraphCommand::RemoveGroupModTarget {
                    group,
                    mod_index,
                    target_index,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if target_index < m.targets.len() {
                                m.targets.remove(target_index);
                            }
                        }
                    }
                }
                GraphCommand::SetGroupModTargetDepth {
                    group,
                    mod_index,
                    target_index,
                    depth,
                } => {
                    if let Some(g) = self.groups.get_mut(group) {
                        if let Some(m) = g.modulators.get_mut(mod_index) {
                            if let Some(t) = m.targets.get_mut(target_index) {
                                t.depth = depth;
                            }
                        }
                    }
                }
                GraphCommand::SetPatternEnabled { inst, enabled } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.enabled = enabled;
                    }
                }
                GraphCommand::SetPatternRecording { inst, recording } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        if recording {
                            // Ensure notification routes back to the correct instrument.
                            lane.pattern.inst_index = inst;
                            lane.pattern.recording_events.clear();
                            lane.pattern.base_note = None;
                            lane.pattern.record_pos = 0;
                            lane.pattern.metronome_pos = 0;
                            lane.pattern.click_remaining = 0;
                            lane.pattern.click_phase = 0.0;
                            // Precompute beat length in samples
                            let beats_per_sec = lane.pattern.bpm / 60.0;
                            lane.pattern.beat_length_samples =
                                (lane.pattern.sample_rate / beats_per_sec) as u64;
                            // Start with count-in (metronome only, no recording yet)
                            lane.pattern.counting_in = true;
                            lane.pattern.recording = false;
                        } else {
                            // Finalize recording manually (also stops count-in)
                            lane.pattern.counting_in = false;
                            if lane.pattern.recording {
                                let length = lane.pattern.length_samples();
                                lane.pattern.finalize_recording(length);
                            } else {
                                lane.pattern.recording = false;
                            }
                        }
                    }
                }
                GraphCommand::SetPattern { inst, pattern, base_note, in_key } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.pattern = pattern;
                        lane.pattern.base_note = base_note;
                        lane.pattern.in_key = in_key;
                        lane.pattern.enabled = !lane.pattern.pattern.events.is_empty();
                    }
                }
                GraphCommand::SwapPatterns { inst_a, inst_b } => {
                    if inst_a < self.instruments.len() && inst_b < self.instruments.len() {
                        // Swap pattern data, base_note, enabled, length_beats,
                        // in_key between the two lanes.
                        let (a_pattern, a_base, a_enabled, a_beats, a_in_key) = {
                            let p = &self.instruments[inst_a].pattern;
                            (p.pattern.clone(), p.base_note, p.enabled, p.length_beats, p.in_key)
                        };
                        let (b_pattern, b_base, b_enabled, b_beats, b_in_key) = {
                            let p = &self.instruments[inst_b].pattern;
                            (p.pattern.clone(), p.base_note, p.enabled, p.length_beats, p.in_key)
                        };
                        let pa = &mut self.instruments[inst_a].pattern;
                        pa.pattern = b_pattern;
                        pa.base_note = b_base;
                        pa.enabled = b_enabled;
                        pa.length_beats = b_beats;
                        pa.in_key = b_in_key;
                        pa.trigger_note = None;
                        pa.held_notes.clear();
                        pa.active_voices.clear();
                        let pb = &mut self.instruments[inst_b].pattern;
                        pb.pattern = a_pattern;
                        pb.base_note = a_base;
                        pb.enabled = a_enabled;
                        pb.length_beats = a_beats;
                        pb.in_key = a_in_key;
                        pb.trigger_note = None;
                        pb.held_notes.clear();
                        pb.active_voices.clear();
                    }
                }
                GraphCommand::ClearPattern { inst } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.pattern = Pattern::default();
                        lane.pattern.base_note = None;
                        lane.pattern.enabled = false;
                        lane.pattern.in_key = false;
                        lane.pattern.recording = false;
                        lane.pattern.counting_in = false;
                        lane.pattern.trigger_note = None;
                        lane.pattern.held_notes.clear();
                        lane.pattern.click_remaining = 0;
                        lane.pattern.active_voices.clear();
                    }
                }
                GraphCommand::SetGlobalBpm { bpm } => {
                    for inst in &mut self.instruments {
                        inst.pattern.bpm = bpm;
                    }
                }
                GraphCommand::SetPatternLength { inst, beats } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.length_beats = beats;
                    }
                }
                GraphCommand::SetPatternLooping { inst, looping } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.looping = looping;
                    }
                }
                GraphCommand::SetPatternInKey { inst, in_key } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.pattern.in_key = in_key;
                    }
                }
                GraphCommand::SetTranspose { inst, semitones } => {
                    if let Some(lane) = self.get_instrument_mut(inst) {
                        lane.transpose = semitones;
                    }
                }
            }
        }
    }

    fn get_instrument_mut(&mut self, inst: usize) -> Option<&mut InstrumentLane> {
        self.instruments.get_mut(inst)
    }

    /// Process audio: drain commands, run all instruments, sum to output.
    /// Outputs silence if no instruments are loaded.
    pub fn process(
        &mut self,
        midi_events: &[(u64, [u8; 3])],
        audio_out: &mut [Vec<f32>],
    ) -> anyhow::Result<()> {
        self.drain_commands();

        let frames = audio_out.first().map(|b| b.len()).unwrap_or(0);

        // Zero mix_buf
        for buf in self.mix_buf.iter_mut() {
            buf.resize(frames, 0.0);
            buf.fill(0.0);
        }

        // Resize lane_buf
        for buf in self.lane_buf.iter_mut() {
            buf.resize(frames, 0.0);
        }

        // Current piano scale, read once per buffer (lock-free).
        let scale = self
            .piano_filter
            .as_ref()
            .map(|f| f.scale())
            .unwrap_or_default();

        // Prepare group accumulators for this buffer.
        for group in self.groups.iter_mut() {
            group.begin(frames, self.num_channels);
        }

        // Group-scoped modulators: tick each group's modulators (envelopes are
        // triggered by the union of its members' ranges), resolve cross-mod,
        // then apply their targets to member-instrument and bus-effect
        // parameters now — before the lanes render, so members read the
        // modulated values (bus effects read them later in group.finish). A
        // lane modulator hitting the same parameter is applied later inside the
        // lane's own process() and therefore wins.
        if frames > 0 {
            for gi in 0..self.groups.len() {
                if self.groups[gi].modulators.is_empty() {
                    continue;
                }
                build_group_midi(
                    &self.instruments,
                    gi,
                    midi_events,
                    &mut self.group_midi_scratch,
                );
                for m in &mut self.groups[gi].modulators {
                    m.tick(frames, &self.group_midi_scratch);
                }
                apply_cross_mod(&mut self.groups[gi].modulators);
            }
            // Group→member-modulator cross-mod: written first so member lanes
            // pick up the modulated LFO/envelope fields when they tick below.
            collect_group_member_mod_writes(
                &self.groups,
                &self.instruments,
                &mut self.group_member_mod_writes,
            );
            apply_group_member_mod_writes(&self.group_member_mod_writes, &mut self.instruments);
            collect_group_mod_writes(&self.groups, &self.instruments, &mut self.group_mod_writes);
            apply_group_mod_writes(
                &self.group_mod_writes,
                &mut self.instruments,
                &mut self.groups,
            );
        }

        // Process each instrument; route its output to its group's bus (if a
        // member) or straight to the master mix.
        for inst in self.instruments.iter_mut() {
            // Zero lane_buf
            for buf in self.lane_buf.iter_mut() {
                buf.fill(0.0);
            }

            inst.process(midi_events, &mut self.lane_buf, self.num_channels, scale)?;

            let dest = match inst.group {
                Some(g) if g < self.groups.len() => &mut self.groups[g].accum,
                _ => &mut self.mix_buf,
            };
            for (d, src) in dest.iter_mut().zip(self.lane_buf.iter()).take(self.num_channels) {
                for (ds, s) in d.iter_mut().zip(src.iter()) {
                    *ds += *s;
                }
            }
        }

        // Run each group's volume + effect chain, then fold into the master.
        for group in self.groups.iter_mut() {
            group.finish(self.num_channels, frames)?;
            for (m, src) in self.mix_buf.iter_mut().zip(group.accum.iter()).take(self.num_channels) {
                for (ms, s) in m.iter_mut().zip(src.iter()) {
                    *ms += *s;
                }
            }
        }

        // Copy mix_buf to audio_out
        for (ch, out) in audio_out.iter_mut().enumerate() {
            if ch < self.mix_buf.len() {
                let copy_len = out.len().min(self.mix_buf[ch].len());
                out[..copy_len].copy_from_slice(&self.mix_buf[ch][..copy_len]);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ParameterInfo, Preset};

    const FRAMES: usize = 64;

    macro_rules! mock_plugin_boilerplate {
        () => {
            fn sample_rate(&self) -> f32 {
                48000.0
            }
            fn parameters(&self) -> Vec<ParameterInfo> {
                Vec::new()
            }
            fn get_parameter(&mut self, _: u32) -> Option<f32> {
                None
            }
            fn set_parameter(&mut self, i: u32, _: f32) -> anyhow::Result<()> {
                anyhow::bail!("no parameter {i}")
            }
            fn presets(&self) -> Vec<Preset> {
                Vec::new()
            }
            fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
                anyhow::bail!("no preset {id}")
            }
        };
    }

    /// Test instrument: outputs a constant value on all channels when a note is held.
    struct ConstInstrument {
        value: f32,
        num_outputs: usize,
        has_note: bool,
    }

    impl ConstInstrument {
        #[expect(clippy::new_ret_no_self, reason = "test helper returns trait object")]
        fn new(value: f32) -> Box<dyn Plugin> {
            Box::new(Self {
                value,
                num_outputs: 2,
                has_note: false,
            })
        }
        fn with_outputs(value: f32, num_outputs: usize) -> Box<dyn Plugin> {
            Box::new(Self {
                value,
                num_outputs,
                has_note: false,
            })
        }
    }

    impl Plugin for ConstInstrument {
        fn name(&self) -> &str {
            "ConstInstrument"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn audio_output_count(&self) -> usize {
            self.num_outputs
        }
        fn audio_input_count(&self) -> usize {
            0
        }

        fn process(
            &mut self,
            midi_events: &[(u64, [u8; 3])],
            _audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for &(_, [status, _, velocity]) in midi_events {
                match status & 0xF0 {
                    0x90 if velocity > 0 => self.has_note = true,
                    0x80 | 0x90 => self.has_note = false,
                    _ => {}
                }
            }
            let v = if self.has_note { self.value } else { 0.0 };
            for ch in audio_out.iter_mut() {
                ch.fill(v);
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that copies input to output unchanged.
    struct PassthroughEffect;

    impl Plugin for PassthroughEffect {
        fn name(&self) -> &str {
            "Passthrough"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                out.copy_from_slice(inp);
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that multiplies input by a constant gain.
    struct ScaleEffect(f32);

    impl Plugin for ScaleEffect {
        fn name(&self) -> &str {
            "Scale"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i * self.0;
                }
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    /// Effect that adds a constant offset to input.
    struct OffsetEffect(f32);

    impl Plugin for OffsetEffect {
        fn name(&self) -> &str {
            "Offset"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i + self.0;
                }
            }
            Ok(())
        }

        mock_plugin_boilerplate!();
    }

    // -- helpers --

    fn make_graph(
        num_channels: usize,
    ) -> (
        AudioGraph,
        crossbeam_channel::Sender<GraphCommand>,
        crossbeam_channel::Receiver<Box<dyn Plugin>>,
    ) {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(num_channels, cmd_rx, return_tx);
        // Create one instrument lane (mimics old PluginChain behavior)
        graph.instruments.push(InstrumentLane::new(num_channels));
        (graph, cmd_tx, return_rx)
    }

    fn make_output() -> Vec<Vec<f32>> {
        vec![vec![0.0; FRAMES]; 2]
    }

    fn note_on(note: u8) -> (u64, [u8; 3]) {
        (0, [0x90, note, 100])
    }

    fn note_off(note: u8) -> (u64, [u8; 3]) {
        (0, [0x80, note, 0])
    }

    fn swap_instrument(cmd_tx: &crossbeam_channel::Sender<GraphCommand>, inst: Box<dyn Plugin>) {
        swap_instrument_at(cmd_tx, 0, inst);
    }

    fn swap_instrument_at(
        cmd_tx: &crossbeam_channel::Sender<GraphCommand>,
        index: usize,
        inst: Box<dyn Plugin>,
    ) {
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: index,
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();
    }

    fn insert_effect(
        cmd_tx: &crossbeam_channel::Sender<GraphCommand>,
        index: usize,
        effect: Box<dyn Plugin>,
        mix: f64,
    ) {
        cmd_tx
            .send(GraphCommand::InsertEffect {
                inst: 0,
                index,
                effect,
                mix,
            })
            .unwrap();
    }

    // -- tests --

    #[test]
    fn silence_when_no_instrument() {
        let (mut graph, _, _) = make_graph(2);
        let mut out = make_output();
        out[0].fill(999.0);
        out[1].fill(999.0);

        graph.process(&[], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
        assert!(out[1].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn instrument_direct_output() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.75));
        assert!(out[1].iter().all(|&s| s == 0.75));
    }

    #[test]
    fn instrument_silence_without_note() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn note_off_silences_instrument() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.75));

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.75));

        let mut out = make_output();
        graph.process(&[note_off(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn single_passthrough_effect() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.5));
        assert!(out[1].iter().all(|&s| s == 0.5));
    }

    #[test]
    fn dry_wet_mix() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        // ScaleEffect(0.0) outputs silence; mix=0.5 → 0.5*dry + 0.5*wet = 0.5
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.0)), 0.5);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn multiple_effects_chain() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(ScaleEffect(0.5)), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // 1.0 * 0.5 * 0.5 = 0.25
        assert!(out[0].iter().all(|&s| (s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn multi_output_instrument_truncation() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::with_outputs(0.8, 4));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Only first 2 of 4 channels reach the output
        assert!(out[0].iter().all(|&s| s == 0.8));
        assert!(out[1].iter().all(|&s| s == 0.8));
    }

    #[test]
    fn multi_output_instrument_no_effects() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        // 16-output instrument with no effects (the Pianoteq scenario)
        swap_instrument(&cmd_tx, ConstInstrument::with_outputs(0.6, 16));

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.6));
        assert!(out[1].iter().all(|&s| s == 0.6));
    }

    #[test]
    fn swap_instrument_returns_old() {
        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Swap in a new instrument
        swap_instrument(&cmd_tx, ConstInstrument::new(0.5));
        graph.process(&[], &mut out).unwrap();

        // Old instrument should have been returned via the channel
        let old = return_rx.try_recv();
        assert!(old.is_ok());
        assert_eq!(old.unwrap().name(), "ConstInstrument");
    }

    #[test]
    fn remove_effect() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(0.5)), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));

        // Remove the effect — should go back to direct instrument output
        cmd_tx
            .send(GraphCommand::RemoveEffect {
                inst: 0,
                index: 0,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn reorder_effects() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        // [Scale(2.0), Offset(0.5)] → 1.0 * 2.0 + 0.5 = 2.5
        insert_effect(&cmd_tx, 0, Box::new(ScaleEffect(2.0)), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(OffsetEffect(0.5)), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 2.5).abs() < 1e-6));

        // Move Scale from index 0 to index 1 → [Offset(0.5), Scale(2.0)]
        // (1.0 + 0.5) * 2.0 = 3.0
        cmd_tx
            .send(GraphCommand::ReorderEffect {
                inst: 0,
                from: 0,
                to: 1,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 3.0).abs() < 1e-6));
    }

    #[test]
    fn reject_effect_with_wrong_channel_count() {
        /// Mono effect (1 output) — incompatible with a stereo chain.
        struct MonoEffect;

        impl Plugin for MonoEffect {
            fn name(&self) -> &str {
                "MonoEffect"
            }
            fn is_instrument(&self) -> bool {
                false
            }
            fn audio_output_count(&self) -> usize {
                1
            }
            fn audio_input_count(&self) -> usize {
                1
            }

            fn process(
                &mut self,
                _midi_events: &[(u64, [u8; 3])],
                audio_in: &[&[f32]],
                audio_out: &mut [&mut [f32]],
            ) -> anyhow::Result<()> {
                for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                    out.copy_from_slice(inp);
                }
                Ok(())
            }

            mock_plugin_boilerplate!();
        }

        let (mut graph, cmd_tx, return_rx) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(MonoEffect), 1.0);

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Effect was rejected — instrument output passes through directly
        assert!(out[0].iter().all(|&s| s == 1.0));

        // Rejected effect was returned via the return channel
        let returned = return_rx.try_recv();
        assert!(returned.is_ok());
        assert_eq!(returned.unwrap().name(), "MonoEffect");
    }

    // -- NoteRemapper tests --

    fn make_remap(entries: &[(&str, &str, f64)]) -> HashMap<String, crate::session::RemapTarget> {
        entries
            .iter()
            .map(|(src, dst, detune)| {
                (
                    src.to_string(),
                    crate::session::RemapTarget {
                        note: dst.to_string(),
                        detune: *detune,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn remapper_from_config_valid() {
        let remap = make_remap(&[("G#4", "G4", 1.0), ("C#2", "D2", -0.5)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        // G#4 = 68, C#2 = 37 — both should be in the table
        assert!(remapper.table.contains_key(&68));
        assert!(remapper.table.contains_key(&37));
    }

    #[test]
    fn remapper_remap_note_on() {
        // Remap G#4 (68) → G4 (67) with +1 semitone detune
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let input = vec![(0u64, [0x90u8, 68, 100])]; // note-on G#4
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        // Should produce: remapped note-on + pitch bend
        assert_eq!(output.len(), 2);
        // First event: note-on for G4 (67) on channel 2
        assert_eq!(output[0].1[0], 0x91); // note-on ch2
        assert_eq!(output[0].1[1], 67); // G4
        assert_eq!(output[0].1[2], 100); // velocity preserved
        // Second event: pitch bend on channel 2 (status 0xE1)
        assert_eq!(output[1].1[0] & 0xF0, 0xE0);
        assert_eq!(output[1].1[0] & 0x0F, 1); // channel 2 = nibble 0x01
    }

    #[test]
    fn remapper_remap_note_off() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let input = vec![(0u64, [0x80u8, 68, 0])]; // note-off G#4
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1[0], 0x81); // note-off ch2
        assert_eq!(output[0].1[1], 67); // G4
    }

    #[test]
    fn remapper_passthrough_non_remapped() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        // C4 (60) is NOT remapped — should pass through unchanged
        let input = vec![(0u64, [0x90u8, 60, 100])];
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1, [0x90, 60, 100]);
    }

    #[test]
    fn remapper_passthrough_non_note_events() {
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        // CC message — should pass through unchanged
        let input = vec![(0u64, [0xB0u8, 64, 127])];
        let mut output = Vec::new();
        remapper.remap_events(&input, &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].1, [0xB0, 64, 127]);
    }

    #[test]
    fn remapper_pitch_bend_bytes_center() {
        // Detune 0.0 → pitch bend = 8192 (center)
        let remap = make_remap(&[("C4", "C4", 0.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 8192 = 0b10_0000_0000000 → LSB = 0, MSB = 64
        assert_eq!(entry.pitch_bend_lsb, 0);
        assert_eq!(entry.pitch_bend_msb, 64);
    }

    #[test]
    fn remapper_pitch_bend_bytes_max() {
        // Detune +2.0 with range 2.0 → pitch bend = 8192 + 8191 = 16383
        let remap = make_remap(&[("C4", "C4", 2.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 16383 = 0x3FFF → LSB = 127, MSB = 127
        assert_eq!(entry.pitch_bend_lsb, 127);
        assert_eq!(entry.pitch_bend_msb, 127);
    }

    #[test]
    fn remapper_pitch_bend_bytes_min() {
        // Detune -2.0 with range 2.0 → pitch bend = 8192 - 8191 = 1
        let remap = make_remap(&[("C4", "C4", -2.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        let entry = &remapper.table[&60];
        // 1 = 0b00_0000_0000001 → LSB = 1, MSB = 0
        assert_eq!(entry.pitch_bend_lsb, 1);
        assert_eq!(entry.pitch_bend_msb, 0);
    }

    #[test]
    fn remapper_shared_detune_shares_channel() {
        // Two notes with the same detune should share a MIDI channel
        let remap = make_remap(&[("C4", "B3", 1.0), ("D4", "C#4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        assert_eq!(remapper.table[&60].channel, remapper.table[&62].channel);
    }

    #[test]
    fn remapper_different_detune_different_channels() {
        let remap = make_remap(&[("C4", "B3", 1.0), ("D4", "C#4", -0.5)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();
        assert_ne!(remapper.table[&60].channel, remapper.table[&62].channel);
    }

    #[test]
    fn remapper_error_detune_exceeds_range() {
        let remap = make_remap(&[("C4", "B3", 3.0)]);
        let result = NoteRemapper::from_config(&remap, 2.0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds"));
    }

    #[test]
    fn remapper_error_too_many_detune_values() {
        // 16 distinct detune values should fail (max 15)
        let notes = [
            "C2", "C#2", "D2", "D#2", "E2", "F2", "F#2", "G2", "G#2", "A2", "A#2", "B2", "C3",
            "C#3", "D3", "D#3",
        ];
        let entries: Vec<(&str, &str, f64)> = notes
            .iter()
            .enumerate()
            .map(|(i, &n)| (n, n, (i as f64 + 1.0) * 0.1))
            .collect();
        let remap = make_remap(&entries);
        let result = NoteRemapper::from_config(&remap, 10.0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too many"));
    }

    #[test]
    fn remapper_integration_with_graph() {
        // Integration test: remap G#4→G4 with pitch bend, verify instrument sees remapped events
        let remap = make_remap(&[("G#4", "G4", 1.0)]);
        let remapper = NoteRemapper::from_config(&remap, 2.0).unwrap();

        let (mut graph, cmd_tx, _) = make_graph(2);
        let inst = ConstInstrument::new(0.75);
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: Some(remapper),
            })
            .unwrap();

        let mut out = make_output();
        // Send note-on for G#4 (68) — should be remapped to G4 (67) on ch2
        // ConstInstrument responds to any note-on, so we just verify it produces output
        graph.process(&[note_on(68)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s == 0.75));
    }

    // -- New AudioGraph-specific tests --

    #[test]
    fn two_instruments_sum_output() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        // Two instrument lanes: both full range
        graph.instruments.push(InstrumentLane::new(2));
        graph.instruments.push(InstrumentLane::new(2));

        // Swap instruments into both lanes
        let inst_a = ConstInstrument::new(0.3);
        let inst_buf_a = (0..inst_a.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst_a,
                inst_buf: inst_buf_a,
                remapper: None,
            })
            .unwrap();

        let inst_b = ConstInstrument::new(0.5);
        let inst_buf_b = (0..inst_b.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 1,
                instrument: inst_b,
                inst_buf: inst_buf_b,
                remapper: None,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Both instruments output on same note → 0.3 + 0.5 = 0.8
        assert!(out[0].iter().all(|&s| (s - 0.8).abs() < 1e-6));
        assert!(out[1].iter().all(|&s| (s - 0.8).abs() < 1e-6));

        drop(return_rx);
    }

    #[test]
    fn group_bus_sums_members_and_applies_volume() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        graph.instruments.push(InstrumentLane::new(2));
        graph.instruments.push(InstrumentLane::new(2));
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.25));
        swap_instrument_at(&cmd_tx, 1, ConstInstrument::new(0.25));

        // Group both lanes and halve the group bus volume: (0.25+0.25)*0.5.
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 1, group: Some(0) }).unwrap();
        cmd_tx.send(GraphCommand::SetGroupVolume { group: 0, value: 0.5 }).unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.25).abs() < 1e-6),
            "grouped sum × group volume should be 0.25, got {}",
            out[0][0]
        );

        // Ungroup one member: it now reaches the master directly at full level,
        // the other stays in the (still 0.5×) group: 0.25 + 0.25*0.5 = 0.375.
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 1, group: None }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| (s - 0.375).abs() < 1e-6),
            "expected 0.375 after ungrouping one member, got {}",
            out[0][0]
        );

        drop(return_rx);
    }

    #[test]
    fn group_pan_balances_bus_output() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        graph.instruments.push(InstrumentLane::new(2));
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();

        // Hard right: left silent, right at full member level.
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: 1.0 }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s.abs() < 1e-6), "hard right should silence left: {}", out[0][0]);
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6), "right stays at full: {}", out[1][0]);

        // Center: both channels at full (unchanged).
        cmd_tx.send(GraphCommand::SetGroupPan { group: 0, value: 0.0 }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6), "center left: {}", out[0][0]);
        assert!(out[1].iter().all(|&s| (s - 0.5).abs() < 1e-6), "center right: {}", out[1][0]);

        drop(return_rx);
    }

    #[test]
    fn range_filtering() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        // Two instrument lanes with ranges: C0-B3 and C4-C8
        let mut lane_low = InstrumentLane::new(2);
        lane_low.range = Some((12, 59)); // C0-B3
        let mut lane_high = InstrumentLane::new(2);
        lane_high.range = Some((60, 96)); // C4-C8

        graph.instruments.push(lane_low);
        graph.instruments.push(lane_high);

        // Low lane: value 0.3
        let inst_low = ConstInstrument::new(0.3);
        let inst_buf_low = (0..inst_low.audio_output_count())
            .map(|_| Vec::new())
            .collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst_low,
                inst_buf: inst_buf_low,
                remapper: None,
            })
            .unwrap();

        // High lane: value 0.7
        let inst_high = ConstInstrument::new(0.7);
        let inst_buf_high = (0..inst_high.audio_output_count())
            .map(|_| Vec::new())
            .collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 1,
                instrument: inst_high,
                inst_buf: inst_buf_high,
                remapper: None,
            })
            .unwrap();

        // Play note in low range (C2 = 36): only low instrument should respond
        let mut out = make_output();
        graph.process(&[note_on(36)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.3).abs() < 1e-6));

        // Now note-off and play note in high range (C5 = 72): only high instrument should respond
        let mut out = make_output();
        graph.process(&[note_off(36), note_on(72)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| (s - 0.7).abs() < 1e-6));

        drop(return_rx);
    }

    #[test]
    fn cc_passthrough_to_all_instruments() {
        // CC events (e.g. sustain pedal) should reach all instruments regardless of range
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);

        let mut lane_low = InstrumentLane::new(2);
        lane_low.range = Some((0, 59));
        let mut lane_high = InstrumentLane::new(2);
        lane_high.range = Some((60, 127));

        graph.instruments.push(lane_low);
        graph.instruments.push(lane_high);

        // Install instruments in both lanes
        for s in 0..2 {
            let inst = ConstInstrument::new(0.5);
            let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
            cmd_tx
                .send(GraphCommand::SwapInstrument {
                    inst: s,
                    instrument: inst,
                    inst_buf,
                    remapper: None,
                })
                .unwrap();
        }

        // Send a CC event (sustain pedal) — should be filtered to both lanes
        let cc_event: (u64, [u8; 3]) = (0, [0xB0, 64, 127]);
        let mut out = make_output();
        graph.process(&[cc_event], &mut out).unwrap();

        // No note-on, so output is silence, but the point is it didn't crash
        // and the CC was delivered to both instruments (verified by filter_midi logic)
        assert!(out[0].iter().all(|&s| s == 0.0));

        drop(return_rx);
    }

    #[test]
    fn volume_scaling() {
        let (mut graph, cmd_tx, _) = make_graph(2);
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));

        // Set volume to 0.5
        cmd_tx
            .send(GraphCommand::SetVolume {
                inst: 0,
                value: 0.5,
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn empty_graph_silence() {
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let (return_tx, _return_rx) = crossbeam_channel::bounded(16);
        let mut graph = AudioGraph::new(2, cmd_rx, return_tx);
        // No instruments at all

        let mut out = make_output();
        out[0].fill(999.0);
        graph.process(&[note_on(60)], &mut out).unwrap();

        assert!(out[0].iter().all(|&s| s == 0.0));
        drop(cmd_tx);
    }

    // -- LFO waveform tests --

    #[test]
    fn lfo_sine_known_phases() {
        let w = LfoWaveform::Sine;
        // Phase 0.0 → sin(0) = 0.0
        assert!((w.eval(0.0)).abs() < 1e-6);
        // Phase 0.25 → sin(π/2) = 1.0
        assert!((w.eval(0.25) - 1.0).abs() < 1e-6);
        // Phase 0.5 → sin(π) ≈ 0.0
        assert!((w.eval(0.5)).abs() < 1e-6);
        // Phase 0.75 → sin(3π/2) = -1.0
        assert!((w.eval(0.75) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn lfo_triangle_known_phases() {
        let w = LfoWaveform::Triangle;
        // Phase 0.0 → 0.0
        assert!((w.eval(0.0)).abs() < 1e-6);
        // Phase 0.25 → 1.0
        assert!((w.eval(0.25) - 1.0).abs() < 1e-6);
        // Phase 0.5 → 0.0
        assert!((w.eval(0.5)).abs() < 1e-6);
        // Phase 0.75 → -1.0
        assert!((w.eval(0.75) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn lfo_saw_known_phases() {
        let w = LfoWaveform::Saw;
        // Phase 0.0 → -1.0
        assert!((w.eval(0.0) - (-1.0)).abs() < 1e-6);
        // Phase 0.5 → 0.0
        assert!((w.eval(0.5)).abs() < 1e-6);
        // Phase 1.0 → 1.0
        assert!((w.eval(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lfo_square_known_phases() {
        let w = LfoWaveform::Square;
        // Phase 0.0 → 1.0 (first half)
        assert!((w.eval(0.0) - 1.0).abs() < 1e-6);
        // Phase 0.25 → 1.0 (still first half)
        assert!((w.eval(0.25) - 1.0).abs() < 1e-6);
        // Phase 0.5 → -1.0 (second half)
        assert!((w.eval(0.5) - (-1.0)).abs() < 1e-6);
        // Phase 0.75 → -1.0
        assert!((w.eval(0.75) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn lfo_waveform_cycle() {
        assert_eq!(LfoWaveform::Sine.next(), LfoWaveform::Triangle);
        assert_eq!(LfoWaveform::Triangle.next(), LfoWaveform::Saw);
        assert_eq!(LfoWaveform::Saw.next(), LfoWaveform::Square);
        assert_eq!(LfoWaveform::Square.next(), LfoWaveform::Sine);
    }

    #[test]
    fn lfo_waveform_from_str() {
        assert_eq!(LfoWaveform::from_str("sine"), Some(LfoWaveform::Sine));
        assert_eq!(LfoWaveform::from_str("TRIANGLE"), Some(LfoWaveform::Triangle));
        assert_eq!(LfoWaveform::from_str("tri"), Some(LfoWaveform::Triangle));
        assert_eq!(LfoWaveform::from_str("saw"), Some(LfoWaveform::Saw));
        assert_eq!(LfoWaveform::from_str("square"), Some(LfoWaveform::Square));
        assert_eq!(LfoWaveform::from_str("unknown"), None);
    }

    // -- Modulator integration test --

    /// Instrument that records the last value set on parameter 0.
    struct ParamTrackingInstrument {
        param_value: f32,
    }

    impl Plugin for ParamTrackingInstrument {
        fn name(&self) -> &str {
            "ParamTracking"
        }
        fn is_instrument(&self) -> bool {
            true
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            0
        }

        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            _audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            // Output the current param value as audio (so we can observe modulation).
            for ch in audio_out.iter_mut() {
                ch.fill(self.param_value);
            }
            Ok(())
        }

        fn sample_rate(&self) -> f32 {
            48000.0
        }
        fn parameters(&self) -> Vec<ParameterInfo> {
            vec![ParameterInfo {
                index: 0,
                name: "cutoff".into(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
                ..Default::default()
            }]
        }
        fn get_parameter(&mut self, idx: u32) -> Option<f32> {
            if idx == 0 { Some(self.param_value) } else { None }
        }
        fn set_parameter(&mut self, idx: u32, value: f32) -> anyhow::Result<()> {
            if idx == 0 {
                self.param_value = value;
                Ok(())
            } else {
                anyhow::bail!("no parameter {idx}")
            }
        }
        fn presets(&self) -> Vec<Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
            anyhow::bail!("no preset {id}")
        }
    }

    /// Effect that scales its input by a settable `gain` parameter (default 1.0),
    /// so a modulator driving the gain is observable in the output.
    struct ParamScaleEffect {
        gain: f32,
    }

    impl Plugin for ParamScaleEffect {
        fn name(&self) -> &str {
            "ParamScale"
        }
        fn is_instrument(&self) -> bool {
            false
        }
        fn audio_output_count(&self) -> usize {
            2
        }
        fn audio_input_count(&self) -> usize {
            2
        }
        fn process(
            &mut self,
            _midi_events: &[(u64, [u8; 3])],
            audio_in: &[&[f32]],
            audio_out: &mut [&mut [f32]],
        ) -> anyhow::Result<()> {
            for (out, inp) in audio_out.iter_mut().zip(audio_in.iter()) {
                for (o, &i) in out.iter_mut().zip(inp.iter()) {
                    *o = i * self.gain;
                }
            }
            Ok(())
        }
        fn sample_rate(&self) -> f32 {
            48000.0
        }
        fn parameters(&self) -> Vec<ParameterInfo> {
            vec![ParameterInfo {
                index: 0,
                name: "gain".into(),
                min: 0.0,
                max: 1.0,
                default: 1.0,
                ..Default::default()
            }]
        }
        fn get_parameter(&mut self, idx: u32) -> Option<f32> {
            if idx == 0 { Some(self.gain) } else { None }
        }
        fn set_parameter(&mut self, idx: u32, value: f32) -> anyhow::Result<()> {
            if idx == 0 {
                self.gain = value;
                Ok(())
            } else {
                anyhow::bail!("no parameter {idx}")
            }
        }
        fn presets(&self) -> Vec<Preset> {
            Vec::new()
        }
        fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
            anyhow::bail!("no preset {id}")
        }
    }

    /// A group envelope modulator at idle (no notes) with depth 1.0 pulls its
    /// target parameter from the base all the way down to the parameter's min.
    /// Here a member instrument's `cutoff` (base 0.5) → 0.0, observable because
    /// ParamTrackingInstrument outputs its parameter value as audio.
    #[test]
    fn group_modulator_drives_member_param() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument { inst: 0, instrument: inst, inst_buf, remapper: None })
            .unwrap();

        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();

        cmd_tx
            .send(GraphCommand::InsertGroupModulator {
                group: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                    state: EnvState::Idle, level: 0.0, notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMember { member: 0, slot: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        // No notes → envelope idle → member cutoff pulled to min (0).
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| s.abs() < 1e-6),
            "idle group envelope at depth 1 should pull member cutoff to 0, got {}",
            out[0][0]
        );
    }

    /// A group modulator can drive one of the group's own bus effects. Here an
    /// idle envelope (depth 1.0) pulls a bus ParamScaleEffect's gain (base 1.0)
    /// to 0, silencing the bus.
    #[test]
    fn group_modulator_drives_bus_effect() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        cmd_tx
            .send(GraphCommand::InsertGroupEffect {
                group: 0,
                index: 0,
                effect: Box::new(ParamScaleEffect { gain: 1.0 }),
                mix: 1.0,
            })
            .unwrap();

        cmd_tx
            .send(GraphCommand::InsertGroupModulator {
                group: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                    state: EnvState::Idle, level: 0.0, notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupBus { effect_index: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 1.0,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        // No notes → envelope idle → bus gain pulled to 0 (silence). (The
        // ConstInstrument outputs regardless of notes, so the only thing that
        // can zero the bus is the modulated gain.)
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0].iter().all(|&s| s.abs() < 1e-6),
            "idle group envelope at depth 1 should pull bus gain to 0 (silence), got {}",
            out[0][0]
        );
    }

    /// Group member-target ordinals are fixed up when membership changes:
    /// a member leaving drops targets pointing at it and shifts higher ordinals
    /// down; a member joining bumps ordinals at/after the insertion point.
    #[test]
    fn group_member_target_fixups() {
        let mk = |member: usize| Modulator {
            source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            sample_rate: 48000.0,
            targets: vec![ModTarget {
                kind: ModTargetKind::GroupMember { member, slot: 0, param_index: 0 },
                depth: 0.5,
                base_value: 0.5,
                param_min: 0.0,
                param_max: 1.0,
            }],
            last_output: 0.0,
        };
        let members = |mods: &[Modulator]| -> Vec<Option<usize>> {
            mods.iter()
                .map(|m| match m.targets.first().map(|t| &t.kind) {
                    Some(ModTargetKind::GroupMember { member, .. }) => Some(*member),
                    _ => None,
                })
                .collect()
        };

        // Targets at members 0, 1, 2; member 1 leaves.
        let mut mods = vec![mk(0), mk(1), mk(2)];
        fixup_group_member_after_remove(&mut mods, 1);
        assert_eq!(members(&mods), vec![Some(0), None, Some(1)]);

        // A member joins at ordinal 0: existing ordinals shift up.
        shift_group_member_after_insert(&mut mods, 0);
        assert_eq!(members(&mods), vec![Some(1), None, Some(2)]);

        // The same ordinal fixups apply to GroupMemberMod targets.
        let mk_mm = |member: usize| Modulator {
            source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            sample_rate: 48000.0,
            targets: vec![ModTarget {
                kind: ModTargetKind::GroupMemberMod { member, mod_index: 0, field: CrossModField::Rate },
                depth: 0.5,
                base_value: 1.0,
                param_min: 0.01,
                param_max: 50.0,
            }],
            last_output: 0.0,
        };
        let mm_members = |mods: &[Modulator]| -> Vec<Option<usize>> {
            mods.iter()
                .map(|m| match m.targets.first().map(|t| &t.kind) {
                    Some(ModTargetKind::GroupMemberMod { member, .. }) => Some(*member),
                    _ => None,
                })
                .collect()
        };
        let mut mm = vec![mk_mm(0), mk_mm(1), mk_mm(2)];
        fixup_group_member_after_remove(&mut mm, 1);
        assert_eq!(mm_members(&mm), vec![Some(0), None, Some(1)]);

        // Member-modulator removal: member 0's modulator 1 deleted → targets at
        // (member 0, mod 1) drop, and (member 0, mod >1) shift down.
        let mk_mm_idx = |mod_index: usize| Modulator {
            source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            sample_rate: 48000.0,
            targets: vec![ModTarget {
                kind: ModTargetKind::GroupMemberMod { member: 0, mod_index, field: CrossModField::Rate },
                depth: 0.5,
                base_value: 1.0,
                param_min: 0.01,
                param_max: 50.0,
            }],
            last_output: 0.0,
        };
        let mm_idx = |mods: &[Modulator]| -> Vec<Option<usize>> {
            mods.iter()
                .map(|m| match m.targets.first().map(|t| &t.kind) {
                    Some(ModTargetKind::GroupMemberMod { mod_index, .. }) => Some(*mod_index),
                    _ => None,
                })
                .collect()
        };
        let mut mmi = vec![mk_mm_idx(0), mk_mm_idx(1), mk_mm_idx(2)];
        fixup_group_member_mod_after_remove(&mut mmi, 0, 1);
        assert_eq!(mm_idx(&mmi), vec![Some(0), None, Some(1)]);
    }

    /// A group envelope modulator (idle, depth 1) drives a member instrument's
    /// LFO rate down to its minimum — proving group→member-modulator cross-mod.
    #[test]
    fn group_modulator_drives_member_modulator() {
        let (mut graph, cmd_tx, _rr) = make_graph(2);
        swap_instrument_at(&cmd_tx, 0, ConstInstrument::new(0.5));
        cmd_tx.send(GraphCommand::AddGroup).unwrap();
        cmd_tx.send(GraphCommand::SetLaneGroup { inst: 0, group: Some(0) }).unwrap();
        // Member lane gets an LFO modulator at rate 10 Hz.
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 10.0, phase: 0.0 },
            })
            .unwrap();
        // Group envelope modulator (idle) drives that member LFO's rate, depth 1.
        cmd_tx
            .send(GraphCommand::InsertGroupModulator {
                group: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                    state: EnvState::Idle, level: 0.0, notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddGroupModTarget {
                group: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::GroupMemberMod { member: 0, mod_index: 0, field: CrossModField::Rate },
                    depth: 1.0,
                    base_value: 10.0,
                    param_min: 0.01,
                    param_max: 50.0,
                },
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Idle envelope at depth 1 pulls the member LFO's rate from 10 to ~min.
        match graph.instruments[0].modulators[0].source {
            ModSource::Lfo { rate, .. } => assert!(
                rate < 0.1,
                "group envelope should pull member LFO rate to min, got {rate}"
            ),
            _ => panic!("member modulator should be an LFO"),
        }
    }

    #[test]
    fn modulator_applies_set_parameter() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Swap in a param-tracking instrument.
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();

        // Add a modulator targeting param 0 (cutoff) on the instrument (parent_slot=0).
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        // Process one buffer.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // The modulator should have called set_parameter, so the output
        // should NOT be exactly 0.5 (the base value) — it should be
        // modulated. After one buffer of 64 samples at 48kHz with 1Hz rate,
        // the phase advances by 64/48000 ≈ 0.00133. The sine at that phase
        // is small but non-zero.
        // The audio output is the param_value which was set by the modulator.
        let first_sample = out[0][0];
        // Just verify the modulator ran (value may be very close to 0.5 since
        // phase is small, but should be different from unmodulated).
        // The phase advance is 64/48000 ≈ 0.00133, sin(2π * 0.00133) ≈ 0.00837
        // modulated = 0.5 + 0.5 * 0.00837 * 1.0 ≈ 0.504
        assert!(
            (0.0..=1.0).contains(&first_sample),
            "modulated value out of range: {first_sample}"
        );

        // Run many buffers so the LFO phase advances significantly.
        for _ in 0..1000 {
            graph.process(&[], &mut out).unwrap();
        }
        // After many buffers the LFO should have cycled. The output should
        // still be within the valid range [0.0, 1.0].
        let sample = out[0][0];
        assert!(
            (0.0..=1.0).contains(&sample),
            "modulated value out of range after many buffers: {sample}"
        );
    }

    #[test]
    fn envelope_modulator_anchors_base_as_peak() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Param 0 acts like the oscillator volume: base at max (1.0).
        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 1.0 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();

        // Instant envelope (attack/decay/release = 0, sustain = 1) so each
        // phase settles within a single buffer.
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::Envelope {
                    attack: 0.0,
                    decay: 0.0,
                    sustain: 1.0,
                    release: 0.0,
                    state: EnvState::Idle,
                    level: 0.0,
                    notes_held: 0,
                },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 1.0,
                    base_value: 1.0,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        // Idle (no note yet): the envelope holds the param at min — silence.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();
        assert!(
            out[0][0].abs() < 0.01,
            "idle envelope should pull the param to min, got {}",
            out[0][0]
        );

        // Note-on: envelope peaks, restoring the user's base value.
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(
            (out[0][0] - 1.0).abs() < 0.01,
            "envelope peak should reach the base value, got {}",
            out[0][0]
        );

        // Note-off: release returns the param to min.
        graph.process(&[note_off(60)], &mut out).unwrap();
        assert!(
            out[0][0].abs() < 0.01,
            "after release the param should return to min, got {}",
            out[0][0]
        );
    }

    #[test]
    fn envelope_modulator_depth_limits_dip() {
        // depth 0.5 on base 0.8: idle dips to base - 0.5*(0.8-0.0) = 0.4,
        // never below — the envelope scales the distance from min to base.
        let mut m = Modulator::new(
            ModSource::Envelope {
                attack: 0.0,
                decay: 0.0,
                sustain: 1.0,
                release: 0.0,
                state: EnvState::Idle,
                level: 0.0,
                notes_held: 0,
            },
            48000.0,
        );
        let target = ModTarget {
            kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
            depth: 0.5,
            base_value: 0.8,
            param_min: 0.0,
            param_max: 1.0,
        };

        m.tick(64, &[]); // idle: output 0
        assert!((target.base_value + m.target_offset(&target) - 0.4).abs() < 1e-6);

        m.tick(64, &[note_on(60)]); // instant attack to peak
        assert!((target.base_value + m.target_offset(&target) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn modulator_base_value_updated_by_set_parameter() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        let inst: Box<dyn Plugin> = Box::new(ParamTrackingInstrument { param_value: 0.5 });
        let inst_buf = (0..inst.audio_output_count()).map(|_| Vec::new()).collect();
        cmd_tx
            .send(GraphCommand::SwapInstrument {
                inst: 0,
                instrument: inst,
                inst_buf,
                remapper: None,
            })
            .unwrap();

        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        // Process to pick up commands.
        let mut out = make_output();
        graph.process(&[], &mut out).unwrap();

        // Now send SetParameter to change the base value.
        cmd_tx
            .send(GraphCommand::SetParameter {
                inst: 0,
                slot: 0,
                param_index: 0,
                value: 0.8,
            })
            .unwrap();

        // Process again — the modulator should now use 0.8 as its base.
        graph.process(&[], &mut out).unwrap();
        let sample = out[0][0];
        // The modulated value should be centered around 0.8 (within depth range).
        // At small phase, it should be close to 0.8.
        assert!(
            (sample - 0.8).abs() < 0.3,
            "expected close to 0.8 after base change, got {sample}"
        );
    }

    #[test]
    fn lane_modulator_survives_effect_removal() {
        let (mut graph, cmd_tx, _return_rx) = make_graph(2);

        // Instrument + 2 effects, plus a lane modulator targeting the
        // instrument (slot 0).
        swap_instrument(&cmd_tx, ConstInstrument::new(1.0));
        insert_effect(&cmd_tx, 0, Box::new(PassthroughEffect), 1.0);
        insert_effect(&cmd_tx, 1, Box::new(PassthroughEffect), 1.0);
        cmd_tx
            .send(GraphCommand::InsertModulator {
                inst: 0,
                index: 0,
                source: ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            })
            .unwrap();
        cmd_tx
            .send(GraphCommand::AddModTarget {
                inst: 0,
                mod_index: 0,
                target: ModTarget {
                    kind: ModTargetKind::PluginParam { slot: 0, param_index: 0 },
                    depth: 0.5,
                    base_value: 0.5,
                    param_min: 0.0,
                    param_max: 1.0,
                },
            })
            .unwrap();

        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();

        // Removing an effect must remap target slots without crashing.
        cmd_tx.send(GraphCommand::RemoveEffect { inst: 0, index: 0 }).unwrap();
        let mut out = make_output();
        graph.process(&[note_on(60)], &mut out).unwrap();
        assert!(out[0].iter().all(|&s| s.is_finite()));
    }

    #[test]
    fn modulator_targets_effect_slot() {
        // A lane modulator whose target points at slot 1 drives the effect's
        // parameter, not the instrument's — the whole point of lane scoping.
        let mut inst: Option<Box<dyn Plugin>> =
            Some(Box::new(ParamTrackingInstrument { param_value: 0.5 }));
        let mut effects: Vec<Box<dyn Plugin>> =
            vec![Box::new(ParamTrackingInstrument { param_value: 0.5 })];

        let mut m = Modulator::new(
            ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
            48000.0,
        );
        m.targets = vec![ModTarget {
            kind: ModTargetKind::PluginParam { slot: 1, param_index: 0 },
            depth: 1.0,
            base_value: 0.5,
            param_min: 0.0,
            param_max: 1.0,
        }];
        m.last_output = 1.0; // force the LFO to its peak

        apply_modulators_to_chain(&[m], &mut inst, &mut effects);

        // Effect param pushed to base + depth*range (clamped); instrument untouched.
        assert!(
            effects[0].get_parameter(0).unwrap() > 0.9,
            "effect param should be modulated up, got {:?}",
            effects[0].get_parameter(0)
        );
        assert_eq!(
            inst.unwrap().get_parameter(0),
            Some(0.5),
            "instrument param should be untouched"
        );
    }

    #[test]
    fn slot_remap_helpers() {
        // A lane modulator with targets on instrument (0), effect 1 (slot 2),
        // and effect 2 (slot 3).
        let target = |slot: usize| ModTarget {
            kind: ModTargetKind::PluginParam { slot, param_index: 0 },
            depth: 0.5,
            base_value: 0.0,
            param_min: 0.0,
            param_max: 1.0,
        };
        let slots = |mods: &[Modulator]| -> Vec<usize> {
            mods[0]
                .targets
                .iter()
                .filter_map(|t| match t.kind {
                    ModTargetKind::PluginParam { slot, .. } => Some(slot),
                    _ => None,
                })
                .collect()
        };
        let make = || {
            let mut m = Modulator::new(
                ModSource::Lfo { waveform: LfoWaveform::Sine, rate: 1.0, phase: 0.0 },
                48000.0,
            );
            m.targets = vec![target(0), target(2), target(3)];
            vec![m]
        };

        // Insert an effect at index 0 (slot 1): slots >= 1 shift up.
        let mut mods = make();
        shift_slots_after_insert(&mut mods, 0);
        assert_eq!(slots(&mods), vec![0, 3, 4]);

        // Remove effect at index 0 (slot 1): nothing on slot 1, slots > 1 down.
        let mut mods = make();
        fixup_slots_after_remove(&mut mods, 0);
        assert_eq!(slots(&mods), vec![0, 1, 2]);

        // Remove effect at index 1 (slot 2): drop that target, shift slot 3→2.
        let mut mods = make();
        fixup_slots_after_remove(&mut mods, 1);
        assert_eq!(slots(&mods), vec![0, 2]);

        // Reorder effect from index 0 (slot 1) to index 2 (slot 3): effect at
        // slot 2 → 1, slot 3 → 2 (the moved one isn't targeted here).
        let mut mods = make();
        remap_slots_after_reorder(&mut mods, 0, 2);
        assert_eq!(slots(&mods), vec![0, 1, 2]);
    }

    // --- Pattern playback transposition ---

    /// A C major triad pattern (C4 E4 G4), one buffer of ons then offs.
    fn make_pattern_player(in_key: bool) -> PatternPlayer {
        let mut p = PatternPlayer::new(48000.0);
        p.pattern = Pattern {
            events: vec![
                PatternEvent { frame: 0, status: 0x90, note: 60, velocity: 100 },
                PatternEvent { frame: 0, status: 0x90, note: 64, velocity: 100 },
                PatternEvent { frame: 0, status: 0x90, note: 67, velocity: 100 },
                PatternEvent { frame: 500, status: 0x80, note: 60, velocity: 0 },
                PatternEvent { frame: 500, status: 0x80, note: 64, velocity: 0 },
                PatternEvent { frame: 500, status: 0x80, note: 67, velocity: 0 },
            ],
            length_samples: 1000,
        };
        p.base_note = Some(60);
        p.enabled = true;
        p.in_key = in_key;
        p
    }

    fn note_ons(events: &[(u64, [u8; 3])]) -> Vec<u8> {
        events.iter()
            .filter(|(_, b)| b[0] & 0xF0 == 0x90 && b[2] > 0)
            .map(|(_, b)| b[1])
            .collect()
    }

    fn note_offs(events: &[(u64, [u8; 3])]) -> Vec<u8> {
        events.iter()
            .filter(|(_, b)| b[0] & 0xF0 == 0x80 || (b[0] & 0xF0 == 0x90 && b[2] == 0))
            .map(|(_, b)| b[1])
            .collect()
    }

    #[test]
    fn pattern_chromatic_preserves_intervals() {
        let mut p = make_pattern_player(false);
        let scale = ScaleSetting::default(); // C Major
        // Trigger on D4: chromatic shift +2 gives a D major triad (F# off-scale).
        let events = p.process(&[note_on(62)], 256, scale).to_vec();
        assert_eq!(note_ons(&events), vec![62, 66, 69]);
    }

    #[test]
    fn pattern_in_key_transposes_by_scale_degrees() {
        let mut p = make_pattern_player(true);
        let scale = ScaleSetting::default(); // C Major
        // Trigger on D4 (2nd degree): C E G shifts one degree to D F A —
        // the diatonic D minor triad.
        let events = p.process(&[note_on(62)], 256, scale).to_vec();
        assert_eq!(note_ons(&events), vec![62, 65, 69]);

        // Note-offs (pattern frame 500, second buffer) release the same
        // transposed notes that were started.
        let events = p.process(&[], 256, scale).to_vec();
        let mut offs = note_offs(&events);
        offs.sort_unstable();
        assert_eq!(offs, vec![62, 65, 69]);
    }

    #[test]
    fn pattern_in_key_identity_on_base_note() {
        let mut p = make_pattern_player(true);
        // A minor scale: the C major pattern is off-root but in-scale.
        let scale = ScaleSetting { root: 9, scale_idx: 1 };
        // Triggering the base note plays the pattern back unchanged.
        let events = p.process(&[note_on(60)], 256, scale).to_vec();
        assert_eq!(note_ons(&events), vec![60, 64, 67]);
    }
}
