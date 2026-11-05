mod clip;
#[cfg(test)]
mod golden_tests;
mod patch_view;
mod piano_view;
mod scope_view;
pub mod splash;
mod terminal_input;
mod web;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Terminal;

use crossbeam_channel::Receiver;
use terminal_input::TerminalReader;
use view::DIM;
use web::{Web, WebStatus};
use view::filter_list::{FilterListItem, FilterListState};
use view::list::{ListItem, ListSpan, ListState};
use view::scroll_view::ScrollLine;
use view::text_input::TextInputState;
use view::{FilterList, List, ScrollView, TabBar, TextInput, centered_rect};

use crate::app::controller::Controller;
use crate::app::graph::NoteRange;
use crate::cli::WebArgs;
use crate::app::intent::{self, Applied, Gain, Intent, ModulatorField, ModulatorKind, PatternField, Taper};
use crate::app::module;
use crate::app::model::{
    AudioCable, BPM_MAX, BPM_MIN, CABLE_GAIN_MAX, DEPTH_MAX, DEPTH_MIN, Driven, EdgeId, FREE, GroupNode, InstrumentNode, MAX_NAME, ModSourceSlot, ModTargetSlot, ModulatorRow, ModulatorSlot, NodeId, NodePath,
    ParamKind, ParamSlot, PluginSlot, STEP_MAX, STEP_MIN, SYNC_DIVISIONS, SessionModel, format_from_id, mod_source_slot_to_graph, modulator_param_is_enum,
    modulator_param_len, modulator_param_range, modulator_row, modulator_row_to_cross_mod,
};
use crate::note_input::Delivery;
use crate::piano_filter::PianoMode;
use crate::plugin;
use crate::plugin::chain::{GraphCommand, ModulatorLocation};
use crate::scale::{ScaleSetting, NOTE_NAMES};
use crate::session::{Feed, Route};
use crate::telemetry;

const TAB_NAMES: &[&str] = &["(1) Session", "(2) Piano", "(3) Scope", "(4) Patch", "(5) Help"];
/// The tabs by their place in `TAB_NAMES`, which is what `State::active_tab` is.
const TAB_SESSION: usize = 0;
const TAB_PIANO: usize = 1;
const TAB_SCOPE: usize = 2;
const TAB_PATCH: usize = 3;
const TAB_HELP: usize = 4;
const TAB_SEP: &str = " │ ";

/// What the tab bar says on each tab: the names, and the mark on the Session tab while the session has changes that are not saved. What a click hits is
/// worked out from the same words, so that it is the tab that is drawn where it is clicked.
fn tab_labels(dirty: bool) -> Vec<&'static str> {
    let mut labels = TAB_NAMES.to_vec();
    if dirty {
        labels[TAB_SESSION] = "(1) Session *";
    }
    labels
}

/// Addresses a specific node in the instrument tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TreeAddress {
    Instrument(usize),
    Effect { inst: usize, index: usize },
    Pattern(usize),
    /// A submix group header node.
    Group(usize),
    /// An effect on a group's bus chain.
    GroupEffect { group: usize, index: usize },
    /// A modulator, wherever the tree lists it (under the lane or the group it hears, or first, if it hears every note). `index` is its position in the session's own rack.
    FreeModulator { index: usize },
}

impl TreeAddress {
    /// Get the instrument index for this address (None for group nodes).
    fn inst(&self) -> Option<usize> {
        match *self {
            TreeAddress::Instrument(inst) => Some(inst),
            TreeAddress::Effect { inst, .. } => Some(inst),
            TreeAddress::Pattern(inst) => Some(inst),
            TreeAddress::Group(_) | TreeAddress::GroupEffect { .. } | TreeAddress::FreeModulator { .. } => None,
        }
    }

}

struct TreeEntry {
    label: String,
    address: TreeAddress,
    /// The node of the session the entry is of (a lane's, for its pattern). An address is a
    /// position, and what is at it changes as nodes come and go; this does not.
    node: NodeId,
    /// What the tree says of the kind of entry by its colour: a group's header is cyan and a modulator magenta; the rest have none of their own.
    color: Option<Color>,
    #[allow(dead_code)]
    indent: usize,
}

/// What an entry of the tree is made of, whatever place it has: the kind of entry (a lane's pattern is an entry of
/// the lane's node, as its instrument is) and the id of the node.
type EntryKey = (std::mem::Discriminant<TreeAddress>, NodeId);

fn entry_key(entry: &TreeEntry) -> EntryKey {
    (std::mem::discriminant(&entry.address), entry.node)
}

// ---------------------------------------------------------------------------
// Action bar
// ---------------------------------------------------------------------------

/// Build the action bar items for the current tree selection.
fn actions_for(addr: Option<&TreeAddress>) -> Vec<(&'static str, &'static str)> {
    match addr {
        // (As long as a terminal of 120 columns shows: M and S are what made it too long, and "replace", "vol" and "add fx" are what it was shortened by.)
        Some(TreeAddress::Instrument(_)) => vec![
            ("n", "new instr"),
            ("i", "replace"),
            ("R", "range"),
            ("v", "vol"),
            ("M", "mute"),
            ("S", "solo"),
            ("a", "add fx"),
            ("m", "modulate"),
            ("g", "group"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Effect { .. }) => vec![
            ("n", "new instr"),
            ("a", "add fx"),
            ("m", "modulate"),
            ("x", "mix"),
            ("d", "delete"),
            ("p", "presets"),
        ],
        Some(TreeAddress::Pattern(_)) => vec![
            ("r", "record"),
            ("d", "clear"),
        ],
        // (A modulator is in no lane: `F` makes another, and is only said here.)
        Some(TreeAddress::FreeModulator { .. }) => vec![
            ("F", "new free"),
            ("t", "add target"),
            ("h", "hears"),
            ("d", "delete"),
        ],
        Some(TreeAddress::Group(_)) => vec![
            ("a", "add fx"),
            ("m", "modulate"),
            ("v", "vol"),
            ("M", "mute"),
            ("S", "solo"),
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
        None => vec![("n", "new instr")],
    }
}

/// Build the action bar items for the parameter pane (when it has focus). These
/// are the keys that act on the *selected parameter*, which differ from the
/// chain-focus actions in `actions_for`.
fn param_actions_for(addr: Option<&TreeAddress>) -> Vec<(&'static str, &'static str)> {
    match addr {
        // Plugin parameters: edit, modulate, MIDI-learn, filter the list.
        Some(TreeAddress::Instrument(_))
        | Some(TreeAddress::Effect { .. })
        | Some(TreeAddress::GroupEffect { .. }) => vec![
            ("Enter", "edit"),
            ("m", "modulate"),
            ("l", "learn"),
            ("/", "filter"),
        ],
        // Modulator pseudo-params (rate/ADSR/depth): edit, cross-modulate, or
        // MIDI-learn the row.
        Some(TreeAddress::FreeModulator { .. }) => {
            vec![("Enter", "edit"), ("m", "modulate"), ("l", "learn")]
        }
        // Group Volume/Pan and pattern settings: edit only.
        Some(TreeAddress::Group(_)) | Some(TreeAddress::Pattern(_)) => vec![("Enter", "edit")],
        None => vec![],
    }
}

/// The action bar items for the currently focused pane.
fn action_bar_items(addr: Option<&TreeAddress>, focus_params: bool) -> Vec<(&'static str, &'static str)> {
    if focus_params {
        param_actions_for(addr)
    } else {
        actions_for(addr)
    }
}

/// What the action bar of the Patch tab says, with the key that a click on each presses: the key as it is written, what it does and the key.
const PATCH_ACTIONS: &[(&str, &str, KeyCode)] = &[
    ("Enter", "pin / unpin", KeyCode::Enter),
    ("+", "louder", KeyCode::Char('+')),
    ("-", "softer", KeyCode::Char('-')),
    ("e", "type a level", KeyCode::Char('e')),
    ("d", "remove", KeyCode::Char('d')),
    ("w", "way out", KeyCode::Char('w')),
    ("M", "mute", KeyCode::Char('M')),
    ("S", "solo", KeyCode::Char('S')),
];

/// `PATCH_ACTIONS` as the action bar draws them.
fn patch_action_items() -> Vec<(&'static str, &'static str)> {
    PATCH_ACTIONS.iter().map(|&(key, what, _)| (key, what)).collect()
}
// ---------------------------------------------------------------------------
// Popup state types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorMode {
    Instrument,
    Effect,
    /// Add an effect that stands by itself, in a lane of its own (step 87).
    FreeEffect,
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

/// Editing a host-side gain (effect mix or instrument/group volume) via the
/// value popup. These live outside the plugin parameter list.
struct GainEditState {
    node: NodeId,
    gain: Gain,
    edit: EditState,
}

impl GainEditState {
    /// A popup for `gain` of `node`, starting at its `current` value.
    fn new(node: NodeId, gain: Gain, current: f32, label: &str) -> Self {
        let (param_min, param_max) = gain.range();
        GainEditState {
            node,
            gain,
            edit: EditState {
                input: TextInputState::new(&format!("{current:.2}")),
                param_name: label.to_string(),
                param_min,
                param_max,
            },
        }
    }
}

/// Editing the level of an audio cable (`e` on the Patch tab) via the value popup.
struct CableEditState {
    edge: EdgeId,
    edit: EditState,
}

impl CableEditState {
    /// A popup for the level of the cable `edge`, which is at `current` now.
    fn new(edge: EdgeId, current: f32) -> Self {
        CableEditState {
            edge,
            edit: EditState { input: TextInputState::new(&format!("{current:.2}")), param_name: "level of the cable".to_string(), param_min: 0.0, param_max: CABLE_GAIN_MAX },
        }
    }
}

/// One entry in the target selector popup.
struct TargetEntry {
    kind: crate::plugin::chain::ModTargetKind,
}

struct TargetSelectorState {
    filter: FilterListState,
    items: Vec<FilterListItem>,
    entries: Vec<TargetEntry>,
    /// The modulator (its place in the session's own rack) that the chosen target is drawn from.
    mod_index: usize,
}

/// A choice in the "modulate this parameter" popup (`m` on a parameter):
/// create a new modulator bound to the parameter, or attach the
/// parameter to one of the session's modulators.
#[derive(Clone, Copy)]
enum ModulateChoice {
    New(ModulatorKind),
    Existing(usize),
}

/// The kinds of modulator the popup makes, in the order it lists them, and what each is called there.
const NEW_MODULATORS: [(ModulatorKind, &str); 6] = [
    (ModulatorKind::Lfo, "New LFO"),
    (ModulatorKind::Envelope, "New envelope"),
    (ModulatorKind::Macro, "New macro"),
    (ModulatorKind::Velocity, "New velocity"),
    (ModulatorKind::Key, "New key"),
    (ModulatorKind::Steps, "New step sequencer"),
];

/// The first choices of the popup for driving `param_name`: a new modulator of each kind, which `Existing` ones follow.
fn new_modulator_choices(param_name: &str) -> (Vec<ModulateChoice>, Vec<FilterListItem>) {
    let choices = NEW_MODULATORS.iter().map(|&(kind, _)| ModulateChoice::New(kind)).collect();
    let items = NEW_MODULATORS
        .iter()
        .enumerate()
        .map(|(index, (_, name))| FilterListItem { cells: vec![format!("{name} → {param_name}")], index })
        .collect();
    (choices, items)
}

/// The modulator that drives something: one that is made for it, with the depth its cable has if it is not the depth a new cable has, or one that
/// is there (its place in its rack).
#[derive(Clone, Copy)]
enum Driver {
    New(ModulatorKind, Option<f32>),
    Existing(usize),
}

/// State for the "modulate <param>" popup.
struct ModulateState {
    /// The target kind to bind: a parameter of a plugin, or a setting of a modulator.
    target_kind: crate::plugin::chain::ModTargetKind,
    param_name: String,
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

/// State for the "what a free modulator hears" popup (`h` on a free modulator): every note, or the notes of a lane or of a group. `choices` is the ear that each row stands for.
struct EarState {
    modulator: NodeId,
    choices: Vec<Option<NodeId>>,
    filter: FilterListState,
    items: Vec<FilterListItem>,
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

/// A plugin that is to be loaded once a frame has said so: it can take a while (about a third of a second for Pianoteq 9 here), and the loop is blocked for as long as it takes (`run_pending`).
struct PendingLoad {
    /// The plugin's name, for the frame.
    name: String,
    /// What asks the controller to load it.
    intent: Intent,
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

/// State for the "send CC" popup.
struct SendCcState {
    input: TextInputState,
    error: Option<String>,
}

/// State for the popup that names the module that a chain of effects is saved as (step 104).
struct SaveModuleState {
    /// The lane whose chain is saved (`Intent::SaveModule` takes a lane or an effect of its chain).
    node: NodeId,
    input: TextInputState,
    /// Why the last attempt was refused, shown in the popup.
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
    /// The session and the engine handles; the view only holds what it shows.
    ctl: Controller,
    /// The CLIP light of the status bar, fed by the master meter.
    clip: clip::ClipLight,
    /// The web interface this TUI serves, if it does (`tang --web`).
    web: Option<WebStatus>,
    active_tab: usize,
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
    ear_popup: Option<EarState>,
    preset_selector: Option<PresetSelectorState>,
    /// A plugin chosen in the selector, which waits for the frame that says it is loading.
    pending_load: Option<PendingLoad>,
    areas: Areas,
    quit: bool,
    // Parameter filter (search bar in param pane).
    param_filter_input: TextInputState,
    param_filtering: bool,
    param_filtered: Vec<usize>,
    // Pattern state.
    bpm_editing: Option<EditState>,
    gain_editing: Option<GainEditState>,
    save_as: Option<SaveAsState>,
    save_module: Option<SaveModuleState>,
    send_cc: Option<SendCcState>,
    /// Prefills the next send-CC popup.
    last_cc: String,
    // Piano tab state.
    /// Center MIDI octave for the piano view (0..=8). Default 4.
    piano_view_octave: i8,
    scale_selector: Option<ScaleSelectorState>,
    // Scope tab state: the view, the time base and the frames of the output that the last frame was drawn from.
    scope: scope_view::Scope,
    /// The scope of the node under the chain cursor, under the Session tab (`o`): its own view, and the node whose signal is asked for.
    strip: scope_view::Strip,
    // Patch tab state: the matrix of the audio cables with its cursor, and the level of a cable that is being typed.
    patch: patch_view::Patch,
    cable_editing: Option<CableEditState>,
}

impl State {
    /// Build the TUI state around a controller holding a loaded session. Pure
    /// construction: no engine commands are sent and no threads are spawned, so
    /// tests can build a `State` around stub channels.
    fn new(ctl: Controller) -> State {
        let tree_entries = build_tree_entries(&ctl.model);
        let param_len = ctl.model.instruments.first()
            .and_then(|n| n.instrument.as_ref())
            .map_or(0, |p| p.params.len());

        let help_lines = build_help_lines(None);
        let clip = clip::ClipLight::new(ctl.telemetry.count(telemetry::MASTER.clips));

        State {
            ctl,
            clip,
            web: None,
            active_tab: TAB_SESSION,
            chain_state: ListState::new(tree_entries.len()),
            param_state: ListState::new(param_len),
            tree_entries,
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
            ear_popup: None,
            preset_selector: None,
            pending_load: None,
            areas: Areas::default(),
            quit: false,
            param_filter_input: TextInputState::new(""),
            param_filtering: false,
            param_filtered: (0..param_len).collect(),
            bpm_editing: None,
            gain_editing: None,
            save_as: None,
            save_module: None,
            send_cc: None,
            last_cc: String::new(),
            piano_view_octave: 4,
            scale_selector: None,
            scope: scope_view::Scope::new(),
            strip: scope_view::Strip::new(),
            patch: patch_view::Patch::new(),
            cable_editing: None,
        }
    }

    /// The web interface is up: say so in the tab bar and on the Help tab. (After this
    /// only the number of browsers changes, in `self.web`.)
    fn set_web(&mut self, web: WebStatus) {
        self.help_lines = build_help_lines(Some(&web));
        self.web = Some(web);
    }

    /// Build the tree again from the model. What is typed into the parameter filter stays if the
    /// node the cursor is on is the same one (a value changed under it, by a browser, say), and goes
    /// if it is not.
    fn rebuild_tree(&mut self) {
        let was_on = self.node_under_cursor();
        self.tree_entries = build_tree_entries(&self.ctl.model);
        self.chain_state.set_len(self.tree_entries.len());
        // The cursor stays on the node it was on, wherever the tree has put it now: what came or went above it
        // (a browser added a modulator to an earlier lane, say) has moved its place, and what the person does
        // next (`d`, a value typed in) would be for whatever is at the old one.
        if let Some(on) = was_on {
            if let Some(position) = self.tree_entries.iter().position(|entry| entry_key(entry) == on) {
                self.chain_state.selected = position;
            }
        }
        if self.node_under_cursor() == was_on {
            self.sync_param_len();
        } else {
            self.sync_param_state();
        }
    }

    /// What the cursor is on. It does not change as the nodes around it come and go, as its address does.
    fn node_under_cursor(&self) -> Option<EntryKey> {
        self.tree_entries.get(self.chain_state.selected).map(entry_key)
    }

    /// Every entry of the tree, in the order they are shown.
    fn tree_places(&self) -> Vec<EntryKey> {
        self.tree_entries.iter().map(entry_key).collect()
    }

    /// Close what is open and holds a place in the tree (a lane's, a modulator's, a group's) that has moved, or is
    /// not there any more: it would act on what is at the place now. What is typed in as the value of a parameter
    /// goes to the node under the cursor, which follows its node (`rebuild_tree`), so it stays unless that node is
    /// gone; a host-side gain has the id of its node.
    fn close_popups_that_hold_places(&mut self, cursor_node_is_there: bool) {
        self.range_edit = None;
        self.rename_group = None;
        self.selector = None;
        self.target_selector = None;
        self.modulate = None;
        self.group_assign = None;
        self.ear_popup = None;
        self.preset_selector = None;
        if !cursor_node_is_there {
            self.editing = None;
        }
    }

    /// The cursor is on another node: what was typed into the parameter filter was for the other one.
    fn sync_param_state(&mut self) {
        self.param_filter_input = TextInputState::new("");
        self.param_filtering = false;
        self.sync_param_len();
    }

    /// The parameter pane has as many rows as the node the cursor is on has parameters.
    fn sync_param_len(&mut self) {
        let sel = self.chain_state.selected;
        if sel < self.tree_entries.len() {
            let addr = &self.tree_entries[sel].address;
            let param_len = match *addr {
                TreeAddress::Pattern(inst) => {
                    self.ctl.model.instruments.get(inst)
                        .and_then(|n| n.pattern.as_ref())
                        .map_or(0, |p| {
                            let mut n = 4; // Length + Enabled + Loop + Transpose
                            if !p.events.is_empty() { n += 1; } // Notes info
                            n
                        })
                }
                TreeAddress::FreeModulator { index } => self.free_modulator(index).map_or(0, modulator_param_len),
                // A group's bus pane shows two host-side params: Volume + Pan.
                TreeAddress::Group(_) => 2,
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
            TreeAddress::FreeModulator { index } => {
                let m = self.free_modulator(index)?;
                modulator_param_range(m, self.param_state.selected)
            }
            TreeAddress::Group(_) => match self.param_state.selected {
                0 => Some((0.0, 4.0)),  // volume
                1 => Some((-1.0, 1.0)), // pan
                _ => None,
            },
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
            TreeAddress::FreeModulator { index } => {
                let Some(m) = self.free_modulator(index) else { return false };
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
            TreeAddress::Pattern(_) | TreeAddress::Group(_) | TreeAddress::FreeModulator { .. } => None,
            TreeAddress::Instrument(inst) => {
                self.ctl.model.instruments.get(inst)?.instrument.as_ref()
            }
            TreeAddress::Effect { inst, index } => {
                self.ctl.model.instruments.get(inst)?.effects.get(index)
            }
            TreeAddress::GroupEffect { group, index } => {
                self.ctl.model.groups.get(group)?.effects.get(index)
            }
        }
    }

    fn selected_address(&self) -> Option<&TreeAddress> {
        self.tree_entries.get(self.chain_state.selected).map(|e| &e.address)
    }

    /// The step that the step sequencer under the chain cursor is at: the whole part of the position that the audio thread last published for it, brought round at the length
    /// the model has (`step_at`, which the engine's own output is made by). `None` for any other node, and for a step sequencer that has no slot to publish to.
    fn selected_playhead(&self) -> Option<usize> {
        let m = self.modulator_at(*self.selected_address()?)?;
        let ModSourceSlot::Steps { length, .. } = m.source else { return None };
        Some(crate::plugin::chain::step_at(length, self.ctl.telemetry.load(m.position?)))
    }

    /// The node under the chain cursor if it has a signal to look at, and what to call it: an instrument that has a plugin, an effect, a group or an effect of its bus. A modulator
    /// and a pattern put out no sound, and a lane that has no instrument puts out none.
    fn selected_signal(&self) -> Option<(NodeId, String)> {
        let model = &self.ctl.model;
        match *self.selected_address()? {
            TreeAddress::Instrument(inst) => {
                let lane = model.instruments.get(inst)?;
                lane.instrument.as_ref().map(|plugin| (lane.node, plugin.name.clone()))
            }
            TreeAddress::Effect { inst, index } => model.instruments.get(inst)?.effects.get(index).map(|fx| (fx.node, fx.name.clone())),
            TreeAddress::Group(group) => {
                let g = model.groups.get(group)?;
                Some((g.node, g.name.clone().unwrap_or_else(|| format!("Group {}", group + 1))))
            }
            TreeAddress::GroupEffect { group, index } => model.groups.get(group)?.effects.get(index).map(|fx| (fx.node, fx.name.clone())),
            TreeAddress::Pattern(_) | TreeAddress::FreeModulator { .. } => None,
        }
    }

    /// Keep what the scope under the Session tab asks the audio thread to listen to where the chain cursor is. The loop's every turn begins with a tick, which does it, after the
    /// events of the terminal and the requests of the browsers (which can take the node away): there is no frame between them and it. (The tests' keys do it in `process_event`.)
    fn sync_strip(&mut self) {
        let selected = self.selected_signal().map(|(node, _)| node);
        self.strip.sync(&mut self.ctl, self.active_tab == TAB_SESSION, selected);
    }

    /// A modulator by its place in the session's own rack.
    fn free_modulator(&self, index: usize) -> Option<&ModulatorSlot> {
        self.ctl.model.free_modulators.get(index)
    }

    /// What the selectors list, and the places that a row of one names: the modules of the library and then the plugins (`Catalog::listed`). Only the selector of an effect that stands by itself
    /// lists the modules: a chain of effects goes in a lane of its own, whole, and not in a chain.
    fn selector_items(&self, mode: SelectorMode) -> Vec<FilterListItem> {
        self.ctl
            .catalog
            .listed()
            .into_iter()
            .enumerate()
            .filter(|(_, e)| match mode {
                SelectorMode::Instrument => e.is_instrument,
                SelectorMode::FreeEffect => !e.is_instrument,
                SelectorMode::Effect | SelectorMode::GroupEffect(_) => !e.is_instrument && module::name_of(&e.id).is_none(),
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
        // An instrument is chosen for a lane that has one (the oscillator that `n` has just put in a new lane, say): the list starts on it, so that Enter keeps it, as the preset
        // selector starts on the preset that is loaded. (With the audio input in the list, which sorts before the oscillator, the first row is not the one that was always there.)
        if matches!(mode, SelectorMode::Instrument) {
            let current = self.selected_inst().and_then(|inst| self.ctl.model.instruments.get(inst)).and_then(|lane| lane.instrument.as_ref()).map(|slot| slot.id.as_str());
            let plugins = self.ctl.catalog.listed();
            if let Some(at) = current.and_then(|id| items.iter().position(|item| plugins.get(item.index).is_some_and(|entry| entry.id == id))) {
                filter.list.selected = at;
            }
        }

        self.selector = Some(SelectorState {
            mode,
            filter,
            items,
        });
    }

    /// Rebuild the rows of an open selector popup from the catalog, keeping its
    /// filter text, so that rows appear as the background scan progresses.
    fn refresh_selector(&mut self) {
        if let Some(mode) = self.selector.as_ref().map(|sel| sel.mode) {
            let items = self.selector_items(mode);
            if let Some(sel) = self.selector.as_mut() {
                sel.items = items;
                sel.filter.apply_filter(&sel.items);
            }
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
        let Some(entry) = self.ctl.catalog.listed().get(chosen).copied() else { return };

        // Determine which instrument to operate on from current selection.
        let inst = self.selected_inst().unwrap_or(0);

        // Load the real plugin, and put it where it goes: the intents that a browser asks for too, so that an effect that is
        // added is a step of the history. (An effect that cannot be added, because it will not load or its outputs are not
        // the chain's, leaves the session as it was: the controller says why.)
        let source = entry.id.clone();
        let name = entry.name.clone();
        // (A module is not made of one plugin but of the plugins that its file names, any of which can be slow to load: it waits for a frame that says so, as a plugin that is not built in does.)
        let built_in = source.starts_with("builtin:");
        log::info!("Loading plugin '{}' (id={}) into inst={}", entry.name, source, inst);
        let intent = match sel.mode {
            SelectorMode::Instrument => {
                let Some(lane) = self.ctl.model.instruments.get(inst) else { return };
                Intent::ReplaceInstrument { node: lane.node, plugin: source }
            }
            SelectorMode::Effect => {
                let Some(lane) = self.ctl.model.instruments.get(inst) else { return };
                Intent::AddEffect { parent: lane.node, plugin: source }
            }
            SelectorMode::FreeEffect => Intent::AddFreeEffect { plugin: source },
            SelectorMode::GroupEffect(group) => {
                let Some(bus) = self.ctl.model.groups.get(group) else { return };
                Intent::AddEffect { parent: bus.node, plugin: source }
            }
        };
        // A built-in is there at once. Any other plugin can take a while, with the loop blocked: it waits for a frame that says so (`run_pending`).
        if built_in {
            self.apply(intent);
        } else {
            self.pending_load = Some(PendingLoad { name, intent });
        }
    }

    /// Move an effect to place `to` of its chain (a lane's or a group's bus), by the intent that a browser's buttons ask for: a step of the
    /// history. The cursor follows the effect it was on.
    fn move_effect_to(&mut self, node: Option<NodeId>, to: usize) {
        if let Some(node) = node {
            self.apply(Intent::MoveEffect { node, index: to as u32 });
        }
    }

    /// Take a node out, by the intent that a browser's "x" asks for: nothing puts it back yet, so the history is forgotten. (A
    /// session keeps a lane: the last one is refused.)
    fn remove_node(&mut self, node: Option<NodeId>) {
        if let Some(node) = node {
            self.apply(Intent::RemoveNode { node });
        }
    }

    /// Get the instrument index for the currently selected tree entry: the lane that the entry is of (`lane_of`).
    fn selected_inst(&self) -> Option<usize> {
        self.lane_of(*self.selected_address()?)
    }

    /// What a modulator of the session's own hears, as the place of the lane or the group (`SessionModel::ear_of`: a lane or a group that is not there is not heard): the tree lists the modulator
    /// with it, and the keys that are of "the lane or the group that the entry is of" go to it.
    fn heard_by(&self, index: usize) -> Option<NodePath> {
        let model = &self.ctl.model;
        model.locate(model.ear_of(self.free_modulator(index)?)?)
    }

    /// The lane that the tree entry at `address` is of: an instrument, its effects and its pattern, and a modulator that hears the lane. None for a group's entries and for a modulator that hears
    /// a group or every note.
    fn lane_of(&self, address: TreeAddress) -> Option<usize> {
        match address {
            TreeAddress::FreeModulator { index } => match self.heard_by(index)? {
                NodePath::Instrument(inst) => Some(inst),
                _ => None,
            },
            address => address.inst(),
        }
    }

    /// The group that the tree entry at `address` is of: its header, its bus effects, and a modulator that hears the group.
    fn group_of(&self, address: TreeAddress) -> Option<usize> {
        match address {
            TreeAddress::Group(group) | TreeAddress::GroupEffect { group, .. } => Some(group),
            TreeAddress::FreeModulator { index } => match self.heard_by(index)? {
                NodePath::Group(group) => Some(group),
                _ => None,
            },
            _ => None,
        }
    }

    /// The lane or the group that the selected entry of the tree is of, with whether it is muted, whether it is soloed and which way out it has: what `M`, `S` and
    /// `w` ask the intents for.
    fn selected_lane_or_group(&self) -> Option<(NodeId, bool, bool, Route)> {
        let address = self.selected_address().copied()?;
        if let Some(group) = self.group_of(address) {
            return self.ctl.model.groups.get(group).map(|bus| (bus.node, bus.muted, bus.solo, bus.route));
        }
        self.lane_of(address).and_then(|inst| self.ctl.model.instruments.get(inst)).map(|lane| (lane.node, lane.muted, lane.solo, lane.route))
    }

    /// The effect that the selected entry of the tree is (a lane's or a bus's), with whether the stage before it feeds it: what `f` asks the intent `SetFeed` for. None for any other entry.
    fn selected_effect_feed(&self) -> Option<(NodeId, bool)> {
        let slot = match self.selected_address().copied()? {
            TreeAddress::Effect { inst, index } => self.ctl.model.instruments.get(inst)?.effects.get(index)?,
            TreeAddress::GroupEffect { group, index } => self.ctl.model.groups.get(group)?.effects.get(index)?,
            _ => return None,
        };
        Some((slot.node, slot.fed))
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
        self.bind(Driver::Existing(ts.mod_index), &entry.kind);
    }

    /// Open the "modulate <param>" popup for the parameter currently selected in the param pane (a plugin parameter on the Instrument, Effect or bus-effect node).
    fn open_modulate(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        // A group's rows are its volume and its pan, which no plugin has: the cable goes to the group (a new modulator hears it).
        if let TreeAddress::Group(group) = addr {
            let Some((node, gain)) = self.group_row_gain(group) else { return };
            let param_name = gain.name().to_string();
            let (mut choices, mut items) = new_modulator_choices(&param_name);
            self.push_the_sessions_own_modulators(&mut choices, &mut items);
            let mut filter = FilterListState::new();
            filter.apply_filter(&items);
            self.modulate = Some(ModulateState { target_kind: crate::plugin::chain::ModTargetKind::Gain { node, gain }, param_name, choices, filter, items });
            return;
        }
        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        let Some(plugin) = self.plugin_at(&addr) else { return };
        let Some(param) = plugin.params.get(pa) else { return };
        let (plugin, param_index, param_name) = (plugin.node, param.index, param.name.clone());

        // Choices: the new ones (made for the parameter: one of the session's own that hears the lane, or the group of a bus effect), then each modulator of the session's own, which can drive any plugin.
        let (mut choices, mut items) = new_modulator_choices(&param_name);
        self.push_the_sessions_own_modulators(&mut choices, &mut items);

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            target_kind: crate::plugin::chain::ModTargetKind::FreeParam { plugin, param_index },
            param_name,
            choices,
            filter,
            items,
        });
    }

    /// The group's node and the value of the group's own that the row under the cursor of its pane is (0 is its volume, 1 its pan).
    fn group_row_gain(&self, group: usize) -> Option<(NodeId, crate::app::model::ModGain)> {
        use crate::app::model::ModGain;
        let node = self.ctl.model.groups.get(group)?.node;
        match self.param_state.selected {
            0 => Some((node, ModGain::Volume)),
            1 => Some((node, ModGain::Pan)),
            _ => None,
        }
    }

    /// Open the "modulate" popup for a parameter of a *modulator* (its rate /
    /// ADSR stage / a target depth) — i.e. cross-modulation: a sibling modulator
    /// (new or existing) drives this field. The
    /// inverse of picking this modulator as a target via `t`. Enum / separator
    /// rows aren't modulatable, and the modulator can't modulate itself.
    fn open_modulate_cross_mod(&mut self) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let TreeAddress::FreeModulator { index: mod_index } = self.tree_entries[sel].address else { return };
        let pa = self.param_state.selected;
        // Resolve the cross-mod target kind (+ range / base / label) for this row.
        let resolved = self.free_modulator(mod_index).and_then(|m| modulator_row_to_cross_mod(m, mod_index, pa));
        let Some((kind, _, _, _, param_name)) = resolved else {
            return;
        };

        // Choices: new LFO / envelope, then each sibling modulator in the same
        // rack except this one (no self-modulation).
        let (mut choices, mut items) = new_modulator_choices(&param_name);
        let rack_len = self.ctl.model.free_modulators.len();
        for i in 0..rack_len {
            if i == mod_index {
                continue; // can't modulate itself
            }
            let label = self.free_modulator(i).map(|m| m.source.label());
            if let Some(label) = label {
                let idx = choices.len();
                choices.push(ModulateChoice::Existing(i));
                items.push(FilterListItem { cells: vec![format!("attach → {label}")], index: idx });
            }
        }

        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.modulate = Some(ModulateState {
            target_kind: kind,
            param_name,
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

        // The modulator that drives the parameter: a new one, or one that is there.
        let driver = match choice {
            ModulateChoice::New(kind) => Driver::New(kind, None),
            ModulateChoice::Existing(i) => Driver::Existing(i),
        };
        self.bind(driver, &ms.target_kind);
    }

    /// Put each modulator of the session's own among the choices of a modulate popup, as "attach → LFO 0.5Hz sine (hears L2)" (a modulator that hears no lane and no group says nothing of it).
    fn push_the_sessions_own_modulators(&self, choices: &mut Vec<ModulateChoice>, items: &mut Vec<FilterListItem>) {
        let model = &self.ctl.model;
        for (i, m) in model.free_modulators.iter().enumerate() {
            let hears = match model.ear_of(m).and_then(|ear| model.locate(ear)) {
                Some(NodePath::Instrument(inst)) => format!(" (hears L{})", inst + 1),
                Some(NodePath::Group(group)) => format!(" (hears G{})", group + 1),
                _ => String::new(),
            };
            let idx = choices.len();
            choices.push(ModulateChoice::Existing(i));
            items.push(FilterListItem { cells: vec![format!("attach → {}{hears}", m.source.label())], index: idx });
        }
    }

    /// A modulator, a new one or one of the session's own that is there, drives what `kind` names (the parameter of a plugin, or the setting of another modulator): by the intents a browser asks
    /// for, a new modulator and the cable to it being the one intent that makes both (`Modulate`: one step, in its words, and one that hears the lane or the group) and a modulator that is there a cable
    /// (`AddModEdge`).
    fn bind(&mut self, driver: Driver, kind: &crate::plugin::chain::ModTargetKind) {
        let Some(to) = self.ctl.model.endpoint_of(ModulatorLocation::Free, kind) else { return };
        match driver {
            Driver::New(modulator, depth) => {
                self.apply(Intent::Modulate { to, kind: modulator, depth });
            }
            Driver::Existing(index) => {
                let Some(modulator) = self.free_modulator(index).map(|m| m.node) else { return };
                self.apply(Intent::AddModEdge { modulator, to });
            }
        }
    }

    /// Open the target selector for a modulator. It reaches the whole session: every parameter of every plugin (each labelled by the lane or the group the plugin is in, as the
    /// Patch tab tags them, and by its name), and the settings of the other modulators and the depths of their cables (cross-mod).
    fn open_free_target_selector(&mut self, mod_index: usize) {
        if self.free_modulator(mod_index).is_none() {
            return;
        }
        let mut entries = Vec::new();
        let mut items = Vec::new();
        for t in self.ctl.model.param_targets(ModulatorLocation::Free) {
            let idx = entries.len();
            let owner = plugin_tag(&self.ctl.model, t.node);
            items.push(FilterListItem { cells: vec![format!("{owner} {}", t.plugin_name), t.param_name], index: idx });
            entries.push(TargetEntry { kind: t.kind });
        }
        // The host-side values: a lane's volume, an effect's mix, a group's volume and pan.
        for t in self.ctl.model.gain_targets(ModulatorLocation::Free) {
            let idx = entries.len();
            let owner = plugin_tag(&self.ctl.model, t.node);
            items.push(FilterListItem { cells: vec![format!("{owner} {}", t.owner_name), t.gain.label().to_string()], index: idx });
            entries.push(TargetEntry { kind: t.kind });
        }
        for t in self.ctl.model.field_targets(ModulatorLocation::Free, mod_index) {
            let idx = entries.len();
            items.push(FilterListItem { cells: vec!["Mod".into(), t.label], index: idx });
            entries.push(TargetEntry { kind: t.kind });
        }
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.target_selector = Some(TargetSelectorState { filter, items, entries, mod_index });
    }

    /// Add a free modulator (an LFO) at the end of the session's own rack, hearing the lane or the group `hears` (every note if none), and put the cursor on it. It is made by the intent that a browser's
    /// "+ Modulator" (none) and the "+" of a lane or a group (that one) ask, a step of the history.
    fn add_free_modulator(&mut self, hears: Option<NodeId>) {
        if self.apply(Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears }).is_none() {
            return;
        }
        let last = self.ctl.model.free_modulators.len().saturating_sub(1);
        if let Some(position) = self.tree_entries.iter().position(|e| matches!(e.address, TreeAddress::FreeModulator { index } if index == last)) {
            self.chain_state.selected = position;
            self.sync_param_state();
        }
    }

    /// MIDI-learn: create an armed `MidiLearn` modulator bound to the parameter
    /// currently selected in the param pane. Works for plugin params
    /// (instrument / effect / group-bus effect) and modulator fields (binding a
    /// control to an LFO's rate, an envelope stage, or a target depth). The
    /// audio thread captures the next CC / pitch-bend and notifies back.
    fn learn_selected_param(&mut self) {
        use crate::plugin::chain::ModTargetKind;
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        let kind: ModTargetKind = match addr {
            TreeAddress::Instrument(_) | TreeAddress::Effect { .. } | TreeAddress::GroupEffect { .. } => {
                let pa = match self.real_param_index() {
                    Some(i) => i,
                    None => return,
                };
                let Some(plugin) = self.plugin_at(&addr) else { return };
                let Some(p) = plugin.params.get(pa) else { return };
                ModTargetKind::FreeParam { plugin: plugin.node, param_index: p.index }
            }
            TreeAddress::FreeModulator { index } => {
                let Some(m) = self.free_modulator(index) else { return };
                let Some((kind, ..)) = modulator_row_to_cross_mod(m, index, self.param_state.selected) else {
                    return;
                };
                kind
            }
            TreeAddress::Group(group) => {
                let Some((node, gain)) = self.group_row_gain(group) else { return };
                ModTargetKind::Gain { node, gain }
            }
            _ => return,
        };

        // An armed MidiLearn modulator, bound to the target (depth 1.0 = full-range absolute control).
        self.bind(Driver::New(ModulatorKind::MidiLearn, Some(1.0)), &kind);
    }

    /// Open the "assign to group" popup for instrument `inst`.
    fn open_group_assign(&mut self, inst: usize) {
        let current = self.ctl.model.instruments.get(inst).and_then(|n| n.group);
        let mut choices = vec![GroupAssignChoice::New];
        let mut items = vec![FilterListItem { cells: vec!["New group".into()], index: 0 }];
        if current.is_some() {
            choices.push(GroupAssignChoice::Ungroup);
            items.push(FilterListItem { cells: vec!["Ungroup".into()], index: 1 });
        }
        for (g_idx, group) in self.ctl.model.groups.iter().enumerate() {
            let name = group.display_name(g_idx);
            let marker = if current == Some(g_idx) { " (current)" } else { "" };
            let idx = choices.len();
            choices.push(GroupAssignChoice::Existing(g_idx));
            items.push(FilterListItem { cells: vec![format!("{name}{marker}")], index: idx });
        }
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.group_assign = Some(GroupAssignState { inst, choices, filter, items });
    }

    /// Open the "what it hears" popup for the free modulator at `mod_index`: every note, the notes that a lane's instrument is given (`L1 Oscillator`), or those of a group's members
    /// (`G1 Group 1`), the one it hears now marked.
    fn open_ear_popup(&mut self, mod_index: usize) {
        let Some(modulator) = self.free_modulator(mod_index) else { return };
        let (node, current) = (modulator.node, self.ctl.model.ear_of(modulator));
        let mark = |ear: Option<NodeId>| if ear == current { " (current)" } else { "" };
        let mut choices: Vec<Option<NodeId>> = vec![None];
        let mut items = vec![FilterListItem { cells: vec![format!("Every note{}", mark(None))], index: 0 }];
        for (i, lane) in self.ctl.model.instruments.iter().enumerate() {
            let name = lane.instrument.as_ref().map_or("(empty)", |plugin| plugin.name.as_str());
            items.push(FilterListItem { cells: vec![format!("L{} {name}{}", i + 1, mark(Some(lane.node)))], index: choices.len() });
            choices.push(Some(lane.node));
        }
        for (g, group) in self.ctl.model.groups.iter().enumerate() {
            items.push(FilterListItem { cells: vec![format!("G{} {}{}", g + 1, group.display_name(g), mark(Some(group.node)))], index: choices.len() });
            choices.push(Some(group.node));
        }
        let mut filter = FilterListState::new();
        filter.apply_filter(&items);
        self.ear_popup = Some(EarState { modulator: node, choices, filter, items });
    }

    fn confirm_ear_popup(&mut self) {
        let Some(state) = self.ear_popup.take() else { return };
        let Some(item) = state.filter.selected_item(&state.items) else { return };
        let ear = state.choices[item.index];
        self.apply(Intent::SetModulatorEar { node: state.modulator, ear });
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
        let Some(lane) = self.ctl.model.instruments.get(gs.inst).map(|lane| lane.node) else { return };
        let intent = match choice {
            GroupAssignChoice::Ungroup => Intent::SetLaneGroup { node: lane, group: None },
            GroupAssignChoice::Existing(g) => match self.ctl.model.groups.get(g) {
                Some(group) => Intent::SetLaneGroup { node: lane, group: Some(group.node) },
                None => return,
            },
            GroupAssignChoice::New => Intent::AddGroup { members: vec![lane] },
        };
        self.apply(intent);
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
        let current = self.ctl.piano_filter.scale();
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
        let current = self.ctl.piano_filter.scale();
        if current.root != root || current.scale_idx != scale_idx {
            let new_scale = ScaleSetting { root, scale_idx };
            if self.apply(Intent::SetScale { scale: new_scale.short() }).is_some() {
                log::info!("Scale set to {}", new_scale.display());
            }
        }
    }

    /// matches the addressing used by GraphCommand: slot=0 = instrument,
    /// slot=1..N = effects.
    fn open_preset_selector(&mut self, inst: usize, slot: usize) {
        let plugin = if slot == 0 {
            self.ctl.model.instruments.get(inst).and_then(|n| n.instrument.as_ref())
        } else {
            self.ctl.model.instruments.get(inst).and_then(|n| n.effects.get(slot - 1))
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
        let Some(ps) = self.preset_selector.take() else { return };
        let Some(item) = ps.filter.selected_item(&ps.items) else { return };
        let preset = ps.presets[item.index].id.clone();
        let lane = self.ctl.model.instruments.get(ps.inst);
        let plugin = if ps.slot == 0 {
            lane.and_then(|n| n.instrument.as_ref())
        } else {
            lane.and_then(|n| n.effects.get(ps.slot - 1))
        };
        let Some(plugin) = plugin else { return };
        let intent = Intent::LoadPreset { node: plugin.node, preset };
        self.apply(intent);
    }

    /// Set a group's host-side bus param (0 = volume, 1 = pan) to what `f` makes
    /// of its value now; the controller clamps it to the param's range.
    fn set_group_bus_param(&mut self, group: usize, pa: usize, f: impl FnOnce(f32) -> f32) {
        let Some(g) = self.ctl.model.groups.get(group) else {
            return;
        };
        let (gain, current) = match pa {
            0 => (Gain::GroupVolume, g.volume),
            1 => (Gain::GroupPan, g.pan),
            _ => return,
        };
        let intent = Intent::SetGain { node: g.node, gain, value: f(current) };
        self.apply(intent);
    }

    /// Take the pattern of lane `inst` out: the audio thread forgets it and the model has none (`ClearPattern`). (What `d` on a pattern does, and Ctrl+R.)
    fn clear_pattern(&mut self, inst: usize) {
        if let Some(node) = self.ctl.model.instruments.get(inst).map(|lane| lane.node) {
            self.apply(Intent::ClearPattern { node });
        }
    }

    /// Step the selected row up (`direction` 1) or down (-1) by what `modifiers` say: a plugin's parameter by that fraction of its range, a modulator's
    /// setting by the law of its scale (`stepped`).
    fn adjust_param(&mut self, direction: f32, modifiers: KeyModifiers) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;
        let delta = direction * param_step(self, modifiers);

        // Group bus effect params take a dedicated command (no instrument).
        if let TreeAddress::GroupEffect { .. } = addr {
            let pa = match self.real_param_index() {
                Some(i) => i,
                None => return,
            };
            self.set_plugin_param(&addr, pa, |v| v + delta);
            return;
        }

        // The rows of a modulator.
        if let TreeAddress::FreeModulator { .. } = addr {
            self.step_modulator_row(addr, self.param_state.selected, direction, step_fraction(modifiers));
            return;
        }

        // A group's host-side bus params (Volume + Pan).
        if let TreeAddress::Group(group) = addr {
            let pa = self.param_state.selected;
            self.set_group_bus_param(group, pa, |v| v + delta);
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

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        self.set_plugin_param(&addr, pa, |v| v + delta);
    }

    /// Set the parameter in row `row` of the plugin at `addr` to what `f` makes of
    /// its value now; the controller clamps it (and rounds an enum to a label).
    fn set_plugin_param(&mut self, addr: &TreeAddress, row: usize, f: impl FnOnce(f32) -> f32) {
        let Some(plugin) = self.plugin_at(addr) else { return };
        let Some(param) = plugin.params.get(row) else { return };
        let intent = Intent::SetParam { node: plugin.node, param: param.index, value: f(param.value) };
        self.apply(intent);
    }

    /// Hand an intent to the controller. A refusal is logged and the view
    /// carries on as it was.
    fn apply(&mut self, intent: Intent) -> Option<Applied> {
        match self.try_apply(intent) {
            Ok(applied) => Some(applied),
            Err(e) => {
                log::warn!("{e}");
                None
            }
        }
    }

    /// Hand an intent to the controller, and the view reacts to what it did; a refusal is the caller's to say (the Patch tab says it in the tab).
    fn try_apply(&mut self, intent: Intent) -> Result<Applied, intent::IntentError> {
        let applied = self.ctl.apply(intent)?;
        self.react_to(&applied);
        Ok(applied)
    }

    /// Bring what the view derives from the model up to date with what `applied` did.
    fn react_to(&mut self, applied: &Applied) {
        // Nodes or cables came or went: the tree is built from the model again, and what is open that is for a
        // place in it is closed if the places have moved.
        if applied.restructured {
            let places = self.tree_places();
            let on = self.node_under_cursor();
            self.rebuild_tree();
            if self.tree_places() != places {
                let cursor_node_is_there = self.node_under_cursor() == on;
                self.close_popups_that_hold_places(cursor_node_is_there);
            }
        }
        for event in &applied.events {
            match event {
                // The group header shows the volume.
                intent::Event::GainSet { gain: Gain::GroupVolume, .. } => self.rebuild_tree(),
                // A lane's label shows its keys, and a lane's and a group's whether it is muted or soloed.
                intent::Event::RangeSet { .. } | intent::Event::MuteSet { .. } | intent::Event::SoloSet { .. } => self.rebuild_tree(),
                // (The Piano tab reads the scale and the lock from the shared filter, on every frame.)
                intent::Event::ScaleSet { .. } | intent::Event::LockedSet { .. } => {}
                // (The status bar reads the tempo and the limiter from the model, on every frame.)
                intent::Event::BpmSet { .. } | intent::Event::LimiterSet { .. } => {}
                // The tree has the pattern of a lane, as a line of its own while it is recorded or has notes.
                intent::Event::PatternSet { .. } => self.rebuild_tree(),
                // (The TUI does not draw where a node is, nor which of its parameters are its knobs.)
                intent::Event::NodeUiSet { .. } | intent::Event::PinnedSet { .. } => {}
                // The group's header and its parameter pane's title are its name.
                intent::Event::GroupRenamed { .. } => self.rebuild_tree(),
                // The tree shows a modulator's rate and waveform, and its cables' depths.
                intent::Event::ModulatorFieldSet { .. } | intent::Event::EdgeDepthSet { .. } => self.rebuild_tree(),
                // (The pane shows the steps of a step sequencer from the model on every frame: the tree's line has no room for them.)
                intent::Event::StepsSet { .. } => {}
                // (The Patch tab reads the audio cables from the model on every frame.)
                intent::Event::CableGainSet { .. } => {}
                intent::Event::ParamSet { .. }
                | intent::Event::GainSet { .. }
                | intent::Event::PresetLoaded { .. }
                // (The tab shows what the controller says is unsaved, which is not the case now.)
                | intent::Event::Saved { .. }
                // (The library is not part of the session: the selectors read the catalog when they open.)
                | intent::Event::ModuleSaved { .. }
                | intent::Event::EditorOpened { .. } => {}
            }
        }
    }

    /// The modulator at `addr`, if the address is one.
    fn modulator_at(&self, addr: TreeAddress) -> Option<&ModulatorSlot> {
        match addr {
            TreeAddress::FreeModulator { index } => self.free_modulator(index),
            _ => None,
        }
    }

    /// Left or Right on row `row` of the modulator at `addr`: a step of `delta` in the
    /// number or the depth the row is, one waveform along, or, on the Type row, the other
    /// kind of source. What a row is, its range and the commands the engine gets are the
    /// intents'; a row that is none of these does nothing.
    fn step_modulator_row(&mut self, addr: TreeAddress, row: usize, direction: f32, fraction: f32) {
        let Some(m) = self.modulator_at(addr) else { return };
        let intent = match modulator_row(m, row) {
            Some(ModulatorRow::Type) => return self.switch_modulator_source(addr),
            Some(ModulatorRow::Waveform) => {
                let ModSourceSlot::Lfo { waveform, .. } = m.source else { return };
                let next = if direction > 0.0 { waveform.next() } else { waveform.prev() };
                Intent::SetModulatorField { node: m.node, field: ModulatorField::Waveform, value: next.to_index() as f32 }
            }
            // A switch has two places: either key goes to the other.
            Some(ModulatorRow::Retrigger) => match m.row_intent(row, |on| 1.0 - on) {
                Some(intent) => intent,
                None => return,
            },
            // The note lengths are a list that has ends, as the choices of a plugin's enum have: one along, and no further than the first and the last.
            Some(ModulatorRow::Sync) => {
                let Some(now) = m.source.setting(ModulatorField::Sync) else { return };
                let (min, max) = ModulatorField::Sync.range();
                let next = (now + direction.signum()).clamp(min, max);
                if next == now {
                    return;
                }
                Intent::SetModulatorField { node: m.node, field: ModulatorField::Sync, value: next }
            }
            // A step goes by a fraction of the range it can be set over, -1 to 1.
            Some(ModulatorRow::Step(_)) => match m.row_intent(row, |value| value + direction * fraction * (STEP_MAX - STEP_MIN)) {
                Some(intent) => intent,
                None => return,
            },
            row_kind => {
                let field = match row_kind {
                    Some(ModulatorRow::Setting(field)) => Some(field),
                    _ => None,
                };
                match m.row_intent(row, |value| stepped(field, value, direction, fraction)) {
                    Some(intent) => intent,
                    None => return,
                }
            }
        };
        self.apply(intent);
    }

    /// A number typed for row `row` of the modulator at `addr`.
    fn set_modulator_row(&mut self, addr: TreeAddress, row: usize, value: f32) {
        let Some(intent) = self.modulator_at(addr).and_then(|m| m.row_intent(row, |_| value)) else { return };
        self.apply(intent);
    }

    /// The Type row: an LFO becomes an envelope with the defaults of one, and an envelope
    /// an LFO. A change of what kind of node it is, which no intent makes yet (a browser
    /// is told by the next `resync`).
    fn switch_modulator_source(&mut self, addr: TreeAddress) {
        let TreeAddress::FreeModulator { index } = addr else { return };
        let at = ModulatorLocation::Free;
        let Some(m) = self.ctl.model.modulators_mut(at).and_then(|rack| rack.get_mut(index)) else { return };
        let new_source = match m.source {
            ModSourceSlot::Lfo { .. } => ModSourceSlot::new_envelope(),
            _ => ModSourceSlot::new_lfo(),
        };
        let source = mod_source_slot_to_graph(&new_source);
        m.source = new_source;
        let command = GraphCommand::SetModulatorSource { location: at, mod_index: index, source };
        let _ = self.ctl.cmd_tx.send(command);
        self.ctl.dirty = true;
        self.param_state.selected = 0;
        self.rebuild_tree();
    }

    /// Turn row `pa` of the pattern pane of lane `inst`: its length by `delta` beats, and a switch over (`SetPatternField`).
    fn adjust_pattern_param(&mut self, inst: usize, pa: usize, delta: f32) {
        let Some(lane) = self.ctl.model.instruments.get(inst) else { return };
        let Some(pattern) = lane.pattern.as_ref() else { return };
        let node = lane.node;
        let switched = |on: bool| f32::from(u8::from(!on));
        let (field, value) = match pa {
            0 => (PatternField::Length, pattern.length_beats + delta),
            1 => (PatternField::Enabled, switched(pattern.enabled)),
            2 => (PatternField::Looping, switched(pattern.looping)),
            3 => (PatternField::Transpose, switched(pattern.in_key)),
            _ => return, // Notes row is informational
        };
        self.apply(Intent::SetPatternField { node, field, value });
    }

    fn set_param_value(&mut self, value: f32) {
        let sel = self.chain_state.selected;
        if sel >= self.tree_entries.len() {
            return;
        }
        let addr = self.tree_entries[sel].address;

        // Group bus effect params take a dedicated command (no instrument).
        if let TreeAddress::GroupEffect { .. } = addr {
            let pa = match self.real_param_index() {
                Some(i) => i,
                None => return,
            };
            self.set_plugin_param(&addr, pa, |_| value);
            return;
        }

        // The rows of a modulator.
        if let TreeAddress::FreeModulator { .. } = addr {
            self.set_modulator_row(addr, self.param_state.selected, value);
            return;
        }

        // A group's host-side bus params (Volume + Pan) — set absolute.
        if let TreeAddress::Group(group) = addr {
            let pa = self.param_state.selected;
            self.set_group_bus_param(group, pa, |_| value);
            return;
        }

        let inst = match addr.inst() {
            Some(i) => i,
            None => return,
        };

        // Handle pattern params.
        if let TreeAddress::Pattern(_) = addr {
            // (Only the length is a number: the other rows are switches, and the notes are not a setting.)
            if self.param_state.selected == 0 {
                if let Some(lane) = self.ctl.model.instruments.get(inst).filter(|lane| lane.pattern.is_some()) {
                    let node = lane.node;
                    self.apply(Intent::SetPatternField { node, field: PatternField::Length, value });
                }
            }
            return;
        }

        let pa = match self.real_param_index() {
            Some(i) => i,
            None => return,
        };
        self.set_plugin_param(&addr, pa, |_| value);
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run(mut ctl: Controller, web: Option<&WebArgs>) -> anyhow::Result<Controller> {
    // Suppress Rust logging for the whole TUI lifetime when stderr is the
    // terminal: the background catalog scan and plugin loads log verbosely and
    // would corrupt the alternate screen. Must be set BEFORE the scan spawns,
    // since that thread starts logging immediately. Logging to a redirected
    // stderr (`tang 2> debug.log`) is left enabled. Restored at teardown.
    let prev_log_level = log::max_level();
    let stderr_is_terminal = std::io::IsTerminal::is_terminal(&std::io::stderr());
    if stderr_is_terminal {
        log::set_max_level(log::LevelFilter::Off);
    }

    // Native plugin code (CLAP preset scanners, VST3 factories, ALSA/JACK, …)
    // writes straight to the stderr fd, bypassing `log`, which would corrupt
    // the alternate screen. Redirect stderr to /dev/null for the TUI lifetime
    // when it's a terminal; when it's redirected (`tang 2> debug.log`) leave it
    // so logs/plugin diagnostics still land in the file. Restored on drop.
    let _stderr_silencer = if stderr_is_terminal {
        crate::tty::StderrSilencer::new()
    } else {
        None
    };

    // Serve the session to browsers too, if asked to. A server that cannot start (the
    // port is taken) is an error before the terminal is taken over; the address is
    // printed on stdout, where a script can find it (the TUI shows it as well).
    let mut web = web.map(|args| Web::start(&mut ctl, args)).transpose()?;
    if let Some(web) = &web {
        let status = web.status();
        println!("Web interface: {}", status.url);
        for warning in status.warnings() {
            println!("{warning}");
        }
    }

    // Scan the plugin catalog on a background thread so the TUI can draw
    // immediately — a full scan instantiates every installed plugin and can
    // take seconds. Results arrive per format and are folded in by
    // `Controller::poll`. The catalog is only needed by the selector popup.
    ctl.catalog.scan();

    let mut s = State::new(ctl);
    if let Some(web) = &web {
        s.set_web(web.status());
    }

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
    let result = TerminalReader::start().and_then(|reader| event_loop(&mut terminal, &mut s, reader.events(), web.as_mut()));

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

    // The controller goes back to the caller, which shuts it down once the audio
    // stream has stopped. (The web server, if there is one, stops when this function
    // returns, and the browsers are told: before the controller is shut down.)
    result.map(|()| s.ctl).map_err(Into::into)
}

/// What happens between two frames, with or without user input: the controller
/// does its housekeeping and folds in what the plugin scan and the audio thread
/// have reported, and the view catches up with what changed.
fn tick(s: &mut State) {
    tick_at(s, Instant::now());
}

/// `tick`, at the time `now`.
fn tick_at(s: &mut State, now: Instant) {
    let polled = s.ctl.poll();
    if polled.model_changed {
        s.rebuild_tree();
    }
    if polled.catalog_changed {
        s.refresh_selector();
    }
    s.clip.update(s.ctl.telemetry.count(telemetry::MASTER.clips), now);
    // (A node that a browser took out, or that the cursor is not on any more, is not listened to.)
    s.sync_strip();
}

/// How far the limiter has to have brought the sound down, in decibels, for the status bar to say so.
const LIMIT_LIGHT_FROM: f32 = 0.1;

/// How often the loop wakes by itself: to fold in what the audio thread and the
/// catalog scan have reported, and to redraw what moves (the meters, the CLIP
/// light, the held notes).
const TICK: Duration = Duration::from_millis(100);

/// How long the loop waits between two turns of the plugins' windows while one is open (`Controller::pump_windows`, which itself waits for an event for a moment): short enough for a window that answers the mouse.
const WINDOW_TURN: Duration = Duration::from_millis(1);

/// Run the interface until the user quits: draw, then wait for `input` (the terminal's
/// events, from a `TerminalReader`), the requests of the browsers if the session is
/// served (`web`), or the tick; handle what came, and draw again. The events that are
/// waiting when the loop wakes are all handled before the next frame.
///
/// The browsers and the terminal act on the one controller from here, one thing at a
/// time. What a browser changes is reacted to like what the TUI itself changed, and
/// what the TUI changes other than by intents is found by `resync` when it has handled
/// its input and told to the browsers.
fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    s: &mut State,
    input: &Receiver<io::Result<Event>>,
    mut web: Option<&mut Web>,
) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    let ticks = crossbeam_channel::tick(TICK);
    let mut web_requests = web.as_deref().map(Web::requests);
    // Whether the frame is drawn before the loop waits again: not after a turn of the plugins' windows (below), which is no news for the screen.
    let mut draw = true;
    loop {
        if draw {
            tick(s);
            if let Some(web) = web.as_deref_mut() {
                web.refresh(&mut s.ctl);
                s.web = Some(web.status());
            }

            render(terminal, s)?;
        }
        if s.quit {
            break;
        }

        let requests = web_requests.clone().unwrap_or_else(crossbeam_channel::never);
        // While a plugin's own window is open (step 112) its events are turned between the others, a few milliseconds apart; with none open, and while the plugin scan goes (f179), this never comes.
        let windows = if s.ctl.windows_open() { crossbeam_channel::after(WINDOW_TURN) } else { crossbeam_channel::never() };
        draw = true;
        crossbeam_channel::select! {
            recv(input) -> event => {
                let Ok(event) = event else {
                    return Err(io::Error::other("the terminal reader has stopped"));
                };
                process_event_showing_the_load(terminal, s, event?)?;
                while let Ok(event) = input.try_recv() {
                    process_event_showing_the_load(terminal, s, event?)?;
                }
                web::resync(&mut s.ctl);
            }
            recv(requests) -> request => match (request, web.as_deref_mut()) {
                (Ok(request), Some(web)) => {
                    for applied in web.serve(&mut s.ctl, request) {
                        s.react_to(&applied);
                    }
                }
                // The server is gone: there is nothing more to wait for from it.
                _ => web_requests = None,
            },
            recv(ticks) -> _ => {}
            recv(windows) -> _ => {
                // (What a knob in a window did is applied by the controller's `poll`, here and not only at the next tick, so that the sound follows the hand.)
                s.ctl.pump_windows();
                if s.ctl.poll().model_changed {
                    s.rebuild_tree();
                }
                draw = false;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Event processing
// ---------------------------------------------------------------------------

/// Whether a popup, or the filter of the parameters, has the keys: they go to it and not to the chain.
fn popup_is_open(s: &State) -> bool {
    s.selector.is_some()
        || s.target_selector.is_some()
        || s.modulate.is_some()
        || s.group_assign.is_some()
        || s.ear_popup.is_some()
        || s.preset_selector.is_some()
        || s.scale_selector.is_some()
        || s.bpm_editing.is_some()
        || s.gain_editing.is_some()
        || s.cable_editing.is_some()
        || s.save_as.is_some()
        || s.save_module.is_some()
        || s.send_cc.is_some()
        || s.editing.is_some()
        || s.range_edit.is_some()
        || s.rename_group.is_some()
        || s.param_filtering
}

/// Handle one event of the terminal, and what it asked that waits for a frame (`run_pending`): the keys of a test have no frame to draw between the two.
#[cfg(test)]
fn process_event(s: &mut State, ev: Event) {
    handle_event(s, ev);
    run_pending(s);
    s.sync_strip();
}

/// What the loop does with an event: `process_event`, with a frame drawn between its two halves when the first asked for something that takes a while. A plugin
/// can take a while to load, and the loop is blocked for as long as it takes, so the frame says what is going on (a box: "Loading Pianoteq 9…") before the load.
fn process_event_showing_the_load<B: Backend>(terminal: &mut Terminal<B>, s: &mut State, ev: Event) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    handle_event(s, ev);
    if s.pending_load.is_some() {
        render(terminal, s)?;
    }
    run_pending(s);
    Ok(())
}

/// Do what an event asked that waited for a frame: ask for the plugin that was chosen (a refusal is the controller's, and says why).
fn run_pending(s: &mut State) {
    if let Some(PendingLoad { intent, .. }) = s.pending_load.take() {
        s.apply(intent);
    }
}

fn handle_event(s: &mut State, ev: Event) {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // A letter with Control held is no text, and no key of a popup: it is not put in as the letter (Ctrl+Z, which undoes in the chain, typed a z), and
            // Ctrl+C and Ctrl+Q leave from here as from the chain. (Windows reports AltGr, which makes `@` and `\` on some keyboards, as Control with Alt: that is text.)
            if popup_is_open(s) && matches!(key.code, KeyCode::Char(_)) && key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT) {
                if matches!(key.code, KeyCode::Char('q' | 'c')) {
                    s.quit = true;
                }
                return;
            }
            if s.selector.is_some() {
                handle_selector_key(s, key.code);
            } else if s.target_selector.is_some() {
                handle_target_selector_key(s, key.code);
            } else if s.modulate.is_some() {
                handle_modulate_key(s, key.code);
            } else if s.group_assign.is_some() {
                handle_group_assign_key(s, key.code);
            } else if s.ear_popup.is_some() {
                handle_ear_key(s, key.code);
            } else if s.preset_selector.is_some() {
                handle_preset_selector_key(s, key.code);
            } else if s.scale_selector.is_some() {
                handle_scale_selector_key(s, key.code);
            } else if s.bpm_editing.is_some() {
                handle_bpm_edit_key(s, key.code);
            } else if s.gain_editing.is_some() {
                handle_gain_edit_key(s, key.code);
            } else if s.cable_editing.is_some() {
                handle_cable_edit_key(s, key.code);
            } else if s.save_as.is_some() {
                handle_save_as_key(s, key.code);
            } else if s.save_module.is_some() {
                handle_save_module_key(s, key.code);
            } else if s.send_cc.is_some() {
                handle_send_cc_key(s, key.code);
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
                || s.ear_popup.is_some()
                || s.preset_selector.is_some()
                || s.scale_selector.is_some()
                || s.editing.is_some()
                || s.range_edit.is_some()
                || s.rename_group.is_some()
                || s.bpm_editing.is_some()
                || s.gain_editing.is_some()
                || s.cable_editing.is_some()
                || s.save_as.is_some()
                || s.save_module.is_some()
                || s.send_cc.is_some()
            {
                if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                    s.selector = None;
                    s.target_selector = None;
                    s.modulate = None;
                    s.group_assign = None;
                    s.ear_popup = None;
                    s.preset_selector = None;
                    s.scale_selector = None;
                    s.editing = None;
                    s.range_edit = None;
                    s.rename_group = None;
                    s.bpm_editing = None;
                    s.gain_editing = None;
                    s.cable_editing = None;
                    s.save_as = None;
                    s.save_module = None;
                    s.send_cc = None;
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

fn handle_ear_key(s: &mut State, code: KeyCode) {
    let es = s.ear_popup.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.ear_popup = None,
        KeyCode::Enter => s.confirm_ear_popup(),
        KeyCode::Up => {
            es.filter.list.up();
            es.filter.list.ensure_visible(20);
        }
        KeyCode::Down => {
            es.filter.list.down();
            es.filter.list.ensure_visible(20);
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

/// The number typed into a popup, if it is one. `str::parse` takes `nan` and `inf` for
/// numbers, and a NaN is not stopped by `clamp`: it would get into the model, and from
/// there to the audio thread and to every browser (JSON has no NaN).
fn parse_number(text: &str) -> Option<f32> {
    text.trim().parse::<f32>().ok().filter(|value| value.is_finite())
}

fn handle_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.editing = None,
        KeyCode::Enter => {
            if let Some(val) = parse_number(&edit.input.value) {
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
            if let Some(node) = s.ctl.model.instruments.get(inst).map(|lane| lane.node) {
                s.apply(Intent::SetRange { node, range: range.map(|(low, high)| NoteRange { low, high }) });
            }
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
            let name = rg.input.value.clone();
            let group = rg.group;
            // Empty clears the custom name (falls back to "Group N"). The name
            // is display-only, so no audio-thread command is needed.
            if let Some(node) = s.ctl.model.groups.get(group).map(|g| g.node) {
                // (A name that is refused keeps the popup open, as a range that is not one does.)
                if s.apply(Intent::RenameGroup { node, name }).is_none() {
                    return;
                }
            }
            s.rename_group = None;
        }
        KeyCode::Backspace => rg.input.backspace(),
        KeyCode::Delete => rg.input.delete(),
        KeyCode::Left => rg.input.move_left(),
        KeyCode::Right => rg.input.move_right(),
        KeyCode::Home => rg.input.home(),
        KeyCode::End => rg.input.end(),
        // (The controller refuses a name longer than `MAX_NAME`: there is no typing past it.)
        KeyCode::Char(ch) if rg.input.value.chars().count() < MAX_NAME => rg.input.insert(ch),
        _ => {}
    }
}

fn handle_bpm_edit_key(s: &mut State, code: KeyCode) {
    let edit = s.bpm_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.bpm_editing = None,
        KeyCode::Enter => {
            if let Some(bpm) = parse_number(&edit.input.value) {
                // (The controller holds it to the tempos there can be, which the popup says.)
                let _ = s.apply(Intent::SetBpm { bpm });
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
            let (node, gain) = (ge.node, ge.gain);
            if let Some(value) = parse_number(&ge.edit.input.value) {
                s.apply(Intent::SetGain { node, gain, value });
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

fn handle_cable_edit_key(s: &mut State, code: KeyCode) {
    let ce = s.cable_editing.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.cable_editing = None,
        KeyCode::Enter => {
            let edge = ce.edge;
            if let Some(gain) = parse_number(&ce.edit.input.value) {
                if let Err(why) = s.try_apply(Intent::SetCableGain { edge, gain }) {
                    s.patch.notice = Some(why.to_string());
                }
            }
            s.cable_editing = None;
        }
        KeyCode::Backspace => ce.edit.input.backspace(),
        KeyCode::Delete => ce.edit.input.delete(),
        KeyCode::Left => ce.edit.input.move_left(),
        KeyCode::Right => ce.edit.input.move_right(),
        KeyCode::Home => ce.edit.input.home(),
        KeyCode::End => ce.edit.input.end(),
        KeyCode::Char(ch) => ce.edit.input.insert(ch),
        _ => {}
    }
}

/// Parses `<cc> <value> [channel]` into a Control Change message. Channel is
/// 1-based and defaults to 1, which the router duplicates to every instrument.
fn parse_cc_message(input: &str) -> Result<[u8; 3], String> {
    let fields: Vec<&str> = input.split_whitespace().collect();
    if fields.len() < 2 || fields.len() > 3 {
        return Err("expected: <cc> <value> [channel]".into());
    }
    let num = |s: &str, name: &str, min: u8, max: u8| match s.parse::<u8>() {
        Ok(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(format!("{name} must be {min}-{max}")),
    };
    let cc = num(fields[0], "CC", 0, 127)?;
    let value = num(fields[1], "value", 0, 127)?;
    let channel = match fields.get(2) {
        Some(f) => num(f, "channel", 1, 16)?,
        None => 1,
    };
    Ok([0xB0 | (channel - 1), cc, value])
}

fn handle_send_cc_key(s: &mut State, code: KeyCode) {
    let Some(sc) = s.send_cc.as_mut() else { return };
    match code {
        KeyCode::Esc => s.send_cc = None,
        KeyCode::Enter => match parse_cc_message(&sc.input.value) {
            Ok(bytes) => match s.ctl.note_input().send(&bytes) {
                Delivery::Sent => {
                    s.ctl.midi_monitor.record(&bytes);
                    s.last_cc = sc.input.value.trim().to_string();
                    s.send_cc = None;
                }
                other => sc.error = Some(format!("not sent: {other:?}")),
            },
            Err(e) => sc.error = Some(e),
        },
        KeyCode::Backspace => {
            sc.error = None;
            sc.input.backspace();
        }
        KeyCode::Delete => {
            sc.error = None;
            sc.input.delete();
        }
        KeyCode::Left => sc.input.move_left(),
        KeyCode::Right => sc.input.move_right(),
        KeyCode::Home => sc.input.home(),
        KeyCode::End => sc.input.end(),
        KeyCode::Char(ch) => {
            sc.error = None;
            sc.input.insert(ch);
        }
        _ => {}
    }
}

fn handle_save_as_key(s: &mut State, code: KeyCode) {
    let sa = s.save_as.as_mut().unwrap();
    match code {
        KeyCode::Esc => s.save_as = None,
        KeyCode::Enter => {
            let name = sa.input.value.trim().to_string();
            if name.is_empty() {
                return; // keep popup open until a name is given
            }
            // (The controller's intent: the name is checked as the page's is, and the browsers are told what the session is called now.)
            match s.ctl.apply(Intent::SaveAs { name }) {
                // Subsequent Ctrl+S now targets the new file.
                Ok(_) => s.save_as = None,
                // Keep the popup (and the original session path) and show the
                // error so the user can fix the name or cancel.
                Err(e) => s.save_as.as_mut().unwrap().error = Some(e.to_string()),
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

/// The keys of the popup that names the module that a chain of effects is saved as: Enter asks the controller (the intent a browser's button asks, `SaveModule`), which says what a name may be and
/// writes the file; a name that is refused stays in the popup with the reason.
fn handle_save_module_key(s: &mut State, code: KeyCode) {
    let Some(sm) = s.save_module.as_mut() else { return };
    match code {
        KeyCode::Esc => s.save_module = None,
        KeyCode::Enter => {
            let name = sm.input.value.trim().to_string();
            if name.is_empty() {
                return; // keep popup open until a name is given
            }
            match s.ctl.apply(Intent::SaveModule { node: sm.node, name }) {
                Ok(_) => s.save_module = None,
                // Keep the popup and show why, so that the name can be fixed or the popup left.
                Err(e) => {
                    if let Some(sm) = s.save_module.as_mut() {
                        sm.error = Some(e.to_string());
                    }
                }
            }
        }
        KeyCode::Backspace => {
            sm.error = None;
            sm.input.backspace();
        }
        KeyCode::Delete => {
            sm.error = None;
            sm.input.delete();
        }
        KeyCode::Left => sm.input.move_left(),
        KeyCode::Right => sm.input.move_right(),
        KeyCode::Home => sm.input.home(),
        KeyCode::End => sm.input.end(),
        KeyCode::Char(ch) => {
            sm.error = None;
            sm.input.insert(ch);
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
                .ctl
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
            if let Err(e) = s.ctl.save() {
                log::error!("Failed to save session: {e}");
            }
        }
        // Ctrl+Shift+Z and Ctrl+Y — redo; Ctrl+Z — undo: the controller's history of what was set (the browser's buttons ask for
        // the same). Nothing to undo or redo is logged and changes nothing. (Checked in this order, as the save keys are.)
        KeyCode::Char('z' | 'Z')
            if modifiers.contains(KeyModifiers::CONTROL) && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            s.apply(Intent::Redo);
        }
        KeyCode::Char('y') if modifiers.contains(KeyModifiers::CONTROL) => {
            s.apply(Intent::Redo);
        }
        KeyCode::Char('z') if modifiers.contains(KeyModifiers::CONTROL) => {
            s.apply(Intent::Undo);
        }
        // Ctrl+P — panic: let go of every note that sounds (the pedal up, each held note, all notes off and all sound off on every channel), which the browser's
        // button does too. It is no change of the session and has no step of the history.
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
            let lost = s.ctl.note_input().panic();
            if lost > 0 {
                log::warn!("Panic: the channel to the audio thread was full, and {lost} messages were lost");
            }
        }
        KeyCode::Char('1') => s.active_tab = TAB_SESSION,
        KeyCode::Char('2') => s.active_tab = TAB_PIANO,
        KeyCode::Char('3') => s.active_tab = TAB_SCOPE,
        KeyCode::Char('4') => s.active_tab = TAB_PATCH,
        KeyCode::Char('5') => s.active_tab = TAB_HELP,
        KeyCode::Tab => s.active_tab = (s.active_tab + 1) % TAB_NAMES.len(),
        KeyCode::BackTab => s.active_tab = (s.active_tab + TAB_NAMES.len() - 1) % TAB_NAMES.len(),

        // ---- Patch tab: every other key is the matrix's ----
        _ if s.active_tab == TAB_PATCH => handle_patch_key(s, code, modifiers),

        // ---- Piano tab ----
        KeyCode::Char('k') if s.active_tab == TAB_PIANO => {
            s.open_scale_selector();
        }
        KeyCode::Char('l') if s.active_tab == TAB_PIANO => {
            let new_mode = match s.ctl.piano_filter.mode() {
                PianoMode::Highlight => PianoMode::Locked,
                PianoMode::Locked => PianoMode::Highlight,
            };
            if s.apply(Intent::SetLocked { locked: new_mode == PianoMode::Locked }).is_some() {
                log::info!("Piano mode → {}", new_mode.label());
            }
        }
        KeyCode::Char('[') if s.active_tab == TAB_PIANO && s.piano_view_octave > 0 => {
            s.piano_view_octave -= 1;
        }
        KeyCode::Char(']') if s.active_tab == TAB_PIANO && s.piano_view_octave < 8 => {
            s.piano_view_octave += 1;
        }

        // ---- Session tab: the scope of the node under the chain cursor, under the chain and the parameters ----
        KeyCode::Char('o') if s.active_tab == TAB_SESSION => s.strip.on = !s.strip.on,
        KeyCode::Char('O') if s.active_tab == TAB_SESSION => s.strip.scope.toggle_kind(),

        // ---- Scope tab ----
        KeyCode::Char('v') if s.active_tab == TAB_SCOPE => s.scope.toggle_kind(),
        KeyCode::Left if s.active_tab == TAB_SCOPE => s.scope.faster(),
        KeyCode::Right if s.active_tab == TAB_SCOPE => s.scope.slower(),

        // 'i' — replace instrument on the selected instrument slot.
        KeyCode::Char('i') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(TreeAddress::Instrument(_)) = s.selected_address().copied() {
                s.open_selector(SelectorMode::Instrument);
            }
        }

        // 'n' — add a new instrument lane (full range) and immediately open the
        // plugin selector to fill it. The lane defaults to builtin:osc, so it
        // makes sound even if the selector is dismissed without a pick. Set its
        // key range afterwards with 'R'. This is how you build a keyboard split.
        // (The lane is made by the intent a browser's "+ Instrument" asks for, a step
        // of the history: the catalog has to name the oscillator, as the scan has long
        // before anyone presses a key; if it does not, there is no lane to fill.)
        KeyCode::Char('n') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if s.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).is_some() {
                // Move the cursor onto the new lane so the selector fills it.
                let new_idx = s.ctl.model.instruments.len().saturating_sub(1);
                if let Some(pos) = s.tree_entries.iter().position(
                    |e| matches!(e.address, TreeAddress::Instrument(i) if i == new_idx),
                ) {
                    s.chain_state.selected = pos;
                    s.sync_param_state();
                }
                s.open_selector(SelectorMode::Instrument);
            }
        }

        // 'N' — add an effect that stands by itself (step 87): the selector lists the
        // effects, and the one that is chosen goes in a lane of its own that has no
        // instrument (`AddFreeEffect`, a step of the history), where cables bring what
        // it is fed. Wherever the cursor is.
        KeyCode::Char('N') if s.active_tab == TAB_SESSION && !s.focus_params => {
            s.open_selector(SelectorMode::FreeEffect);
        }

        // 'K' — keep the chain of effects of the lane the cursor is of as a module (step 104): a popup asks for the name, and the intent `SaveModule` writes the chain to the
        // library, whose modules `N` lists with the effects and adds, each as a lane of its own. A lane that has no effect has no chain: nothing opens.
        KeyCode::Char('K') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(lane) = s.selected_inst().and_then(|inst| s.ctl.model.instruments.get(inst)) {
                if !lane.effects.is_empty() {
                    s.save_module = Some(SaveModuleState { node: lane.node, input: TextInputState::new(""), error: None });
                }
            }
        }

        // 'R' — set the key range of the selected instrument, or rename the
        // selected group (range doesn't apply to groups).
        KeyCode::Char('R') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(TreeAddress::Group(group)) = s.selected_address().copied() {
                let initial = s
                    .ctl
                    .model
                    .groups
                    .get(group)
                    .and_then(|g| g.name.clone())
                    .unwrap_or_default();
                let mut input = TextInputState::new(&initial);
                input.end();
                s.rename_group = Some(RenameGroupState { group, input });
            } else if let Some(inst) = s.selected_inst() {
                let initial = s
                    .ctl
                    .model
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
        KeyCode::Char('a') if s.active_tab == TAB_SESSION && !s.focus_params => {
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
                Some(TreeAddress::FreeModulator { .. }) => {}
                None => {}
            }
        }

        // 'm' (param focus) — modulate the selected parameter: open a popup to
        // bind it to a new or existing modulator.
        KeyCode::Char('m') if s.active_tab == TAB_SESSION && s.focus_params => {
            match s.selected_address().copied() {
                // A plugin's parameter (an instrument's, an effect's or a bus effect's), or a group's volume or pan → create/attach a modulator.
                Some(TreeAddress::Instrument(_) | TreeAddress::Effect { .. } | TreeAddress::GroupEffect { .. } | TreeAddress::Group(_)) => {
                    s.open_modulate();
                }
                // A modulator's own param (rate / ADSR / a target depth) →
                // cross-modulate it with a sibling modulator.
                Some(TreeAddress::FreeModulator { .. }) => {
                    s.open_modulate_cross_mod();
                }
                _ => {}
            }
        }

        // 'l' (param focus) — MIDI learn: bind the next CC / pitch-bend to the
        // selected parameter (plugin param or modulator field).
        KeyCode::Char('l') if s.active_tab == TAB_SESSION && s.focus_params => {
            s.learn_selected_param();
        }

        // 'm' (chain focus) — add an LFO, one of the session's own that hears the lane or the group that the cursor is in. (In param focus, `m` modulates the selected parameter —
        // handled above.)
        KeyCode::Char('m') if s.active_tab == TAB_SESSION && !s.focus_params => {
            // On a group, a group-bus node or a modulator that hears a group, add one that hears the group; on an instrument-side node or a modulator that hears a lane, one that hears the lane;
            // on a modulator that hears every note, another that does.
            let address = s.selected_address().copied();
            let owner = address.and_then(|a| match s.group_of(a) {
                Some(group) => s.ctl.model.groups.get(group).map(|bus| bus.node),
                None => s.lane_of(a).and_then(|inst| s.ctl.model.instruments.get(inst)).map(|lane| lane.node),
            });
            if owner.is_some() || matches!(address, Some(TreeAddress::FreeModulator { .. })) {
                s.add_free_modulator(owner);
            }
        }

        // 'F' (chain focus) — add a free modulator: in no lane and no group, wherever the cursor is, and the cursor goes onto it (`AddFreeModulator`, a step of
        // the history). Its cables (`t`) go to a parameter of any plugin of the session.
        KeyCode::Char('F') if s.active_tab == TAB_SESSION && !s.focus_params => {
            s.add_free_modulator(None);
        }

        // 'h' (chain focus) — choose whose notes a free modulator hears: every note, a lane's or a group's (`SetModulatorEar`, a step of the history), via a popup.
        KeyCode::Char('h') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(TreeAddress::FreeModulator { index }) = s.selected_address().copied() {
                s.open_ear_popup(index);
            }
        }

        // 'g' (chain focus) — assign the selected instrument to a group
        // (new, existing, or ungroup), via a popup.
        KeyCode::Char('g') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(TreeAddress::Instrument(inst)) = s.selected_address().copied() {
                s.open_group_assign(inst);
            }
        }

        // 'u' (chain focus) — open the selected plugin's own window (`OpenEditor`, step 112): the face that the plugin draws for itself, in a window on this machine that takes the keyboard, no step of the history. A plugin that has
        // none, a machine that has no screen and a scan that is still going are refused, and said in the log.
        KeyCode::Char('u') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let node = match s.selected_address().copied() {
                Some(TreeAddress::Instrument(inst)) => s.ctl.model.instruments.get(inst).map(|lane| lane.node),
                Some(TreeAddress::Effect { inst, index }) => s.ctl.model.instruments.get(inst).and_then(|lane| lane.effects.get(index)).map(|slot| slot.node),
                Some(TreeAddress::GroupEffect { group, index }) => s.ctl.model.groups.get(group).and_then(|bus| bus.effects.get(index)).map(|slot| slot.node),
                _ => None,
            };
            if let Some(node) = node {
                s.apply(Intent::OpenEditor { node });
            }
        }

        // 'p' — open preset selector for the selected plugin slot.
        KeyCode::Char('p') if s.active_tab == TAB_SESSION && !s.focus_params => {
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
        KeyCode::Char('t') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(TreeAddress::FreeModulator { index }) = s.selected_address().copied() {
                s.open_free_target_selector(index);
            }
        }

        // Ctrl+R — clear the pattern of the lane (on its Pattern or Instrument node), as `d` on the pattern does. (Before `r`, which records, whatever
        // the modifiers: the help said Ctrl+R clears, and it recorded.)
        KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) && s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some(TreeAddress::Pattern(inst) | TreeAddress::Instrument(inst)) = s.selected_address().copied() {
                if s.ctl.model.instruments.get(inst).is_some_and(|lane| lane.pattern.is_some()) {
                    s.clear_pattern(inst);
                }
            }
        }

        // 'r' — toggle pattern recording (on Pattern or Instrument node).
        KeyCode::Char('r') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let target = match s.selected_address().copied() {
                Some(TreeAddress::Pattern(inst)) => Some(inst),
                Some(TreeAddress::Instrument(inst)) => Some(inst),
                _ => None,
            };
            if let Some(lane) = target.and_then(|inst| s.ctl.model.instruments.get(inst)) {
                let (node, going) = (lane.node, lane.pattern.as_ref().is_some_and(|pattern| pattern.recording));
                // (A recording is stopped by a second `r`; the tempo and the length are sent first when it starts.)
                s.apply(Intent::SetRecording { node, recording: !going });
            }
        }

        KeyCode::Char('c') if s.active_tab == TAB_SESSION => {
            let mut input = TextInputState::new(&s.last_cc);
            input.end();
            s.send_cc = Some(SendCcState { input, error: None });
        }

        // 'b' — edit BPM.
        KeyCode::Char('b') if s.active_tab == TAB_SESSION && !s.focus_params => {
            s.bpm_editing = Some(EditState {
                input: TextInputState::new(&format!("{:.0}", s.ctl.model.bpm)),
                param_name: "BPM".to_string(),
                param_min: BPM_MIN,
                param_max: BPM_MAX,
            });
        }

        // 'L' — put the limiter on the output mix in the sound, or take it out (the master's, whatever the cursor is on: step 102).
        KeyCode::Char('L') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let on = !s.ctl.model.master.limiter;
            s.apply(Intent::SetLimiter { on });
        }

        // 'C' — set the limiter's ceiling, in decibels below full scale.
        KeyCode::Char('C') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let ceiling = s.ctl.model.master.ceiling;
            s.gain_editing = Some(GainEditState::new(NodeId::MASTER, Gain::LimiterCeiling, ceiling, "limiter ceiling (dB below full scale, -24 to 0)"));
        }

        // 'V' — set the master volume, the gain on the output mix before the limiter.
        KeyCode::Char('V') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let volume = s.ctl.model.master.volume;
            s.gain_editing = Some(GainEditState::new(NodeId::MASTER, Gain::MasterVolume, volume, "master volume (1.0 = unity)"));
        }

        // 'x' — edit the dry/wet mix of the selected effect (host-side blend).
        KeyCode::Char('x') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let effect = match s.selected_address().copied() {
                Some(TreeAddress::Effect { inst, index }) => {
                    s.ctl.model.instruments.get(inst).and_then(|n| n.effects.get(index))
                }
                Some(TreeAddress::GroupEffect { group, index }) => {
                    s.ctl.model.groups.get(group).and_then(|g| g.effects.get(index))
                }
                _ => None,
            };
            if let Some(fx) = effect {
                s.gain_editing = Some(GainEditState::new(fx.node, Gain::Mix, fx.mix, "mix (0=dry, 1=wet)"));
            }
        }

        // 'v' — edit the output volume of the selected instrument or group
        // (host-side gain, applied before effects). UI range 0–4.
        KeyCode::Char('v') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let target = match s.selected_address().copied() {
                Some(TreeAddress::Group(group)) => {
                    s.ctl.model.groups.get(group).map(|g| (g.node, Gain::GroupVolume, g.volume))
                }
                Some(addr) => s
                    .lane_of(addr)
                    .and_then(|inst| s.ctl.model.instruments.get(inst))
                    .map(|n| (n.node, Gain::Volume, n.volume)),
                None => None,
            };
            if let Some((node, gain, volume)) = target {
                s.gain_editing = Some(GainEditState::new(node, gain, volume, "volume (1.0 = unity)"));
            }
        }

        // 'M' / 'S' (chain focus) — mute or solo the lane or the group that the selected entry is of (the instrument,
        // its effects, its modulators and its pattern are the lane's; a group's header, its bus effects and its
        // modulators are the group's), as the page's M and S buttons do.
        KeyCode::Char('M') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some((node, muted, _, _)) = s.selected_lane_or_group() {
                s.apply(Intent::SetMute { node, muted: !muted });
            }
        }
        KeyCode::Char('S') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some((node, _, solo, _)) = s.selected_lane_or_group() {
                s.apply(Intent::SetSolo { node, solo: !solo });
            }
        }
        // 'w' (chain focus) — cut the way out of the lane or the group that the selected entry is of, or give it back: where its end goes (its group's bus or the mix),
        // or nowhere but its audio cables. A way out that would make the audio go round is refused (logged), as the page's is.
        KeyCode::Char('w') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some((node, _, _, route)) = s.selected_lane_or_group() {
                s.apply(Intent::SetRoute { node, route: if route == Route::Out { Route::None } else { Route::Out } });
            }
        }

        // 'f' (chain focus) — cut the link into the effect that the cursor is on from the stage before it in its chain (the instrument, the effect before it, or the sum of the bus), or give
        // it back (the intent `SetFeed`): the effect is then fed by its audio cables and by nothing else. A feed that would make the audio go round is refused (logged), as the page's is.
        KeyCode::Char('f') if s.active_tab == TAB_SESSION && !s.focus_params => {
            if let Some((node, fed)) = s.selected_effect_feed() {
                s.apply(Intent::SetFeed { node, feed: if fed { Feed::None } else { Feed::Chain } });
            }
        }

        KeyCode::Char('d') if s.active_tab == TAB_SESSION && !s.focus_params => {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                let addr = s.tree_entries[sel].address;
                // (A node is taken out by the intent that a browser's "x" asks for too.)
                match addr {
                    TreeAddress::Effect { inst, index } => {
                        let node = s.ctl.model.instruments.get(inst).and_then(|lane| lane.effects.get(index)).map(|fx| fx.node);
                        s.remove_node(node);
                    }
                    TreeAddress::Instrument(inst) => {
                        // (The instrument is taken out by the intent that the page's "Remove only the instrument" asks, the lane keeping its effects and the rest; a lane that has
                        // none is taken out by the intent that takes out a lane.)
                        let lane = s.ctl.model.instruments.get(inst).map(|lane| (lane.node, lane.instrument.is_some()));
                        match lane {
                            Some((node, true)) => {
                                s.apply(Intent::ClearInstrument { node });
                            }
                            Some((node, false)) => s.remove_node(Some(node)),
                            None => {}
                        }
                    }
                    TreeAddress::Pattern(inst) => s.clear_pattern(inst),
                    TreeAddress::GroupEffect { group, index } => {
                        let node = s.ctl.model.groups.get(group).and_then(|bus| bus.effects.get(index)).map(|fx| fx.node);
                        s.remove_node(node);
                    }
                    TreeAddress::FreeModulator { index } => {
                        let node = s.ctl.model.free_modulators.get(index).map(|m| m.node);
                        s.remove_node(node);
                    }
                    TreeAddress::Group(group) => {
                        // Delete the group: members become ungrouped, higher
                        // group indices shift down.
                        let node = s.ctl.model.groups.get(group).map(|bus| bus.node);
                        s.remove_node(node);
                    }
                }
            }
        }

        // Enter: focus params or open value editor.
        KeyCode::Enter if s.active_tab == TAB_SESSION => {
            if s.focus_params {
                let sel = s.chain_state.selected;
                let pa = s.param_state.selected;
                if sel < s.tree_entries.len() {
                    let addr = s.tree_entries[sel].address;
                    match addr {
                        TreeAddress::FreeModulator { index } => {
                            let edit = s.ctl.model.free_modulators.get(index).and_then(|m| modulator_edit_state(m, pa));
                            if edit.is_some() {
                                s.editing = edit;
                            }
                        }
                        TreeAddress::Pattern(inst) => {
                            let pat = s.ctl.model.instruments.get(inst)
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
                        TreeAddress::Group(group) => {
                            // Group bus pane: 0 = volume, 1 = pan.
                            if let Some(g) = s.ctl.model.groups.get(group) {
                                let (val, name, min, max) = match pa {
                                    0 => (g.volume, "volume (1.0 = unity)", 0.0, 4.0),
                                    1 => (g.pan, "pan (-1 L … +1 R)", -1.0, 1.0),
                                    _ => return,
                                };
                                s.editing = Some(EditState {
                                    input: TextInputState::new(&format!("{val:.2}")),
                                    param_name: name.to_string(),
                                    param_min: min,
                                    param_max: max,
                                });
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
        KeyCode::Esc if s.active_tab == TAB_SESSION => {
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
        KeyCode::Char('/') if s.active_tab == TAB_SESSION && s.focus_params && !s.param_filtering => {
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
        KeyCode::Left if s.active_tab == TAB_SESSION && s.focus_params && !s.param_filtering => {
            s.adjust_param(-1.0, modifiers);
        }
        KeyCode::Right if s.active_tab == TAB_SESSION && s.focus_params && !s.param_filtering => {
            s.adjust_param(1.0, modifiers);
        }

        // Reorder effects / reorder instruments.
        KeyCode::Up
            if s.active_tab == TAB_SESSION
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { inst, index } if index > 0 => {
                        let node = s.ctl.model.instruments.get(inst).and_then(|lane| lane.effects.get(index)).map(|fx| fx.node);
                        s.move_effect_to(node, index - 1);
                    }
                    TreeAddress::Instrument(inst) if inst > 0 => {
                        s.ctl.swap_lanes(inst, inst - 1);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Instrument(inst - 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern(inst) if inst > 0 => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SwapPatterns {
                            inst_a: inst,
                            inst_b: inst - 1,
                        });
                        let a_pat = s.ctl.model.instruments[inst].pattern.take();
                        let b_pat = s.ctl.model.instruments[inst - 1].pattern.take();
                        s.ctl.model.instruments[inst].pattern = b_pat;
                        s.ctl.model.instruments[inst - 1].pattern = a_pat;
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern(inst - 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::GroupEffect { group, index } if index > 0 => {
                        let node = s.ctl.model.groups.get(group).and_then(|bus| bus.effects.get(index)).map(|fx| fx.node);
                        s.move_effect_to(node, index - 1);
                    }
                    _ => {}
                }
            }
        }
        KeyCode::Down
            if s.active_tab == TAB_SESSION
                && !s.focus_params
                && modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let sel = s.chain_state.selected;
            if sel < s.tree_entries.len() {
                match s.tree_entries[sel].address {
                    TreeAddress::Effect { inst, index } => {
                        let effect_count = s.ctl.model.instruments.get(inst)
                            .map_or(0, |n| n.effects.len());
                        if index + 1 < effect_count {
                            let node = s.ctl.model.instruments.get(inst).and_then(|lane| lane.effects.get(index)).map(|fx| fx.node);
                            s.move_effect_to(node, index + 1);
                        }
                    }
                    TreeAddress::Instrument(inst) if inst + 1 < s.ctl.model.instruments.len() => {
                        s.ctl.swap_lanes(inst, inst + 1);
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Instrument(inst + 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::Pattern(inst) if inst + 1 < s.ctl.model.instruments.len() => {
                        let _ = s.ctl.cmd_tx.send(GraphCommand::SwapPatterns {
                            inst_a: inst,
                            inst_b: inst + 1,
                        });
                        let a_pat = s.ctl.model.instruments[inst].pattern.take();
                        let b_pat = s.ctl.model.instruments[inst + 1].pattern.take();
                        s.ctl.model.instruments[inst].pattern = b_pat;
                        s.ctl.model.instruments[inst + 1].pattern = a_pat;
                        s.ctl.dirty = true;
                        s.rebuild_tree();
                        let new_addr = TreeAddress::Pattern(inst + 1);
                        if let Some(pos) = s.tree_entries.iter().position(|e| e.address == new_addr) {
                            s.chain_state.selected = pos;
                        }
                        s.sync_param_state();
                    }
                    TreeAddress::GroupEffect { group, index } => {
                        let count = s.ctl.model.groups.get(group).map_or(0, |g| g.effects.len());
                        if index + 1 < count {
                            let node = s.ctl.model.groups.get(group).and_then(|bus| bus.effects.get(index)).map(|fx| fx.node);
                            s.move_effect_to(node, index + 1);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Navigation.
        KeyCode::Up => match s.active_tab {
            TAB_SESSION if s.focus_params => s.param_state.up(),
            TAB_SESSION => {
                s.chain_state.up();
                s.sync_param_state();
            }
            TAB_HELP => s.help_offset = s.help_offset.saturating_sub(1),
            _ => {}
        },
        KeyCode::Down => match s.active_tab {
            TAB_SESSION if s.focus_params => s.param_state.down(),
            TAB_SESSION => {
                s.chain_state.down();
                s.sync_param_state();
            }
            TAB_HELP => s.help_offset += 1,
            _ => {}
        },
        KeyCode::PageUp => match s.active_tab {
            TAB_SESSION if s.focus_params => s.param_state.page_up(20),
            TAB_SESSION => {
                s.chain_state.page_up(20);
                s.sync_param_state();
            }
            TAB_HELP => s.help_offset = s.help_offset.saturating_sub(20),
            _ => {}
        },
        KeyCode::PageDown => match s.active_tab {
            TAB_SESSION if s.focus_params => s.param_state.page_down(20),
            TAB_SESSION => {
                s.chain_state.page_down(20);
                s.sync_param_state();
            }
            TAB_HELP => s.help_offset += 20,
            _ => {}
        },
        _ => {}
    }
}

/// A key on the Patch tab that is none of the global keys: the matrix says what it asks of the controller, which is carried out here, and a refusal is said in the tab.
fn handle_patch_key(s: &mut State, code: KeyCode, modifiers: KeyModifiers) {
    match s.patch.press(&s.ctl.model, code, modifiers) {
        patch_view::Act::None => {}
        patch_view::Act::Notice(text) => s.patch.notice = Some(text),
        patch_view::Act::Apply(intent) => {
            if let Err(why) = s.try_apply(intent) {
                s.patch.notice = Some(why.to_string());
            }
        }
        patch_view::Act::TypeLevel { edge, gain } => s.cable_editing = Some(CableEditState::new(edge, gain)),
    }
}

fn handle_mouse(s: &mut State, kind: MouseEventKind, x: u16, y: u16) {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            s.scrollbar_dragging = false;
            s.param_dragging = false;
            s.param_scrollbar_dragging = false;

            if let Some(tab) = TabBar::tab_at(x, y, s.areas.tab, &tab_labels(s.ctl.dirty), TAB_SEP) {
                s.active_tab = tab;
                return;
            }

            // Action bar.
            if s.active_tab == TAB_SESSION {
                let sel = s.chain_state.selected;
                let addr = s.tree_entries.get(sel).map(|e| &e.address);
                let actions = action_bar_items(addr, s.focus_params);
                if let Some(key) = action_bar_hit(x, y, s.areas.action_bar, &actions) {
                    handle_key(s, KeyCode::Char(key), KeyModifiers::NONE);
                    return;
                }
            }
            if s.active_tab == TAB_PATCH {
                if let Some(at) = action_bar_index(x, y, s.areas.action_bar, &patch_action_items()) {
                    handle_patch_key(s, PATCH_ACTIONS[at].2, KeyModifiers::NONE);
                    return;
                }
            }

            match s.active_tab {
                TAB_PATCH => {
                    s.patch.notice = None;
                    s.patch.click(&s.ctl.model, x, y);
                }
                TAB_SESSION => {
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
                                        s.adjust_param(-1.0, KeyModifiers::NONE);
                                    } else {
                                        s.adjust_param(1.0, KeyModifiers::NONE);
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
                TAB_HELP => {
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
            if s.scrollbar_dragging && s.active_tab == TAB_HELP {
                let total = s.help_lines.len();
                s.help_offset = ScrollView::offset_from_scrollbar(y, s.areas.content, total);
            } else if s.param_scrollbar_dragging && s.active_tab == TAB_SESSION {
                s.param_state.select_from_scrollbar(y, s.areas.param_inner);
            } else if s.param_dragging && s.active_tab == TAB_SESSION {
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
            TAB_SESSION if s.focus_params => {
                for _ in 0..3 { s.param_state.up_nowrap(); }
            }
            TAB_SESSION => {
                for _ in 0..3 { s.chain_state.up_nowrap(); }
                s.sync_param_state();
            }
            TAB_PATCH => s.patch.scroll(&s.ctl.model, -3),
            TAB_HELP => s.help_offset = s.help_offset.saturating_sub(3),
            _ => {}
        },
        MouseEventKind::ScrollDown => match s.active_tab {
            TAB_SESSION if s.focus_params => {
                for _ in 0..3 { s.param_state.down_nowrap(); }
            }
            TAB_SESSION => {
                for _ in 0..3 { s.chain_state.down_nowrap(); }
                s.sync_param_state();
            }
            TAB_PATCH => s.patch.scroll(&s.ctl.model, 3),
            TAB_HELP => s.help_offset += 3,
            _ => {}
        },
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render<B: Backend>(terminal: &mut Terminal<B>, s: &mut State) -> io::Result<()>
where
    B::Error: Send + Sync + 'static,
{
    terminal.draw(|frame| draw(frame, s)).map_err(io::Error::other)?;
    Ok(())
}

/// Draw one frame. Kept free of the terminal backend so the golden-render
/// tests (`golden_tests.rs`) can drive it through ratatui's `TestBackend`.
fn draw(frame: &mut ratatui::Frame, s: &mut State) {
    let area = frame.area();
    // `--debug` carves a MIDI-monitor strip off the bottom (0 rows otherwise).
    let debug_h: u16 = if s.ctl.midi_monitor.is_enabled() { 9 } else { 0 };
    let [tab_area, content_area, action_area, debug_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(debug_h),
    ])
    .areas(area);

    s.areas.tab = tab_area;
    s.areas.content = content_area;
    s.areas.action_bar = action_area;

    let tab_names = tab_labels(s.ctl.dirty);
    frame.render_widget(TabBar::new(&tab_names, s.active_tab), tab_area);

    // The right side of the tab bar: the web interface when there is one, the CLIP
    // light while it is lit, then the BPM. The web label gives way first: it is
    // shortened, or left out, to keep clear of the tabs.
    let bpm_text = format!("{:.0} BPM", s.ctl.model.bpm);
    let clip_text = if s.clip.is_lit() { "CLIP  " } else { "" };
    // The limiter says so while it works: how far it has brought the sound down lately (a reduction under a tenth of a decibel is none).
    let reduction = s.ctl.telemetry.load(telemetry::MASTER_REDUCTION);
    let limit_text = if s.ctl.model.master.limiter && reduction >= LIMIT_LIGHT_FROM { format!("LIM -{reduction:.1} dB  ") } else { String::new() };
    let tabs_width = tab_names.iter().map(|name| name.chars().count()).sum::<usize>() + 3 * (tab_names.len() - 1);
    // (The limiter's light gives way, as the web label does, when it would touch the tabs: the light and the tempo are what a narrow terminal keeps.)
    let limit_text = if (tab_area.width as usize) > tabs_width + 2 + 2 + limit_text.len() + clip_text.len() + bpm_text.len() { limit_text } else { String::new() };
    // (The tabs, a gap of two, the label, two blanks after it, the rest, and a blank at the edge.)
    let room = (tab_area.width as usize).saturating_sub(tabs_width + 2 + 2 + limit_text.len() + clip_text.len() + bpm_text.len() + 1);
    let web_text = s.web.as_ref().and_then(|web| web.label(room)).map(|label| format!("{label}  ")).unwrap_or_default();
    let status_width = (web_text.len() + limit_text.len() + clip_text.len() + bpm_text.len()) as u16;
    if tab_area.width > status_width + 2 {
        let status_area = Rect {
            x: tab_area.right() - status_width - 1,
            y: tab_area.y,
            width: status_width + 1,
            height: 1,
        };
        let clip_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        let limit_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(ratatui::text::Line::from(vec![
                ratatui::text::Span::raw(web_text),
                ratatui::text::Span::styled(limit_text, limit_style),
                ratatui::text::Span::styled(clip_text, clip_style),
                ratatui::text::Span::raw(bpm_text),
            ]))
            .style(Style::default().fg(DIM)),
            status_area,
        );
    }

    match s.active_tab {
        TAB_SESSION => {
            // The scope of the node under the cursor, if it is on and there is room, takes the foot of the tab: the chain and the parameters are what is above it.
            let (session_area, strip_area) = scope_view::split_for_strip(content_area, s.strip.on);
            // Pre-compute approximate inner heights and sync scroll offsets
            // so mouse click_at uses the correct offset.
            let inner_h = session_area.height.saturating_sub(2) as usize;
            s.chain_state.ensure_visible(inner_h);
            s.param_state.ensure_visible(inner_h);

            let playhead = s.selected_playhead();
            let (ci, pi) = render_session(
                frame,
                session_area,
                &s.tree_entries,
                &s.chain_state,
                &s.ctl.model.instruments,
                &s.ctl.model.groups,
                &s.ctl.model.free_modulators,
                &s.ctl.model.cables,
                &s.param_state,
                s.focus_params,
                &s.param_filter_input,
                s.param_filtering,
                &s.param_filtered,
                playhead,
            );
            s.areas.chain_inner = ci;
            s.areas.param_inner = pi;
            if let Some(strip_area) = strip_area {
                s.strip.take(&s.ctl, Instant::now());
                let looking = match s.selected_signal() {
                    None => scope_view::Looking::Nothing,
                    Some((node, name)) if s.ctl.probe_ring(node).is_some() => scope_view::Looking::Ring(name),
                    Some((_, name)) => scope_view::Looking::NoRing(name),
                };
                scope_view::render_strip(frame, strip_area, &s.strip, &looking, s.ctl.sample_rate);
            }

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
                render_selector_popup(frame, area, sel, s.ctl.catalog.is_scanning());
            }
            if let Some(ts) = &s.target_selector {
                render_target_selector_popup(frame, area, ts);
            }
            if let Some(ms) = &s.modulate {
                render_modulate_popup(frame, area, ms);
            }
            if let Some(pending) = &s.pending_load {
                render_loading_popup(frame, area, &pending.name);
            }
            if let Some(gs) = &s.group_assign {
                render_group_assign_popup(frame, area, gs);
            }
            if let Some(es) = &s.ear_popup {
                render_ear_popup(frame, area, es);
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
            if let Some(sm) = &s.save_module {
                render_save_module_popup(frame, area, sm);
            }
            if let Some(sc) = &s.send_cc {
                render_send_cc_popup(frame, area, sc);
            }
        }
        TAB_PIANO => {
            render_piano_tab(frame, content_area, s);
            if let Some(ss) = &s.scale_selector {
                render_scale_selector_popup(frame, area, ss);
            }
        }
        TAB_SCOPE => {
            s.scope.take(&s.ctl.telemetry.scope, Instant::now());
            scope_view::render(frame, content_area, &s.scope, s.ctl.sample_rate);
        }
        TAB_PATCH => {
            s.patch.render(frame, content_area, &s.ctl.model);
            render_actions(frame, action_area, &patch_action_items());
            if let Some(ce) = &s.cable_editing {
                render_edit_popup(frame, area, &ce.edit);
            }
        }
        TAB_HELP => render_help(frame, content_area, &s.help_lines, s.help_offset),
        _ => {}
    }

    if s.ctl.midi_monitor.is_enabled() {
        render_midi_monitor(frame, debug_area, &s.ctl.midi_monitor);
    }
}

/// Live incoming-MIDI tally shown at the bottom under `--debug`. Each row is one
/// message kind (note / bend / channel pressure / poly aftertouch / CC N) with
/// its latest raw bytes and a hit count — so you can see at a glance what a
/// controller actually emits (e.g. whether a hard press sends Channel Pressure).
fn render_midi_monitor(
    frame: &mut ratatui::Frame,
    area: Rect,
    monitor: &crate::midi_monitor::MidiMonitor,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(DIM))
        .title(" MIDI in  (--debug) ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = monitor.snapshot();
    let text = if rows.is_empty() {
        "waiting for MIDI…  play a note, then wiggle / press hard and watch what appears".to_string()
    } else {
        rows.join("\n")
    };
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(Color::White)), inner);
}

#[allow(clippy::too_many_arguments)]
fn render_session(
    frame: &mut ratatui::Frame,
    area: Rect,
    tree_entries: &[TreeEntry],
    chain_state: &ListState,
    instruments: &[InstrumentNode],
    groups: &[GroupNode],
    free_modulators: &[ModulatorSlot],
    cables: &[AudioCable],
    param_state: &ListState,
    focus_params: bool,
    param_filter_input: &TextInputState,
    param_filtering: bool,
    param_filtered: &[usize],
    playhead: Option<usize>,
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

    // An entry that has a colour is drawn in it: dimmed, as the others are, until it is the selected one, which is bold.
    let items: Vec<ListItem> = tree_entries
        .iter()
        .enumerate()
        .map(|(i, e)| match e.color {
            Some(color) if i == chain_state.selected => ListItem::styled(&e.label, Style::default().fg(color)),
            Some(color) => ListItem::styled(&e.label, Style::default().fg(color).add_modifier(Modifier::DIM)),
            None => ListItem::raw(&e.label),
        })
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
    let mut mod_looks: Vec<RowLook> = Vec::new(); // how each of them is drawn, if it is not as the others are
    let mut plugin_slot: Option<&PluginSlot> = None; // the plugin whose parameters the pane shows, which says which of them have no effect now
    let (plugin_name, plugin_params) = if selected < tree_entries.len() {
        let addr = &tree_entries[selected].address;
        match addr {
            TreeAddress::FreeModulator { index } => {
                match free_modulators.get(*index) {
                    Some(m) => {
                        let (name, rows, looks) = modulator_pane(m, playhead);
                        mod_params = rows;
                        mod_looks = looks;
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
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "Enabled".to_string(),
                            index: 1,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.enabled { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "Loop".to_string(),
                            index: 2,
                            min: 0.0,
                            max: 1.0,
                            default: 1.0,
                            value: if p.looping { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Off".to_string(), "On".to_string()]),
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "Transpose".to_string(),
                            index: 3,
                            min: 0.0,
                            max: 1.0,
                            default: 0.0,
                            value: if p.in_key { 1.0 } else { 0.0 },
                            kind: ParamKind::Enum(vec!["Chromatic".to_string(), "In Key".to_string()]),
                            ..Default::default()
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
                                ..Default::default()
                            });
                        }
                        ("Pattern".to_string(), mod_params.as_slice())
                    }
                    None => ("Pattern".to_string(), &[] as &[ParamSlot]),
                }
            }
            TreeAddress::Group(group) => {
                // The group's bus pane: host-side Volume + Pan (no plugin).
                match groups.get(*group) {
                    Some(g) => {
                        mod_params.push(ParamSlot {
                            name: "volume".to_string(),
                            index: 0,
                            min: 0.0,
                            max: 4.0,
                            default: 1.0,
                            value: g.volume,
                            kind: ParamKind::Float,
                            ..Default::default()
                        });
                        mod_params.push(ParamSlot {
                            name: "pan".to_string(),
                            index: 1,
                            min: -1.0,
                            max: 1.0,
                            default: 0.0,
                            value: g.pan,
                            kind: ParamKind::Float,
                            ..Default::default()
                        });
                        let name = g.display_name(*group);
                        (name, mod_params.as_slice())
                    }
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
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
                    Some(p) => {
                        plugin_slot = Some(p);
                        (p.name.clone(), p.params.as_slice())
                    }
                    None => ("(none)".to_string(), &[] as &[ParamSlot]),
                }
            }
        }
    } else {
        ("(none)".to_string(), &[] as &[ParamSlot])
    };

    // Determine if the filter bar can be shown.
    let is_plugin_node = selected < tree_entries.len()
        && matches!(
            tree_entries[selected].address,
            TreeAddress::Instrument(_) | TreeAddress::Effect { .. }
        );

    // Build the display params — apply filter for plugin nodes.
    let display_params: Vec<&ParamSlot> = if is_plugin_node && !param_filtered.is_empty() {
        param_filtered.iter().filter_map(|&i| plugin_params.get(i)).collect()
    } else if is_plugin_node && param_filtered.is_empty() && !param_filter_input.value.is_empty() {
        // Filter active but no matches.
        vec![]
    } else {
        plugin_params.iter().collect()
    };
    // Why each of them has no effect now, if it has none (step 95b). A row is told from its parameter and not from its place, which a filter changes.
    let unused: Vec<Option<&str>> = display_params.iter().map(|p| plugin_slot.and_then(|slot| slot.unused_because(p, cables))).collect();
    // The reason for the row that the cursor is on, said under the pane while the cursor is in it.
    let reason = focus_params
        .then(|| display_params.get(param_state.selected).zip(unused.get(param_state.selected).copied().flatten()))
        .flatten()
        .map(|(p, why)| format!(" {} has no effect now: {why} ", p.name));

    let right_style = if focus_params {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(DIM)
    };
    let mut right_block = Block::default()
        .borders(Borders::ALL)
        .border_style(right_style)
        .title(format!(" {} ", plugin_name));
    if let Some(reason) = reason {
        right_block = right_block.title_bottom(reason);
    }
    let right_inner = right_block.inner(right);
    frame.render_widget(right_block, right);

    // Determine if the filter bar should be shown.
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
        .enumerate()
        .map(|(i, (name, col1, col2, col3, row_kind))| {
            // (Only a modulator's pane has looks of its own, and it shows every row it has, so a row's look is at its own place; a plugin's parameter that has no effect now is dimmed as a step past the length is.)
            let look = if unused.get(i).copied().flatten().is_some() { RowLook::Unused } else { mod_looks.get(i).copied().unwrap_or_default() }.style();
            match row_kind {
                ParamRow::Separator => ListItem::spans(vec![
                    ListSpan::new(name, sep_style),
                    ListSpan::new(col1, sep_style),
                ]),
                ParamRow::Enum => ListItem::spans(vec![
                    ListSpan::new(name, look),
                    ListSpan::new(col1, look),
                ]),
                ParamRow::Normal => ListItem::spans(vec![
                    ListSpan::new(name, look),
                    ListSpan::new(col1, look),
                    ListSpan::new(col2, look),
                    ListSpan::new(col3, look),
                ]),
            }
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
    let sel = chain_state.selected;
    let addr = tree_entries.get(sel).map(|e| &e.address);
    // Show the actions for whichever pane has focus (chain vs. parameters).
    render_actions(frame, area, &action_bar_items(addr, focus_params));
}

/// Draw `actions`, a key and what it does each, in the action bar.
fn render_actions(frame: &mut ratatui::Frame, area: Rect, actions: &[(&str, &str)]) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let key_style = Style::default().fg(Color::Black).bg(Color::White).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::White);

    let y = area.y;
    let mut x = area.x;

    for &(key, desc) in actions {
        if x > area.x {
            x += 1;
        }
        for ch in format!(" {key} ").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(key_style); }
            x += 1;
        }
        for ch in format!(" {desc}").chars() {
            if x >= area.right() { break; }
            if let Some(c) = frame.buffer_mut().cell_mut((x, y)) { c.set_char(ch); c.set_style(label_style); }
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

/// The box that says that a plugin is being loaded, for the frame that is drawn before the loop is blocked by it.
fn render_loading_popup(frame: &mut ratatui::Frame, area: Rect, name: &str) {
    let text = format!("Loading {name}…");
    let width = (text.chars().count() as u16 + 4).clamp(20, area.width.max(20));
    let popup = centered_rect(width, 3, area);
    frame.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Paragraph::new(text).alignment(ratatui::layout::Alignment::Center), inner);
}

fn render_send_cc_popup(frame: &mut ratatui::Frame, area: Rect, sc: &SendCcState) {
    let popup = centered_rect(40, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Send CC ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        let hint = match &sc.error {
            Some(e) => Paragraph::new(e.as_str()).style(Style::default().fg(Color::Red)),
            None => Paragraph::new("<cc> <value> [channel=1], e.g. 64 80")
                .style(Style::default().fg(DIM)),
        };
        frame.render_widget(hint, Rect::new(inner.x, inner.y, inner.width, 1));
        frame.render_widget(
            TextInput::new(&sc.input),
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

fn render_save_module_popup(frame: &mut ratatui::Frame, area: Rect, sm: &SaveModuleState) {
    let popup = centered_rect(52, 5, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Save Chain as Module ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height >= 2 {
        // The hint line doubles as the error line after a refused name.
        let hint = match &sm.error {
            Some(e) => Paragraph::new(e.as_str()).style(Style::default().fg(Color::Red)),
            None => Paragraph::new("Module name (saved in the library; .toml added)").style(Style::default().fg(DIM)),
        };
        frame.render_widget(hint, Rect::new(inner.x, inner.y, inner.width, 1));
        frame.render_widget(TextInput::new(&sm.input), Rect::new(inner.x, inner.y + 1, inner.width, 1));
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
        (SelectorMode::FreeEffect, false) => " Select Free Effect ",
        (SelectorMode::FreeEffect, true) => " Select Free Effect (scanning…) ",
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

    // (The format is a column wider than `Built-in`, which is the longest, so that it does not run into the number of parameters.)
    let columns: &[(&str, u16)] = &[
        ("Name", inner.width.saturating_sub(23)),
        ("Format", 9),
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

fn render_ear_popup(frame: &mut ratatui::Frame, area: Rect, es: &EarState) {
    let w = (area.width * 45 / 100).max(32).min(area.width);
    let h = (area.height * 40 / 100).max(7).min(area.height);
    let popup = centered_rect(w, h, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
        .title(" Whose notes it hears ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let columns: &[(&str, u16)] = &[("Hears", inner.width)];
    frame.render_widget(FilterList::new(&es.filter, &es.items, columns), inner);
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
    let scale = s.ctl.piano_filter.scale();
    let mode = s.ctl.piano_filter.mode();
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
    let held_notes = s.ctl.held.held();
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
    piano_view::render_keyboard(kb_area, buf, &s.ctl.held, &scale, s.piano_view_octave);

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

/// What a row of a modulator's pane is called, with the unit it is in.
fn modulator_field_label(field: ModulatorField) -> &'static str {
    match field {
        ModulatorField::Rate => "Rate (Hz)",
        ModulatorField::Waveform => "Waveform",
        ModulatorField::Attack => "Attack (s)",
        ModulatorField::Decay => "Decay (s)",
        ModulatorField::Sustain => "Sustain",
        ModulatorField::Release => "Release (s)",
        ModulatorField::Value => "Value",
        ModulatorField::Glide => "Glide",
        ModulatorField::Length => "Length (steps)",
        ModulatorField::Retrigger => "Retrigger",
        ModulatorField::Fade => "Fade (s)",
        ModulatorField::Sync => "Sync",
    }
}

/// How a row of a pane is drawn besides what it says. Only the steps of a step sequencer, the rate of a modulator that follows the tempo and a plugin's parameter that has no effect now have a look of their own.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum RowLook {
    #[default]
    Normal,
    /// A step that the sequencer does not go through: it is past its length (it keeps its value for when the sequencer is longer); the rate of a modulator that follows the tempo
    /// (it keeps it for when it is free again); or a plugin's parameter that has no effect now (`PluginSlot::unused_because`: it keeps its value for when it has one).
    Unused,
    /// The step that the sequencer is at.
    Playing,
}

impl RowLook {
    /// What the row's cells have on top of the style of their list: a dimmed step is a step the sequencer does not play, as a chain entry that is not selected is dimmed to
    /// recede, and the step that it is at is magenta, the colour of a modulator in the chain.
    fn style(self) -> Style {
        match self {
            RowLook::Normal => Style::default(),
            RowLook::Unused => Style::default().add_modifier(Modifier::DIM),
            RowLook::Playing => Style::default().fg(Color::Magenta),
        }
    }
}

/// The pane of a modulator: its title, a row for each of what `modulator_row` says its rows are (the
/// kind it is, its waveform, its settings, a separator, a depth for each cable), as parameters to draw, and the look of each. A step sequencer's step that is `playhead` is
/// marked in its name (`Step 3 ▶`), and the steps past its length are `Unused`.
fn modulator_pane(m: &ModulatorSlot, playhead: Option<usize>) -> (String, Vec<ParamSlot>, Vec<RowLook>) {
    use crate::plugin::chain::LfoWaveform;
    let title = match &m.source {
        ModSourceSlot::Lfo { .. } => m.source.label(),
        ModSourceSlot::Envelope { .. } => "ADSR".to_string(),
        ModSourceSlot::MidiLearn { .. } => "MIDI".to_string(),
        ModSourceSlot::Macro { .. } => "Macro".to_string(),
        ModSourceSlot::Steps { .. } => "Step sequencer".to_string(),
    };
    let row = |name: String, index: usize, min: f32, max: f32, default: f32, value: f32, kind: ParamKind| ParamSlot {
        name,
        index: index as u32,
        min,
        max,
        default,
        value,
        kind,
        ..Default::default()
    };
    // What a modulator of each kind starts as, which a row's default is.
    let (new_lfo, new_envelope, new_macro, new_steps) =
        (ModSourceSlot::new_lfo(), ModSourceSlot::new_envelope(), ModSourceSlot::new_macro(), ModSourceSlot::new_steps());
    let is_steps = matches!(m.source, ModSourceSlot::Steps { .. });
    let steps_in_use = match m.source {
        ModSourceSlot::Steps { length, .. } => length,
        _ => 0,
    };
    // (A modulator that follows the tempo does not go by its rate, which it keeps for when it is free again.)
    let follows_tempo = m.source.setting(ModulatorField::Sync).is_some_and(|sync| sync > 0.0);
    let (rows, looks) = (0..modulator_param_len(m))
        .filter_map(|position| {
            let modulator_row = modulator_row(m, position)?;
            let look = match modulator_row {
                ModulatorRow::Step(step) if Some(step) == playhead => RowLook::Playing,
                ModulatorRow::Step(step) if step >= steps_in_use => RowLook::Unused,
                ModulatorRow::Setting(ModulatorField::Rate) if follows_tempo => RowLook::Unused,
                _ => RowLook::Normal,
            };
            let slot = match modulator_row {
                ModulatorRow::Type => {
                    let envelope = matches!(m.source, ModSourceSlot::Envelope { .. });
                    row("Type".to_string(), position, 0.0, 1.0, 0.0, if envelope { 1.0 } else { 0.0 }, ParamKind::Enum(vec!["LFO".to_string(), "Envelope".to_string()]))
                }
                ModulatorRow::Waveform => {
                    let (min, max) = ModulatorField::Waveform.range();
                    let value = m.source.setting(ModulatorField::Waveform).unwrap_or(0.0);
                    let names = LfoWaveform::ALL.iter().map(|w| w.name().to_string()).collect();
                    row(modulator_field_label(ModulatorField::Waveform).to_string(), position, min, max, 0.0, value, ParamKind::Enum(names))
                }
                ModulatorRow::Setting(field) => {
                    let (min, max) = field.range();
                    // (A step sequencer's rate starts at 8, and not at an LFO's 1.)
                    let default = if is_steps { new_steps.setting(field) } else { new_lfo.setting(field).or_else(|| new_envelope.setting(field)).or_else(|| new_macro.setting(field)) };
                    row(modulator_field_label(field).to_string(), position, min, max, default.unwrap_or(min), m.source.setting(field)?, ParamKind::Float)
                }
                ModulatorRow::Retrigger => {
                    let value = m.source.setting(ModulatorField::Retrigger)?;
                    row(modulator_field_label(ModulatorField::Retrigger).to_string(), position, 0.0, 1.0, 1.0, value, ParamKind::Enum(vec!["off".to_string(), "on".to_string()]))
                }
                ModulatorRow::Sync => {
                    let (min, max) = ModulatorField::Sync.range();
                    let value = m.source.setting(ModulatorField::Sync)?;
                    let names = std::iter::once(FREE).chain(SYNC_DIVISIONS.iter().map(|&(name, _)| name)).map(str::to_string).collect();
                    row(modulator_field_label(ModulatorField::Sync).to_string(), position, min, max, 0.0, value, ParamKind::Enum(names))
                }
                ModulatorRow::Step(step) => {
                    let value = *m.source.steps()?.get(step)?;
                    let marker = if look == RowLook::Playing { " ▶" } else { "" };
                    row(format!("Step {}{marker}", step + 1), position, STEP_MIN, STEP_MAX, ModSourceSlot::staircase()[step], value, ParamKind::Float)
                }
                // A control's source, shown and not edited.
                ModulatorRow::Info => row(format!("Source: {}", m.source.label()), position, 0.0, 0.0, 0.0, 0.0, ParamKind::Separator),
                ModulatorRow::Separator => row("Targets".to_string(), position, 0.0, 0.0, 0.0, 0.0, ParamKind::Separator),
                ModulatorRow::Depth(target) => {
                    let target = m.targets.get(target)?;
                    row(format!("{} depth", target.param_name), position, DEPTH_MIN, DEPTH_MAX, 0.5, target.depth, ParamKind::Float)
                }
            };
            Some((slot, look))
        })
        .unzip();
    (title, rows, looks)
}

/// Build the value-entry `EditState` for row `pa` of a modulator's pane, or None for a row that is
/// not a number to type (the kind, the waveform, a separator). Scope-independent (lane or group).
fn modulator_edit_state(m: &ModulatorSlot, pa: usize) -> Option<EditState> {
    // (An envelope's numbers are shown to a thousandth, the others' to a hundredth.)
    let places = if matches!(m.source, ModSourceSlot::Envelope { .. }) { 3 } else { 2 };
    let (value, name, min, max) = match modulator_row(m, pa)? {
        ModulatorRow::Setting(field) => {
            let (min, max) = field.range();
            (m.source.setting(field)?, modulator_field_label(field).to_string(), min, max)
        }
        ModulatorRow::Step(step) => (*m.source.steps()?.get(step)?, format!("Step {}", step + 1), STEP_MIN, STEP_MAX),
        ModulatorRow::Depth(target) => {
            let target = m.targets.get(target)?;
            (target.depth, format!("{} depth", target.param_name), DEPTH_MIN, DEPTH_MAX)
        }
        ModulatorRow::Type | ModulatorRow::Info | ModulatorRow::Waveform | ModulatorRow::Retrigger | ModulatorRow::Sync | ModulatorRow::Separator => return None,
    };
    Some(EditState { input: TextInputState::new(&format!("{value:.places$}")), param_name: name, param_min: min, param_max: max })
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

    range * step_fraction(modifiers)
}

/// How far a press of Left or Right goes, as a fraction of the range of the row: a twentieth, a hundredth with Shift, a tenth with Ctrl.
fn step_fraction(modifiers: KeyModifiers) -> f32 {
    if modifiers.contains(KeyModifiers::CONTROL) {
        0.10
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        0.01
    } else {
        0.05
    }
}

/// What a step of `direction` (1 up, -1 down) by `fraction` of the range of a modulator's row makes of `value`. A setting that runs over decades (`Taper::Log`:
/// a rate, a time) is multiplied by that fraction of the ratio of its ends, so that a step goes as far on a knob wherever on it the value is; a level, and the
/// depth of a cable (a fraction of what it drives), move by the fraction of one. (A value below the lowest, which a file may give, steps from the lowest:
/// a ratio of nothing is nothing.)
fn stepped(field: Option<ModulatorField>, value: f32, direction: f32, fraction: f32) -> f32 {
    match field {
        // (A whole number, a length, goes by one: a twentieth of its range is not a step of it.)
        Some(field) if field.is_whole() => value + direction,
        Some(field) if field.taper() == Taper::Log => {
            let (min, max) = field.range();
            value.max(min) * (max / min).powf(direction * fraction)
        }
        // (A level that has a range of its own, a fade of 0 to 5 seconds, goes by that fraction of it; the depth of a cable is a fraction of what it drives.)
        Some(field) => {
            let (min, max) = field.range();
            value + direction * fraction * (max - min)
        }
        None => value + direction * fraction,
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

/// A modulation depth as the tree writes it: a percentage of the parameter's range, whole, and with the decimal that it needs when it is
/// under 10% and not a whole percentage, so that the depth that suits a parameter with a wide range (0.4% of a delay's two seconds) is not
/// written as 0%.
fn depth_percent(depth: f32) -> String {
    let percent = depth * 100.0;
    if percent.abs() < 10.0 && (percent - percent.round()).abs() >= 0.05 {
        format!("{percent:.1}%")
    } else {
        // (A whole percentage, or one that is within a twentieth of it: -0.04 is 0, and not -0.)
        format!("{}%", percent.round() + 0.0)
    }
}

/// Render one instrument and its children (pattern, effects, lane mod rack).
/// `prefix` is prepended to every label so the whole sub-tree can be nested
/// under a group header.
/// What the tree says at the end of a lane's or a group's line of whether it is muted, whether it is soloed and whether its way out is cut: `  [mute]`, `  [solo]` and `  [no out]`,
/// the ones that there are, in that order, after two spaces.
fn flags_label(muted: bool, solo: bool, route: Route) -> String {
    let flags: Vec<&str> = [(muted, "[mute]"), (solo, "[solo]"), (route == Route::None, "[no out]")].into_iter().filter_map(|(on, word)| on.then_some(word)).collect();
    if flags.is_empty() { String::new() } else { format!("  {}", flags.join(" ")) }
}

/// What the tree says at the end of an effect's line when the stage before it does not feed it (its link is cut: `SetFeed`): `  [no in]`, and nothing for an effect that is fed.
fn feed_label(fed: bool) -> &'static str {
    if fed { "" } else { "  [no in]" }
}

fn push_instrument_entries(
    entries: &mut Vec<TreeEntry>,
    model: &SessionModel,
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
        label: format!("{prefix}{plugin_label}{range_label}{transpose_label}{}", flags_label(inst.muted, inst.solo, inst.route)),
        address: TreeAddress::Instrument(inst_idx),
        node: inst.node,
        color: None,
        indent: 0,
    });

    let has_pattern = inst.pattern.as_ref().is_some_and(|p| p.recording || !p.events.is_empty());
    // The modulators that hear the lane are listed with it, after its effects.
    let hearing: Vec<(usize, &ModulatorSlot)> = model.free_modulators.iter().enumerate().filter(|(_, m)| model.ear_of(m) == Some(inst.node)).collect();
    let child_count = if has_pattern { 1 } else { 0 } + inst.effects.len() + hearing.len();
    let mut child_idx = 0;
    let branch_for = |idx: usize| if idx == child_count - 1 { "╰" } else { "├" };

    if let Some(pat) = &inst.pattern {
        if pat.recording || !pat.events.is_empty() {
            let (icon, detail) = if pat.recording {
                ("\u{23fa}", "recording...".to_string())
            } else {
                let n = pat.events.iter().filter(|e| e.1 == 0x90).count();
                let mode = if pat.in_key { ", in-key" } else { "" };
                ("\u{25b6}", format!("{:.0} beats, {n} notes{mode}", pat.length_beats))
            };
            entries.push(TreeEntry {
                label: format!("{prefix}{} {icon} Pattern  {detail}", branch_for(child_idx)),
                address: TreeAddress::Pattern(inst_idx),
                node: inst.node,
                color: None,
                indent: 1,
            });
            child_idx += 1;
        }
    }

    for (fx_idx, fx) in inst.effects.iter().enumerate() {
        entries.push(TreeEntry {
            label: format!("{prefix}{} fx {}  [{}]{}", branch_for(child_idx), fx.name, fx.format, feed_label(fx.fed)),
            address: TreeAddress::Effect { inst: inst_idx, index: fx_idx },
            node: fx.node,
            color: None,
            indent: 1,
        });
        child_idx += 1;
    }

    for (mod_idx, m) in hearing {
        let targets = if m.targets.is_empty() {
            String::new()
        } else {
            let list = m.targets.iter().map(|t| format!("{} ({})", heard_target_label(model, t, inst_idx), depth_percent(t.depth))).collect::<Vec<_>>().join(", ");
            format!(" \u{2192} {list}")
        };
        entries.push(TreeEntry {
            label: format!("{prefix}{} ~ {}{targets}", branch_for(child_idx), m.source.label()),
            address: TreeAddress::FreeModulator { index: mod_idx },
            node: m.node,
            color: Some(Color::Magenta),
            indent: 1,
        });
        child_idx += 1;
    }
}

/// What a cable of a modulator that hears group `group` drives, as the tree says it under the group: a parameter of a member's plugin by the member's place in the group (`M0 Oscillator:
/// detune`), a parameter of one of the group's bus effects (`Bus Reverb: damping`), and anything else as every modulator's is (`L2 Filter: cutoff`).
fn heard_by_group_target_label(model: &SessionModel, target: &ModTargetSlot, group: usize) -> String {
    if let Driven::Param { plugin, .. } = target.aim {
        let slot = model.plugin_slot(plugin);
        match (model.locate(plugin), slot) {
            (Some(NodePath::GroupEffect { group: of, .. }), Some(slot)) if of == group => return format!("Bus {}: {}", slot.name, target.param_name),
            (Some(NodePath::Instrument(lane) | NodePath::Effect { inst: lane, .. }), Some(slot)) if model.instruments[lane].group == Some(group) => {
                let member = model.instruments[..lane].iter().filter(|other| other.group == Some(group)).count();
                return format!("M{member} {}: {}", slot.name, target.param_name);
            }
            _ => {}
        }
    }
    free_target_label(model, target)
}

/// What a cable of a modulator that hears lane `inst` drives, as the tree says it under the lane: a parameter of a plugin of the lane by the plugin (`Oscillator: detune`), and anything else
/// as every modulator's is (`L2 Filter: cutoff`).
fn heard_target_label(model: &SessionModel, target: &ModTargetSlot, inst: usize) -> String {
    if let Driven::Param { plugin, .. } = target.aim {
        if matches!(model.locate(plugin), Some(NodePath::Instrument(lane) | NodePath::Effect { inst: lane, .. }) if lane == inst) {
            if let Some(slot) = model.plugin_slot(plugin) {
                return format!("{}: {}", slot.name, target.param_name);
            }
        }
    }
    free_target_label(model, target)
}

/// Where the plugin in `node` is, as the Patch tab tags a lane and a group: `L1` for a plugin of the first lane (its instrument or an effect of its chain) and `G1` for an effect of the
/// first group's bus or for the group itself; nothing for a node that is none of them.
fn plugin_tag(model: &SessionModel, node: NodeId) -> String {
    match model.locate(node) {
        Some(NodePath::Instrument(inst) | NodePath::Effect { inst, .. }) => format!("L{}", inst + 1),
        Some(NodePath::GroupEffect { group, .. } | NodePath::Group(group)) => format!("G{}", group + 1),
        _ => String::new(),
    }
}

/// What a cable that drives a host-side value is called, by whose it is (a lane's volume, an effect's mix, a group's volume or pan), as the Patch tab tags them: `L1 Oscillator volume`,
/// `L1 Filter mix`, `G1 Reverb (bus) mix`, `G1 volume`. (A plugin's own parameter of the same name is `L1 Oscillator: volume`, with a colon.)
fn gain_target_label(model: &SessionModel, node: NodeId, gain: crate::app::model::ModGain) -> String {
    let tag = plugin_tag(model, node);
    match (model.locate(node), model.plugin_slot(node)) {
        (Some(NodePath::GroupEffect { .. }), Some(slot)) => format!("{tag} {} (bus) {}", slot.name, gain.name()),
        (_, Some(slot)) => format!("{tag} {} {}", slot.name, gain.name()),
        (_, None) => format!("{tag} {}", gain.name()),
    }
}

/// What a cable of a free modulator drives, as the tree says it: a parameter by the lane or the group its plugin is in (`L1 Oscillator: detune`, `G1 Filter (bus): cutoff`, as the Patch tab
/// tags them), and the setting of another free modulator by its label (`Mod 1 attack`).
fn free_target_label(model: &SessionModel, target: &ModTargetSlot) -> String {
    if let Driven::Gain { node, gain } = target.aim {
        return gain_target_label(model, node, gain);
    }
    let Driven::Param { plugin, .. } = target.aim else { return target.param_name.clone() };
    let Some(slot) = model.plugin_slot(plugin) else { return target.param_name.clone() };
    let bus = if matches!(model.locate(plugin), Some(NodePath::GroupEffect { .. })) { " (bus)" } else { "" };
    format!("{} {}{bus}: {}", plugin_tag(model, plugin), slot.name, target.param_name)
}

fn build_tree_entries(model: &SessionModel) -> Vec<TreeEntry> {
    let (instruments, groups) = (&model.instruments[..], &model.groups[..]);
    let mut entries = Vec::new();

    // The free modulators that hear no lane and no group first (magenta): they are the session's own, and each says what its cables drive wherever it is. The ones that hear a lane or a group are
    // listed with it, where a lane's and a group's own were (a modulator is the session's own, and is listed where its notes are).
    for (mod_idx, m) in model.free_modulators.iter().enumerate().filter(|(_, m)| model.ear_of(m).is_none()) {
        let targets = if m.targets.is_empty() {
            String::new()
        } else {
            let list = m.targets.iter().map(|t| format!("{} ({})", free_target_label(model, t), depth_percent(t.depth))).collect::<Vec<_>>().join(", ");
            format!(" \u{2192} {list}")
        };
        entries.push(TreeEntry {
            label: format!("~ {}{targets}  (free)", m.source.label()),
            address: TreeAddress::FreeModulator { index: mod_idx },
            node: m.node,
            color: Some(Color::Magenta),
            indent: 0,
        });
    }

    // Groups: a header, the member instruments nested under it, then the
    // group's bus effects.
    for (g_idx, group) in groups.iter().enumerate() {
        let name = group.display_name(g_idx);
        let vol = if (group.volume - 1.0).abs() > f32::EPSILON {
            format!("  vol {:.2}", group.volume)
        } else {
            String::new()
        };
        entries.push(TreeEntry {
            label: format!("\u{25a6} {name}{vol}{}", flags_label(group.muted, group.solo, group.route)),
            address: TreeAddress::Group(g_idx),
            node: group.node,
            color: Some(Color::Cyan),
            indent: 0,
        });
        for (inst_idx, inst) in instruments.iter().enumerate() {
            if inst.group == Some(g_idx) {
                push_instrument_entries(&mut entries, model, inst, inst_idx, "  ");
            }
        }
        for (fx_idx, fx) in group.effects.iter().enumerate() {
            entries.push(TreeEntry {
                label: format!("  fx {}  [{}]  (bus){}", fx.name, fx.format, feed_label(fx.fed)),
                address: TreeAddress::GroupEffect { group: g_idx, index: fx_idx },
                node: fx.node,
                color: None,
                indent: 1,
            });
        }

        // The modulators that hear the group, after its bus effects (a cable into a member's plugin or a bus effect of the group says so by the member and the effect, and any other as
        // every modulator's does).
        for (mod_idx, m) in model.free_modulators.iter().enumerate().filter(|(_, m)| model.ear_of(m) == Some(group.node)) {
            let targets = if m.targets.is_empty() {
                String::new()
            } else {
                let list = m.targets.iter().map(|t| format!("{} ({})", heard_by_group_target_label(model, t, g_idx), depth_percent(t.depth))).collect::<Vec<_>>().join(", ");
                format!(" \u{2192} {list}")
            };
            entries.push(TreeEntry {
                label: format!("  ~ {}{targets}  (bus)", m.source.label()),
                address: TreeAddress::FreeModulator { index: mod_idx },
                node: m.node,
                color: Some(Color::Magenta),
                indent: 1,
            });
        }
    }

    // Ungrouped instruments at the top level.
    for (inst_idx, inst) in instruments.iter().enumerate() {
        if inst.group.is_none() {
            push_instrument_entries(&mut entries, model, inst, inst_idx, "");
        }
    }

    entries
}

/// Truncate `s` to at most `max` characters, appending an ellipsis when cut.
/// Operates on `char`s (not bytes) so it never splits a multi-byte character —
/// param names can contain non-ASCII glyphs (e.g. the MIDI-learn source row
/// "MIDI (learning…)"), and byte-slicing those panics.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let kept: String = s.chars().take(max - 1).collect();
        format!("{kept}…")
    }
}

/// Which of the actions of the action bar a click at (`x`, `y`) is on: its place in `actions`.
fn action_bar_index(x: u16, y: u16, area: Rect, actions: &[(&str, &str)]) -> Option<usize> {
    if y != area.y || x < area.x || x >= area.right() {
        return None;
    }
    let rel_x = (x - area.x) as usize;
    let mut pos = 0;
    for (at, &(key, desc)) in actions.iter().enumerate() {
        if pos > 0 {
            pos += 1;
        }
        let total = key.len() + 2 + desc.len() + 1;
        if rel_x >= pos && rel_x < pos + total {
            return Some(at);
        }
        pos += total;
    }
    None
}

/// The key of the action of the action bar that a click is on.
fn action_bar_hit(x: u16, y: u16, area: Rect, actions: &[(&str, &str)]) -> Option<char> {
    action_bar_index(x, y, area, actions).and_then(|at| actions[at].0.chars().next())
}

fn build_help_lines(web: Option<&WebStatus>) -> Vec<String> {
    let mut lines = vec![
        "Tang — Terminal Audio Plugin Host".into(),
        "".into(),
    ];
    if let Some(web) = web {
        lines.extend([
            "Web interface (--web):".into(),
            format!("  {}", web.url),
            "  Open it in a browser: what is changed there shows here, and what is".into(),
            "  changed here shows there.".into(),
        ]);
        lines.extend(web.warnings().into_iter().map(|warning| format!("  {warning}")));
        lines.push("".into());
    }
    lines.extend(vec![
        "Global keybindings:".into(),
        "  1 2 3 4 5  Switch to tab by number".into(),
        "  Tab        Next tab".into(),
        "  Shift+Tab  Previous tab".into(),
        "  Ctrl+S     Save session".into(),
        "  Ctrl+Shift+S  Save session as (new filename)".into(),
        "  Ctrl+Z     Undo the last thing done (as the browser's Undo does)".into(),
        "  Ctrl+Y     Redo (Ctrl+Shift+Z too)".into(),
        "  Ctrl+P     Panic: let go of every note that sounds".into(),
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
        "  M          Mute / unmute the lane or group (a mute wins over a solo)".into(),
        "  S          Solo / unsolo it (while anything is soloed only it is heard)".into(),
        "  w          Cut / give back the way out of the lane or group (it is heard only through its audio cables)".into(),
        "  f          Cut / give back the link into the effect (it is fed by its audio cables and by nothing else)".into(),
        "  a          Add effect after selected".into(),
        "  N          Add an effect that stands by itself (in a lane of its own; cables feed it), or a module of the library (a chain of effects, whole)".into(),
        "  K          Keep the chain of effects of the lane as a module (a popup asks for the name; `N` adds it, here or in another session)".into(),
        "  d          Delete selected".into(),
        "  u          Open the plugin's own window (a VST3 plugin's face, on this machine; it takes the keyboard)".into(),
        "  m          Add a modulator that hears the lane or group the cursor is of (on a modulator: what it hears)".into(),
        "  F          Add a modulator that hears every note (its cables reach any plugin of the session)".into(),
        "  h          Choose whose notes a modulator hears (a lane's, a group's, or every note)".into(),
        "  g          Assign instrument to a group (submix bus)".into(),
        "  x          Set effect / group-bus dry/wet mix".into(),
        "  r          Record/stop pattern".into(),
        "  Ctrl+R     Clear pattern".into(),
        "  b          Set BPM".into(),
        "  c          Send a MIDI CC (e.g. 64 80 = sustain), any focus".into(),
        "  L          Put the limiter on the output in the sound, or take it out (it holds the mix under its ceiling and does not let it clip)".into(),
        "  C          Set the limiter's ceiling (dB below full scale)".into(),
        "  V          Set the master volume (before the limiter)".into(),
        "".into(),
        "Modulator (chain focus):".into(),
        "  t          Add modulation target".into(),
        "  d          Delete modulator".into(),
        "".into(),
        "Patch tab (the audio cables, as a matrix of outputs against inputs):".into(),
        "  Arrows     Move the cursor (Home / End: first / last input; PageUp / PageDown)".into(),
        "  Enter      Draw a cable at the crosspoint, or take it out (Space too)".into(),
        "  +  -       Turn the level of the cable by a twentieth (0 to 2)".into(),
        "  e          Type the level of the cable".into(),
        "  d          Take the cable out".into(),
        "  w          Cut / give back the way out of the lane or group of the row".into(),
        "  M  S       Mute / solo the lane or group of the row".into(),
        "  Ctrl+Z     Undo (Ctrl+Y redoes); a cross is where a cable would make the audio go round".into(),
        "  ↳B         The second input of an effect that has one (the Math): a column of its own after the first".into(),
        "".into(),
        "Piano tab:".into(),
        "  k          Open scale picker (root + scale)".into(),
        "  [          Shift view octave down".into(),
        "  ]          Shift view octave up".into(),
        "".into(),
        "Scope tab:".into(),
        "  v          Wave or spectrum of the output".into(),
        "  Left/Right Shorter / longer time base (the wave)".into(),
        "".into(),
        "Scope of a node (Session tab):".into(),
        "  o          Show / hide the scope of the selected node, under the chain (24 rows or more)".into(),
        "  O          Wave or spectrum of that scope".into(),
        "".into(),
        "Session tab (param focus):".into(),
        "  Up/Down    Navigate parameters".into(),
        "  Left/Right Adjust value (5%)".into(),
        "  Shift+←/→  Fine adjust (1%)".into(),
        "  Ctrl+←/→   Coarse adjust (10%)".into(),
        "  Enter      Type a value".into(),
        "  m          Modulate this parameter (a new LFO, envelope, macro, velocity or key, or one that is there)".into(),
        "  l          MIDI learn: bind the next CC / pitch-bend / aftertouch".into(),
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
    ]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cc_message_defaults_to_channel_1() {
        assert_eq!(parse_cc_message("64 80"), Ok([0xB0, 64, 80]));
        assert_eq!(parse_cc_message(" 1 127 16 "), Ok([0xBF, 1, 127]));
        assert!(parse_cc_message("64").is_err());
        assert!(parse_cc_message("128 0").is_err());
        assert!(parse_cc_message("64 80 0").is_err());
        assert!(parse_cc_message("64 80 1 2").is_err());
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        // The MIDI-learn "learning" source row: 26 bytes but only 24 chars, so
        // the old byte-length check triggered truncation and `&s[..23]` sliced
        // through the 3-byte ellipsis — a panic when the param pane rendered a
        // still-learning modulator. It must not panic now.
        let s = "Source: MIDI (learning…)";
        assert_eq!(s.len(), 26);
        assert_eq!(s.chars().count(), 24);
        let out = truncate(s, 24); // name_width used by the param pane
        assert_eq!(out, s); // fits in 24 chars → unchanged

        // A genuinely-too-long multi-byte string truncates cleanly on a char
        // boundary and gains an ellipsis.
        let arrows = "→".repeat(30);
        let out = truncate(&arrows, 10);
        assert!(out.chars().count() <= 10);
        assert!(out.ends_with('…'));
        // Round-trips as valid UTF-8 (String is always valid, but assert the cut
        // landed on a boundary by re-counting).
        assert_eq!(out.chars().filter(|&c| c == '→').count(), 9);

        // Degenerate width.
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn param_action_bar_shows_learn() {
        // Regression for "I can't see the 'l' at the bottom": the parameter pane
        // action bar must advertise MIDI-learn on plugin params.
        let inst = TreeAddress::Instrument(0);
        let acts = action_bar_items(Some(&inst), true);
        assert!(acts.iter().any(|(k, _)| *k == "l"), "learn missing: {acts:?}");
        assert!(acts.iter().any(|(k, _)| *k == "m"), "modulate missing: {acts:?}");
        // Chain focus keeps the chain actions (no bare 'l').
        let chain = action_bar_items(Some(&inst), false);
        assert!(chain.iter().any(|(k, _)| *k == "i"));
        assert!(!chain.iter().any(|(k, _)| *k == "l"));

        // A modulator's own params can be cross-modulated ('m') and learned ('l').
        let m = TreeAddress::FreeModulator { index: 0 };
        let macts = action_bar_items(Some(&m), true);
        assert!(macts.iter().any(|(k, _)| *k == "m"), "modulate missing on modulator: {macts:?}");
        assert!(macts.iter().any(|(k, _)| *k == "l"), "learn missing on modulator: {macts:?}");
    }

    #[test]
    fn modulator_target_depth_allows_negative() {
        // A depth row must be editable across -1..1 so modulation can invert.
        let m = ModulatorSlot {
            node: crate::app::model::NodeId(0),
            source: ModSourceSlot::Lfo {
                waveform: crate::plugin::chain::LfoWaveform::Sine,
                rate: 1.0,
                retrigger: false,
                fade: 0.0,
                sync: 0,
            },
            targets: vec![ModTargetSlot {
                edge: crate::app::model::EdgeId(0),
                param_name: "cutoff".into(),
                aim: crate::app::model::Driven::Param { plugin: crate::app::model::NodeId(1), index: 0 },
                depth: -0.5,
                param_min: 0.0,
                param_max: 1.0,
                effective: None,
            }],
            output: None,
            position: None,
            ear: None,
        };
        // LFO rows: 0=Type, 1=Waveform, 2=Rate, 3=Sync, 4=Retrigger, 5=Fade, 6=Targets separator, 7=first depth.
        assert_eq!(modulator_param_range(&m, 5), Some((0.0, 5.0)), "the fade is a number too");
        assert_eq!(modulator_param_range(&m, 3), None, "the note length is a choice");
        assert!(modulator_edit_state(&m, 3).is_none(), "and nothing to type");
        assert_eq!(modulator_param_range(&m, 7), Some((-1.0, 1.0)));
        let edit = modulator_edit_state(&m, 7).expect("depth row is editable");
        assert_eq!((edit.param_min, edit.param_max), (-1.0, 1.0));
    }
}
