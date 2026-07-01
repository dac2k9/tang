mod piano_view;
pub mod splash;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

use view::DIM;
use view::filter_list::{FilterListItem, FilterListState};
use view::list::{ListItem, ListSpan, ListState};
use view::scroll_view::ScrollLine;
use view::text_input::TextInputState;
use view::{FilterList, List, ScrollView, TabBar, TextInput, centered_rect};

use crate::audio;
use crate::held_notes::HeldNotes;
use crate::piano_filter::{PianoFilter, PianoMode};
use crate::plugin;
use crate::plugin::chain::GraphCommand;
use crate::plugin::PluginInfo;
use crate::scale::{ScaleSetting, NOTE_NAMES};

const TAB_NAMES: &[&str] = &["(1) Session", "(2) Piano", "(3) Scope", "(4) Help"];
const TAB_SEP: &str = " │ ";

/// Default dry/wet mix for a newly added effect. Half-wet suits blend/parallel
/// effects (reverb, delay), the common DAW default for sends.
const DEFAULT_EFFECT_MIX: f32 = 0.5;

/// Default dry/wet mix for a freshly added effect, by plugin. Filters and
/// other tone-shaping inserts must be fully wet — at 50% the dry path passes
/// the very frequencies the filter removes, so e.g. a highpass barely changes
/// the sound. Blend effects (reverb) keep the half-wet default.
fn default_effect_mix(id: &str) -> f32 {
    if id == "builtin:filter" {
        1.0
    } else {
        DEFAULT_EFFECT_MIX
    }
}

// ---------------------------------------------------------------------------
// Plugin slot — main-thread mirror of what the audio thread has
// ---------------------------------------------------------------------------

struct PluginSlot {
    name: String,
    format: String,
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    is_instrument: bool,
    params: Vec<ParamSlot>,
    /// All presets the plugin reports, cached at load time. Used by the
    /// preset selector popup.
    presets: Vec<plugin::Preset>,
    /// Name of the currently-loaded preset (if any). Tracked for save
    /// round-trip and display.
    current_preset: Option<String>,
    /// Effect dry/wet mix (1.0 = full wet). Only meaningful for effect slots;
    /// instrument slots leave this at 1.0 and don't read it.
    mix: f32,
}

enum ParamKind {
    Float,
    Enum(Vec<String>),
    Separator,
}

struct ParamSlot {
    name: String,
    index: u32,
    min: f32,
    max: f32,
    default: f32,
    value: f32,
    kind: ParamKind,
}

// ---------------------------------------------------------------------------
// Instrument tree model
// ---------------------------------------------------------------------------

/// Main-thread mirror of pattern state for an instrument.
struct PatternState {
    bpm: f32,
    length_beats: f32,
    looping: bool,
    base_note: Option<u8>,
    events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    enabled: bool,
    recording: bool,
    /// Transpose playback by scale degrees (in-key) instead of semitones.
    in_key: bool,
}

struct InstrumentNode {
    range: Option<(u8, u8)>,
    transpose: i8,
    /// Host-side output gain, applied before effects (1.0 = unity).
    volume: f32,
    instrument: Option<PluginSlot>,
    effects: Vec<PluginSlot>,
    /// Lane-scoped modulators. Each target addresses a plugin in this chain by
    /// slot (0 = instrument, 1..N = effects).
    modulators: Vec<ModulatorSlot>,
    pattern: Option<PatternState>,
    /// Carried through from session config; not editable in the TUI.
    pitch_bend_range: f64,
    remap: std::collections::HashMap<String, crate::session::RemapTarget>,
    /// Submix group membership: index into `State.groups`, or None.
    group: Option<usize>,
}

/// A submix group in the TUI model: members are the instruments whose `group`
/// points here; it sums them and runs its own effect chain + volume.
struct GroupNode {
    name: Option<String>,
    volume: f32,
    effects: Vec<PluginSlot>,
    /// Group-scoped modulators. Each target addresses a member instrument
    /// (`GroupMember`), a bus effect (`GroupBus`), or a sibling group modulator.
    modulators: Vec<ModulatorSlot>,
}

enum ModSourceSlot {
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
}

struct ModulatorSlot {
    source: ModSourceSlot,
    targets: Vec<ModTargetSlot>,
}

struct ModTargetSlot {
    /// Slot the target lives on: 0 = instrument, 1..N = effects. Used to label
    /// the target with its plugin and to resolve cross-mod vs plugin params.
    slot: usize,
    param_name: String,
    kind: crate::plugin::chain::ModTargetKind,
    depth: f32,
    #[allow(dead_code)]
    param_min: f32,
    #[allow(dead_code)]
    param_max: f32,
}

/// Addresses a specific node in the instrument tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TreeAddress {
    Instrument(usize),
    Effect { inst: usize, index: usize },
    Pattern(usize),
    /// A lane-scoped modulator. `index` is its position in the instrument's
    /// modulator list.
    Modulator { inst: usize, index: usize },
    /// A submix group header node.
    Group(usize),
    /// An effect on a group's bus chain.
    GroupEffect { group: usize, index: usize },
    /// A group-scoped modulator. `index` is its position in the group's
    /// modulator list.
    GroupModulator { group: usize, index: usize },
}

impl TreeAddress {
    /// Get the instrument index for this address (None for group nodes).
    fn inst(&self) -> Option<usize> {
        match *self {
            TreeAddress::Instrument(inst) => Some(inst),
            TreeAddress::Effect { inst, .. } => Some(inst),
            TreeAddress::Pattern(inst) => Some(inst),
            TreeAddress::Modulator { inst, .. } => Some(inst),
            TreeAddress::Group(_)
            | TreeAddress::GroupEffect { .. }
            | TreeAddress::GroupModulator { .. } => None,
        }
    }

    /// Get the audio thread slot index (0 = instrument, 1..N = effects).
    /// Modulators are lane-scoped (not tied to a slot), so they report 0.
    fn slot(&self) -> usize {
        match *self {
            TreeAddress::Instrument(_) => 0,
            TreeAddress::Effect { index, .. } => index + 1,
            TreeAddress::Pattern(_) => 0,
            TreeAddress::Modulator { .. } => 0,
            TreeAddress::Group(_) => 0,
            TreeAddress::GroupEffect { index, .. } => index + 1,
            TreeAddress::GroupModulator { .. } => 0,
        }
    }
}

struct TreeEntry {
    label: String,
    address: TreeAddress,
    #[allow(dead_code)]
    color: Color,
    #[allow(dead_code)]
    indent: usize,
}

// ---------------------------------------------------------------------------
// Action bar
// ---------------------------------------------------------------------------

/// Build the action bar items for the current tree selection.
fn actions_for(addr: Option<&TreeAddress>) -> Vec<(&'static str, &'static str)> {
    match addr {
        Some(TreeAddress::Instrument(_)) => vec![
            ("n", "new instr"),
            ("i", "instrument"),
            ("R", "range"),
            ("v", "volume"),
            ("a", "add effect"),
            ("m", "modulate"),
            ("g", "group"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Effect { .. }) => vec![
            ("n", "new instr"),
            ("a", "add effect"),
            ("m", "modulate"),
            ("x", "mix"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Pattern(_)) => vec![
            ("r", "record"),
            ("d", "clear"),
        ],
        Some(TreeAddress::Modulator { .. }) => vec![
            ("t", "add target"),
            ("d", "delete"),
        ],
        Some(TreeAddress::Group(_)) => vec![
            ("a", "add fx"),
            ("m", "modulate"),
            ("v", "volume"),
            ("R", "rename"),
            ("d", "delete group"),
        ],
        Some(TreeAddress::GroupEffect { .. }) => vec![
            ("a", "add fx"),
            ("m", "modulate"),
            ("x", "mix"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::GroupModulator { .. }) => vec![
            ("t", "add target"),
            ("d", "delete"),
        ],
        None => vec![("n", "new instr")],
    }
}

// ---------------------------------------------------------------------------
// Popup state types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorMode {
    Instrument,
    Effect,
    /// Add an effect to a group's bus chain.
    GroupEffect(usize),
}

struct SelectorState {
    mode: SelectorMode,
    filter: FilterListState,
    items: Vec<FilterListItem>,
}

struct EditState {
    input: TextInputState,
    param_name: String,
    param_min: f32,
    param_max: f32,
}

/// Which host-side gain a `GainEditState` is editing.
enum GainTarget {
    /// Dry/wet mix of the effect at `effects[index]`.
    Mix { index: usize },
    /// Instrument output volume (applied before effects).
    Volume,
    /// A group's output volume.
    GroupVolume { group: usize },
    /// Dry/wet mix of a group bus effect.
    GroupMix { group: usize, index: usize },
}

/// Editing a host-side gain (effect mix or instrument/group volume) via the
/// value popup. These live outside the plugin parameter list.
struct GainEditState {
    /// Instrument index for instrument-scoped targets (ignored for group ones).
    inst: usize,
    target: GainTarget,
    edit: EditState,
}

/// One entry in the target selector popup.
struct TargetEntry {
    label: String,
    /// Chain slot for plugin-param entries (0 = instrument, 1..N = effect);
    /// 0 and unused for cross-mod entries.
    slot: usize,
    kind: crate::plugin::chain::ModTargetKind,
    param_min: f32,
    param_max: f32,
    base_value: f32,
}

struct TargetSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    entries: Vec<TargetEntry>,
    /// Which modulator rack this selector targets (lane or group).
    scope: ModScope,
    mod_index: usize,
}

/// Which modulator list a target selector / modulate popup operates on:
/// an instrument lane's rack or a group's rack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModScope {
    Lane(usize),
    Group(usize),
}

/// A choice in the "modulate this parameter" popup (`m` on a parameter):
/// create a new lane modulator bound to the parameter, or attach the
/// parameter to one of the instrument's existing lane modulators.
#[derive(Clone, Copy)]
enum ModulateChoice {
    NewLfo,
    NewEnvelope,
    Existing(usize),
}

/// State for the "modulate <param>" popup.
struct ModulateState {
    /// Which modulator rack a created/attached modulator lives in.
    scope: ModScope,
    /// Chain slot the parameter lives on (0 = instrument, 1..N = effect);
    /// used for the model's display label only.
    slot: usize,
    /// The target kind to bind (PluginParam for lanes, GroupBus for groups).
    target_kind: crate::plugin::chain::ModTargetKind,
    param_name: String,
    param_min: f32,
    param_max: f32,
    /// Current value of the parameter, used as the modulation base.
    base_value: f32,
    choices: Vec<ModulateChoice>,
    filter: FilterListState,
    items: Vec<FilterListItem>,
}

/// A choice in the "assign to group" popup (`g` on an instrument).
#[derive(Clone, Copy)]
enum GroupAssignChoice {
    New,
    Ungroup,
    Existing(usize),
}

/// State for the "assign <instrument> to group" popup.
struct GroupAssignState {
    inst: usize,
    choices: Vec<GroupAssignChoice>,
    filter: FilterListState,
    items: Vec<FilterListItem>,
}

struct PresetSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    /// Parallel to `items`: the (name, id) pair for each row before filtering.
    presets: Vec<plugin::Preset>,
    inst: usize,
    /// 0 = instrument, 1..N = effects.
    slot: usize,
}

struct RangeEditState {
    /// The instrument whose key range is being edited.
    inst: usize,
    input: TextInputState,
}

/// State for the "rename group" popup.
struct RenameGroupState {
    /// The group whose display name is being edited.
    group: usize,
    input: TextInputState,
}

/// State for the "save as" filename popup.
struct SaveAsState {
    input: TextInputState,
    /// Error from the last failed save attempt, shown in the popup.
    error: Option<String>,
}

/// State for the scale picker popup (Piano tab).
struct ScaleSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    /// Parallel to items: the (root, scale_idx) pair for each row before filtering.
    entries: Vec<(u8, usize)>,
}

#[derive(Default, Clone)]
struct Areas {
    tab: Rect,
    content: Rect,
    action_bar: Rect,
    chain_inner: Rect,
    param_inner: Rect,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct State {
    active_tab: usize,
    instruments: Vec<InstrumentNode>,
    groups: Vec<GroupNode>,
    tree_entries: Vec<TreeEntry>,
    chain_state: ListState,
    param_state: ListState,
    focus_params: bool,
    help_lines: Vec<String>,
    help_offset: usize,
    scrollbar_dragging: bool,
    param_dragging: bool,
    param_scrollbar_dragging: bool,
    editing: Option<EditState>,
    range_edit: Option<RangeEditState>,
    rename_group: Option<RenameGroupState>,
    selector: Option<SelectorState>,
    target_selector: Option<TargetSelectorState>,
    modulate: Option<ModulateState>,
    group_assign: Option<GroupAssignState>,
    preset_selector: Option<PresetSelectorState>,
    catalog: Vec<PluginInfo>,
    /// Receives per-format catalog batches from the background scan thread.
    catalog_rx: crossbeam_channel::Receiver<Vec<PluginInfo>>,
    /// True until the background catalog scan finishes (channel disconnects).
    catalog_scanning: bool,
    areas: Areas,
    quit: bool,
    session_path: Option<PathBuf>,
    dirty: bool,
    // Parameter filter (search bar in param pane).
    param_filter_input: TextInputState,
    param_filtering: bool,
    param_filtered: Vec<usize>,
    // Connections to the audio engine.
    cmd_tx: Sender<GraphCommand>,
    #[allow(dead_code)]
    midi_tx: Sender<audio::MidiEvent>,
    runtime: plugin::Runtime,
    sample_rate: f32,
    max_block_size: usize,
    // Pattern state.
    global_bpm: f32,
    bpm_editing: Option<EditState>,
    gain_editing: Option<GainEditState>,
    save_as: Option<SaveAsState>,
    pattern_rx: crossbeam_channel::Receiver<crate::plugin::chain::PatternNotification>,
    // Piano tab state.
    held: Arc<HeldNotes>,
    piano_filter: Arc<PianoFilter>,
    /// Center MIDI octave for the piano view (0..=8). Default 4.
    piano_view_octave: i8,
    scale_selector: Option<ScaleSelectorState>,
}

impl State {
    fn rebuild_tree(&mut self) {
        self.tree_entries = build_tree_entries(&self.instruments, &self.groups);
        self.chain_state.set_len(self.tree_entries.len());
        self.sync_param_state();
    }

    fn sync_param_state(&mut self) {
        // Clear filter when selected node changes.
        self.param_filter_input = TextInputState::new("");
        self.param_filtering = false;

        let sel = self.chain_state.selected;
        if sel < self.tree_entries.len() {
            let addr = &self.tree_entries[sel].address;
            let param_len = match *addr {
                TreeAddress::Pattern(inst) => {
                    self.instruments.get(inst)
                        .and_then(|n| n.pattern.as_ref())
                        .map_or(0, |p| {
                            let mut n = 4; // Length + Enabled + Loop + Transpose
                            if !p.events.is_empty() { n += 1; } // Notes info
                            n
                        })
                }
                TreeAddress::Modulator { inst, index } => {
                    self.modulator(inst, index).map_or(0, modulator_param_len)
                }
                TreeAddress::GroupModulator { group, index } => {
                    self.group_modulator(group, index).map_or(0, modulator_param_len)
                }
                _ => self.plugin_at(addr).map_or(0, |p| p.params.len()),
            };
            self.param_state.set_len(param_len);
        }
        self.recompute_param_filter();
    }

    /// Recompute the filtered parameter indices based on the current filter text.
    /// Only applies to Instrument/Effect nodes (not modulators).
    fn recompute_param_filter(&mut self) {
        let sel = self.chain_state.selected;
        let is_plugin = sel < self.tree_entries.len()
            && matches!(
                self.tree_entries[sel].address,
                TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
            );
        if !is_plugin {
            self.param_filtered.clear();
            return;
        }
        let addr = &self.tree_entries[sel].address;
        let params = match self.plugin_at(addr) {
            Some(p) => &p.params,
            None => {
                self.param_filtered.clear();
                return;
            }
        };
        let filter = self.param_filter_input.value.to_lowercase();
        if filter.is_empty() {
            self.param_filtered = (0..params.len()).collect();
        } else {
            self.param_filtered = params
                .iter()
                .enumerate()
                .filter(|(_, p)| p.name.to_lowercase().contains(&filter))
                .map(|(i, _)| i)
                .collect();
        }
        self.param_state.set_len(self.param_filtered.len());
    }

    /// Map the current param_state.selected (index into filtered list) to the
    /// real param index. Returns None if no valid mapping.
    fn real_param_index(&self) -> Option<usize> {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return None;
        }
        let is_plugin = matches!(
            self.tree_entries[sel].address,
            TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
        );
        if is_plugin && !self.param_filtered.is_empty() {
            self.param_filtered.get(self.param_state.selected).copied()
        } else {
            Some(self.param_state.selected)
        }
    }

    /// Returns (min, max) for the currently selected parameter, handling both
    /// plugin params (via filter mapping) and modulator pseudo-params.
    fn selected_param_range(&self) -> Option<(f32, f32)> {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return None;
        }
        let addr = self.tree_entries[sel].address;
        match addr {
            TreeAddress::Modulator { inst, index } => {
                let m = self.modulator(inst, index)?;
                modulator_param_range(m, self.param_state.selected)
            }
            TreeAddress::GroupModulator { group, index } => {
                let m = self.group_modulator(group, index)?;
                modulator_param_range(m, self.param_state.selected)
            }
            _ => {
                let pa = self.real_param_index()?;
                let param = self.plugin_at(&addr)?.params.get(pa)?;
                Some((param.min, param.max))
            }
        }
    }

    /// Returns true if the currently selected parameter is an enum (e.g. Type, Waveform).
    fn selected_param_is_enum(&self) -> bool {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return false;
        }
        let addr = self.tree_entries[sel].address;
        match addr {
            TreeAddress::Modulator { inst, index } => {
                let Some(m) = self.modulator(inst, index) else { return false };
                modulator_param_is_enum(m, self.param_state.selected)
            }
            TreeAddress::GroupModulator { group, index } => {
                let Some(m) = self.group_modulator(group, index) else { return false };
                modulator_param_is_enum(m, self.param_state.selected)
            }
            _ => {
                if let Some(pa) = self.real_param_index() {
                    self.plugin_at(&addr)
                        .and_then(|p| p.params.get(pa))
                        .is_some_and(|p| matches!(p.kind, ParamKind::Enum(_)))
                } else {
                    false
                }
            }
        }
    }

    /// Get a reference to the PluginSlot at the given tree address.
    fn plugin_at(&self, addr: &TreeAddress) -> Option<&PluginSlot> {
        match *addr {
            TreeAddress::Pattern(_)
            | TreeAddress::Modulator { .. }
            | TreeAddress::Group(_)
            | TreeAddress::GroupModulator { .. } => None,
            TreeAddress::Instrument(inst) => {
                self.instruments.get(inst)?.instrument.as_ref()
            }
            TreeAddress::Effect { inst, index } => {
                self.instruments.get(inst)?.effects.get(index)
            }
            TreeAddress::GroupEffect { group, index } => {
                self.groups.get(group)?.effects.get(index)
            }
        }
    }

    /// Get a mutable reference to the PluginSlot at the given tree address.
    fn plugin_at_mut(&mut self, addr: &TreeAddress) -> Option<&mut PluginSlot> {
        match *addr {
            TreeAddress::Pattern(_)
            | TreeAddress::Modulator { .. }
            | TreeAddress::Group(_)
            | TreeAddress::GroupModulator { .. } => None,
            TreeAddress::Instrument(inst) => {
                self.instruments.get_mut(inst)?.instrument.as_mut()
            }
            TreeAddress::Effect { inst, index } => {
                self.instruments.get_mut(inst)?.effects.get_mut(index)
            }
            TreeAddress::GroupEffect { group, index } => {
                self.groups.get_mut(group)?.effects.get_mut(index)
            }
        }
    }

    fn selected_address(&self) -> Option<&TreeAddress> {
        self.tree_entries.get(self.chain_state.selected).map(|e| &e.address)
    }

    /// A lane modulator by instrument + lane-global index.
    fn modulator(&self, inst: usize, mod_index: usize) -> Option<&ModulatorSlot> {
        self.instruments.get(inst)?.modulators.get(mod_index)
    }

    /// A group-scoped modulator by group + index.
    fn group_modulator(&self, group: usize, mod_index: usize) -> Option<&ModulatorSlot> {
        self.groups.get(group)?.modulators.get(mod_index)
    }

    /// The modulator list for a scope (lane rack or group rack).
    fn scoped_modulators_mut(&mut self, scope: ModScope) -> Option<&mut Vec<ModulatorSlot>> {
        match scope {
            ModScope::Lane(inst) => self.instruments.get_mut(inst).map(|n| &mut n.modulators),
            ModScope::Group(group) => self.groups.get_mut(group).map(|g| &mut g.modulators),
        }
    }

    fn selector_items(&self, mode: SelectorMode) -> Vec<FilterListItem> {
        self.catalog
            .iter()
            .enumerate()
            .filter(|(_, e)| match mode {
                SelectorMode::Instrument => e.is_instrument,
                SelectorMode::Effect | SelectorMode::GroupEffect(_) => !e.is_instrument,
            })
            .map(|(i, e)| {
                let fmt = format_from_id(&e.id);
                FilterListItem {
                    cells: vec![
                        e.name.clone(),
                        fmt,
                        e.param_count.to_string(),
                        e.preset_count.to_string(),
                    ],
                    index: i,
                }
            })
            .collect()
    }

    fn open_selector(&mut self, mode: SelectorMode) {
        log::info!("open_selector: mode={:?}", mode);
        let items = self.selector_items(mode);

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);

        self.selector = Some(SelectorState {
            mode,
            filter,
            items,
        });
    }

    /// Fold newly scanned plugins into the catalog and refresh an open
    /// selector popup (keeping its filter text) so rows appear as the
    /// background scan progresses.
    fn extend_catalog(&mut self, batch: Vec<PluginInfo>) {
        if batch.is_empty() {
            return;
        }
        self.catalog.extend(batch);
        self.catalog.sort_by_key(|a| a.name.to_lowercase());
        if let Some(mode) = self.selector.as_ref().map(|sel| sel.mode) {
            let items = self.selector_items(mode);
            if let Some(sel) = self.selector.as_mut() {
                sel.items = items;
                sel.filter.apply_filter(&sel.items);
            }
        }
    }

    /// Load `source` and build its session-model `PluginSlot`. Returns the
    /// live plugin (to hand to the audio thread) alongside the slot, or None
    /// on load failure.
    fn load_plugin_slot(&self, source: &str) -> Option<(Box<dyn plugin::Plugin>, PluginSlot)> {
        let loaded = match plugin::load(source, self.sample_rate, self.max_block_size, &self.runtime) {
            Ok(p) => p,
            Err(e) => {
                log::error!("Failed to load plugin '{source}': {e}");
                return None;
            }
        };
        let params: Vec<ParamSlot> = loaded
            .parameters()
            .into_iter()
            .filter(|p| !p.name.starts_with("(locked)"))
            .map(|p| ParamSlot {
                kind: p.labels.map_or(ParamKind::Float, ParamKind::Enum),
                name: p.name,
                index: p.index,
                min: p.min,
                max: p.max,
                default: p.default,
                value: p.default,
            })
            .collect();
        let presets = loaded.presets();
        let slot = PluginSlot {
            name: loaded.name().to_string(),
            format: format_from_id(source),
            id: source.to_string(),
            is_instrument: loaded.is_instrument(),
            params,
            presets,
            current_preset: None,
            mix: 1.0,
        };
        Some((loaded, slot))
    }

    /// Install `loaded`/`slot` as the instrument plugin for lane `inst`, on
    /// both the audio thread and the session model.
    fn set_instrument(&mut self, inst: usize, loaded: Box<dyn plugin::Plugin>, slot: PluginSlot) {
        let inst_buf = (0..loaded.audio_output_count()).map(|_| Vec::new()).collect();
        let _ = self.cmd_tx.send(GraphCommand::SwapInstrument {
            inst,
            instrument: loaded,
            inst_buf,
            remapper: None,
        });
        if let Some(inst_node) = self.instruments.get_mut(inst) {
            inst_node.instrument = Some(slot);
        }
    }

    fn confirm_selector(&mut self) {
        let sel = match self.selector.take() {
            Some(s) => s,
            None => return,
        };
        let chosen = match sel.filter.selected_item(&sel.items) {
            Some(item) => item.index,
            None => {
                log::warn!("confirm_selector: no item selected");
                return;
            }
        };
        let entry = &self.catalog[chosen];

        // Determine which instrument to operate on from current selection.
        let inst = self.selected_inst().unwrap_or(0);

        // Load the real plugin.
        let source = entry.id.clone();
        log::info!("Loading plugin '{}' (id={}) into inst={}", entry.name, source, inst);
        let (loaded, slot) = match self.load_plugin_slot(&source) {
            Some(x) => x,
            None => return,
        };

        match sel.mode {
            SelectorMode::Instrument => {
                self.set_instrument(inst, loaded, slot);
            }
            SelectorMode::Effect => {
                let mut slot = slot;
                let mix = default_effect_mix(&source);
                slot.mix = mix;
                if let Some(inst_node) = self.instruments.get_mut(inst) {
                    let insert_at = inst_node.effects.len();
                    let _ = self.cmd_tx.send(GraphCommand::InsertEffect {
                        inst,
                        index: insert_at,
                        effect: loaded,
                        mix: mix as f64,
                    });
                    inst_node.effects.push(slot);
                }
            }
            SelectorMode::GroupEffect(group) => {
                let mut slot = slot;
                let mix = default_effect_mix(&source);
                slot.mix = mix;
                if let Some(g) = self.groups.get_mut(group) {
                    let insert_at = g.effects.len();
                    let _ = self.cmd_tx.send(GraphCommand::InsertGroupEffect {
                        group,
                        index: insert_at,
                        effect: loaded,
                        mix: mix as f64,
                    });
                    g.effects.push(slot);
                }
            }
        }

        self.dirty = true;
        self.rebuild_tree();
    }

    /// Get the instrument index for the currently selected tree entry.
    fn selected_inst(&self) -> Option<usize> {
        self.selected_address()?.inst()
    }

    fn open_target_selector(&mut self, inst: usize, mod_index: usize) {
        let inst_node = match self.instruments.get(inst) {
            Some(n) => n,
            None => return,
        };

        let mut entries = Vec::new();
        let mut items = Vec::new();

        // Plugin parameters across the whole chain (instrument + effects),
        // labelled by plugin and routed by slot.
        let chain: Vec<(usize, &PluginSlot)> = std::iter::once(inst_node.instrument.as_ref())
            .chain(inst_node.effects.iter().map(Some))
            .enumerate()
            .filter_map(|(slot, p)| p.map(|p| (slot, p)))
            .collect();
        for (slot, plugin) in &chain {
            for p in &plugin.params {
                let idx = entries.len();
                // `label` is the bare param name (stored on the target and
                // serialized); the selector list shows plugin + param columns.
                entries.push(TargetEntry {
                    label: p.name.clone(),
                    slot: *slot,
                    kind: crate::plugin::chain::ModTargetKind::PluginParam {
                        slot: *slot,
                        param_index: p.index,
                    },
                    param_min: p.min,
                    param_max: p.max,
                    base_value: p.value,
                });
                items.push(FilterListItem {
                    cells: vec![plugin.name.clone(), p.name.clone()],
                    index: idx,
                });
            }
        }

        // Sibling lane modulators (cross-mod).
        for (sib_idx, sib) in inst_node.modulators.iter().enumerate() {
            if sib_idx == mod_index {
                continue; // Skip self.
            }
            let prefix = format!("Mod {sib_idx} ");
            let push = |entries: &mut Vec<TargetEntry>, items: &mut Vec<FilterListItem>, label: String, kind, min, max, base| {
                let idx = entries.len();
                entries.push(TargetEntry { label: label.clone(), slot: 0, kind, param_min: min, param_max: max, base_value: base });
                items.push(FilterListItem { cells: vec!["Mod".into(), label], index: idx });
            };
            match &sib.source {
                ModSourceSlot::Lfo { rate, .. } => {
                    push(&mut entries, &mut items, format!("{prefix}rate"),
                        crate::plugin::chain::ModTargetKind::ModulatorRate { mod_index: sib_idx }, 0.01, 50.0, *rate);
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    push(&mut entries, &mut items, format!("{prefix}attack"),
                        crate::plugin::chain::ModTargetKind::ModulatorAttack { mod_index: sib_idx }, 0.001, 10.0, *attack);
                    push(&mut entries, &mut items, format!("{prefix}decay"),
                        crate::plugin::chain::ModTargetKind::ModulatorDecay { mod_index: sib_idx }, 0.001, 10.0, *decay);
                    push(&mut entries, &mut items, format!("{prefix}sustain"),
                        crate::plugin::chain::ModTargetKind::ModulatorSustain { mod_index: sib_idx }, 0.0, 1.0, *sustain);
                    push(&mut entries, &mut items, format!("{prefix}release"),
                        crate::plugin::chain::ModTargetKind::ModulatorRelease { mod_index: sib_idx }, 0.001, 10.0, *release);
                }
            }
            for (tgt_idx, tgt) in sib.targets.iter().enumerate() {
                push(&mut entries, &mut items, format!("{prefix}{} depth", tgt.param_name),
                    crate::plugin::chain::ModTargetKind::ModulatorDepth { mod_index: sib_idx, target_index: tgt_idx }, 0.0, 1.0, tgt.depth);
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);

        self.target_selector = Some(TargetSelectorState {
            filter,
            items,
            entries,
            scope: ModScope::Lane(inst),
            mod_index,
        });
    }

    fn confirm_target_selector(&mut self) {
        let ts = match self.target_selector.take() {
            Some(s) => s,
            None => return,
        };
        let chosen = match ts.filter.selected_item(&ts.items) {
            Some(item) => item.index,
            None => return,
        };
        let entry = &ts.entries[chosen];

        let target = crate::plugin::chain::ModTarget {
            kind: entry.kind.clone(),
            depth: 0.5,
            base_value: entry.base_value,
            param_min: entry.param_min,
            param_max: entry.param_max,
        };
        match ts.scope {
            ModScope::Lane(inst) => {
                let _ = self.cmd_tx.send(GraphCommand::AddModTarget {
                    inst,
                    mod_index: ts.mod_index,
                    target,
                });
            }
            ModScope::Group(group) => {
                let _ = self.cmd_tx.send(GraphCommand::AddGroupModTarget {
                    group,
                    mod_index: ts.mod_index,
                    target,
                });
            }
        }

        let slot = entry.slot;
        let label = entry.label.clone();
        let kind = entry.kind.clone();
        let (min, max) = (entry.param_min, entry.param_max);
        let mod_index = ts.mod_index;
        if let Some(mods) = self.scoped_modulators_mut(ts.scope) {
            if let Some(m) = mods.get_mut(mod_index) {
                m.targets.push(ModTargetSlot {
                    slot,
                    param_name: label,
                    kind,
                    depth: 0.5,
                    param_min: min,
                    param_max: max,
                });
            }
        }
        self.dirty = true;
        self.rebuild_tree();
    }

    /// Open the "modulate <param>" popup for the parameter currently selected
    /// in the param pane (a plugin parameter on the Instrument/Effect node).
    fn open_modulate(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let (inst, slot) = match addr {
            TreeAddress::Instrument(inst) => (inst, 0),
            TreeAddress::Effect { inst, index } => (inst, index + 1),
            _ => return,
        };
        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let param = match self.plugin_at(&addr).and_then(|p| p.params.get(pa)) {
            Some(p) => p,
            None => return,
        };
        let (param_index, param_name, param_min, param_max, base_value) =
            (param.index, param.name.clone(), param.min, param.max, param.value);

        // Choices: new LFO, new envelope, then each existing lane modulator.
        let mut choices = vec![ModulateChoice::NewLfo, ModulateChoice::NewEnvelope];
        let mut items = vec![
            FilterListItem { cells: vec![format!("New LFO → {param_name}")], index: 0 },
            FilterListItem { cells: vec![format!("New envelope → {param_name}")], index: 1 },
        ];
        if let Some(node) = self.instruments.get(inst) {
            for (i, m) in node.modulators.iter().enumerate() {
                let label = match &m.source {
                    ModSourceSlot::Lfo { waveform, rate } => format!("LFO {:.1}Hz {}", rate, waveform.name()),
                    ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
                };
                let idx = choices.len();
                choices.push(ModulateChoice::Existing(i));
                items.push(FilterListItem { cells: vec![format!("attach → {label}")], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            scope: ModScope::Lane(inst),
            slot,
            target_kind: crate::plugin::chain::ModTargetKind::PluginParam {
                slot,
                param_index,
            },
            param_name,
            param_min,
            param_max,
            base_value,
            choices,
            filter,
            items,
        });
    }

    fn confirm_modulate(&mut self) {
        let ms = match self.modulate.take() {
            Some(m) => m,
            None => return,
        };
        let choice = match ms.filter.selected_item(&ms.items) {
            Some(item) => ms.choices[item.index],
            None => return,
        };

        // Resolve the target modulator index, creating one in the right rack if
        // requested.
        let mod_index = match choice {
            ModulateChoice::NewLfo | ModulateChoice::NewEnvelope => {
                let (graph_source, slot_source) = match choice {
                    ModulateChoice::NewEnvelope => (
                        crate::plugin::chain::ModSource::Envelope {
                            attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                            state: crate::plugin::chain::EnvState::Idle,
                            level: 0.0,
                            notes_held: 0,
                        },
                        ModSourceSlot::Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
                    ),
                    _ => (
                        crate::plugin::chain::ModSource::Lfo {
                            waveform: crate::plugin::chain::LfoWaveform::Sine,
                            rate: 1.0,
                            phase: 0.0,
                        },
                        ModSourceSlot::Lfo { waveform: crate::plugin::chain::LfoWaveform::Sine, rate: 1.0 },
                    ),
                };
                let mod_index = match self.scoped_modulators_mut(ms.scope) {
                    Some(mods) => mods.len(),
                    None => return,
                };
                match ms.scope {
                    ModScope::Lane(inst) => {
                        let _ = self.cmd_tx.send(GraphCommand::InsertModulator {
                            inst,
                            index: mod_index,
                            source: graph_source,
                        });
                    }
                    ModScope::Group(group) => {
                        let _ = self.cmd_tx.send(GraphCommand::InsertGroupModulator {
                            group,
                            index: mod_index,
                            source: graph_source,
                        });
                    }
                }
                if let Some(mods) = self.scoped_modulators_mut(ms.scope) {
                    mods.push(ModulatorSlot { source: slot_source, targets: vec![] });
                }
                mod_index
            }
            ModulateChoice::Existing(i) => i,
        };

        // Bind the parameter as a target of that modulator.
        let kind = ms.target_kind.clone();
        let target = crate::plugin::chain::ModTarget {
            kind: kind.clone(),
            depth: 0.5,
            base_value: ms.base_value,
            param_min: ms.param_min,
            param_max: ms.param_max,
        };
        match ms.scope {
            ModScope::Lane(inst) => {
                let _ = self.cmd_tx.send(GraphCommand::AddModTarget { inst, mod_index, target });
            }
            ModScope::Group(group) => {
                let _ = self.cmd_tx.send(GraphCommand::AddGroupModTarget { group, mod_index, target });
            }
        }
        if let Some(mods) = self.scoped_modulators_mut(ms.scope) {
            if let Some(m) = mods.get_mut(mod_index) {
                m.targets.push(ModTargetSlot {
                    slot: ms.slot,
                    param_name: ms.param_name,
                    kind,
                    depth: 0.5,
                    param_min: ms.param_min,
                    param_max: ms.param_max,
                });
            }
        }
        self.dirty = true;
        self.rebuild_tree();
    }

    /// Open the target selector for a group modulator. Candidate targets span
    /// the group's world: every member instrument's chain (`GroupMember`), the
    /// group's own bus effects (`GroupBus`), and sibling group modulators.
    fn open_group_target_selector(&mut self, group: usize, mod_index: usize) {
        use crate::plugin::chain::ModTargetKind;
        let group_node = match self.groups.get(group) {
            Some(g) => g,
            None => return,
        };
        let mut entries = Vec::new();
        let mut items = Vec::new();

        // Member instruments' params (instrument slot 0 + effects 1..N), labelled
        // by member ordinal and plugin.
        let members: Vec<&InstrumentNode> =
            self.instruments.iter().filter(|n| n.group == Some(group)).collect();
        for (ord, member) in members.iter().enumerate() {
            let chain: Vec<(usize, &PluginSlot)> = std::iter::once(member.instrument.as_ref())
                .chain(member.effects.iter().map(Some))
                .enumerate()
                .filter_map(|(slot, p)| p.map(|p| (slot, p)))
                .collect();
            for (slot, plugin) in &chain {
                for p in &plugin.params {
                    let idx = entries.len();
                    entries.push(TargetEntry {
                        label: p.name.clone(),
                        slot: *slot,
                        kind: ModTargetKind::GroupMember {
                            member: ord,
                            slot: *slot,
                            param_index: p.index,
                        },
                        param_min: p.min,
                        param_max: p.max,
                        base_value: p.value,
                    });
                    items.push(FilterListItem {
                        cells: vec![format!("M{ord} {}", plugin.name), p.name.clone()],
                        index: idx,
                    });
                }
            }
        }

        // Member instruments' modulators (cross-mod a member's LFO/envelope).
        for (ord, member) in members.iter().enumerate() {
            for (mk, mm) in member.modulators.iter().enumerate() {
                let prefix = format!("M{ord} Mod{mk}");
                let mut push_mm = |fname: &str,
                                   field: crate::plugin::chain::CrossModField,
                                   min: f32,
                                   max: f32,
                                   base: f32| {
                    let idx = entries.len();
                    entries.push(TargetEntry {
                        label: format!("{prefix} {fname}"),
                        slot: 0,
                        kind: ModTargetKind::GroupMemberMod { member: ord, mod_index: mk, field },
                        param_min: min,
                        param_max: max,
                        base_value: base,
                    });
                    items.push(FilterListItem {
                        cells: vec![prefix.clone(), fname.to_string()],
                        index: idx,
                    });
                };
                use crate::plugin::chain::CrossModField;
                match &mm.source {
                    ModSourceSlot::Lfo { rate, .. } => {
                        push_mm("rate", CrossModField::Rate, 0.01, 50.0, *rate);
                    }
                    ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                        push_mm("attack", CrossModField::Attack, 0.001, 10.0, *attack);
                        push_mm("decay", CrossModField::Decay, 0.001, 10.0, *decay);
                        push_mm("sustain", CrossModField::Sustain, 0.0, 1.0, *sustain);
                        push_mm("release", CrossModField::Release, 0.001, 10.0, *release);
                    }
                }
                for (ti, t) in mm.targets.iter().enumerate() {
                    push_mm(&format!("{} depth", t.param_name), CrossModField::Depth(ti), 0.0, 1.0, t.depth);
                }
            }
        }

        // The group's own bus effects.
        for (eff_idx, plugin) in group_node.effects.iter().enumerate() {
            for p in &plugin.params {
                let idx = entries.len();
                entries.push(TargetEntry {
                    label: p.name.clone(),
                    slot: 0,
                    kind: ModTargetKind::GroupBus {
                        effect_index: eff_idx,
                        param_index: p.index,
                    },
                    param_min: p.min,
                    param_max: p.max,
                    base_value: p.value,
                });
                items.push(FilterListItem {
                    cells: vec![format!("Bus {}", plugin.name), p.name.clone()],
                    index: idx,
                });
            }
        }

        // Sibling group modulators (cross-mod).
        for (sib_idx, sib) in group_node.modulators.iter().enumerate() {
            if sib_idx == mod_index {
                continue;
            }
            let prefix = format!("Mod {sib_idx} ");
            let push = |entries: &mut Vec<TargetEntry>, items: &mut Vec<FilterListItem>, label: String, kind, min, max, base| {
                let idx = entries.len();
                entries.push(TargetEntry { label: label.clone(), slot: 0, kind, param_min: min, param_max: max, base_value: base });
                items.push(FilterListItem { cells: vec!["Mod".into(), label], index: idx });
            };
            match &sib.source {
                ModSourceSlot::Lfo { rate, .. } => {
                    push(&mut entries, &mut items, format!("{prefix}rate"),
                        ModTargetKind::ModulatorRate { mod_index: sib_idx }, 0.01, 50.0, *rate);
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    push(&mut entries, &mut items, format!("{prefix}attack"),
                        ModTargetKind::ModulatorAttack { mod_index: sib_idx }, 0.001, 10.0, *attack);
                    push(&mut entries, &mut items, format!("{prefix}decay"),
                        ModTargetKind::ModulatorDecay { mod_index: sib_idx }, 0.001, 10.0, *decay);
                    push(&mut entries, &mut items, format!("{prefix}sustain"),
                        ModTargetKind::ModulatorSustain { mod_index: sib_idx }, 0.0, 1.0, *sustain);
                    push(&mut entries, &mut items, format!("{prefix}release"),
                        ModTargetKind::ModulatorRelease { mod_index: sib_idx }, 0.001, 10.0, *release);
                }
            }
            for (tgt_idx, tgt) in sib.targets.iter().enumerate() {
                push(&mut entries, &mut items, format!("{prefix}{} depth", tgt.param_name),
                    ModTargetKind::ModulatorDepth { mod_index: sib_idx, target_index: tgt_idx }, 0.0, 1.0, tgt.depth);
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.target_selector = Some(TargetSelectorState {
            filter,
            items,
            entries,
            scope: ModScope::Group(group),
            mod_index,
        });
    }

    /// Open the "modulate <param>" popup for a group bus-effect parameter.
    /// Lets the user create a new group modulator or attach to an existing one.
    fn open_group_modulate(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let (group, eff_idx) = match addr {
            TreeAddress::GroupEffect { group, index } => (group, index),
            _ => return,
        };
        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let param = match self.plugin_at(&addr).and_then(|p| p.params.get(pa)) {
            Some(p) => p,
            None => return,
        };
        let (param_index, param_name, param_min, param_max, base_value) =
            (param.index, param.name.clone(), param.min, param.max, param.value);

        let mut choices = vec![ModulateChoice::NewLfo, ModulateChoice::NewEnvelope];
        let mut items = vec![
            FilterListItem { cells: vec![format!("New LFO → {param_name}")], index: 0 },
            FilterListItem { cells: vec![format!("New envelope → {param_name}")], index: 1 },
        ];
        if let Some(node) = self.groups.get(group) {
            for (i, m) in node.modulators.iter().enumerate() {
                let label = match &m.source {
                    ModSourceSlot::Lfo { waveform, rate } => format!("LFO {:.1}Hz {}", rate, waveform.name()),
                    ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
                };
                let idx = choices.len();
                choices.push(ModulateChoice::Existing(i));
                items.push(FilterListItem { cells: vec![format!("attach → {label}")], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            scope: ModScope::Group(group),
            slot: 0,
            target_kind: crate::plugin::chain::ModTargetKind::GroupBus {
                effect_index: eff_idx,
                param_index,
            },
            param_name,
            param_min,
            param_max,
            base_value,
            choices,
            filter,
            items,
        });
    }

    /// Open the "assign to group" popup for instrument `inst`.
    fn open_group_assign(&mut self, inst: usize) {
        let current = self.instruments.get(inst).and_then(|n| n.group);
        let mut choices = vec![GroupAssignChoice::New];
        let mut items = vec![FilterListItem { cells: vec!["New group".into()], index: 0 }];
        if current.is_some() {
            choices.push(GroupAssignChoice::Ungroup);
            items.push(FilterListItem { cells: vec!["Ungroup".into()], index: 1 });
        }
        for (g_idx, group) in self.groups.iter().enumerate() {
            let name = group.name.clone().unwrap_or_else(|| format!("Group {}", g_idx + 1));
            let marker = if current == Some(g_idx) { " (current)" } else { "" };
            let idx = choices.len();
            choices.push(GroupAssignChoice::Existing(g_idx));
            items.push(FilterListItem { cells: vec![format!("{name}{marker}")], index: idx });
        }
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.group_assign = Some(GroupAssignState { inst, choices, filter, items });
    }

    fn confirm_group_assign(&mut self) {
        let gs = match self.group_assign.take() {
            Some(g) => g,
            None => return,
        };
        let choice = match gs.filter.selected_item(&gs.items) {
            Some(item) => gs.choices[item.index],
            None => return,
        };
        let inst = gs.inst;
        let new_group = match choice {
            GroupAssignChoice::Ungroup => None,
            GroupAssignChoice::Existing(g) => Some(g),
            GroupAssignChoice::New => {
                let g = self.groups.len();
                let _ = self.cmd_tx.send(GraphCommand::AddGroup);
                self.groups.push(GroupNode { name: None, volume: 1.0, effects: vec![], modulators: vec![] });
                Some(g)
            }
        };
        let _ = self.cmd_tx.send(GraphCommand::SetLaneGroup { inst, group: new_group });
        // Keep group-modulator member ordinals in sync with membership (mirrors
        // the audio thread's SetLaneGroup fixups).
        let old = tui_lane_member_ordinal(&self.instruments, inst);
        if let Some(node) = self.instruments.get_mut(inst) {
            node.group = new_group;
        }
        let new = tui_lane_member_ordinal(&self.instruments, inst);
        let old_group = old.map(|(g, _)| g);
        let new_group_idx = new.map(|(g, _)| g);
        if old_group != new_group_idx {
            if let Some((og, oo)) = old {
                if let Some(g) = self.groups.get_mut(og) {
                    fixup_tui_group_member_after_remove(&mut g.modulators, oo);
                }
            }
            if let Some((ng, no)) = new {
                if let Some(g) = self.groups.get_mut(ng) {
                    shift_tui_group_member_after_insert(&mut g.modulators, no);
                }
            }
        }
        self.dirty = true;
        self.rebuild_tree();
    }

    /// Open the preset selector for the selected plugin slot. inst+slot
    fn open_scale_selector(&mut self) {
        let entries = piano_view::scale_picker_entries();
        let items: Vec<FilterListItem> = entries
            .iter()
            .enumerate()
            .map(|(i, (name, _root, _idx))| FilterListItem {
                cells: vec![name.clone()],
                index: i,
            })
            .collect();
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        // Pre-select the current scale if it's in the list.
        let current = self.piano_filter.scale();
        if let Some(i) = entries
            .iter()
            .position(|(_, r, s)| *r == current.root && *s == current.scale_idx)
        {
            filter.list.selected = i;
        }
        self.scale_selector = Some(ScaleSelectorState {
            filter,
            items,
            entries: entries.into_iter().map(|(_, r, s)| (r, s)).collect(),
        });
    }

    fn confirm_scale_selector(&mut self) {
        let ss = match self.scale_selector.take() {
            Some(s) => s,
            None => return,
        };
        let item = match ss.filter.selected_item(&ss.items) {
            Some(it) => it,
            None => return,
        };
        let (root, scale_idx) = ss.entries[item.index];
        let current = self.piano_filter.scale();
        if current.root != root || current.scale_idx != scale_idx {
            let new_scale = ScaleSetting { root, scale_idx };
            self.piano_filter.set_scale(new_scale);
            self.dirty = true;
            log::info!("Scale set to {}", new_scale.display());
        }
    }

    /// matches the addressing used by GraphCommand: slot=0 = instrument,
    /// slot=1..N = effects.
    fn open_preset_selector(&mut self, inst: usize, slot: usize) {
        let plugin = if slot == 0 {
            self.instruments.get(inst).and_then(|n| n.instrument.as_ref())
        } else {
            self.instruments.get(inst).and_then(|n| n.effects.get(slot - 1))
        };
        let plugin = match plugin {
            Some(p) => p,
            None => return,
        };
        if plugin.presets.is_empty() {
            log::info!("Plugin '{}' has no presets", plugin.name);
            return;
        }

        let presets = plugin.presets.clone();
        let items: Vec<FilterListItem> = presets
            .iter()
            .enumerate()
            .map(|(idx, p)| FilterListItem {
                cells: vec![p.name.clone()],
                index: idx,
            })
            .collect();
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        // Pre-select the current preset if any.
        if let Some(ref current) = plugin.current_preset {
            if let Some(i) = presets.iter().position(|p| p.name == *current) {
                filter.list.selected = i;
            }
        }
        self.preset_selector = Some(PresetSelectorState {
            filter,
            items,
            presets,
            inst,
            slot,
        });
    }

    fn confirm_preset_selector(&mut self) {
        let ps = match self.preset_selector.take() {
            Some(s) => s,
            None => return,
        };
        let chosen_idx = match ps.filter.selected_item(&ps.items) {
            Some(item) => item.index,
            None => return,
        };
        let chosen = &ps.presets[chosen_idx];
        let preset_name = chosen.name.clone();
        let preset_id = chosen.id.clone();

        // Look up the slot's plugin source id so we can build a temporary
        // plugin to query post-preset parameter values for the UI mirror.
        let source = {
            let plugin = if ps.slot == 0 {
                self.instruments.get(ps.inst).and_then(|n| n.instrument.as_ref())
            } else {
                self.instruments.get(ps.inst).and_then(|n| n.effects.get(ps.slot - 1))
            };
            match plugin {
                Some(p) => p.id.clone(),
                None => return,
            }
        };

        // Drive the audio thread first — that's what actually changes the sound.
        let _ = self.cmd_tx.send(GraphCommand::LoadPreset {
            inst: ps.inst,
            slot: ps.slot,
            preset_id: preset_id.clone(),
        });

        // Mirror the preset's parameter values into the TUI slot. We do this
        // via a throwaway plugin instance because the live plugin is owned by
        // the audio thread; loading twice is acceptable at preset-switch time.
        let mut new_values: Option<Vec<f32>> = None;
        match plugin::load(&source, self.sample_rate, self.max_block_size, &self.runtime) {
            Ok(mut temp) => {
                crate::session::apply_preset(&mut temp, &preset_name);
                let params = temp.parameters();
                let mut values = Vec::with_capacity(params.len());
                for p in &params {
                    values.push(temp.get_parameter(p.index).unwrap_or(p.default));
                }
                new_values = Some(values);
            }
            Err(e) => {
                log::warn!("Failed to mirror preset values for {source}: {e}");
            }
        }

        // Update mirror. Set both `value` and `default` (the save-filter
        // baseline) so the new preset's parameters aren't written as overrides.
        let plugin_mut = if ps.slot == 0 {
            self.instruments.get_mut(ps.inst).and_then(|n| n.instrument.as_mut())
        } else {
            self.instruments.get_mut(ps.inst).and_then(|n| n.effects.get_mut(ps.slot - 1))
        };
        if let Some(p) = plugin_mut {
            p.current_preset = Some(preset_name);
            if let Some(values) = new_values {
                for (i, slot) in p.params.iter_mut().enumerate() {
                    let v = values
                        .get(slot.index as usize)
                        .copied()
                        .or_else(|| values.get(i).copied());
                    if let Some(v) = v {
                        slot.value = v;
                        slot.default = v;
                    }
                }
            }
        }

        self.dirty = true;
        self.rebuild_tree();
    }

    fn adjust_param(&mut self, delta: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        // Group bus effect params take a dedicated command (no instrument).
        if let TreeAddress::GroupEffect { group, index } = addr {
            let pa = match self.real_param_index() {
                Some(i) => i,
                None => return,
            };
            if let Some(param) = self.plugin_at_mut(&addr).and_then(|p| p.params.get_mut(pa)) {
                param.value = (param.value + delta).clamp(param.min, param.max);
                if matches!(param.kind, ParamKind::Enum(_)) {
                    param.value = param.value.round();
                }
                let (value, param_index) = (param.value, param.index);
                let _ = self.cmd_tx.send(GraphCommand::SetGroupParameter {
                    group,
                    index,
                    param_index,
                    value,
                });
                self.dirty = true;
            }
            return;
        }

        // Group modulator params (no instrument) take dedicated group commands.
        if let TreeAddress::GroupModulator { group, index } = addr {
            let pa = self.param_state.selected;
            self.adjust_group_modulator_param(group, index, pa, delta);
            return;
        }

        let inst = match addr.inst() {
            Some(i) => i,
            None => return,
        };

        // Handle pattern params separately.
        if let TreeAddress::Pattern(_) = addr {
            let pa = self.param_state.selected;
            self.adjust_pattern_param(inst, pa, delta);
            return;
        }

        // Handle modulator params separately.
        if let TreeAddress::Modulator { index, .. } = addr {
            let pa = self.param_state.selected;
            self.adjust_modulator_param(inst, index, pa, delta);
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let slot = addr.slot();
        if let Some(param) = self.plugin_at_mut(&addr).and_then(|p| p.params.get_mut(pa)) {
            param.value = (param.value + delta).clamp(param.min, param.max);
            // Enum params snap to whole values.
            if matches!(param.kind, ParamKind::Enum(_)) {
                param.value = param.value.round();
            }
            let new_value = param.value;
            let idx = param.index;
            let _ = self.cmd_tx.send(GraphCommand::SetParameter {
                inst,
                slot,
                param_index: idx,
                value: new_value,
            });
            self.dirty = true;
        }
    }

    fn adjust_pattern_param(&mut self, inst: usize, pa: usize, delta: f32) {
        let pat = match self.instruments.get_mut(inst)
            .and_then(|n| n.pattern.as_mut())
        {
            Some(p) => p,
            None => return,
        };
        match pa {
            0 => {
                // Length (beats)
                pat.length_beats = (pat.length_beats + delta).clamp(1.0, 32.0);
                let _ = self.cmd_tx.send(GraphCommand::SetPatternLength {
                    inst, beats: pat.length_beats,
                });
                self.dirty = true;
                self.rebuild_tree();
            }
            1 => {
                // Enabled (enum toggle)
                pat.enabled = !pat.enabled;
                let _ = self.cmd_tx.send(GraphCommand::SetPatternEnabled {
                    inst, enabled: pat.enabled,
                });
                self.dirty = true;
                self.rebuild_tree();
            }
            2 => {
                // Loop (enum toggle)
                pat.looping = !pat.looping;
                let _ = self.cmd_tx.send(GraphCommand::SetPatternLooping {
                    inst, looping: pat.looping,
                });
                self.dirty = true;
            }
            3 => {
                // Transpose (enum toggle: chromatic / in-key)
                pat.in_key = !pat.in_key;
                let _ = self.cmd_tx.send(GraphCommand::SetPatternInKey {
                    inst, in_key: pat.in_key,
                });
                self.dirty = true;
            }
            _ => {} // Notes row is informational
        }
    }

    fn adjust_modulator_param(&mut self, inst: usize, mod_index: usize, pa: usize, delta: f32) {
        // Direct field access (not the modulator_mut helper) so the modulator
        // borrow stays disjoint from self.cmd_tx for the command sends below.
        let m = match self.instruments.get_mut(inst).and_then(|n| n.modulators.get_mut(mod_index)) {
            Some(m) => m,
            None => return,
        };
        if pa == 0 {
            // Type (enum) — switch between LFO and Envelope.
            let new_source = match &m.source {
                ModSourceSlot::Lfo { .. } => ModSourceSlot::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                },
                ModSourceSlot::Envelope { .. } => ModSourceSlot::Lfo {
                    waveform: crate::plugin::chain::LfoWaveform::Sine,
                    rate: 1.0,
                },
            };
            let graph_source = mod_source_slot_to_graph(&new_source);
            m.source = new_source;
            let _ = self.cmd_tx.send(GraphCommand::SetModulatorSource {
                inst, mod_index,
                source: graph_source,
            });
            self.param_state.selected = 0;
            self.rebuild_tree();
        } else {
            match &mut m.source {
                ModSourceSlot::Lfo { waveform, rate } => {
                    if pa == 1 {
                        // Waveform (enum).
                        *waveform = if delta > 0.0 { waveform.next() } else { waveform.prev() };
                        let _ = self.cmd_tx.send(GraphCommand::SetModulatorWaveform {
                            inst, mod_index,
                            waveform: *waveform,
                        });
                        self.rebuild_tree();
                    } else if pa == 2 {
                        // Rate.
                        *rate = (*rate + delta).clamp(0.01, 50.0);
                        let _ = self.cmd_tx.send(GraphCommand::SetModulatorRate {
                            inst, mod_index,
                            rate: *rate,
                        });
                        self.rebuild_tree();
                    } else if pa == 3 {
                        // Separator row — no-op.
                    } else if let Some(t) = m.targets.get_mut(pa - 4) {
                        t.depth = (t.depth + delta).clamp(0.0, 1.0);
                        let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                            inst, mod_index,
                            target_index: pa - 4,
                            depth: t.depth,
                        });
                    }
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    match pa {
                        1 => {
                            *attack = (*attack + delta).clamp(0.001, 10.0);
                        }
                        2 => {
                            *decay = (*decay + delta).clamp(0.001, 10.0);
                        }
                        3 => {
                            *sustain = (*sustain + delta).clamp(0.0, 1.0);
                        }
                        4 => {
                            *release = (*release + delta).clamp(0.001, 10.0);
                        }
                        5 => {
                            // Separator row — no-op.
                        }
                        _ => {
                            let target_idx = pa - 6;
                            if let Some(t) = m.targets.get_mut(target_idx) {
                                t.depth = (t.depth + delta).clamp(0.0, 1.0);
                                let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                                    inst, mod_index,
                                    target_index: target_idx,
                                    depth: t.depth,
                                });
                            }
                        }
                    }
                    if (1..=4).contains(&pa) {
                        let _ = self.cmd_tx.send(GraphCommand::SetModulatorEnvelopeParam {
                            inst, mod_index,
                            attack: *attack, decay: *decay, sustain: *sustain, release: *release,
                        });
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn set_param_value(&mut self, value: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        // Group bus effect params take a dedicated command (no instrument).
        if let TreeAddress::GroupEffect { group, index } = addr {
            let pa = match self.real_param_index() {
                Some(i) => i,
                None => return,
            };
            if let Some(param) = self.plugin_at_mut(&addr).and_then(|p| p.params.get_mut(pa)) {
                param.value = value.clamp(param.min, param.max);
                let (value, param_index) = (param.value, param.index);
                let _ = self.cmd_tx.send(GraphCommand::SetGroupParameter {
                    group,
                    index,
                    param_index,
                    value,
                });
                self.dirty = true;
            }
            return;
        }

        // Group modulator params (no instrument) take dedicated group commands.
        if let TreeAddress::GroupModulator { group, index } = addr {
            let pa = self.param_state.selected;
            self.set_group_modulator_param_value(group, index, pa, value);
            return;
        }

        let inst = match addr.inst() {
            Some(i) => i,
            None => return,
        };

        // Handle pattern params.
        if let TreeAddress::Pattern(_) = addr {
            let pa = self.param_state.selected;
            if pa == 0 {
                // Length (beats) — set directly.
                let clamped = value.clamp(1.0, 32.0);
                if let Some(pat) = self.instruments.get_mut(inst)
                    .and_then(|n| n.pattern.as_mut())
                {
                    pat.length_beats = clamped;
                    let _ = self.cmd_tx.send(GraphCommand::SetPatternLength {
                        inst, beats: clamped,
                    });
                    self.dirty = true;
                    self.rebuild_tree();
                }
            }
            return;
        }

        // Handle modulator params.
        if let TreeAddress::Modulator { index, .. } = addr {
            let pa = self.param_state.selected;
            self.set_modulator_param_value(inst, index, pa, value);
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let slot = addr.slot();
        if let Some(param) = self.plugin_at_mut(&addr).and_then(|p| p.params.get_mut(pa)) {
            param.value = value.clamp(param.min, param.max);
            let new_value = param.value;
            let idx = param.index;
            let _ = self.cmd_tx.send(GraphCommand::SetParameter {
                inst,
                slot,
                param_index: idx,
                value: new_value,
            });
            self.dirty = true;
        }
    }

    fn set_modulator_param_value(&mut self, inst: usize, mod_index: usize, pa: usize, value: f32) {
        // Direct field access (not the modulator_mut helper) so the modulator
        // borrow stays disjoint from self.cmd_tx for the command sends below.
        let m = match self.instruments.get_mut(inst).and_then(|n| n.modulators.get_mut(mod_index)) {
            Some(m) => m,
            None => return,
        };
        if pa == 0 {
            // Type enum — not settable via numeric value entry, skip.
            return;
        }
        match &mut m.source {
            ModSourceSlot::Lfo { waveform: _, rate } => {
                if pa == 1 {
                    // Waveform enum — not settable via numeric value entry.
                    return;
                } else if pa == 2 {
                    *rate = value.clamp(0.01, 50.0);
                    let _ = self.cmd_tx.send(GraphCommand::SetModulatorRate {
                        inst, mod_index, rate: *rate,
                    });
                    self.rebuild_tree();
                } else if pa == 3 {
                    // Separator — not settable.
                    return;
                } else if let Some(t) = m.targets.get_mut(pa - 4) {
                    t.depth = value.clamp(0.0, 1.0);
                    let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                        inst, mod_index,
                        target_index: pa - 4,
                        depth: t.depth,
                    });
                }
            }
            ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                match pa {
                    1 => *attack = value.clamp(0.001, 10.0),
                    2 => *decay = value.clamp(0.001, 10.0),
                    3 => *sustain = value.clamp(0.0, 1.0),
                    4 => *release = value.clamp(0.001, 10.0),
                    5 => return, // Separator — not settable.
                    _ => {
                        let target_idx = pa - 6;
                        if let Some(t) = m.targets.get_mut(target_idx) {
                            t.depth = value.clamp(0.0, 1.0);
                            let _ = self.cmd_tx.send(GraphCommand::SetModTargetDepth {
                                inst, mod_index,
                                target_index: target_idx,
                                depth: t.depth,
                            });
                        }
                        self.dirty = true;
                        return;
                    }
                }
                let _ = self.cmd_tx.send(GraphCommand::SetModulatorEnvelopeParam {
                    inst, mod_index,
                    attack: *attack, decay: *decay, sustain: *sustain, release: *release,
                });
            }
        }
        self.dirty = true;
    }

    /// Group-rack mirror of `adjust_modulator_param`: edits the group modulator
    /// at `group`/`mod_index` and emits the corresponding `SetGroupModulator*`.
    fn adjust_group_modulator_param(&mut self, group: usize, mod_index: usize, pa: usize, delta: f32) {
        let m = match self.groups.get_mut(group).and_then(|g| g.modulators.get_mut(mod_index)) {
            Some(m) => m,
            None => return,
        };
        if pa == 0 {
            // Type (enum) — switch between LFO and Envelope.
            let new_source = match &m.source {
                ModSourceSlot::Lfo { .. } => ModSourceSlot::Envelope {
                    attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5,
                },
                ModSourceSlot::Envelope { .. } => ModSourceSlot::Lfo {
                    waveform: crate::plugin::chain::LfoWaveform::Sine,
                    rate: 1.0,
                },
            };
            let graph_source = mod_source_slot_to_graph(&new_source);
            m.source = new_source;
            let _ = self.cmd_tx.send(GraphCommand::SetGroupModulatorSource {
                group, mod_index,
                source: graph_source,
            });
            self.param_state.selected = 0;
            self.rebuild_tree();
        } else {
            match &mut m.source {
                ModSourceSlot::Lfo { waveform, rate } => {
                    if pa == 1 {
                        *waveform = if delta > 0.0 { waveform.next() } else { waveform.prev() };
                        let _ = self.cmd_tx.send(GraphCommand::SetGroupModulatorWaveform {
                            group, mod_index,
                            waveform: *waveform,
                        });
                        self.rebuild_tree();
                    } else if pa == 2 {
                        *rate = (*rate + delta).clamp(0.01, 50.0);
                        let _ = self.cmd_tx.send(GraphCommand::SetGroupModulatorRate {
                            group, mod_index,
                            rate: *rate,
                        });
                        self.rebuild_tree();
                    } else if pa == 3 {
                        // Separator row — no-op.
                    } else if let Some(t) = m.targets.get_mut(pa - 4) {
                        t.depth = (t.depth + delta).clamp(0.0, 1.0);
                        let _ = self.cmd_tx.send(GraphCommand::SetGroupModTargetDepth {
                            group, mod_index,
                            target_index: pa - 4,
                            depth: t.depth,
                        });
                    }
                }
                ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                    match pa {
                        1 => *attack = (*attack + delta).clamp(0.001, 10.0),
                        2 => *decay = (*decay + delta).clamp(0.001, 10.0),
                        3 => *sustain = (*sustain + delta).clamp(0.0, 1.0),
                        4 => *release = (*release + delta).clamp(0.001, 10.0),
                        5 => {} // Separator row — no-op.
                        _ => {
                            let target_idx = pa - 6;
                            if let Some(t) = m.targets.get_mut(target_idx) {
                                t.depth = (t.depth + delta).clamp(0.0, 1.0);
                                let _ = self.cmd_tx.send(GraphCommand::SetGroupModTargetDepth {
                                    group, mod_index,
                                    target_index: target_idx,
                                    depth: t.depth,
                                });
                            }
                        }
                    }
                    if (1..=4).contains(&pa) {
                        let _ = self.cmd_tx.send(GraphCommand::SetGroupModulatorEnvelopeParam {
                            group, mod_index,
                            attack: *attack, decay: *decay, sustain: *sustain, release: *release,
                        });
                    }
                }
            }
        }
        self.dirty = true;
    }

    /// Group-rack mirror of `set_modulator_param_value`.
    fn set_group_modulator_param_value(&mut self, group: usize, mod_index: usize, pa: usize, value: f32) {
        let m = match self.groups.get_mut(group).and_then(|g| g.modulators.get_mut(mod_index)) {
            Some(m) => m,
            None => return,
        };
        if pa == 0 {
            return; // Type enum — not settable via numeric entry.
        }
        match &mut m.source {
            ModSourceSlot::Lfo { waveform: _, rate } => {
                if pa == 1 {
                    return; // Waveform enum.
                } else if pa == 2 {
                    *rate = value.clamp(0.01, 50.0);
                    let _ = self.cmd_tx.send(GraphCommand::SetGroupModulatorRate {
                        group, mod_index, rate: *rate,
                    });
                    self.rebuild_tree();
                } else if pa == 3 {
                    return; // Separator.
                } else if let Some(t) = m.targets.get_mut(pa - 4) {
                    t.depth = value.clamp(0.0, 1.0);
                    let _ = self.cmd_tx.send(GraphCommand::SetGroupModTargetDepth {
                        group, mod_index,
                        target_index: pa - 4,
                        depth: t.depth,
                    });
                }
            }
            ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                match pa {
                    1 => *attack = value.clamp(0.001, 10.0),
                    2 => *decay = value.clamp(0.001, 10.0),
                    3 => *sustain = value.clamp(0.0, 1.0),
                    4 => *release = value.clamp(0.001, 10.0),
                    5 => return, // Separator.
                    _ => {
                        let target_idx = pa - 6;
                        if let Some(t) = m.targets.get_mut(target_idx) {
                            t.depth = value.clamp(0.0, 1.0);
                            let _ = self.cmd_tx.send(GraphCommand::SetGroupModTargetDepth {
                                group, mod_index,
                                target_index: target_idx,
                                depth: t.depth,
                            });
                        }
                        self.dirty = true;
                        return;
                    }
                }
                let _ = self.cmd_tx.send(GraphCommand::SetGroupModulatorEnvelopeParam {
                    group, mod_index,
                    attack: *attack, decay: *decay, sustain: *sustain, release: *release,
                });
            }
        }
        self.dirty = true;
    }

    fn save_session(&mut self) {
        let path = match &self.session_path {
            Some(p) => p.clone(),
            None => {
                log::warn!("No session path — cannot save");
                return;
            }
        };
        if let Err(e) = self.save_session_to(&path) {
            log::error!("Failed to save session: {e}");
        }
    }

    /// Save the session to `path`. Does not touch `session_path` — callers
    /// decide what a failed save means for the current path.
    fn save_session_to(&mut self, path: &Path) -> anyhow::Result<()> {
        // Ensure parent directory exists (e.g. ~/.config/tang/sessions/).
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    anyhow::anyhow!("failed to create directory {}: {e}", parent.display())
                })?;
            }
        }

        let mods_to_save = |mods: &[ModulatorSlot]| -> Vec<crate::session::SaveModulator> {
            mods.iter()
                .map(|m| {
                    let source = match &m.source {
                        ModSourceSlot::Lfo { waveform, rate } => {
                            crate::session::SaveModSource::Lfo {
                                waveform: waveform.name().to_string(),
                                rate: *rate,
                            }
                        }
                        ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                            crate::session::SaveModSource::Envelope {
                                attack: *attack,
                                decay: *decay,
                                sustain: *sustain,
                                release: *release,
                            }
                        }
                    };
                    crate::session::SaveModulator {
                        source,
                        targets: m
                            .targets
                            .iter()
                            .map(|t| crate::session::SaveModTarget {
                                kind: t.kind.clone(),
                                label: t.param_name.clone(),
                                depth: t.depth,
                                slot: t.slot,
                            })
                            .collect(),
                    }
                })
                .collect()
        };
        let to_save_effect = |fx: &PluginSlot| crate::session::SaveEffect {
            plugin: fx.id.clone(),
            mix: fx.mix,
            preset: fx.current_preset.clone(),
            params: fx
                .params
                .iter()
                .filter(|p| (p.value - p.default).abs() > f32::EPSILON)
                .map(|p| (p.name.clone(), p.value))
                .collect(),
        };
        let save_groups: Vec<crate::session::SaveGroup> = self
            .groups
            .iter()
            .map(|g| crate::session::SaveGroup {
                name: g.name.clone(),
                volume: g.volume,
                effects: g.effects.iter().map(&to_save_effect).collect(),
                modulators: mods_to_save(&g.modulators),
            })
            .collect();
        let save_instruments: Vec<crate::session::SaveInstrumentSlot> = self
            .instruments
            .iter()
            .map(|sp| crate::session::SaveInstrumentSlot {
                range: sp.range,
                transpose: sp.transpose,
                group: sp.group,
                modulators: mods_to_save(&sp.modulators),
                instrument: sp.instrument.as_ref().map(|inst| {
                    crate::session::SaveInstrument {
                        plugin: inst.id.clone(),
                        volume: sp.volume,
                        preset: inst.current_preset.clone(),
                        params: inst
                            .params
                            .iter()
                            .filter(|p| (p.value - p.default).abs() > f32::EPSILON)
                            .map(|p| (p.name.clone(), p.value))
                            .collect(),
                        pitch_bend_range: sp.pitch_bend_range,
                        remap: sp.remap.clone(),
                    }
                }),
                effects: sp.effects.iter().map(&to_save_effect).collect(),
                pattern: sp.pattern.as_ref().map(|p| crate::session::SavePattern {
                    bpm: p.bpm,
                    length_beats: p.length_beats,
                    looping: p.looping,
                    base_note: p.base_note,
                    events: p.events.clone(),
                    enabled: p.enabled,
                    in_key: p.in_key,
                }),
            })
            .collect();

        let piano_config = crate::session::PianoConfig {
            scale: Some(self.piano_filter.scale().short()),
            locked: matches!(self.piano_filter.mode(), PianoMode::Locked),
        };
        crate::session::save(path, &save_instruments, &save_groups, Some(&piano_config))?;
        self.dirty = false;
        log::info!("Session saved to {}", path.display());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Information about a loaded instrument slot for the TUI.
pub struct LoadedInstrument {
    pub range: Option<(u8, u8)>,
    pub transpose: i8,
    /// Host-side output gain from the session (1.0 = unity).
    pub volume: f32,
    pub instrument: Option<LoadedPlugin>,
    pub effects: Vec<LoadedPlugin>,
    /// Lane-scoped modulators (targets address the chain by slot).
    pub modulators: Vec<LoadedModulator>,
    pub pattern: Option<LoadedPattern>,
    /// Carried through from session config; not editable in the TUI.
    pub pitch_bend_range: f64,
    pub remap: std::collections::HashMap<String, crate::session::RemapTarget>,
    /// Submix group membership (index into the loaded groups), or None.
    pub group: Option<usize>,
}

/// A submix group loaded from session config, passed to the TUI.
pub struct LoadedGroup {
    pub name: Option<String>,
    pub volume: f32,
    pub effects: Vec<LoadedPlugin>,
    /// Group-scoped modulators loaded from the session.
    pub modulators: Vec<LoadedModulator>,
}

/// Pattern data loaded from session config, passed to the TUI.
pub struct LoadedPattern {
    pub bpm: f32,
    pub length_beats: f32,
    pub looping: bool,
    pub base_note: Option<u8>,
    pub events: Vec<(u64, u8, u8, u8)>, // (frame, status, note, velocity)
    pub enabled: bool,
    pub in_key: bool,
}

pub enum LoadedModSource {
    Lfo {
        waveform: crate::plugin::chain::LfoWaveform,
        rate: f32,
    },
    Envelope {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
}

pub struct LoadedModulator {
    pub source: LoadedModSource,
    pub targets: Vec<LoadedModTarget>,
}

pub struct LoadedModTarget {
    /// Chain slot for plugin-param targets (0 = instrument, 1..N = effect);
    /// 0 for cross-mod targets.
    pub slot: usize,
    pub param_name: String,
    pub kind: crate::plugin::chain::ModTargetKind,
    pub depth: f32,
    pub param_min: f32,
    pub param_max: f32,
}

/// Information about a loaded plugin slot, passed from play() to the TUI.
pub struct LoadedPlugin {
    pub name: String,
    pub id: String,
    pub is_instrument: bool,
    pub params: Vec<plugin::ParameterInfo>,
    /// Post-preset baseline value per parameter, before any session-file
    /// overrides are applied. The TUI uses this as `ParamSlot.default` so
    /// the save filter only writes parameters the user has actually changed
    /// relative to the active preset.
    pub param_defaults: Vec<f32>,
    pub param_values: Vec<f32>,
    pub presets: Vec<plugin::Preset>,
    pub current_preset: Option<String>,
    /// Effect dry/wet mix. 1.0 = full wet. Carried through from session
    /// config so it round-trips on save. Ignored for instrument slots.
    pub mix: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    loaded_instruments: Vec<LoadedInstrument>,
    loaded_groups: Vec<LoadedGroup>,
    cmd_tx: Sender<GraphCommand>,
    midi_tx: Sender<audio::MidiEvent>,
    runtime: plugin::Runtime,
    sample_rate: f32,
    max_block_size: usize,
    session_path: Option<PathBuf>,
    pattern_rx: crossbeam_channel::Receiver<crate::plugin::chain::PatternNotification>,
    held: Arc<HeldNotes>,
    piano_filter: Arc<PianoFilter>,
) -> anyhow::Result<()> {
    // Suppress Rust logging for the whole TUI lifetime when stderr is the
    // terminal: the background catalog scan and plugin loads log verbosely and
    // would corrupt the alternate screen. Must be set BEFORE the scan spawns,
    // since that thread starts logging immediately. Logging to a redirected
    // stderr (`tang 2> debug.log`) is left enabled. Restored at teardown.
    let prev_log_level = log::max_level();
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        log::set_max_level(log::LevelFilter::Off);
    }

    // Scan the plugin catalog on a background thread so the TUI can draw
    // immediately — a full scan instantiates every installed plugin and can
    // take seconds. Results arrive per-format on `catalog_rx` and are drained
    // in the event loop. The catalog is only needed by the selector popup.
    let catalog_rx = spawn_catalog_scan();

    let groups: Vec<GroupNode> = loaded_groups
        .into_iter()
        .map(|g| GroupNode {
            name: g.name,
            volume: g.volume,
            effects: g.effects.into_iter().map(to_plugin_slot).collect(),
            modulators: g.modulators.into_iter().map(to_modulator_slot).collect(),
        })
        .collect();

    // Convert loaded instruments into flat InstrumentNode list.
    let instruments: Vec<InstrumentNode> = loaded_instruments
        .into_iter()
        .map(|li| {
            let instrument = li.instrument.map(to_plugin_slot);
            let effects = li.effects.into_iter().map(to_plugin_slot).collect();
            let modulators = li.modulators.into_iter().map(to_modulator_slot).collect();
            let pattern = li.pattern.map(|p| PatternState {
                bpm: p.bpm,
                length_beats: p.length_beats,
                looping: p.looping,
                base_note: p.base_note,
                events: p.events,
                enabled: p.enabled,
                recording: false,
                in_key: p.in_key,
            });
            InstrumentNode {
                range: li.range,
                transpose: li.transpose,
                volume: li.volume,
                instrument,
                effects,
                modulators,
                pattern,
                pitch_bend_range: li.pitch_bend_range,
                remap: li.remap,
                group: li.group,
            }
        })
        .collect();

    let tree_entries = build_tree_entries(&instruments, &groups);
    let param_len = instruments.first()
        .and_then(|n| n.instrument.as_ref())
        .map_or(0, |p| p.params.len());

    let help_lines = build_help_lines();

    // Determine initial BPM from loaded patterns (if any).
    let initial_bpm = instruments.iter()
        .filter_map(|n| n.pattern.as_ref())
        .map(|p| p.bpm)
        .next()
        .unwrap_or(120.0);

    // Send transpose and pattern data to audio graph for loaded instruments.
    for (inst_idx, inst_node) in instruments.iter().enumerate() {
        if inst_node.transpose != 0 {
            let _ = cmd_tx.send(GraphCommand::SetTranspose {
                inst: inst_idx,
                semitones: inst_node.transpose,
            });
        }
        if let Some(ref p) = inst_node.pattern {
            let pattern_events: Vec<crate::plugin::chain::PatternEvent> = p.events.iter().map(|&(frame, status, note, vel)| {
                crate::plugin::chain::PatternEvent {
                    frame,
                    status,
                    note,
                    velocity: vel,
                }
            }).collect();
            let beats_per_sec = p.bpm / 60.0;
            let length_samples = (p.length_beats / beats_per_sec * sample_rate) as u64;
            let _ = cmd_tx.send(GraphCommand::SetPattern {
                inst: inst_idx,
                pattern: crate::plugin::chain::Pattern {
                    events: pattern_events,
                    length_samples,
                },
                base_note: p.base_note,
                in_key: p.in_key,
            });
            let _ = cmd_tx.send(GraphCommand::SetPatternEnabled {
                inst: inst_idx,
                enabled: p.enabled,
            });
            if !p.looping {
                let _ = cmd_tx.send(GraphCommand::SetPatternLooping {
                    inst: inst_idx,
                    looping: false,
                });
            }
        }
    }

    let mut s = State {
        active_tab: 0,
        chain_state: ListState::new(tree_entries.len()),
        param_state: ListState::new(param_len),
        tree_entries,
        instruments,
        groups,
        focus_params: false,
        help_lines,
        help_offset: 0,
        scrollbar_dragging: false,
        param_dragging: false,
        param_scrollbar_dragging: false,
        editing: None,
        range_edit: None,
        rename_group: None,
        selector: None,
        target_selector: None,
        modulate: None,
        group_assign: None,
        preset_selector: None,
        catalog: Vec::new(),
        catalog_rx,
        catalog_scanning: true,
        areas: Areas::default(),
        quit: false,
        session_path,
        dirty: false,
        param_filter_input: TextInputState::new(""),
        param_filtering: false,
        param_filtered: (0..param_len).collect(),
        cmd_tx,
        midi_tx,
        runtime,
        sample_rate,
        max_block_size,
        global_bpm: initial_bpm,
        bpm_editing: None,
        gain_editing: None,
        save_as: None,
        pattern_rx,
        held,
        piano_filter,
        piano_view_octave: 4,
        scale_selector: None,
    };

    // Probe keyboard enhancement support (must be done before entering raw mode).
    let kitty_supported = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);

    // Set up terminal.
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Disambiguate modifier combos where supported, so Ctrl+Shift+S (save as)
    // is distinguishable from Ctrl+S. Pushed after EnterAlternateScreen —
    // kitty keeps separate flag stacks for the main and alternate screens.
    if kitty_supported {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // (Rust logging was already suppressed at the top of this function, before
    // the background scan spawned; it's restored below at teardown.)
    let result = event_loop(&mut terminal, &mut s);

    log::set_max_level(prev_log_level);

    if kitty_supported {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    crossterm::terminal::disable_raw_mode()?;

    result.map_err(Into::into)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    s: &mut State,
) -> io::Result<()> {
    loop {
        // Drain plugin catalog batches from the background scan thread.
        while s.catalog_scanning {
            match s.catalog_rx.try_recv() {
                Ok(batch) => s.extend_catalog(batch),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    s.catalog_scanning = false;
                }
            }
        }

        // Drain pattern recording completion notifications.
        while let Ok(notif) = s.pattern_rx.try_recv() {
            if let Some(inst_node) = s.instruments.get_mut(notif.inst) {
                // Re-recording replaces the events but keeps the transpose mode.
                let in_key = inst_node.pattern.as_ref().is_some_and(|p| p.in_key);
                inst_node.pattern = Some(PatternState {
                    bpm: s.global_bpm,
                    length_beats: notif.length_beats,
                    looping: notif.looping,
                    base_note: notif.base_note,
                    events: notif.events,
                    enabled: notif.enabled,
                    recording: false,
                    in_key,
                });
                s.rebuild_tree();
            }
        }

        render(terminal, s)?;
        if s.quit {
            break;
        }

        // Poll with timeout so we wake up to drain pattern notifications
        // even when there's no user input.
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let ev = event::read()?;
        process_event(s, ev);
        while event::poll(Duration::ZERO)? {
            process_event(s, event::read()?);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Event processing
// ---------------------------------------------------------------------------

fn process_event(s: &mut State, ev: Event) {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if s.selector.is_some() {
                handle_selector_key(s, key.code);
            } else if s.target_selector.is_some() {
                handle_target_selector_key(s, key.code);
            } else if s.modulate.is_some() {
                handle_modulate_key(s, key.code);
            } else if s.group_assign.is_some() {
                handle_group_assign_key(s, key.code);
            } else if s.preset_selector.is_some() {
                handle_preset_selector_key(s, key.code);
            } else if s.scale_selector.is_some() {
                handle_scale_selector_key(s, key.code);
            } else if s.bpm_editing.is_some() {
                handle_bpm_edit_key(s, key.code);
            } else if s.gain_editing.is_some() {
                handle_gain_edit_key(s, key.code);
            } else if s.save_as.is_some() {
                handle_save_as_key(s, key.code);
            } else if s.editing.is_some() {
                handle_edit_key(s, key.code);
            } else if s.range_edit.is_some() {
                handle_range_edit_key(s, key.code);
            } else if s.rename_group.is_some() {
                handle_rename_group_key(s, key.code);
            } else if s.param_filtering {
                handle_param_filter_key(s, key.code);
            } else {
                handle_key(s, key.code, key.modifiers);
            }
        }
        Event::Mouse(mouse) => {
            if s.selector.is_some()
                || s.target_selector.is_some()
                || s.modulate.is_some()
                || s.group_assign.is_some()
                || s.preset_selector.is_some()
                || s.scale_selector.is_some()
                || s.editing.is_some()
                || s.range_edit.is_some()
                || s.rename_group.is_some()
                || s.bpm_editing.is_some()
                || s.gain_editing.is_some()
                || s.save_as.is_some()
            {
                if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                    s.selector = None;
                    s.target_selector = None;
                    s.modulate = None;
                    s.group_assign = None;
                    s.preset_selector = None;
                    s.scale_selector = None;
                    s.editing = None;
                    s.range_edit = None;
                    s.rename_group = None;
                    s.bpm_editing = None;
                    s.gain_editing = None;
                    s.save_as = None;
                }
                return;
            }
            handle_mouse(s, mouse.kind, mouse.column, mouse.row);
        }
        _ => {}
    }
}

fn handle_selector_key(s: &mut State, code: KeyCode) {
    let sel = s.selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.selector = None,
        KeyCode::Enter => s.confirm_selector(),
        KeyCode::Up => {
            sel.filter.list.up();
            sel.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            sel.filter.list.down();
            sel.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            sel.filter.input.backspace();
            sel.filter.apply_filter(&sel.items);
        }
        KeyCode::Char(ch) => {
            sel.filter.input.insert(ch);
            sel.filter.apply_filter(&sel.items);
        }
        _ => {}
    }
}

fn handle_target_selector_key(s: &mut State, code: KeyCode) {
    let ts = s.target_selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.target_selector = None,
        KeyCode::Enter => s.confirm_target_selector(),
        KeyCode::Up => {
            ts.filter.list.up();
            ts.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ts.filter.list.down();
            ts.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            ts.filter.input.backspace();
            ts.filter.apply_filter(&ts.items);
        }
        KeyCode::Char(ch) => {
            ts.filter.input.insert(ch);
            ts.filter.apply_filter(&ts.items);
        }
        _ => {}
    }
}

fn handle_modulate_key(s: &mut State, code: KeyCode) {
    let ms = s.modulate.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.modulate = None,
        KeyCode::Enter => s.confirm_modulate(),
        KeyCode::Up => {
            ms.filter.list.up();
            ms.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ms.filter.list.down();
            ms.filter.list.ensure_visible(20);
        }
        _ => {}
    }
}

fn handle_group_assign_key(s: &mut State, code: KeyCode) {
    let gs = s.group_assign.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.group_assign = None,
        KeyCode::Enter => s.confirm_group_assign(),
        KeyCode::Up => {
            gs.filter.list.up();
            gs.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            gs.filter.list.down();
            gs.filter.list.ensure_visible(20);
        }
        _ => {}
    }
}

fn handle_scale_selector_key(s: &mut State, code: KeyCode) {
    let ss = s.scale_selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.scale_selector = None,
        KeyCode::Enter => s.confirm_scale_selector(),
        KeyCode::Up => {
            ss.filter.list.up();
            ss.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ss.filter.list.down();
            ss.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            ss.filter.input.backspace();
            ss.filter.apply_filter(&ss.items);
        }
        KeyCode::Char(ch) => {
            ss.filter.input.insert(ch);
            ss.filter.apply_filter(&ss.items);
        }
        _ => {}
    }
}

fn handle_preset_selector_key(s: &mut State, code: KeyCode) {
    let ps = s.preset_selector.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.preset_selector = None,
        KeyCode::Enter => s.confirm_preset_selector(),
        KeyCode::Up => {
            ps.filter.list.up();
            ps.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            ps.filter.list.down();
            ps.filter.list.ensure_visible(20);
        }
        KeyCode::Backspace => {
            ps.filter.input.backspace();
            ps.filter.apply_filter(&ps.items);
        }
        KeyCode::Char(ch) => {
            ps.filter.input.insert(ch);
            ps.filter.apply_filter(&ps.items);
        }
        _ => {}
    }
}

fn handle_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.editing = None,
        KeyCode::Enter => {
            if let Ok(val) = edit.input.value.parse::<f32>() {
                s.set_param_value(val);
            }
            s.editing = None;
        }
        KeyCode::Backspace => edit.input.backspace(),
        KeyCode::Delete => edit.input.delete(),
        KeyCode::Left => edit.input.move_left(),
        KeyCode::Right => edit.input.move_right(),
        KeyCode::Home => edit.input.home(),
        KeyCode::End => edit.input.end(),
        KeyCode::Char(ch) => edit.input.insert(ch),
        _ => {}
    }
}

fn handle_range_edit_key(s: &mut State, code: KeyCode) {
    let re = s.range_edit.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.range_edit = None,
        KeyCode::Enter => {
            let input = re.input.value.trim().to_string();
            let inst = re.inst;
            let range = if input.is_empty() {
                None
            } else {
                match crate::session::parse_range(&input) {
                    Ok(r) => Some(r),
                    Err(_) => return, // keep popup open on parse error
                }
            };
            let _ = s
                .cmd_tx
                .send(GraphCommand::SetInstrumentRange { inst, range });
            if let Some(node) = s.instruments.get_mut(inst) {
                node.range = range;
            }
            s.dirty = true;
            s.rebuild_tree();
            s.range_edit = None;
        }
        KeyCode::Backspace => re.input.backspace(),
        KeyCode::Delete => re.input.delete(),
        KeyCode::Left => re.input.move_left(),
        KeyCode::Right => re.input.move_right(),
        KeyCode::Home => re.input.home(),
        KeyCode::End => re.input.end(),
        KeyCode::Char(ch) => re.input.insert(ch),
        _ => {}
    }
}

fn handle_rename_group_key(s: &mut State, code: KeyCode) {
    let rg = s.rename_group.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.rename_group = None,
        KeyCode::Enter => {
            let name = rg.input.value.trim().to_string();
            let group = rg.group;
            // Empty clears the custom name (falls back to "Group N"). The name
            // is display-only, so no audio-thread command is needed.
            if let Some(g) = s.groups.get_mut(group) {
                g.name = if name.is_empty() { None } else { Some(name) };
            }
            s.dirty = true;
            s.rebuild_tree();
            s.rename_group = None;
        }
        KeyCode::Backspace => rg.input.backspace(),
        KeyCode::Delete => rg.input.delete(),
        KeyCode::Left => rg.input.move_left(),
        KeyCode::Right => rg.input.move_right(),
        KeyCode::Home => rg.input.home(),
        KeyCode::End => rg.input.end(),
        KeyCode::Char(ch) => rg.input.insert(ch),
        _ => {}
    }
}

fn handle_bpm_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.bpm_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.bpm_editing = None,
        KeyCode::Enter => {
            if let Ok(val) = edit.input.value.trim().parse::<f32>() {
                let bpm = val.clamp(edit.param_min, edit.param_max);
                s.global_bpm = bpm;
                let _ = s.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm });
                // Update all pattern states
                for inst_node in &mut s.instruments {
                    if let Some(ref mut p) = inst_node.pattern {
                        p.bpm = bpm;
                    }
                }
            }
            s.bpm_editing = None;
        }
        KeyCode::Backspace => edit.input.backspace(),
        KeyCode::Delete => edit.input.delete(),
        KeyCode::Left => edit.input.move_left(),
        KeyCode::Right => edit.input.move_right(),
        KeyCode::Home => edit.input.home(),
        KeyCode::End => edit.input.end(),
        KeyCode::Char(ch) => edit.input.insert(ch),
        _ => {}
    }
}

fn handle_gain_edit_key(s: &mut State, code: KeyCode) {
    let ge = s.gain_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.gain_editing = None,
        KeyCode::Enter => {
            if let Ok(val) = ge.edit.input.value.trim().parse::<f32>() {
                let value = val.clamp(ge.edit.param_min, ge.edit.param_max);
                let inst = ge.inst;
                match ge.target {
                    GainTarget::Mix { index } => {
                        // Audio slot: 0 = instrument, 1..N = effects.
                        let _ = s.cmd_tx.send(GraphCommand::SetMix {
                            inst,
                            slot: index + 1,
                            value,
                        });
                        if let Some(fx) =
                            s.instruments.get_mut(inst).and_then(|n| n.effects.get_mut(index))
                        {
                            fx.mix = value;
                        }
                    }
                    GainTarget::Volume => {
                        let _ = s.cmd_tx.send(GraphCommand::SetVolume { inst, value });
                        if let Some(node) = s.instruments.get_mut(inst) {
                            node.volume = value;
                        }
                    }
                    GainTarget::GroupVolume { group } => {
                        let _ = s.cmd_tx.send(GraphCommand::SetGroupVolume { group, value });
                        if let Some(g) = s.groups.get_mut(group) {
                            g.volume = value;
                        }
                    }
                    GainTarget::GroupMix { group, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::SetGroupMix { group, index, value });
                        if let Some(fx) = s.groups.get_mut(group).and_then(|g| g.effects.get_mut(index)) {
                            fx.mix = value;
                        }
                    }
                }
                s.dirty = true;
            }
            s.gain_editing = None;
        }
        KeyCode::Backspace => ge.edit.input.backspace(),
        KeyCode::Delete => ge.edit.input.delete(),
        KeyCode::Left => ge.edit.input.move_left(),
        KeyCode::Right => ge.edit.input.move_right(),
        KeyCode::Home => ge.edit.input.home(),
        KeyCode::End => ge.edit.input.end(),
        KeyCode::Char(ch) => ge.edit.input.insert(ch),
        _ => {}
    }
}

fn handle_save_as_key(s: &mut State, code: KeyCode) {
    let sa = s.save_as.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.save_as = None,
        KeyCode::Enter => {
            let mut name = sa.input.value.trim().to_string();
            if name.is_empty() {
                return; // keep popup open until a name is given
            }
            if !name.ends_with(".toml") {
                name.push_str(".toml");
            }
            // Save into the current session's directory (fallback: cwd).
            let dir = s
                .session_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            let path = dir.join(name);
            match s.save_session_to(&path) {
                Ok(()) => {
                    // Subsequent Ctrl+S now targets the new file.
                    s.session_path = Some(path);
                    s.save_as = None;
                }
                Err(e) => {
                    // Keep the popup (and the original session_path) and show
                    // the error so the user can fix the name or cancel.
                    s.save_as.as_mut().unwrap().error = Some(e.to_string());
                }
            }
        }
        KeyCode::Backspace => {
            sa.error = None;
            sa.input.backspace();
        }
        KeyCode::Delete => {
            sa.error = None;
            sa.input.delete();
        }
        KeyCode::Left => sa.input.move_left(),
        KeyCode::Right => sa.input.move_right(),
        KeyCode::Home => sa.input.home(),
        KeyCode::End => sa.input.end(),
        KeyCode::Char(ch) => {
            sa.error = None;
            sa.input.insert(ch);
        }
        _ => {}
    }
}

fn handle_param_filter_key(s: &mut State, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            // Cancel filter, clear text.
            s.param_filtering = false;
            s.param_filter_input = TextInputState::new("");
            s.recompute_param_filter();
        }
        KeyCode::Enter => {
            // Accept filter, keep text active, stop typing.
            s.param_filtering = false;
        }
        KeyCode::Backspace => {
            s.param_filter_input.backspace();
            s.recompute_param_filter();
        }
        KeyCode::Delete => {
            s.param_filter_input.delete();
            s.recompute_param_filter();
        }
        KeyCode::Left => s.param_filter_input.move_left(),
        KeyCode::Right => s.param_filter_input.move_right(),
        KeyCode::Home => s.param_filter_input.home(),
        KeyCode::End => s.param_filter_input.end(),
        KeyCode::Up => s.param_state.up(),
        KeyCode::Down => s.param_state.down(),
        KeyCode::PageUp => s.param_state.page_up(20),
        KeyCode::PageDown => s.param_state.page_down(20),
        KeyCode::Char(ch) => {
            s.param_filter_input.insert(ch);
            s.recompute_param_filter();
        }
        _ => {}
    }
}

fn handle_key(s: &mut State, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Char('q') | KeyCode::Char('c')
            if modifiers.contains(KeyModifiers::CONTROL) =>
        {
            s.quit = true;
        }
        // Ctrl+Shift+S — save as (filename popup). Checked before plain Ctrl+S.
        KeyCode::Char('s' | 'S')
            if modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let current = s
                .session_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            s.save_as = Some(SaveAsState {
                input: TextInputState::new(&current),
                error: None,
            });
        }
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            s.save_session();
        }
        KeyCode::Char('1') => s.active_tab = 0,
        KeyCode::Char('2') => s.active_tab = 1,
        KeyCode::Char('3') => s.active_tab = 2,
        KeyCode::Char('4') => s.active_tab = 3,
        KeyCode::Tab => s.active_tab = (s.active_tab + 1) % TAB_NAMES.len(),
        KeyCode::BackTab => s.active_tab = (s.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len(),

        // ---- Piano tab ----
        KeyCode::Char('k') if s.active_tab == 1 => {
            s.open_scale_selector();
        }
        KeyCode::Char('l') if s.active_tab == 1 => {
            let new_mode = match s.piano_filter.mode() {
                PianoMode::Highlight => PianoMode::Locked,
                PianoMode::Locked => PianoMode::Highlight,
            };
            s.piano_filter.set_mode(new_mode);
            s.dirty = true;
            log::info!("Piano mode → {}", new_mode.label());
        }
        KeyCode::Char('[') if s.active_tab == 1 && s.piano_view_octave > 0 => {
            s.piano_view_octave -= 1;
        }
        KeyCode::Char(']') if s.active_tab == 1 && s.piano_view_octave < 8 => {
            s.piano_view_octave += 1;
        }

        // 'i' — replace instrument on the selected instrument slot.
        KeyCode::Char('i') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Instrument(_)) = s.selected_address().copied() {
                s.open_selector(SelectorMode::Instrument);
            }
        }

        // 'n' — add a new instrument lane (full range) and immediately open the
        // plugin selector to fill it. The lane defaults to builtin:osc, so it
        // makes sound even if the selector is dismissed without a pick. Set its
        // key range afterwards with 'R'. This is how you build a keyboard split.
        KeyCode::Char('n') if s.active_tab == 0 && !s.focus_params => {
            let new_idx = s.instruments.len();
            let _ = s.cmd_tx.send(GraphCommand::AddInstrument { range: None });
            s.instruments.push(InstrumentNode {
                range: None,
                transpose: 0,
                volume: 1.0,
                instrument: None,
                effects: vec![],
                modulators: vec![],
                pattern: None,
                pitch_bend_range: 2.0,
                remap: Default::default(),
                group: None,
            });
            // Default the new lane to builtin:osc so it isn't silent if the
            // selector is cancelled.
            if let Some((loaded, slot)) = s.load_plugin_slot("builtin:osc") {
                s.set_instrument(new_idx, loaded, slot);
            }
            s.dirty = true;
            s.rebuild_tree();
            // Move the cursor onto the new lane so the selector fills it.
            if let Some(pos) = s.tree_entries.iter().position(
                |e| matches!(e.address, TreeAddress::Instrument(i) if i == new_idx),
            ) {
                s.chain_state.selected = pos;
                s.sync_param_state();
            }
            s.open_selector(SelectorMode::Instrument);
        }

        // 'R' — set the key range of the selected instrument, or rename the
        // selected group (range doesn't apply to groups).
        KeyCode::Char('R') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Group(group)) = s.selected_address().copied() {
                let initial = s
                    .groups
                    .get(group)
                    .and_then(|g| g.name.clone())
                    .unwrap_or_default();
                let mut input = TextInputState::new(&initial);
                input.end();
                s.rename_group = Some(RenameGroupState { group, input });
            } else if let Some(inst) = s.selected_inst() {
                let initial = s
                    .instruments
                    .get(inst)
                    .and_then(|n| n.range)
                    .map(format_range)
                    .unwrap_or_default();
                let mut input = TextInputState::new(&initial);
                input.end();
                s.range_edit = Some(RangeEditState { inst, input });
            }
        }

        // Session: contextual add (chain focus only).
        // Instrument/Effect → add effect.
        KeyCode::Char('a') if s.active_tab == 0 && !s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Instrument(_) | TreeAddress::Effect { .. }) => {
                    s.open_selector(SelectorMode::Effect);
                }
                Some(TreeAddress::Group(g)) => {
                    s.open_selector(SelectorMode::GroupEffect(g));
                }
                Some(TreeAddress::GroupEffect { group, .. }) => {
                    s.open_selector(SelectorMode::GroupEffect(group));
                }
                Some(TreeAddress::Pattern(_)) => {}
                Some(TreeAddress::Modulator { .. }) => {}
                Some(TreeAddress::GroupModulator { .. }) => {}
                None => {}
            }
        }

        // 'm' (param focus) — modulate the selected parameter: open a popup to
        // bind it to a new or existing lane modulator.
        KeyCode::Char('m') if s.active_tab == 0 && s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Instrument(_) | TreeAddress::Effect { .. }) => {
                    s.open_modulate();
                }
                // A group bus-effect parameter → create/attach a group modulator.
                Some(TreeAddress::GroupEffect { .. }) => {
                    s.open_group_modulate();
                }
                _ => {}
            }
        }

        // 'm' (chain focus) — add an empty LFO modulator to the instrument's
        // lane rack. (In param focus, `m` modulates the selected parameter —
        // handled above.)
        KeyCode::Char('m') if s.active_tab == 0 && !s.focus_params => {
            // On a group / group-bus node, add a group-scoped modulator; on an
            // instrument-side node, add a lane modulator.
            let group = match s.selected_address().copied() {
                Some(TreeAddress::Group(g))
                | Some(TreeAddress::GroupEffect { group: g, .. })
                | Some(TreeAddress::GroupModulator { group: g, .. }) => Some(g),
                _ => None,
            };
            if let Some(g) = group {
                if let Some(node) = s.groups.get_mut(g) {
                    let mod_index = node.modulators.len();
                    let _ = s.cmd_tx.send(GraphCommand::InsertGroupModulator {
                        group: g,
                        index: mod_index,
                        source: crate::plugin::chain::ModSource::Lfo {
                            waveform: crate::plugin::chain::LfoWaveform::Sine,
                            rate: 1.0,
                            phase: 0.0,
                        },
                    });
                    node.modulators.push(ModulatorSlot {
                        source: ModSourceSlot::Lfo {
                            waveform: crate::plugin::chain::LfoWaveform::Sine,
                            rate: 1.0,
                        },
                        targets: vec![],
                    });
                    s.dirty = true;
                    s.rebuild_tree();
                }
            } else if let Some(inst) = s.selected_address().and_then(|a| a.inst()) {
                if let Some(node) = s.instruments.get_mut(inst) {
                    let mod_index = node.modulators.len();
                    let _ = s.cmd_tx.send(GraphCommand::InsertModulator {
                        inst,
                        index: mod_index,
                        source: crate::plugin::chain::ModSource::Lfo {
                            waveform: crate::plugin::chain::LfoWaveform::Sine,
                            rate: 1.0,
                            phase: 0.0,
                        },
                    });
                    node.modulators.push(ModulatorSlot {
                        source: ModSourceSlot::Lfo {
                            waveform: crate::plugin::chain::LfoWaveform::Sine,
                            rate: 1.0,
                        },
                        targets: vec![],
                    });
                    s.dirty = true;
                    s.rebuild_tree();
                }
            }
        }

        // 'g' (chain focus) — assign the selected instrument to a group
        // (new, existing, or ungroup), via a popup.
        KeyCode::Char('g') if s.active_tab == 0 && !s.focus_params => {
            if let Some(TreeAddress::Instrument(inst)) = s.selected_address().copied() {
                s.open_group_assign(inst);
            }
        }

        // 'p' — open preset selector for the selected plugin slot.
        KeyCode::Char('p') if s.active_tab == 0 && !s.focus_params => {
            if let Some(addr) = s.selected_address().copied() {
                let target = match addr {
                    TreeAddress::Instrument(inst) => Some((inst, 0)),
                    TreeAddress::Effect { inst, index } => Some((inst, index + 1)),
                    _ => None,
                };
                if let Some((inst, slot)) = target {
                    s.open_preset_selector(inst, slot);
                }
            }
        }

        // 't' — add modulation target (when modulator selected).
        KeyCode::Char('t') if s.active_tab == 0 && !s.focus_params => {
            match s.selected_address().copied() {
                Some(TreeAddress::Modulator { inst, index }) => {
                    s.open_target_selector(inst, index);
                }
                Some(TreeAddress::GroupModulator { group, index }) => {
                    s.open_group_target_selector(group, index);
                }
                _ => {}
            }
        }

        // 'r' — toggle pattern recording (on Pattern or Instrument node).
        KeyCode::Char('r') if s.active_tab == 0 && !s.focus_params => {
            let target = match s.selected_address().copied() {
                Some(TreeAddress::Pattern(inst)) => Some(inst),
                Some(TreeAddress::Instrument(inst)) => Some(inst),
                _ => None,
            };
            if let Some(inst) = target {
                if let Some(inst_node) = s.instruments.get_mut(inst) {
                    let currently_recording = inst_node.pattern.as_ref().is_some_and(|p| p.recording);
                    if currently_recording {
                        // Stop recording
                        let _ = s.cmd_tx.send(GraphCommand::SetPatternRecording { inst, recording: false });
                        if let Some(ref mut p) = inst_node.pattern {
                            p.recording = false;
                        }
                    } else {
                        // Start recording: create pattern state if needed
                        if inst_node.pattern.is_none() {
                            inst_node.pattern = Some(PatternState {
                                bpm: s.global_bpm,
                                length_beats: 4.0,
                                looping: true,
                                base_note: None,
                                events: vec![],
                                enabled: false,
                                recording: false,
                                in_key: false,
                            });
                        }
                        // Send BPM and length first
                        let _ = s.cmd_tx.send(GraphCommand::SetGlobalBpm { bpm: s.global_bpm });
                        let _ = s.cmd_tx.send(GraphCommand::SetPatternLength {
                            inst,
                            beats: inst_node.pattern.as_ref().unwrap().length_beats,
                        });
                        let _ = s.cmd_tx.send(GraphCommand::SetPatternRecording { inst, recording: true });
                        inst_node.pattern.as_mut().unwrap().recording = true;
                    }
                    s.dirty = true;
                    s.rebuild_tree();
                }
            }
        }

        // 'b' — edit BPM.
        KeyCode::Char('b') if s.active_tab == 0 && !s.focus_params => {
            s.bpm_editing = Some(EditState {
                input: TextInputState::new(&format!("{:.0}", s.global_bpm)),
                param_name: "BPM".to_string(),
                param_min: 20.0,
                param_max: 300.0,
            });
        }

        // 'x' — edit the dry/wet mix of the selected effect (host-side blend).
        KeyCode::Char('x') if s.active_tab == 0 && !s.focus_params => {
            let (target, mix) = match s.selected_address().copied() {
                Some(TreeAddress::Effect { inst, index }) => {
                    let mix = s.instruments.get(inst).and_then(|n| n.effects.get(index)).map_or(1.0, |fx| fx.mix);
                    (Some((inst, GainTarget::Mix { index })), mix)
                }
                Some(TreeAddress::GroupEffect { group, index }) => {
                    let mix = s.groups.get(group).and_then(|g| g.effects.get(index)).map_or(1.0, |fx| fx.mix);
                    (Some((0, GainTarget::GroupMix { group, index })), mix)
                }
                _ => (None, 1.0),
            };
            if let Some((inst, target)) = target {
                s.gain_editing = Some(GainEditState {
                    inst,
                    target,
                    edit: EditState {
                        input: TextInputState::new(&format!("{mix:.2}")),
                        param_name: "mix (0=dry, 1=wet)".to_string(),
                        param_min: 0.0,
                        param_max: 1.0,
                    },
                });
            }
        }

        // 'v' — edit the output volume of the selected instrument or group
        // (host-side gain, applied before effects). UI range 0–4.
        KeyCode::Char('v') if s.active_tab == 0 && !s.focus_params => {
            let (inst, target, volume) = match s.selected_address().copied() {
                Some(TreeAddress::Group(group)) => {
                    let volume = s.groups.get(group).map_or(1.0, |g| g.volume);
                    (0, Some(GainTarget::GroupVolume { group }), volume)
                }
                Some(addr) => match addr.inst() {
                    Some(inst) => {
                        let volume = s.instruments.get(inst).map_or(1.0, |n| n.volume);
                        (inst, Some(GainTarget::Volume), volume)
                    }
                    None => (0, None, 1.0),
                },
                None => (0, None, 1.0),
            };
            if let Some(target) = target {
                s.gain_editing = Some(GainEditState {
                    inst,
                    target,
                    edit: EditState {
                        input: TextInputState::new(&format!("{volume:.2}")),
                        param_name: "volume (1.0 = unity)".to_string(),
                        param_min: 0.0,
                        param_max: 4.0,
                    },
                });
            }
        }

        KeyCode::Char('d') if s.active_tab == 0 && !s.focus_params => {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                let addr = s.tree_entries[sel].address;
                match addr {
                    TreeAddress::Effect { inst, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::RemoveEffect { inst, index });
                        if let Some(inst_node) = s.instruments.get_mut(inst) {
                            if index < inst_node.effects.len() {
                                inst_node.effects.remove(index);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Instrument(inst) => {
                        let has_plugin =
                            s.instruments.get(inst).is_some_and(|n| n.instrument.is_some());
                        if has_plugin {
                            let _ = s.cmd_tx.send(GraphCommand::ClearInstrument { inst });
                            if let Some(n) = s.instruments.get_mut(inst) {
                                n.instrument = None;
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                        } else if s.instruments.len() > 1 {
                            // If this lane was a group member, fix up that group's
                            // modulator member ordinals (mirrors the audio thread).
                            let member_of = tui_lane_member_ordinal(&s.instruments, inst);
                            let _ = s.cmd_tx.send(GraphCommand::RemoveInstrument { inst });
                            s.instruments.remove(inst);
                            if let Some((group, ordinal)) = member_of {
                                if let Some(g) = s.groups.get_mut(group) {
                                    fixup_tui_group_member_after_remove(&mut g.modulators, ordinal);
                                }
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                        }
                    }
                    TreeAddress::Pattern(inst) => {
                        let _ = s.cmd_tx.send(GraphCommand::ClearPattern { inst });
                        if let Some(inst_node) = s.instruments.get_mut(inst) {
                            inst_node.pattern = None;
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Modulator { inst, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::RemoveModulator { inst, index });
                        if let Some(node) = s.instruments.get_mut(inst) {
                            if index < node.modulators.len() {
                                node.modulators.remove(index);
                                // Clean up cross-mod targets in siblings.
                                fixup_tui_cross_mod_after_remove(&mut node.modulators, index);
                            }
                        }
                        // If this lane is a group member, fix up group modulators
                        // that cross-mod its modulators.
                        if let Some((group, ordinal)) = tui_lane_member_ordinal(&s.instruments, inst) {
                            if let Some(g) = s.groups.get_mut(group) {
                                fixup_tui_group_member_mod_after_remove(&mut g.modulators, ordinal, index);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::RemoveGroupEffect { group, index });
                        if let Some(g) = s.groups.get_mut(group) {
                            if index < g.effects.len() {
                                g.effects.remove(index);
                                // Drop/shift group-mod bus targets after the removed effect.
                                fixup_tui_group_bus_after_remove(&mut g.modulators, index);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::GroupModulator { group, index } => {
                        let _ = s.cmd_tx.send(GraphCommand::RemoveGroupModulator { group, index });
                        if let Some(g) = s.groups.get_mut(group) {
                            if index < g.modulators.len() {
                                g.modulators.remove(index);
                                // Clean up cross-mod targets in sibling group modulators.
                                fixup_tui_cross_mod_after_remove(&mut g.modulators, index);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                    TreeAddress::Group(group) => {
                        // Delete the group: members become ungrouped, higher
                        // group indices shift down.
                        let _ = s.cmd_tx.send(GraphCommand::RemoveGroup { group });
                        if group < s.groups.len() {
                            s.groups.remove(group);
                            for inst_node in s.instruments.iter_mut() {
                                match inst_node.group {
                                    Some(g) if g == group => inst_node.group = None,
                                    Some(g) if g > group => inst_node.group = Some(g - 1),
                                    _ => {}
                                }
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                    }
                }
            }
        }

        // Enter: focus params or open value editor.
        KeyCode::Enter if s.active_tab == 0 => {
            if s.focus_params {
                let sel = s.chain_state.selected;
                let pa = s.param_state.selected;
                if sel < s.tree_entries.len() {
                    let addr = s.tree_entries[sel].address;
                    match addr {
                        TreeAddress::Modulator { inst, index } => {
                            let edit = s.instruments.get(inst)
                                .and_then(|n| n.modulators.get(index))
                                .and_then(|m| modulator_edit_state(m, pa));
                            if edit.is_some() {
                                s.editing = edit;
                            }
                        }
                        TreeAddress::GroupModulator { group, index } => {
                            let edit = s.groups.get(group)
                                .and_then(|g| g.modulators.get(index))
                                .and_then(|m| modulator_edit_state(m, pa));
                            if edit.is_some() {
                                s.editing = edit;
                            }
                        }
                        TreeAddress::Pattern(inst) => {
                            let pat = s.instruments.get(inst)
                                .and_then(|n| n.pattern.as_ref());
                            if let Some(p) = pat {
                                match pa {
                                    0 => {
                                        s.editing = Some(EditState {
                                            input: TextInputState::new(&format!("{:.0}", p.length_beats)),
                                            param_name: "Length (beats)".to_string(),
                                            param_min: 1.0,
                                            param_max: 32.0,
                                        });
                                    }
                                    1 => {} // Enabled is enum — use Left/Right
                                    2 => {} // Loop is enum — use Left/Right
                                    3 => {} // Transpose is enum — use Left/Right
                                    _ => {} // Notes is info
                                }
                            }
                        }
                        _ => {
                            let real_pa = s.real_param_index().unwrap_or(pa);
                            if let Some(param) = s.plugin_at(&addr).and_then(|p| p.params.get(real_pa)) {
                                if matches!(param.kind, ParamKind::Enum(_)) {
                                    // Enum — switch with Left/Right, no numeric entry.
                                } else {
                                    s.editing = Some(EditState {
                                        input: TextInputState::new(&format!("{:.2}", param.value)),
                                        param_name: param.name.clone(),
                                        param_min: param.min,
                                        param_max: param.max,
                                    });
                                }
                            }
                        }
                    }
                }
            } else {
                let sel = s.chain_state.selected;
                if sel < s.tree_entries.len() {
                    s.focus_params = true;
                }
            }
        }
        KeyCode::Esc if s.active_tab == 0 => {
            if s.param_filtering {
                // Cancel filter input, clear filter text.
                s.param_filtering = false;
                s.param_filter_input = TextInputState::new("");
                s.recompute_param_filter();
            } else if s.focus_params && !s.param_filter_input.value.is_empty() {
                // Clear active filter first.
                s.param_filter_input = TextInputState::new("");
                s.recompute_param_filter();
            } else {
                s.focus_params = false;
            }
        }

        // '/' — activate parameter filter (only for plugin nodes, not modulators).
        KeyCode::Char('/') if s.active_tab == 0 && s.focus_params && !s.param_filtering => {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                let is_plugin = matches!(
                    s.tree_entries[sel].address,
                    TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
                );
                if is_plugin {
                    s.param_filtering = true;
                }
            }
        }

        // Parameter adjustment.
        KeyCode::Left if s.active_tab == 0 && s.focus_params && !s.param_filtering => {
            let step = param_step(s, modifiers);
            s.adjust_param(-step);
        }
        KeyCode::Right if s.active_tab == 0 && s.focus_params && !s.param_filtering => {
            let step = param_step(s, modifiers);
            s.adjust_param(step);
        }

        // Reorder effects / reorder instruments.
        KeyCode::Up
            if s.active_tab == 0
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { inst, index } if index > 0 => {
                        let _ = s.cmd_tx.send(GraphCommand::ReorderEffect {
                            inst,
                            from: index,
                            to: index - 1,
                        });
                        if let Some(inst_node) = s.instruments.get_mut(inst) {
                            if index < inst_node.effects.len() {
                                inst_node.effects.swap(index, index - 1);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                        if s.chain_state.selected > 0 {
                            s.chain_state.selected -= 1;
                        }
                    }
                    TreeAddress::Instrument(inst) if inst > 0 => {
                        let _ = s.cmd_tx.send(GraphCommand::SwapInstruments {
                            inst_a: inst,
                            inst_b: inst - 1,
                        });
                        s.instruments.swap(inst, inst - 1);
                        s.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Instrument(inst - 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern(inst) if inst > 0 => {
                        let _ = s.cmd_tx.send(GraphCommand::SwapPatterns {
                            inst_a: inst,
                            inst_b: inst - 1,
                        });
                        let a_pat = s.instruments[inst].pattern.take();
                        let b_pat = s.instruments[inst - 1].pattern.take();
                        s.instruments[inst].pattern = b_pat;
                        s.instruments[inst - 1].pattern = a_pat;
                        s.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern(inst - 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::GroupEffect { group, index } if index > 0 => {
                        let _ = s.cmd_tx.send(GraphCommand::ReorderGroupEffect {
                            group,
                            from: index,
                            to: index - 1,
                        });
                        if let Some(g) = s.groups.get_mut(group) {
                            if index < g.effects.len() {
                                g.effects.swap(index, index - 1);
                            }
                        }
                        s.dirty = true;
                        s.rebuild_tree();
                        if s.chain_state.selected > 0 {
                            s.chain_state.selected -= 1;
                        }
                    }
                    _ => {}
                }
            }
        }
        KeyCode::Down
            if s.active_tab == 0
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { inst, index } => {
                        let effect_count = s.instruments.get(inst)
                            .map_or(0, |n| n.effects.len());
                        if index + 1 < effect_count {
                            let _ = s.cmd_tx.send(GraphCommand::ReorderEffect {
                                inst,
                                from: index,
                                to: index + 1,
                            });
                            if let Some(inst_node) = s.instruments.get_mut(inst) {
                                inst_node.effects.swap(index, index + 1);
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                            s.chain_state.selected += 1;
                        }
                    }
                    TreeAddress::Instrument(inst) if inst + 1 < s.instruments.len() => {
                        let _ = s.cmd_tx.send(GraphCommand::SwapInstruments {
                            inst_a: inst,
                            inst_b: inst + 1,
                        });
                        s.instruments.swap(inst, inst + 1);
                        s.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Instrument(inst + 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern(inst) if inst + 1 < s.instruments.len() => {
                        let _ = s.cmd_tx.send(GraphCommand::SwapPatterns {
                            inst_a: inst,
                            inst_b: inst + 1,
                        });
                        let a_pat = s.instruments[inst].pattern.take();
                        let b_pat = s.instruments[inst + 1].pattern.take();
                        s.instruments[inst].pattern = b_pat;
                        s.instruments[inst + 1].pattern = a_pat;
                        s.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern(inst + 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        let count = s.groups.get(group).map_or(0, |g| g.effects.len());
                        if index + 1 < count {
                            let _ = s.cmd_tx.send(GraphCommand::ReorderGroupEffect {
                                group,
                                from: index,
                                to: index + 1,
                            });
                            if let Some(g) = s.groups.get_mut(group) {
                                g.effects.swap(index, index + 1);
                            }
                            s.dirty = true;
                            s.rebuild_tree();
                            s.chain_state.selected += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        // Navigation.
        KeyCode::Up => match s.active_tab {
            0 if s.focus_params => s.param_state.up(),
            0 => {
                s.chain_state.up();
                s.sync_param_state();
            }
            3 => s.help_offset = s.help_offset.saturating_sub(1),
            _ => {}
        },
        KeyCode::Down => match s.active_tab {
            0 if s.focus_params => s.param_state.down(),
            0 => {
                s.chain_state.down();
                s.sync_param_state();
            }
            3 => s.help_offset += 1,
            _ => {}
        },
        KeyCode::PageUp => match s.active_tab {
            0 if s.focus_params => s.param_state.page_up(20),
            0 => {
                s.chain_state.page_up(20);
                s.sync_param_state();
            }
            3 => s.help_offset = s.help_offset.saturating_sub(20),
            _ => {}
        },
        KeyCode::PageDown => match s.active_tab {
            0 if s.focus_params => s.param_state.page_down(20),
            0 => {
                s.chain_state.page_down(20);
                s.sync_param_state();
            }
            3 => s.help_offset += 20,
            _ => {}
        },
        _ => {}
    }
}

fn handle_mouse(s: &mut State, kind: MouseEventKind, x: u16, y: u16) {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;
            s.param_scrollbar_dragging = false;

            if let Some(tab) = TabBar::tab_at(x, y, s.areas.tab, TAB_NAMES, TAB_SEP) {
                s.active_tab = tab;
                return;
            }

            // Action bar.
            if s.active_tab == 0 {
                let sel = s.chain_state.selected;
                let addr = s.tree_entries.get(sel).map(|e| &e.address);
                let actions = actions_for(addr);
                if let Some(key) = action_bar_hit(x, y, s.areas.action_bar, &actions) {
                    handle_key(s, KeyCode::Char(key), KeyModifiers::NONE);
                    return;
                }
            }

            match s.active_tab {
                0 => {
                    if s.areas.chain_inner.contains((x, y).into()) {
                        if s.chain_state.click_at(y, s.areas.chain_inner) {
                            s.focus_params = false;
                            s.sync_param_state();
                        }
                    } else if s.areas.param_inner.contains((x, y).into()) {
                        s.focus_params = true;
                        // Check scrollbar first.
                        if s.param_state.is_scrollbar_hit(x, s.areas.param_inner) {
                            s.param_state.select_from_scrollbar(y, s.areas.param_inner);
                            s.param_scrollbar_dragging = true;
                        } else {
                            s.param_state.click_at(y, s.areas.param_inner);
                            if s.selected_param_is_enum() {
                                // Enum param: click left half → prev, right half → next.
                                // Enum text starts after cursor(2) + name(25) = 27.
                                let enum_start = s.areas.param_inner.x + 27;
                                let enum_end = s.areas.param_inner.right();
                                if x >= enum_start && x < enum_end {
                                    let mid = enum_start + (enum_end - enum_start) / 2;
                                    if x < mid {
                                        s.adjust_param(-1.0);
                                    } else {
                                        s.adjust_param(1.0);
                                    }
                                }
                            } else if let Some(val) = bar_value_at(x, s.areas.param_inner) {
                                if let Some((min, max)) = s.selected_param_range() {
                                    let mapped = min + val * (max - min);
                                    s.set_param_value(mapped);
                                    s.param_dragging = true;
                                }
                            }
                        }
                    }
                }
                3 => {
                    let total = s.help_lines.len();
                    if ScrollView::is_scrollbar_hit(x, s.areas.content, total) {
                        s.help_offset =
                            ScrollView::offset_from_scrollbar(y, s.areas.content, total);
                        s.scrollbar_dragging = true;
                    }
                }
                _ => {}
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if s.scrollbar_dragging && s.active_tab == 3 {
                let total = s.help_lines.len();
                s.help_offset = ScrollView::offset_from_scrollbar(y, s.areas.content, total);
            } else if s.param_scrollbar_dragging && s.active_tab == 0 {
                s.param_state.select_from_scrollbar(y, s.areas.param_inner);
            } else if s.param_dragging && s.active_tab == 0 {
                if let Some(val) = bar_value_at(x, s.areas.param_inner) {
                    if let Some((min, max)) = s.selected_param_range() {
                        let mapped = min + val * (max - min);
                        s.set_param_value(mapped);
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;
            s.param_scrollbar_dragging = false;
        }
        MouseEventKind::ScrollUp => match s.active_tab {
            0 if s.focus_params => {
                for _ in 0..3 { s.param_state.up_nowrap(); }
            }
            0 => {
                for _ in 0..3 { s.chain_state.up_nowrap(); }
                s.sync_param_state();
            }
            3 => s.help_offset = s.help_offset.saturating_sub(3),
            _ => {}
        },
        MouseEventKind::ScrollDown => match s.active_tab {
            0 if s.focus_params => {
                for _ in 0..3 { s.param_state.down_nowrap(); }
            }
            0 => {
                for _ in 0..3 { s.chain_state.down_nowrap(); }
                s.sync_param_state();
            }
            3 => s.help_offset += 3,
            _ => {}
        },
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    s: &mut State,
) -> io::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        let [tab_area, content_area, action_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);

        s.areas.tab = tab_area;
        s.areas.content = content_area;
        s.areas.action_bar = action_area;

        let session_label = if s.dirty { "(1) Session *" } else { "(1) Session" };
        let tab_names: &[&str] = &[session_label, TAB_NAMES[1], TAB_NAMES[2], TAB_NAMES[3]];
        frame.render_widget(TabBar::new(tab_names, s.active_tab), tab_area);

        // BPM display on the right side of the tab bar.
        let bpm_text = format!("{:.0} BPM", s.global_bpm);
        let bpm_width = bpm_text.len() as u16;
        if tab_area.width > bpm_width + 2 {
            let bpm_area = Rect {
                x: tab_area.right() - bpm_width - 1,
                y: tab_area.y,
                width: bpm_width + 1,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(bpm_text).style(Style::default().fg(DIM)),
                bpm_area,
            );
        }

        match s.active_tab {
            0 => {
                // Pre-compute approximate inner heights and sync scroll offsets
                // so mouse click_at uses the correct offset.
                let inner_h = content_area.height.saturating_sub(2) as usize;
                s.chain_state.ensure_visible(inner_h);
                s.param_state.ensure_visible(inner_h);

                let (ci, pi) = render_session(
                    frame,
                    content_area,
                    &s.tree_entries,
                    &s.chain_state,
                    &s.instruments,
                    &s.groups,
                    &s.param_state,
                    s.focus_params,
                    &s.param_filter_input,
                    s.param_filtering,
                    &s.param_filtered,
                );
                s.areas.chain_inner = ci;
                s.areas.param_inner = pi;

                render_action_bar(frame, action_area, &s.tree_entries, &s.chain_state, s.focus_params);

                if let Some(edit) = &s.editing {
                    render_edit_popup(frame, area, edit);
                }
                if let Some(re) = &s.range_edit {
                    render_range_edit_popup(frame, area, re);
                }
                if let Some(rg) = &s.rename_group {
                    render_rename_group_popup(frame, area, rg);
                }
                if let Some(sel) = &s.selector {
                    render_selector_popup(frame, area, sel, s.catalog_scanning);
                }
                if let Some(ts) = &s.target_selector {
                    render_target_selector_popup(frame, area, ts);
                }
                if let Some(ms) = &s.modulate {
                    render_modulate_popup(frame, area, ms);
                }
                if let Some(gs) = &s.group_assign {
                    render_group_assign_popup(frame, area, gs);
                }
                if let Some(ps) = &s.preset_selector {
                    render_preset_selector_popup(frame, area, ps);
                }
                if let Some(edit) = &s.bpm_editing {
                    render_edit_popup(frame, area, edit);
                }
                if let Some(ge) = &s.gain_editing {
                    render_edit_popup(frame, area, &ge.edit);
                }
                if let Some(sa) = &s.save_as {
                    render_save_as_popup(frame, area, sa);
                }
            }
            1 => {
                render_piano_tab(frame, content_area, s);
                if let Some(ss) = &s.scale_selector {
                    render_scale_selector_popup(frame, area, ss);
                }
            }
            2 => {
                frame.render_widget(
                    Paragraph::new("Oscilloscope — not yet implemented")
                        .style(Style::default().fg(DIM)),
                    content_area,
                );
            }
            3 => render_help(frame, content_area, &s.help_lines, s.help_offset),
            _ => {}
        }
    })?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render_session(
    frame: &mut ratatui::Frame,
    area: Rect,
    tree_entries: &[TreeEntry],
    chain_state: &ListState,
    instruments: &[InstrumentNode],
    groups: &[GroupNode],
    param_state: &ListState,
    focus_params: bool,
    param_filter_input: &TextInputState,
    param_filtering: bool,
    param_filtered: &[usize],
) -> (Rect, Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Fill(1)]).areas(area);

    // Chain pane.
    let left_style = if !focus_params {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(DIM)
    };
    let left_block = Block::default()
        .borders(Borders::ALL)
        .border_style(left_style)
        .title(" Chain ");
    let left_inner = left_block.inner(left);
    frame.render_widget(left_block, left);

    let items: Vec<ListItem> = tree_entries
        .iter()
        .map(|e| ListItem::raw(&e.label))
        .collect();
    let mut cs = chain_state.clone();
    cs.ensure_visible(left_inner.height as usize);
    frame.render_widget(
        List::new(&items, &cs)
            .cursor("", 0)
            .style(Style::default().fg(DIM))
            .selected_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        left_inner,
    );

    // Param pane — find the selected plugin or modulator.
    let selected = chain_state.selected;
    let mut mod_params: Vec<ParamSlot> = Vec::new(); // temp storage for modulator pseudo-params
    let (plugin_name, plugin_params) = if selected < tree_entries.len() {
        let addr = &tree_entries[selected].address;
        match addr {
            TreeAddress::Modulator { .. } | TreeAddress::GroupModulator { .. } => {
                let m = match addr {
                    TreeAddress::Modulator { inst, index } => {
                        instruments.get(*inst).and_then(|n| n.modulators.get(*index))
                    }
                    TreeAddress::GroupModulator { group, index } => {
                        groups.get(*group).and_then(|g| g.modulators.get(*index))
                    }
                    _ => None,
                };
                match m {
                    Some(m) => {
                        use crate::plugin::chain::LfoWaveform;
                        // Type enum (index 0) — always present.
                        let type_names = vec!["LFO".to_string(), "Envelope".to_string()];
                        let (name, type_idx) = match &m.source {
                            ModSourceSlot::Lfo { waveform, rate } => {
                                let name = format!("LFO {:.1}Hz {}", rate, waveform.name());
                                mod_params.push(ParamSlot {
                                    name: "Type".to_string(),
                                    index: 0,
                                    min: 0.0,
                                    max: 1.0,
                                    default: 0.0,
                                    value: 0.0,
                                    kind: ParamKind::Enum(type_names),
                                });
                                mod_params.push(ParamSlot {
                                    name: "Waveform".to_string(),
                                    index: 1,
                                    min: 0.0,
                                    max: (LfoWaveform::ALL.len() - 1) as f32,
                                    default: 0.0,
                                    value: waveform.to_index() as f32,
                                    kind: ParamKind::Enum(
                                        LfoWaveform::ALL.iter().map(|w| w.name().to_string()).collect(),
                                    ),
                                });
                                mod_params.push(ParamSlot {
                                    name: "Rate (Hz)".to_string(),
                                    index: 2,
                                    min: 0.01,
                                    max: 50.0,
                                    default: 1.0,
                                    value: *rate,
                                    kind: ParamKind::Float,
                                });
                                (name, 0)
                            }
                            ModSourceSlot::Envelope { attack, decay, sustain, release } => {
                                let name = "ADSR".to_string();
                                mod_params.push(ParamSlot {
                                    name: "Type".to_string(),
                                    index: 0,
                                    min: 0.0,
                                    max: 1.0,
                                    default: 0.0,
                                    value: 1.0,
                                    kind: ParamKind::Enum(type_names),
                                });
                                mod_params.push(ParamSlot {
                                    name: "Attack (s)".to_string(),
                                    index: 1,
                                    min: 0.001,
                                    max: 10.0,
                                    default: 0.01,
                                    value: *attack,
                                    kind: ParamKind::Float,
                                });
                                mod_params.push(ParamSlot {
                                    name: "Decay (s)".to_string(),
                                    index: 2,
                                    min: 0.001,
                                    max: 10.0,
                                    default: 0.3,
                                    value: *decay,
                                    kind: ParamKind::Float,
                                });
                                mod_params.push(ParamSlot {
                                    name: "Sustain".to_string(),
                                    index: 3,
                                    min: 0.0,
                                    max: 1.0,
                                    default: 0.7,
                                    value: *sustain,
                                    kind: ParamKind::Float,
                                });
                                mod_params.push(ParamSlot {
                                    name: "Release (s)".to_string(),
                                    index: 4,
                                    min: 0.001,
                                    max: 10.0,
                                    default: 0.5,
                                    value: *release,
                                    kind: ParamKind::Float,
                                });
                                (name, 1)
                            }
                        };
                        let _ = type_idx;
                        // Separator before target depths.
                        let depth_offset = match &m.source {
                            ModSourceSlot::Lfo { .. } => 4,  // 3 source params + 1 separator
                            ModSourceSlot::Envelope { .. } => 6,  // 5 source params + 1 separator
                        };
                        mod_params.push(ParamSlot {
                            name: "Targets".to_string(),
                            index: 0,
                            min: 0.0,
                            max: 0.0,
                            default: 0.0,
                            value: 0.0,
                            kind: ParamKind::Separator,
                        });
                        for (i, t) in m.targets.iter().enumerate() {
                            mod_params.push(ParamSlot {
                                name: format!("{} depth", t.param_name),
                                index: (i + depth_offset) as u32,
                                min: 0.0,
                                max: 1.0,
                                default: 0.5,
                                value: t.depth,
                                kind: ParamKind::Float,
                            });
                        }
                        (name, mod_params.as_slice())
                    }
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
            TreeAddress::Pattern(inst) => {
                let pat = instruments.get(*inst)
                    .and_then(|n| n.pattern.as_ref());
                match pat {
                    Some(p) => {
                        mod_params.push(ParamSlot {
                            name: "Length (beats)".to_string(),
                            index: 0,
                            min: 1.0,
                            max: 32.0,
                            default: 4.0,
                            value: p.length_beats,
                            kind: ParamKind::Float,
                        });
                        mod_params.push(ParamSlot {
                            name: "Enabled".to_string(),
                            index: 1,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.enabled { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                        });
                        mod_params.push(ParamSlot {
                            name: "Loop".to_string(),
                            index: 2,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.looping { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                        });
                        mod_params.push(ParamSlot {
                            name: "Transpose".to_string(),
                            index: 3,
                            min: 0.0,
                            max: 1.0,
                            default: 0.0,
                            value: if p.in_key { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Chromatic".to_string(), "In Key".to_string()]),
                        });
                        if !p.events.is_empty() {
                            let notes = p.events.iter().filter(|e| e.1 == 0x90).count();
                            mod_params.push(ParamSlot {
                                name: "Notes".to_string(),
                                index: 4,
                                min: 0.0,
                                max: 0.0,
                                default: 0.0,
                                value: notes as f32,
                                kind: ParamKind::Separator,
                            });
                        }
                        ("Pattern".to_string(), mod_params.as_slice())
                    }
                    None => ("Pattern".to_string(), &[] as &[ParamSlot]),
                }
            }
            _ => {
                // Find the PluginSlot for this address.
                let slot = match addr {
                    TreeAddress::Instrument(inst) => {
                        instruments.get(*inst)
                            .and_then(|n| n.instrument.as_ref())
                    }
                    TreeAddress::Effect { inst, index } => {
                        instruments.get(*inst)
                            .and_then(|n| n.effects.get(*index))
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        groups.get(*group).and_then(|g| g.effects.get(*index))
                    }
                    _ => None,
                };
                match slot {
                    Some(p) => (p.name.clone(), p.params.as_slice()),
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
        }
    } else {
        ("(none)".to_string(), &[] as &[ParamSlot])
    };

    let right_style = if focus_params {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(DIM)
    };
    let right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(right_style)
        .title(format!(" {} ", plugin_name));
    let right_inner = right_block.inner(right);
    frame.render_widget(right_block, right);

    // Determine if the filter bar should be shown.
    let is_plugin_node = selected < tree_entries.len()
        && matches!(
            tree_entries[selected].address,
            TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
        );
    let show_filter = is_plugin_node
        && (param_filtering || !param_filter_input.value.is_empty());

    // Split right_inner into filter bar + list area when filter is active.
    let (filter_area, list_area) = if show_filter && right_inner.height > 1 {
        let [fa, la] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
        ]).areas(right_inner);
        (Some(fa), la)
    } else {
        (None, right_inner)
    };

    // Render filter bar.
    if let Some(fa) = filter_area {
        let prompt = "/ ";
        let pw = prompt.len() as u16;
        frame.render_widget(
            Paragraph::new(prompt).style(Style::default().fg(Color::Yellow)),
            Rect::new(fa.x, fa.y, pw, 1),
        );
        frame.render_widget(
            TextInput::new(param_filter_input),
            Rect::new(fa.x + pw, fa.y, fa.width.saturating_sub(pw), 1),
        );
    }

    // Build the display params — apply filter for plugin nodes.
    let display_params: Vec<&ParamSlot> = if is_plugin_node && !param_filtered.is_empty() {
        param_filtered.iter().filter_map(|&i| plugin_params.get(i)).collect()
    } else if is_plugin_node && param_filtered.is_empty() && !param_filter_input.value.is_empty() {
        // Filter active but no matches.
        vec![]
    } else {
        plugin_params.iter().collect()
    };

    let name_width = 24;
    let bar_width = list_area.width.saturating_sub(name_width as u16 + 12) as usize;

    #[derive(PartialEq)]
    enum ParamRow { Normal, Enum, Separator }
    // (name, col1, col2, col3, row_kind)
    let param_strings: Vec<(String, String, String, String, ParamRow)> = display_params
        .iter()
        .map(|p| {
            let name_str = format!("{:<width$} ", truncate(&p.name, name_width), width = name_width);
            match &p.kind {
                ParamKind::Separator => {
                    (name_str, "──────".to_string(), String::new(), String::new(), ParamRow::Separator)
                }
                ParamKind::Enum(options) => {
                    let idx = p.value.round() as usize;
                    let label = options.get(idx).map_or("?", |s| s.as_str());
                    (name_str, format!("◂ {} ▸", label), String::new(), String::new(), ParamRow::Enum)
                }
                ParamKind::Float => {
                    let normalized = if (p.max - p.min).abs() > f32::EPSILON {
                        (p.value - p.min) / (p.max - p.min)
                    } else {
                        0.0
                    };
                    let filled = (normalized * bar_width as f32).round() as usize;
                    let empty = bar_width.saturating_sub(filled);
                    (
                        name_str,
                        "▓".repeat(filled),
                        "░".repeat(empty),
                        format!(" {:>8.2}", p.value),
                        ParamRow::Normal,
                    )
                }
            }
        })
        .collect();
    let sep_style = Style::default().fg(DIM);
    let param_items: Vec<ListItem> = param_strings
        .iter()
        .map(|(name, col1, col2, col3, row_kind)| match row_kind {
            ParamRow::Separator => ListItem::spans(vec![
                ListSpan::new(name, sep_style),
                ListSpan::new(col1, sep_style),
            ]),
            ParamRow::Enum => ListItem::spans(vec![
                ListSpan::new(name, Style::default()),
                ListSpan::new(col1, Style::default()),
            ]),
            ParamRow::Normal => ListItem::spans(vec![
                ListSpan::new(name, Style::default()),
                ListSpan::new(col1, Style::default()),
                ListSpan::new(col2, Style::default()),
                ListSpan::new(col3, Style::default()),
            ]),
        })
        .collect();

    let mut ps = param_state.clone();
    ps.ensure_visible(list_area.height as usize);
    let param_list = if focus_params {
        List::new(&param_items, &ps)
            .style(Style::default().fg(DIM))
            .selected_style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD))
    } else {
        List::new(&param_items, &ps)
            .style(Style::default().fg(DIM))
            .selected_style(Style::default().fg(Color::White))
            .cursor("  ", 2)
    };
    frame.render_widget(param_list, list_area);

    (left_inner, right_inner)
}

fn render_action_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    tree_entries: &[TreeEntry],
    chain_state: &ListState,
    focus_params: bool,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let sel = chain_state.selected;
    let addr = tree_entries.get(sel).map(|e| &e.address);
    let actions = actions_for(addr);

    let key_style = Style::default().fg(Color::Black).bg(DIM).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(DIM);
    let active_key_style = Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD);
    let active_label_style = Style::default().fg(Color::White);

    let y = area.y;
    let mut x = area.x;

    for &(key, desc) in &actions {
        let (ks, ls) = if focus_params {
            (key_style, label_style)
        } else {
            (active_key_style, active_label_style)
        };
        if x > area.x {
            x += 1;
        }
        for ch in format!(" {key} ").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(ks); }
            x += 1;
        }
        for ch in format!(" {desc}").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(ls); }
            x += 1;
        }
    }
}

fn render_edit_popup(frame: &mut ratatui::Frame, area: Rect, edit: &EditState) {
    let popup = centered_rect(34, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" {} ", edit.param_name));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        let hint = format!("Range: {:.2} — {:.2}", edit.param_min, edit.param_max);
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let label = "Value: ";
        let lw = label.len() as u16;
        frame.render_widget(
            Paragraph::new(label).style(Style::default().fg(Color::White)),
            Rect::new(inner.x, inner.y + 1, lw, 1),
        );
        frame.render_widget(
            TextInput::new(&edit.input),
            Rect::new(inner.x + lw, inner.y + 1, inner.width.saturating_sub(lw), 1),
        );
    }
}

fn render_range_edit_popup(frame: &mut ratatui::Frame, area: Rect, re: &RangeEditState) {
    let popup = centered_rect(34, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Set Range ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new("C0-B3 / C4- / -B3 / empty=all")
                .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        frame.render_widget(
            TextInput::new(&re.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_rename_group_popup(frame: &mut ratatui::Frame, area: Rect, rg: &RenameGroupState) {
    let popup = centered_rect(40, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Rename Group ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        frame.render_widget(
            Paragraph::new("Group name (empty = default)")
                .style(Style::default().fg(DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        frame.render_widget(
            TextInput::new(&rg.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_save_as_popup(frame: &mut ratatui::Frame, area: Rect, sa: &SaveAsState) {
    let popup = centered_rect(44, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Save As ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        // The hint line doubles as the error line after a failed save.
        let hint = match &sa.error {
            Some(e) => Paragraph::new(e.as_str()).style(Style::default().fg(Color::Red)),
            None => Paragraph::new("Filename (saved in session dir; .toml added)")
                .style(Style::default().fg(DIM)),
        };
        frame.render_widget(hint, Rect::new(inner.x, inner.y, inner.width, 1));
        frame.render_widget(
            TextInput::new(&sa.input),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
    }
}

fn render_selector_popup(
    frame: &mut ratatui::Frame,
    area: Rect,
    sel: &SelectorState,
    scanning: bool,
) {
    let title = match (sel.mode, scanning) {
        (SelectorMode::Instrument, false) => " Select Instrument ",
        (SelectorMode::Instrument, true) => " Select Instrument (scanning…) ",
        (SelectorMode::Effect, false) => " Select Effect ",
        (SelectorMode::Effect, true) => " Select Effect (scanning…) ",
        (SelectorMode::GroupEffect(_), false) => " Select Group Effect ",
        (SelectorMode::GroupEffect(_), true) => " Select Group Effect (scanning…) ",
    };
    let w = (area.width * 70 / 100).max(40).min(area.width);
    let h = (area.height * 60 / 100).max(10).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[
        ("Name", inner.width.saturating_sub(22)),
        ("Format", 8),
        ("Params", 7),
        ("Presets", 7),
    ];
    frame.render_widget(FilterList::new(&sel.filter, &sel.items, columns), inner);
}

fn render_group_assign_popup(frame: &mut ratatui::Frame, area: Rect, gs: &GroupAssignState) {
    let w = (area.width * 45 / 100).max(30).min(area.width);
    let h = (area.height * 40 / 100).max(7).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Assign to group ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Group", inner.width)];
    frame.render_widget(FilterList::new(&gs.filter, &gs.items, columns), inner);
}

fn render_modulate_popup(frame: &mut ratatui::Frame, area: Rect, ms: &ModulateState) {
    let w = (area.width * 50 / 100).max(36).min(area.width);
    let h = (area.height * 45 / 100).max(8).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(format!(" Modulate {} ", ms.param_name));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Modulator", inner.width)];
    frame.render_widget(FilterList::new(&ms.filter, &ms.items, columns), inner);
}

fn render_target_selector_popup(frame: &mut ratatui::Frame, area: Rect, ts: &TargetSelectorState) {
    let w = (area.width * 60 / 100).max(36).min(area.width);
    let h = (area.height * 50 / 100).max(10).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" Select Target Parameter ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[
        ("Plugin", inner.width.saturating_sub(20).min(20)),
        ("Parameter", inner.width.saturating_sub(20)),
    ];
    frame.render_widget(FilterList::new(&ts.filter, &ts.items, columns), inner);
}

fn render_preset_selector_popup(
    frame: &mut ratatui::Frame,
    area: Rect,
    ps: &PresetSelectorState,
) {
    let w = (area.width * 50 / 100).max(32).min(area.width);
    let h = (area.height * 50 / 100).max(10).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select Preset ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Name", inner.width)];
    frame.render_widget(FilterList::new(&ps.filter, &ps.items, columns), inner);
}

fn render_scale_selector_popup(frame: &mut ratatui::Frame, area: Rect, ss: &ScaleSelectorState) {
    let w = (area.width * 50 / 100).max(36).min(area.width);
    let h = (area.height * 60 / 100).max(12).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select Scale ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Scale", inner.width)];
    frame.render_widget(FilterList::new(&ss.filter, &ss.items, columns), inner);
}

fn render_piano_tab(frame: &mut ratatui::Frame, area: Rect, s: &State) {
    // Vertical split:
    //  - 2 row status (scale+mode on top, held notes + chord on second)
    //  - main keyboard area
    //  - 1 row "scale strip" listing the scale's notes
    //  - 1 row hint line at bottom
    let [status_area, kb_area, strip_area, hint_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [config_row, live_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(status_area);

    // ---- Row 1: scale + mode ----
    let scale = s.piano_filter.scale();
    let mode = s.piano_filter.mode();
    let config_text = format!(
        " Scale: {}   Mode: {}   Octave view: C{}",
        scale.display(),
        mode.label(),
        s.piano_view_octave,
    );
    frame.render_widget(
        Paragraph::new(config_text).style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        config_row,
    );

    // ---- Row 2: held notes + chord detection ----
    let held_notes = s.held.held();
    let held_names: Vec<String> = held_notes
        .iter()
        .map(|&n| {
            let nm = crate::note_name(n);
            match scale.degree(n) {
                Some(d) => format!("{nm}({d})"),
                None => nm,
            }
        })
        .collect();
    let held_str = if held_names.is_empty() {
        String::from("—")
    } else {
        held_names.join(" ")
    };

    // Chord: top match if 3+ notes, interval if exactly 2, blank for 0–1.
    let chord_text = if held_notes.len() >= 2 {
        let matches = crate::chord::detect(&held_notes, &scale);
        if matches.is_empty() {
            if let Some(iv) = crate::chord::two_note_interval(&held_notes) {
                iv
            } else {
                String::from("—")
            }
        } else {
            let mut parts: Vec<String> = Vec::new();
            for (i, m) in matches.iter().take(2).enumerate() {
                let label = match &m.roman {
                    Some(r) if i == 0 => format!("{} ({})", m.display(), r),
                    _ => m.display(),
                };
                parts.push(label);
            }
            parts.join("  ·  ")
        }
    } else {
        String::from("—")
    };

    let live_line = ratatui::text::Line::from(vec![
        ratatui::text::Span::styled(
            " Held: ",
            Style::default().fg(DIM),
        ),
        ratatui::text::Span::styled(
            held_str,
            Style::default().fg(Color::White),
        ),
        ratatui::text::Span::styled(
            "   Chord: ",
            Style::default().fg(DIM),
        ),
        ratatui::text::Span::styled(
            chord_text,
            Style::default()
                .fg(Color::Rgb(240, 185, 70))
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(live_line), live_row);

    // ---- Keyboard ----
    frame.render_widget(Clear, kb_area);
    let buf = frame.buffer_mut();
    piano_view::render_keyboard(kb_area, buf, &s.held, &scale, s.piano_view_octave);

    // ---- Scale strip: degree → note pairs ----
    use crate::scale::{SCALES, SCALE_DEGREES};
    let scale_intervals = SCALES[scale.scale_idx].intervals;
    let mut strip_parts: Vec<(String, Style)> = Vec::new();
    for (i, &iv) in scale_intervals.iter().enumerate() {
        let pc = (scale.root as u16 + iv as u16) % 12;
        let name = NOTE_NAMES[pc as usize];
        let degree = SCALE_DEGREES[iv as usize];
        let is_root = iv == 0;
        let key_style = if is_root {
            Style::default()
                .fg(Color::Rgb(240, 185, 70))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(120, 200, 215))
        };
        let degree_style = Style::default().fg(Color::Rgb(120, 120, 120));
        if i > 0 {
            strip_parts.push(("   ".into(), Style::default()));
        }
        strip_parts.push((format!("{degree}:"), degree_style));
        strip_parts.push((name.to_string(), key_style));
    }
    let line: ratatui::text::Line = ratatui::text::Line::from(
        strip_parts
            .into_iter()
            .map(|(t, st)| ratatui::text::Span::styled(t, st))
            .collect::<Vec<_>>(),
    );
    frame.render_widget(
        Paragraph::new(line).alignment(ratatui::layout::Alignment::Center),
        strip_area,
    );

    // ---- Hint line ----
    let hint = " [k] scale  [l] mode  [[/]] octave  [Esc] back ";
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(DIM)),
        hint_area,
    );
}

fn render_help(frame: &mut ratatui::Frame, area: Rect, lines: &[String], offset: usize) {
    let scroll_lines: Vec<ScrollLine> = lines
        .iter()
        .map(|l| {
            if l.starts_with("  ") {
                ScrollLine::raw(l)
            } else if l.starts_with("---") {
                ScrollLine::styled(l, Style::default().fg(DIM))
            } else {
                ScrollLine::styled(
                    l,
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )
            }
        })
        .collect();
    let clamped = ScrollView::clamp_offset(offset, scroll_lines.len(), area.height as usize);
    frame.render_widget(ScrollView::new(&scroll_lines, clamped), area);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// After removing a modulator at `removed_index` from the TUI model,
/// clean up cross-mod targets in siblings: remove targets pointing at the
/// removed index, and decrement indices > removed_index.
fn fixup_tui_cross_mod_after_remove(modulators: &mut [ModulatorSlot], removed_index: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| {
            let idx = match &t.kind {
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
            };
            idx != Some(removed_index)
        });
        for t in &mut m.targets {
            let idx = match &mut t.kind {
                ModTargetKind::PluginParam { .. }
                | ModTargetKind::GroupMember { .. }
                | ModTargetKind::GroupBus { .. }
                | ModTargetKind::GroupMemberMod { .. } => continue,
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
    }
}

/// Mirror of the audio-thread `fixup_group_bus_after_remove` for the TUI model:
/// after a group bus effect at `removed` is deleted, drop GroupBus targets on
/// it and shift higher bus indices down.
fn fixup_tui_group_bus_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
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

/// TUI-model mirror of `fixup_group_member_after_remove`: after the member at
/// ordinal `removed` leaves a group, drop GroupMember targets on it and shift
/// higher ordinals down.
fn fixup_tui_group_member_after_remove(modulators: &mut [ModulatorSlot], removed: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        m.targets.retain(|t| match &t.kind {
            ModTargetKind::GroupMember { member, .. }
            | ModTargetKind::GroupMemberMod { member, .. } => *member != removed,
            _ => true,
        });
        for t in &mut m.targets {
            let member = match &mut t.kind {
                ModTargetKind::GroupMember { member, .. }
                | ModTargetKind::GroupMemberMod { member, .. } => member,
                _ => continue,
            };
            if *member > removed {
                *member -= 1;
            }
        }
    }
}

/// TUI-model mirror of `shift_group_member_after_insert`: after a member joins
/// a group at ordinal `inserted`, bump group-target member ordinals at/after it.
fn shift_tui_group_member_after_insert(modulators: &mut [ModulatorSlot], inserted: usize) {
    use crate::plugin::chain::ModTargetKind;
    for m in modulators.iter_mut() {
        for t in &mut m.targets {
            let member = match &mut t.kind {
                ModTargetKind::GroupMember { member, .. }
                | ModTargetKind::GroupMemberMod { member, .. } => member,
                _ => continue,
            };
            if *member >= inserted {
                *member += 1;
            }
        }
    }
}

/// TUI-model mirror of `fixup_group_member_mod_after_remove`: after member
/// `member`'s lane modulator at `removed` is deleted, drop GroupMemberMod
/// targets on it and shift that member's higher mod-indices down.
fn fixup_tui_group_member_mod_after_remove(
    modulators: &mut [ModulatorSlot],
    member: usize,
    removed: usize,
) {
    use crate::plugin::chain::ModTargetKind;
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

/// The (group, member-ordinal) of TUI instrument `inst`, or None if ungrouped.
fn tui_lane_member_ordinal(instruments: &[InstrumentNode], inst: usize) -> Option<(usize, usize)> {
    let group = instruments.get(inst)?.group?;
    let ordinal = instruments[..inst]
        .iter()
        .filter(|n| n.group == Some(group))
        .count();
    Some((group, ordinal))
}

/// Convert a TUI ModSourceSlot to an audio-thread ModSource for GraphCommands.
fn mod_source_slot_to_graph(slot: &ModSourceSlot) -> crate::plugin::chain::ModSource {
    match slot {
        ModSourceSlot::Lfo { waveform, rate } => crate::plugin::chain::ModSource::Lfo {
            waveform: *waveform,
            rate: *rate,
            phase: 0.0,
        },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => crate::plugin::chain::ModSource::Envelope {
            attack: *attack,
            decay: *decay,
            sustain: *sustain,
            release: *release,
            state: crate::plugin::chain::EnvState::Idle,
            level: 0.0,
            notes_held: 0,
        },
    }
}

fn to_plugin_slot(lp: LoadedPlugin) -> PluginSlot {
    let params = lp
        .params
        .into_iter()
        .zip(lp.param_defaults)
        .zip(lp.param_values)
        .filter(|((p, _), _)| !p.name.starts_with("(locked)"))
        .map(|((p, baseline), v)| ParamSlot {
            kind: p.labels.map_or(ParamKind::Float, ParamKind::Enum),
            name: p.name,
            index: p.index,
            min: p.min,
            max: p.max,
            default: baseline,
            value: v,
        })
        .collect();
    PluginSlot {
        name: lp.name,
        format: format_from_id(&lp.id),
        id: lp.id,
        is_instrument: lp.is_instrument,
        params,
        presets: lp.presets,
        current_preset: lp.current_preset,
        mix: lp.mix,
    }
}

/// Number of param-pane rows for a modulator: source params + "Targets"
/// separator + one row per target. Scope-independent (lane or group).
fn modulator_param_len(m: &ModulatorSlot) -> usize {
    let fixed = match &m.source {
        ModSourceSlot::Lfo { .. } => 3,     // Type + Waveform + Rate
        ModSourceSlot::Envelope { .. } => 5, // Type + A + D + S + R
    };
    fixed + 1 + m.targets.len()
}

/// (min, max) for a modulator pseudo-param at row `pa`, or None for enum /
/// separator rows. Scope-independent.
fn modulator_param_range(m: &ModulatorSlot, pa: usize) -> Option<(f32, f32)> {
    if pa == 0 {
        return None; // Type enum
    }
    match &m.source {
        ModSourceSlot::Lfo { .. } => match pa {
            1 => None, // Waveform enum
            2 => Some((0.01, 50.0)),
            3 => None, // Separator
            _ => m.targets.get(pa - 4).map(|_| (0.0f32, 1.0f32)),
        },
        ModSourceSlot::Envelope { .. } => match pa {
            1 => Some((0.001, 10.0)),
            2 => Some((0.001, 10.0)),
            3 => Some((0.0, 1.0)),
            4 => Some((0.001, 10.0)),
            5 => None, // Separator
            _ => m.targets.get(pa - 6).map(|_| (0.0f32, 1.0f32)),
        },
    }
}

/// True if a modulator pseudo-param at row `pa` is an enum (Type / Waveform).
fn modulator_param_is_enum(m: &ModulatorSlot, pa: usize) -> bool {
    match &m.source {
        ModSourceSlot::Lfo { .. } => pa == 0 || pa == 1, // Type, Waveform
        ModSourceSlot::Envelope { .. } => pa == 0,       // Type
    }
}

/// Build the value-entry `EditState` for a modulator pseudo-param at row `pa`,
/// or None for enum/separator rows. Scope-independent (lane or group).
fn modulator_edit_state(m: &ModulatorSlot, pa: usize) -> Option<EditState> {
    if pa == 0 {
        return None; // Type enum
    }
    match &m.source {
        ModSourceSlot::Lfo { rate, .. } => match pa {
            1 => None, // Waveform enum
            2 => Some(EditState {
                input: TextInputState::new(&format!("{:.2}", rate)),
                param_name: "Rate (Hz)".to_string(),
                param_min: 0.01,
                param_max: 50.0,
            }),
            3 => None, // Separator
            _ => m.targets.get(pa - 4).map(|t| EditState {
                input: TextInputState::new(&format!("{:.2}", t.depth)),
                param_name: format!("{} depth", t.param_name),
                param_min: 0.0,
                param_max: 1.0,
            }),
        },
        ModSourceSlot::Envelope { attack, decay, sustain, release } => {
            let edit = match pa {
                1 => Some((*attack, "Attack (s)".to_string(), 0.001f32, 10.0f32)),
                2 => Some((*decay, "Decay (s)".to_string(), 0.001, 10.0)),
                3 => Some((*sustain, "Sustain".to_string(), 0.0, 1.0)),
                4 => Some((*release, "Release (s)".to_string(), 0.001, 10.0)),
                5 => None, // Separator
                _ => m.targets.get(pa - 6).map(|t| {
                    (t.depth, format!("{} depth", t.param_name), 0.0f32, 1.0f32)
                }),
            };
            edit.map(|(val, pname, min, max)| EditState {
                input: TextInputState::new(&format!("{:.3}", val)),
                param_name: pname,
                param_min: min,
                param_max: max,
            })
        }
    }
}

fn to_modulator_slot(lm: LoadedModulator) -> ModulatorSlot {
    let source = match lm.source {
        LoadedModSource::Lfo { waveform, rate } => ModSourceSlot::Lfo { waveform, rate },
        LoadedModSource::Envelope { attack, decay, sustain, release } => {
            ModSourceSlot::Envelope { attack, decay, sustain, release }
        }
    };
    ModulatorSlot {
        source,
        targets: lm
            .targets
            .into_iter()
            .map(|lt| ModTargetSlot {
                slot: lt.slot,
                param_name: lt.param_name,
                kind: lt.kind,
                depth: lt.depth,
                param_min: lt.param_min,
                param_max: lt.param_max,
            })
            .collect(),
    }
}

fn param_step(s: &State, modifiers: KeyModifiers) -> f32 {
    // Enum params step one value at a time, regardless of modifiers.
    if s.selected_param_is_enum() {
        return 1.0;
    }
    let pa = s.real_param_index().unwrap_or(s.param_state.selected);
    let sel = s.chain_state.selected;
    let range = if sel < s.tree_entries.len() {
        let addr = &s.tree_entries[sel].address;
        s.plugin_at(addr)
            .and_then(|p| p.params.get(pa))
            .map(|p| p.max - p.min)
            .unwrap_or(1.0)
    } else {
        1.0
    };

    if modifiers.contains(KeyModifiers::CONTROL) {
        range * 0.10
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        range * 0.01
    } else {
        range * 0.05
    }
}

fn bar_value_at(x: u16, param_inner: Rect) -> Option<f32> {
    // cursor(2) + name(24) + space(1) = 27
    let bar_start = param_inner.x + 27;
    let bar_width = param_inner.width.saturating_sub(24 + 12);
    if bar_width == 0 || x < bar_start || x >= bar_start + bar_width {
        return None;
    }
    Some(((x - bar_start) as f32 / (bar_width - 1).max(1) as f32).clamp(0.0, 1.0))
}

/// Format a note range as "C4-B5" style string (open-ended: "-B3", "C4-").
fn format_range(range: (u8, u8)) -> String {
    crate::session::format_range(range)
}

/// Render one instrument and its children (pattern, effects, lane mod rack).
/// `prefix` is prepended to every label so the whole sub-tree can be nested
/// under a group header.
fn push_instrument_entries(
    entries: &mut Vec<TreeEntry>,
    inst: &InstrumentNode,
    inst_idx: usize,
    prefix: &str,
) {
    let plugin_label = inst.instrument.as_ref()
        .map(|p| format!("\u{266a} {}  [{}]", p.name, p.format))
        .unwrap_or_else(|| "\u{266a} (empty)".to_string());
    let range_label = inst.range
        .map(|r| format!("  {}", format_range(r)))
        .unwrap_or_default();
    let transpose_label = if inst.transpose != 0 {
        let sign = if inst.transpose > 0 { "+" } else { "" };
        format!("  {sign}{}", inst.transpose)
    } else {
        String::new()
    };
    entries.push(TreeEntry {
        label: format!("{prefix}{plugin_label}{range_label}{transpose_label}"),
        address: TreeAddress::Instrument(inst_idx),
        color: Color::Green,
        indent: 0,
    });

    let has_pattern = inst.pattern.as_ref().is_some_and(|p| p.recording || !p.events.is_empty());
    let child_count = if has_pattern { 1 } else { 0 } + inst.effects.len() + inst.modulators.len();
    let mut child_idx = 0;
    let branch_for = |idx: usize| if idx == child_count - 1 { "╰" } else { "├" };

    if let Some(pat) = &inst.pattern {
        if pat.recording || !pat.events.is_empty() {
            let (icon, color, detail) = if pat.recording {
                ("\u{23fa}", Color::Red, "recording...".to_string())
            } else {
                let n = pat.events.iter().filter(|e| e.1 == 0x90).count();
                let mode = if pat.in_key { ", in-key" } else { "" };
                ("\u{25b6}", Color::Blue, format!("{:.0} beats, {n} notes{mode}", pat.length_beats))
            };
            entries.push(TreeEntry {
                label: format!("{prefix}{} {icon} Pattern  {detail}", branch_for(child_idx)),
                address: TreeAddress::Pattern(inst_idx),
                color,
                indent: 1,
            });
            child_idx += 1;
        }
    }

    for (fx_idx, fx) in inst.effects.iter().enumerate() {
        entries.push(TreeEntry {
            label: format!("{prefix}{} fx {}  [{}]", branch_for(child_idx), fx.name, fx.format),
            address: TreeAddress::Effect { inst: inst_idx, index: fx_idx },
            color: Color::Yellow,
            indent: 1,
        });
        child_idx += 1;
    }

    let slot_name = |slot: usize| -> String {
        if slot == 0 {
            inst.instrument.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Instrument".into())
        } else {
            inst.effects.get(slot - 1).map(|p| p.name.clone()).unwrap_or_else(|| format!("fx{slot}"))
        }
    };
    let target_disp = |t: &ModTargetSlot| -> String {
        match t.kind {
            crate::plugin::chain::ModTargetKind::PluginParam { slot, .. } => {
                format!("{}: {}", slot_name(slot), t.param_name)
            }
            _ => t.param_name.clone(),
        }
    };
    for (mod_idx, m) in inst.modulators.iter().enumerate() {
        let source_label = match &m.source {
            ModSourceSlot::Lfo { waveform, rate } => format!("LFO {:.1}Hz {}", rate, waveform.name()),
            ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
        };
        let targets = if m.targets.is_empty() {
            String::new()
        } else {
            let list = m
                .targets
                .iter()
                .map(|t| format!("{} ({:.0}%)", target_disp(t), t.depth * 100.0))
                .collect::<Vec<_>>()
                .join(", ");
            format!(" \u{2192} {list}")
        };
        entries.push(TreeEntry {
            label: format!("{prefix}{} ~ {source_label}{targets}", branch_for(child_idx)),
            address: TreeAddress::Modulator { inst: inst_idx, index: mod_idx },
            color: Color::Magenta,
            indent: 1,
        });
        child_idx += 1;
    }
}

fn build_tree_entries(instruments: &[InstrumentNode], groups: &[GroupNode]) -> Vec<TreeEntry> {
    let mut entries = Vec::new();

    // Groups first: a header, the member instruments nested under it, then the
    // group's bus effects.
    for (g_idx, group) in groups.iter().enumerate() {
        let name = group.name.clone().unwrap_or_else(|| format!("Group {}", g_idx + 1));
        let vol = if (group.volume - 1.0).abs() > f32::EPSILON {
            format!("  vol {:.2}", group.volume)
        } else {
            String::new()
        };
        entries.push(TreeEntry {
            label: format!("\u{25a6} {name}{vol}"),
            address: TreeAddress::Group(g_idx),
            color: Color::Cyan,
            indent: 0,
        });
        for (inst_idx, inst) in instruments.iter().enumerate() {
            if inst.group == Some(g_idx) {
                push_instrument_entries(&mut entries, inst, inst_idx, "  ");
            }
        }
        for (fx_idx, fx) in group.effects.iter().enumerate() {
            entries.push(TreeEntry {
                label: format!("  fx {}  [{}]  (bus)", fx.name, fx.format),
                address: TreeAddress::GroupEffect { group: g_idx, index: fx_idx },
                color: Color::Yellow,
                indent: 1,
            });
        }

        // Group-scoped modulators (magenta), after the bus effects.
        if !group.modulators.is_empty() {
            // Resolve member ordinal -> plugin name for GroupMember targets.
            let members: Vec<&InstrumentNode> =
                instruments.iter().filter(|n| n.group == Some(g_idx)).collect();
            let member_plugin = |member: usize, slot: usize| -> String {
                let Some(m) = members.get(member) else { return format!("M{member}") };
                if slot == 0 {
                    m.instrument.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Instrument".into())
                } else {
                    m.effects.get(slot - 1).map(|p| p.name.clone()).unwrap_or_else(|| format!("fx{slot}"))
                }
            };
            let target_disp = |t: &ModTargetSlot| -> String {
                match t.kind {
                    crate::plugin::chain::ModTargetKind::GroupMember { member, slot, .. } => {
                        format!("M{member} {}: {}", member_plugin(member, slot), t.param_name)
                    }
                    crate::plugin::chain::ModTargetKind::GroupBus { effect_index, .. } => {
                        let name = group.effects.get(effect_index)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| format!("fx{effect_index}"));
                        format!("Bus {name}: {}", t.param_name)
                    }
                    crate::plugin::chain::ModTargetKind::GroupMemberMod { member, mod_index, field } => {
                        // Render live from the kind so it survives membership fixups.
                        use crate::plugin::chain::CrossModField;
                        let fname = match field {
                            CrossModField::Rate => "rate".to_string(),
                            CrossModField::Attack => "attack".to_string(),
                            CrossModField::Decay => "decay".to_string(),
                            CrossModField::Sustain => "sustain".to_string(),
                            CrossModField::Release => "release".to_string(),
                            CrossModField::Depth(ti) => format!("depth {ti}"),
                        };
                        format!("M{member} Mod{mod_index} {fname}")
                    }
                    _ => t.param_name.clone(),
                }
            };
            for (mod_idx, m) in group.modulators.iter().enumerate() {
                let source_label = match &m.source {
                    ModSourceSlot::Lfo { waveform, rate } => format!("LFO {:.1}Hz {}", rate, waveform.name()),
                    ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
                };
                let targets = if m.targets.is_empty() {
                    String::new()
                } else {
                    let list = m
                        .targets
                        .iter()
                        .map(|t| format!("{} ({:.0}%)", target_disp(t), t.depth * 100.0))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" \u{2192} {list}")
                };
                entries.push(TreeEntry {
                    label: format!("  ~ {source_label}{targets}  (bus)"),
                    address: TreeAddress::GroupModulator { group: g_idx, index: mod_idx },
                    color: Color::Magenta,
                    indent: 1,
                });
            }
        }
    }

    // Ungrouped instruments at the top level.
    for (inst_idx, inst) in instruments.iter().enumerate() {
        if inst.group.is_none() {
            push_instrument_entries(&mut entries, inst, inst_idx, "");
        }
    }

    entries
}

fn format_from_id(id: &str) -> String {
    if id.starts_with("builtin:") {
        "Built-in".into()
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

fn action_bar_hit(x: u16, y: u16, area: Rect, actions: &[(&str, &str)]) -> Option<char> {
    if y != area.y || x < area.x || x >= area.right() {
        return None;
    }
    let rel_x = (x - area.x) as usize;
    let mut pos = 0;
    for &(key, desc) in actions {
        if pos > 0 {
            pos += 1;
        }
        let total = key.len() + 2 + desc.len() + 1;
        if rel_x >= pos && rel_x < pos + total {
            return key.chars().next();
        }
        pos += total;
    }
    None
}

/// Enumerate all plugin formats on a background thread, sending each format's
/// results as a separate batch so the selector fills in progressively (built-ins
/// arrive instantly, VST3 — which instantiates every plugin — arrives last).
/// The channel disconnecting signals that the scan is complete.
fn spawn_catalog_scan() -> crossbeam_channel::Receiver<Vec<PluginInfo>> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::spawn(move || {
        let _ = tx.send(plugin::builtin::enumerate_plugins());

        #[cfg(feature = "lv2")]
        let _ = tx.send(plugin::lv2::enumerate_plugins());

        let _ = tx.send(plugin::clap::enumerate_plugins());

        #[cfg(feature = "vst3")]
        let _ = tx.send(plugin::vst3::enumerate_plugins());
    });
    rx
}

fn build_help_lines() -> Vec<String> {
    vec![
        "Tang — Terminal Audio Plugin Host".into(),
        "".into(),
        "Global keybindings:".into(),
        "  1 2 3 4    Switch to tab by number".into(),
        "  Tab        Next tab".into(),
        "  Shift+Tab  Previous tab".into(),
        "  Ctrl+S     Save session".into(),
        "  Ctrl+Shift+S  Save session as (new filename)".into(),
        "  Ctrl+Q     Quit".into(),
        "".into(),
        "Session tab (chain focus):".into(),
        "  Up/Down    Navigate chain".into(),
        "  Shift+↑/↓  Move effect up/down".into(),
        "  Enter      Focus parameter list".into(),
        "  n          Add instrument (split); defaults to oscillator".into(),
        "  i          Replace instrument".into(),
        "  R          Set instrument key range / rename group".into(),
        "  v          Set instrument volume".into(),
        "  a          Add effect after selected".into(),
        "  d          Delete selected".into(),
        "  m          Add modulator (lane rack; group rack on a group node)".into(),
        "  g          Assign instrument to a group (submix bus)".into(),
        "  x          Set effect / group-bus dry/wet mix".into(),
        "  r          Record/stop pattern".into(),
        "  Ctrl+R     Clear pattern".into(),
        "  b          Set BPM".into(),
        "".into(),
        "Modulator (chain focus, lane or group):".into(),
        "  t          Add modulation target".into(),
        "  d          Delete modulator".into(),
        "".into(),
        "Piano tab:".into(),
        "  k          Open scale picker (root + scale)".into(),
        "  [          Shift view octave down".into(),
        "  ]          Shift view octave up".into(),
        "".into(),
        "Session tab (param focus):".into(),
        "  Up/Down    Navigate parameters".into(),
        "  Left/Right Adjust value (5%)".into(),
        "  Shift+←/→  Fine adjust (1%)".into(),
        "  Ctrl+←/→   Coarse adjust (10%)".into(),
        "  Enter      Type a value".into(),
        "  m          Modulate this parameter (new/existing modulator)".into(),
        "  /          Search parameters".into(),
        "  Esc        Clear filter / back to chain".into(),
        "".into(),
        "Plugin selector:".into(),
        "  Type       Filter by name/format".into(),
        "  Up/Down    Navigate results".into(),
        "  Enter      Confirm".into(),
        "  Esc        Cancel".into(),
        "".into(),
        "Mouse:".into(),
        "  Click      Select items, tabs, actions".into(),
        "  Drag       Adjust parameter bars".into(),
        "  Scroll     Navigate lists".into(),
    ]
}
