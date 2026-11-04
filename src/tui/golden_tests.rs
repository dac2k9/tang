//! Golden-render tests for the TUI — the regression net for the controller
//! refactor (`docs/web-ui.md`, phase 0).
//!
//! Each test loads a session (TOML text) through the real loader against a stub
//! command channel, builds a `State` from the resulting model, drives it with real
//! key events through `process_event`, draws it into ratatui's `TestBackend`, and
//! snapshots the buffer with `insta` —
//! characters *and* styles, because colour carries meaning here (the piano
//! paints held / root / scale keys with background colour on blank cells).
//!
//! After an intentional UI change, regenerate with
//! `INSTA_UPDATE=always cargo test golden`, then review the snapshot diff.

use std::path::Path;

use crossbeam_channel::{Receiver, unbounded};
use crossterm::event::KeyEvent;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;
use crate::app::load::load_session;
use crate::app::testutil::{DropProbe, described, recording, stub_engine, told_all};
use crate::plugin::PluginInfo;
use crate::plugin::chain::{MidiBindSource, MidiLearnedNotification, ModulatorLocation};
use crate::app::graph::{Endpoint, Port};
use crate::session;
use crate::session::Route;

const SAMPLE_RATE: f32 = 48_000.0;
const BLOCK: usize = 256;
const WIDTH: u16 = 120;
const HEIGHT: u16 = 32;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A `State` wired to stub engine channels, plus a `TestBackend` terminal.
struct Harness {
    s: State,
    terminal: Terminal<TestBackend>,
    /// The audio thread's end of the command channel: what the TUI asked the
    /// engine to do. Kept alive so `send` never fails or blocks.
    cmd_rx: Receiver<GraphCommand>,
    /// The notes the controller would play (what a key of the terminal that plays or silences notes sends): kept so that sending them cannot fail, and drained by `notes`.
    midi_rx: Receiver<crate::audio::MidiEvent>,
    /// The audio thread's end of the channel that hands swapped-out plugins back.
    return_tx: crossbeam_channel::Sender<Box<dyn plugin::Plugin>>,
    /// The audio thread's ends of the notification channels.
    pattern_tx: crossbeam_channel::Sender<crate::plugin::chain::PatternNotification>,
    learn_tx: crossbeam_channel::Sender<crate::plugin::chain::MidiLearnedNotification>,
    /// The scan thread's end; `None` unless a test started a scan with
    /// `start_scan`, and again once the scan is over.
    catalog_tx: Option<crossbeam_channel::Sender<Vec<PluginInfo>>>,
}

/// The modulators of the session's own that hear the lane `lane`: what the lane's rack was before step 96.
#[allow(dead_code)]
fn hearing_lane(model: &crate::app::model::SessionModel, lane: usize) -> Vec<&crate::app::model::ModulatorSlot> {
    let node = model.instruments[lane].node;
    model.free_modulators.iter().filter(|m| m.ear == Some(node)).collect()
}

/// The modulators of the session's own that hear the group `group`: what the group's rack was before step 96.
#[allow(dead_code)]
fn hearing_group(model: &crate::app::model::SessionModel, group: usize) -> Vec<&crate::app::model::ModulatorSlot> {
    let node = model.groups[group].node;
    model.free_modulators.iter().filter(|m| m.ear == Some(node)).collect()
}

/// The place in the tree of the `n`th modulator that hears the node `ear` (a lane or a group): the entry that the rack of the lane or of the group had before step 96.
fn heard_modulator(h: &Harness, ear: crate::app::model::NodeId, n: usize) -> TreeAddress {
    let index = h.s.ctl.model.free_modulators.iter().enumerate().filter(|(_, m)| m.ear == Some(ear)).map(|(index, _)| index).nth(n);
    TreeAddress::FreeModulator { index: index.unwrap_or_else(|| panic!("{ear:?} is not heard by {} modulators", n + 1)) }
}

/// The `n`th modulator that hears the lane `lane`.
#[allow(dead_code)]
fn lane_modulator(h: &Harness, lane: usize, n: usize) -> TreeAddress {
    heard_modulator(h, h.s.ctl.model.instruments[lane].node, n)
}

/// The `n`th modulator that hears the group `group`.
#[allow(dead_code)]
fn group_modulator(h: &Harness, group: usize, n: usize) -> TreeAddress {
    heard_modulator(h, h.s.ctl.model.groups[group].node, n)
}

impl Harness {
    fn new(session_toml: impl AsRef<str>) -> Self {
        let config = session::parse(session_toml.as_ref()).expect("scene parses");
        Self::of(config)
    }

    fn of(config: session::SessionConfig) -> Self {
        // The loader sends the audio thread its commands; nothing listens here, but
        // the receiver has to stay alive for the sends to succeed.
        let (load_tx, _load_rx) = unbounded();
        let model = load_session(
            &config,
            Path::new("."),
            SAMPLE_RATE,
            BLOCK,
            &plugin::Runtime::default(),
            &load_tx,
            &mut |_| {},
        )
        .expect("scene loads");
        // What the TUI sends from here on is what `commands()` reports.
        let (handles, engine) = stub_engine();
        // No scan has run: an empty, finished catalog unless a test fills it in.
        let s = State::new(Controller::new(model, handles, None));
        let terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        Harness {
            s,
            terminal,
            cmd_rx: engine.cmd_rx,
            midi_rx: engine.midi_rx,
            return_tx: engine.return_tx,
            pattern_tx: engine.pattern_tx,
            learn_tx: engine.learn_tx,
            catalog_tx: None,
        }
    }

    /// A plugin scan is under way: what a test sends on `catalog_tx` is folded
    /// into the catalog by the next `tick`, and the scan is over when the
    /// sender is dropped.
    fn start_scan(&mut self) -> &mut Self {
        let (tx, rx) = unbounded();
        self.s.ctl.catalog.follow(rx);
        self.catalog_tx = Some(tx);
        self
    }

    fn press(&mut self, code: KeyCode) -> &mut Self {
        self.press_with(code, KeyModifiers::NONE)
    }

    fn press_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> &mut Self {
        process_event(&mut self.s, Event::Key(KeyEvent::new(code, modifiers)));
        // Whatever the key did, every node still has an id of its own.
        if let Err(e) = self.s.ctl.model.check_ids() {
            panic!("after {code:?} {modifiers:?}: {e}");
        }
        self
    }

    fn type_chars(&mut self, text: &str) -> &mut Self {
        for c in text.chars() {
            self.press(KeyCode::Char(c));
        }
        self
    }

    fn press_n(&mut self, code: KeyCode, n: usize) -> &mut Self {
        for _ in 0..n {
            self.press(code);
        }
        self
    }

    /// The chain tree as the user reads it.
    fn tree(&self) -> Vec<&str> {
        self.s.tree_entries.iter().map(|e| e.label.as_str()).collect()
    }

    /// Move the chain cursor onto `addr` with real Up/Down presses. A press moves it one
    /// entry, so more presses than the tree has entries means the keys go somewhere else
    /// (the focus is in the parameter pane): that panics, where pressing on would not end.
    fn goto(&mut self, addr: TreeAddress) -> &mut Self {
        let target = self
            .s
            .tree_entries
            .iter()
            .position(|e| e.address == addr)
            .unwrap_or_else(|| panic!("{addr:?} is not in the tree"));
        for _ in 0..self.s.tree_entries.len() {
            let at = self.s.chain_state.selected;
            if at == target {
                break;
            }
            self.press(if at < target { KeyCode::Down } else { KeyCode::Up });
        }
        assert_eq!(
            self.s.chain_state.selected, target,
            "the chain cursor did not reach {addr:?} (is the focus in the parameter pane?)"
        );
        self
    }

    /// Everything the TUI has sent to the audio thread since the last call.
    fn commands(&self) -> Vec<String> {
        described(self.cmd_rx.try_iter())
    }

    /// The MIDI messages that the TUI has sent since the last call (what silences notes, when it does).
    fn notes(&self) -> Vec<crate::audio::MidiEvent> {
        self.midi_rx.try_iter().collect()
    }

    /// Draw a frame and dump it (text + style map).
    fn dump(&mut self) -> String {
        self.terminal.draw(|frame| draw(frame, &mut self.s)).unwrap();
        dump_buffer(self.terminal.backend().buffer())
    }
}

/// Compare the current frame against `snapshots/<name>.snap`.
fn assert_golden(name: &str, h: &mut Harness) {
    let dump = h.dump();
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_omit_expression(true);
    settings.bind(|| insta::assert_snapshot!(name, dump));
}

// ---------------------------------------------------------------------------
// Buffer dump: characters + a per-cell style map
// ---------------------------------------------------------------------------

const STYLE_SYMBOLS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Render a buffer as three blocks: the characters (trailing blanks trimmed),
/// a map giving every non-default cell a letter, and the legend for those
/// letters (in order of first appearance, reading left→right, top→bottom).
fn dump_buffer(buf: &Buffer) -> String {
    let symbols: Vec<char> = STYLE_SYMBOLS.chars().collect();
    let mut legend: Vec<(Color, Color, Modifier)> = Vec::new();
    let mut text = Vec::new();
    let mut map = Vec::new();
    for y in 0..buf.area.height {
        let mut t = String::new();
        let mut m = String::new();
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            t.push_str(cell.symbol());
            let key = (cell.fg, cell.bg, cell.modifier);
            m.push(if key == (Color::Reset, Color::Reset, Modifier::empty()) {
                '.'
            } else {
                let i = legend.iter().position(|k| *k == key).unwrap_or_else(|| {
                    legend.push(key);
                    legend.len() - 1
                });
                *symbols.get(i).expect("more distinct styles than STYLE_SYMBOLS")
            });
        }
        text.push(t.trim_end().to_string());
        map.push(m.trim_end_matches('.').to_string());
    }
    let legend: Vec<String> = legend
        .iter()
        .enumerate()
        .map(|(i, (fg, bg, modifier))| {
            let modifier = if modifier.is_empty() { "-".to_string() } else { format!("{modifier:?}") };
            format!("{}  fg={fg:?} bg={bg:?} mod={modifier}", symbols[i])
        })
        .collect();
    format!(
        "{}x{}\n── text ──\n{}\n── styles ('.' = default) ──\n{}\n── legend ──\n{}\n",
        buf.area.width,
        buf.area.height,
        text.join("\n"),
        map.join("\n"),
        legend.join("\n"),
    )
}

// ---------------------------------------------------------------------------
// Fixtures: sessions, as TOML, loaded by the real loader
// ---------------------------------------------------------------------------

/// `examples/split.toml`: a sine bass (C0–B3) and a square lead (C4–C8), each
/// with an LFO on `detune` for vibrato.
fn scene_split() -> &'static str {
    include_str!("../../examples/split.toml")
}

/// The Pad group: a bus reverb, a group LFO on a member's `detune` and the bus
/// `damping`, and a group ADSR that cross-mods the LFO's rate.
const GROUP: &str = r#"
[[group]]
name = "Pad"
volume = 0.8
pan = -0.3

[[group.effect]]
plugin = "builtin:reverb"

[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
param = "detune"
depth = 0.4
[[group.modulator.target]]
bus = 0
param = "damping"
depth = 0.25

[[group.modulator]]
type = "envelope"
[[group.modulator.target]]
mod_rate = 0
depth = 0.3
"#;

/// The first pad member, a triangle with a longer release.
const PAD_A: &str = r#"
[[instrument]]
plugin = "builtin:osc"
group = 0
[instrument.params]
waveform = 1.0
release = 0.8
"#;

/// The second pad member: C4 and up, with a filter.
const PAD_B: &str = r#"
[[instrument]]
plugin = "builtin:osc"
range = "C4-"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.6
"#;

/// An ungrouped bass up to B3 with a ladder filter swept by a lane LFO.
const BASS: &str = r#"
[[instrument]]
plugin = "builtin:osc"
range = "-B3"
volume = 0.8
[[instrument.effect]]
plugin = "builtin:filter"
[instrument.effect.params]
cutoff = 0.35
type = 4.0
[[instrument.modulator]]
waveform = "triangle"
rate = 0.2
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.5
"#;

/// The pad group (two members, a bus reverb, a group LFO and a group ADSR that
/// cross-mods the LFO's rate) next to an ungrouped bass with a filter swept by a
/// lane LFO. Exercises every target-label path in the tree.
fn scene_group() -> String {
    [GROUP, PAD_A, PAD_B, BASS].concat()
}

/// One oscillator with a filter and a reverb in series, and a single LFO driving
/// both (`cutoff` on the filter at slot 1, `damping` on the reverb at slot 2).
fn scene_two_effects() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
mix = 0.5
[[instrument.modulator]]
rate = 0.5
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator.target]]
slot = 2
param = "damping"
depth = 0.3
"#
}

/// A delay with a lane LFO on four of its parameters, at depths that are 0.4%, 2.5%, 25.7% and minus 0.4%, and on the oscillator's at minus 0.04%
/// and minus 25.3%, and a group whose bus has a delay that a group LFO drives at 0.4%: a depth that suits a parameter with a wide range (a time
/// of up to two seconds).
fn scene_small_depths() -> &'static str {
    r#"
[[group]]
[[group.effect]]
plugin = "builtin:delay"
[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
bus = 0
param = "time"
depth = 0.004

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:delay"
[[instrument.modulator]]
rate = 0.5
[[instrument.modulator.target]]
slot = 1
param = "time"
depth = 0.004
[[instrument.modulator.target]]
slot = 1
param = "feedback"
depth = 0.025
[[instrument.modulator.target]]
slot = 1
param = "tone"
depth = 0.257
[[instrument.modulator.target]]
slot = 1
param = "pingpong"
depth = -0.004
[[instrument.modulator.target]]
param = "detune"
depth = -0.0004
[[instrument.modulator.target]]
param = "volume"
depth = -0.253
"#
}

/// A group with a filter and a reverb on its bus, and a group LFO driving both.
fn scene_bus_pair() -> &'static str {
    r#"
[[group]]
name = "Pad"
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"
mix = 0.5
[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
bus = 0
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
bus = 1
param = "damping"
depth = 0.3

[[instrument]]
plugin = "builtin:osc"
group = 0
"#
}

/// A group of two oscillators with effects, and a group LFO driving three of those
/// plugins: member 0's filter (`cutoff`, slot 1) and reverb (`damping`, slot 2),
/// and member 1's filter (`cutoff`, slot 1).
fn scene_member_effects() -> &'static str {
    r#"
[[group]]
name = "Pad"
[[group.modulator]]
rate = 0.5
[[group.modulator.target]]
member = 0
slot = 1
param = "cutoff"
depth = 0.4
[[group.modulator.target]]
member = 0
slot = 2
param = "damping"
depth = 0.3
[[group.modulator.target]]
member = 1
slot = 1
param = "cutoff"
depth = 0.2

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
"#
}

// ---------------------------------------------------------------------------
// Session tab
// ---------------------------------------------------------------------------

#[test]
fn session_split_chain() {
    // The first instrument selected: the chain tree plus its parameter bars,
    // including the enum row (`waveform ◂ sine ▸`).
    let mut h = Harness::new(scene_split());
    assert_golden("session_split_chain", &mut h);
}

#[test]
fn session_split_modulator_param_focus() {
    // A lane LFO selected, parameter pane focused on its Rate row: the
    // modulator pseudo-params (Type / Waveform / Rate / Targets / depth).
    let mut h = Harness::new(scene_split());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down);
    assert_golden("session_split_modulator_param_focus", &mut h);
}

/// A lane with the three kinds of modulator, each driving the filter and told apart by what it is set to.
fn scene_three_kinds_of_modulator() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 2.5
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator]]
type = "envelope"
attack = 0.02
decay = 0.3
sustain = 0.6
release = 0.5
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = -0.25
[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator.target]]
slot = 1
param = "resonance"
depth = 1.0
"#
}

#[test]
fn session_envelope_modulator_param_focus() {
    // An envelope selected, the parameter pane focused on its Sustain row: Type, the four stages, Targets, a depth.
    let mut h = Harness::new(scene_three_kinds_of_modulator());
    h.goto(TreeAddress::FreeModulator { index: 1 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Down);
    assert_golden("session_envelope_modulator_param_focus", &mut h);
}

#[test]
fn session_midi_modulator_param_focus() {
    // A MIDI control selected, the parameter pane focused on its depth: what it is bound to, Targets, a depth.
    let mut h = Harness::new(scene_three_kinds_of_modulator());
    h.goto(TreeAddress::FreeModulator { index: 2 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down);
    assert_golden("session_midi_modulator_param_focus", &mut h);
}

#[test]
fn enter_on_a_row_of_a_modulators_pane_opens_the_value_for_the_numbers_and_nothing_for_the_rest() {
    // (modulator, row) -> the value popup's title, the text it starts with, and the range it is held to; None where
    // the row is not a number to type.
    type Popup = (&'static str, &'static str, f32, f32);
    let rows: [(usize, usize, Option<Popup>); 18] = [
        (0, 0, None), // the kind
        (0, 1, None), // the waveform
        (0, 2, Some(("Rate (Hz)", "2.50", 0.01, 50.0))),
        (0, 3, None), // the note length to follow the tempo by, a choice
        (0, 4, None), // the retrigger, a choice
        (0, 5, Some(("Fade (s)", "0.00", 0.0, 5.0))),
        (0, 6, None), // the separator
        (0, 7, Some(("cutoff depth", "0.40", -1.0, 1.0))),
        (1, 0, None),
        (1, 1, Some(("Attack (s)", "0.020", 0.001, 10.0))),
        (1, 2, Some(("Decay (s)", "0.300", 0.001, 10.0))),
        (1, 3, Some(("Sustain", "0.600", 0.0, 1.0))),
        (1, 4, Some(("Release (s)", "0.500", 0.001, 10.0))),
        (1, 5, None),
        (1, 6, Some(("cutoff depth", "-0.250", -1.0, 1.0))),
        (2, 0, None), // what it is bound to
        (2, 1, None),
        (2, 2, Some(("resonance depth", "1.00", -1.0, 1.0))),
    ];
    for (modulator, row, expected) in rows {
        let mut h = Harness::new(scene_three_kinds_of_modulator());
        h.goto(TreeAddress::FreeModulator { index: modulator });
        h.press(KeyCode::Enter);
        for _ in 0..row {
            h.press(KeyCode::Down);
        }
        h.press(KeyCode::Enter);
        let got = h.s.editing.as_ref().map(|e| (e.param_name.as_str(), e.input.value.as_str(), e.param_min, e.param_max));
        assert_eq!(got, expected, "modulator {modulator}, row {row}");
    }
}

#[test]
fn session_group_header() {
    // A group header selected: the bus pane (Volume + Pan), the nested members,
    // the bus effect and both group modulators in the tree.
    let mut h = Harness::new(scene_group());
    assert_golden("session_group_header", &mut h);
}

#[test]
fn session_group_modulator() {
    // A group LFO selected: its two targets (a member param and a bus-effect
    // param) as depth rows.
    let mut h = Harness::new(scene_group());
    let group_lfo = group_modulator(&h, 0, 0);
    h.goto(group_lfo);
    assert_golden("session_group_modulator", &mut h);
}

#[test]
fn session_filter_param_focus() {
    // An effect's parameters with the pane focused: enum row (`type`) and the
    // modulated-cutoff row.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter);
    assert_golden("session_filter_param_focus", &mut h);
}

/// A lane with a Math effect in its chain, in its `follow` mode (the Pump preset), and a second lane whose sound is patched at the Math's second input.
fn scene_math_patched() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument.effect]]
plugin = "builtin:math"
preset = "Pump"

[[instrument]]
plugin = "builtin:osc"
route = "none"

[[cable]]
from = "instrument[1]"
to = "instrument[0].effect[0].side"
"#
}

/// Whether the row of the parameter pane that has the name `name` at its start is drawn dimmed: the look of its name, since the rest of a choice's row has none.
fn pane_row_is_dimmed(h: &mut Harness, name: &str) -> bool {
    h.dump();
    let (buf, pane) = (h.terminal.backend().buffer(), h.s.areas.param_inner);
    let text = |y: u16| (pane.x..pane.right()).map(|x| buf[(x, y)].symbol()).collect::<String>();
    let wanted = format!("{name} ");
    let (y, at) = (pane.y..pane.bottom())
        .find_map(|y| {
            let line = text(y);
            let start = line.find(|c: char| c != '▸' && c != ' ')?;
            line[start..].starts_with(&wanted).then(|| (y, line[..start].chars().count() as u16))
        })
        .unwrap_or_else(|| panic!("the pane has no row that begins with {name:?}: {}", dump_buffer(buf)));
    buf[(pane.x + at, y)].modifier.contains(Modifier::DIM)
}

/// The names of the parameters of the Math in the pane that are drawn dimmed: the ones that have no effect now.
fn dimmed_math_rows(h: &mut Harness) -> Vec<&'static str> {
    ["mode", "amount", "bits", "pitch", "wave", "attack", "release", "level"].into_iter().filter(|name| pane_row_is_dimmed(h, name)).collect()
}

#[test]
fn session_math_param_focus_with_a_cable_at_b() {
    // The Pump preset is the follow mode: `amount`, `attack` and `release` are used, `bits` is not, and `pitch` and `wave` are not while a cable is patched at B. The cursor is on `pitch`, and the pane says why.
    let mut h = Harness::new(scene_math_patched());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 3);
    assert_golden("session_math_param_focus_with_a_cable_at_b", &mut h);
}

#[test]
fn a_parameter_that_has_no_effect_now_is_dimmed_and_the_pane_says_why_while_the_cursor_is_on_it() {
    let mut h = Harness::new(scene_math_patched());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter);
    assert_eq!(dimmed_math_rows(&mut h), ["bits", "pitch", "wave"], "follow, with a cable at B");
    // The pane says why for the row that the cursor is on, under it, and for no other: a row that has an effect has nothing to say.
    let says = |h: &mut Harness| h.dump().lines().find(|line| line.contains("has no effect now")).map(|line| line.trim().to_string());
    assert_eq!(says(&mut h), None, "the cursor is on the mode");
    h.press(KeyCode::Down);
    assert_eq!(says(&mut h), None, "the cursor is on `amount`, which follow reads");
    h.press(KeyCode::Down);
    let line = says(&mut h).expect("the cursor is on `bits`");
    assert!(line.contains(" bits has no effect now: only in the modes xor, and, or "), "{line}");
    h.press(KeyCode::Down);
    let line = says(&mut h).expect("the cursor is on `pitch`");
    assert!(line.contains(" pitch has no effect now: only while nothing is patched at B "), "{line}");
    // (Not while the cursor is in the chain: the pane's rows are not the ones that the keys go to.)
    h.press(KeyCode::Esc);
    assert_eq!(says(&mut h), None);
}

#[test]
fn what_has_no_effect_follows_the_mode_that_is_set_and_the_cable_that_is_patched() {
    let mut h = Harness::new(scene_math_patched());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter);
    // The mode is stepped down from follow to xor (four places): the logic reads `bits` and not `amount`, `attack` and `release`.
    h.press_n(KeyCode::Left, 4);
    assert_eq!(dimmed_math_rows(&mut h), ["amount", "pitch", "wave", "attack", "release"], "xor, with a cable at B");
    // One more and it is a ring, which reads none of them.
    h.press_n(KeyCode::Left, 7);
    assert_eq!(dimmed_math_rows(&mut h), ["amount", "bits", "pitch", "wave", "attack", "release"], "ring, with a cable at B");
    // The cable taken out: B is the carrier, and its `pitch` and `wave` are used.
    let edge = h.s.ctl.model.cables[0].id;
    h.s.ctl.apply(Intent::RemoveEdge { edge }).unwrap();
    assert_eq!(dimmed_math_rows(&mut h), ["amount", "bits", "attack", "release"], "ring, with nothing at B");
    // And the cable drawn again, the carrier is not.
    let (from, to) = (h.s.ctl.model.instruments[1].node, h.s.ctl.model.instruments[0].effects[0].node);
    h.s.ctl.apply(Intent::AddCable { from: Endpoint { node: from, port: Port::AudioOut }, to: Endpoint { node: to, port: Port::AudioInSide }, gain: None }).unwrap();
    assert_eq!(dimmed_math_rows(&mut h), ["amount", "bits", "pitch", "wave", "attack", "release"], "ring, with a cable at B again");
}

#[test]
fn a_row_is_dimmed_by_its_parameter_and_not_by_its_place_when_the_list_is_filtered() {
    let mut h = Harness::new(scene_math_patched());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    // The filter keeps `wave` alone, which comes first in the list and was fifth; the Math is an effect of a lane, whose pane has a filter.
    h.press(KeyCode::Enter).press(KeyCode::Char('/'));
    h.type_chars("wave");
    let text = h.dump();
    assert!(text.contains("wave") && !text.contains("pitch"), "{text}");
    assert!(pane_row_is_dimmed(&mut h, "wave"), "it has no effect with a cable at B");
    h.press(KeyCode::Esc);
    // `amount` is the one that is left of the next filter, and follow reads it.
    h.press(KeyCode::Char('/'));
    h.type_chars("amount");
    assert!(!pane_row_is_dimmed(&mut h, "amount"), "follow reads it");
}

#[test]
fn the_parameters_of_an_effect_on_a_bus_are_dimmed_as_those_of_an_effect_of_a_lane_are() {
    let mut h = Harness::new(
        r#"
[[group]]
[[group.effect]]
plugin = "builtin:math"

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument]]
plugin = "builtin:osc"
route = "none"
"#,
    );
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter);
    assert_eq!(dimmed_math_rows(&mut h), ["amount", "bits", "attack", "release"], "a ring, with nothing at B: its carrier is B");
    let (from, to) = (h.s.ctl.model.instruments[1].node, h.s.ctl.model.groups[0].effects[0].node);
    h.s.ctl.apply(Intent::AddCable { from: Endpoint { node: from, port: Port::AudioOut }, to: Endpoint { node: to, port: Port::AudioInSide }, gain: None }).unwrap();
    assert_eq!(dimmed_math_rows(&mut h), ["amount", "bits", "pitch", "wave", "attack", "release"], "a cable at B");
}

// ---------------------------------------------------------------------------
// Session tab popups
// ---------------------------------------------------------------------------

#[test]
fn the_selector_keeps_the_format_and_the_number_of_parameters_apart() {
    // (Finding f02: `Built-in` filled its column, and a row read `Built-in4`.)
    let mut h = Harness::new(scene_split());
    h.start_scan();
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a'));
    let frame = h.dump();
    assert!(frame.contains("Built-in"), "{frame}");
    for digit in '0'..='9' {
        assert!(!frame.contains(&format!("Built-in{digit}")), "a row reads `Built-in{digit}`: {frame}");
    }
}

#[test]
fn popup_effect_selector_while_scanning() {
    // `a` on an instrument: the effect picker, mid-scan, with the built-in
    // catalog already in.
    let mut h = Harness::new(scene_split());
    h.start_scan();
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a'));
    assert_golden("popup_effect_selector_while_scanning", &mut h);
}

#[test]
fn popup_free_effect_selector_while_scanning() {
    // `N`: the effect picker for an effect that stands by itself, mid-scan, with the built-in catalog already in: it lists the effects and has a title of its own.
    let mut h = Harness::new(scene_split());
    h.start_scan();
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('N'));
    assert_golden("popup_free_effect_selector_while_scanning", &mut h);
}

#[test]
fn capital_n_belongs_to_the_session_tab_and_opens_nothing_on_the_others() {
    for tab in ['2', '3', '4', '5'] {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.press(KeyCode::Char(tab)).press(KeyCode::Char('N'));
        assert!(h.s.selector.is_none(), "tab {tab}");
        assert_eq!(h.commands(), Vec::<String>::new(), "tab {tab}");
    }
}

#[test]
fn popup_free_effect_selector_when_the_scan_is_done_has_the_title_of_its_own_and_no_word_of_a_scan() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('N'));
    let frame = h.dump();
    assert!(frame.contains("Select Free Effect"), "{frame}");
    assert!(!frame.contains("scanning"), "{frame}");
}

#[test]
fn the_help_tab_lists_the_key_that_adds_an_effect_that_stands_by_itself() {
    let h = Harness::new(scene_split());
    assert!(h.s.help_lines.iter().any(|line| line.trim_start().starts_with("N ") && line.contains("Add an effect that stands by itself")), "{:?}", h.s.help_lines);
}

#[test]
fn popup_group_target_selector() {
    // `t` on a group modulator: member params, member modulators, bus effects
    // and sibling modulators, each labelled by owner.
    let mut h = Harness::new(scene_group());
    let group_lfo = group_modulator(&h, 0, 0);
    h.goto(group_lfo);
    h.press(KeyCode::Char('t'));
    assert_golden("popup_group_target_selector", &mut h);
}

#[test]
fn popup_lane_target_selector() {
    // `t` on a lane modulator: the whole instrument chain, labelled by plugin.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('t'));
    assert_golden("popup_lane_target_selector", &mut h);
}

#[test]
fn popup_modulate_parameter() {
    // `m` on a focused parameter: a new modulator of each kind / attach to one of the session's own (each says whose notes it hears).
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m'));
    assert_golden("popup_modulate_parameter", &mut h);
}

#[test]
fn popup_value_entry() {
    // Enter on a focused float parameter: the value popup with its range.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Enter);
    assert_golden("popup_value_entry", &mut h);
}

#[test]
fn popup_group_assign() {
    // `g` on an instrument: new group / ungroup / existing groups.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('g'));
    assert_golden("popup_group_assign", &mut h);
}

// ---------------------------------------------------------------------------
// Piano tab
// ---------------------------------------------------------------------------

#[test]
fn piano_c_major_triad_held() {
    // C4 E4 G4 held in C major: held in-scale keys, the chord readout, the
    // scale strip.
    let mut h = Harness::new(scene_split());
    for note in [60, 64, 67] {
        h.s.ctl.held.note_on(note);
    }
    h.press(KeyCode::Char('2'));
    assert_golden("piano_c_major_triad_held", &mut h);
}

#[test]
fn piano_off_scale_note_held() {
    // C#4 held in C major (Highlight mode): the "wrong note" red.
    let mut h = Harness::new(scene_split());
    h.s.ctl.held.note_on(61);
    h.press(KeyCode::Char('2'));
    assert_golden("piano_off_scale_note_held", &mut h);
}

#[test]
fn piano_scale_selector() {
    // `k`: the scale picker (12 roots × every scale type).
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('2')).press(KeyCode::Char('k'));
    assert_golden("piano_scale_selector", &mut h);
}

#[test]
fn piano_locked_mode_in_d_dorian() {
    // `l` toggles Locked; the scale comes from the shared filter. D dorian,
    // view shifted up an octave.
    let mut h = Harness::new(scene_split());
    h.s.ctl.piano_filter.set_scale(ScaleSetting::parse("D dorian").expect("scale parses"));
    h.press(KeyCode::Char('2')).press(KeyCode::Char('l')).press(KeyCode::Char(']'));
    assert_golden("piano_locked_mode_in_d_dorian", &mut h);
}

// ---------------------------------------------------------------------------
// Scope tab
// ---------------------------------------------------------------------------

/// `frames` of the output as the audio thread keeps them for the scope: a sine of `left` (hertz, amplitude) on the left side and one of `right` on the right.
fn sound(h: &Harness, frames: usize, left: (f64, f64), right: (f64, f64)) {
    let side = |(hertz, amplitude): (f64, f64)| -> Vec<f32> {
        (0..frames).map(|n| (amplitude * (std::f64::consts::TAU * hertz * n as f64 / f64::from(SAMPLE_RATE)).sin()) as f32).collect()
    };
    h.s.ctl.telemetry.scope.push(&[side(left), side(right)]);
}

#[test]
fn scope_wave_of_a_sine_on_each_side() {
    // The wave across 21 ms: ten turns of a sine of 468.75 Hz, 0.8 high on the left (teal) and half of that on the right (green), the status line and the keys.
    let mut h = Harness::new(scene_split());
    sound(&h, 2048, (468.75, 0.8), (468.75, 0.4));
    h.press(KeyCode::Char('3'));
    assert_golden("scope_wave_of_a_sine_on_each_side", &mut h);
}

#[test]
fn scope_wave_in_silence() {
    // Nothing has come out yet: a flat line along the middle between the lines at a half, and a dash for the peak.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('3'));
    assert_golden("scope_wave_in_silence", &mut h);
}

#[test]
fn scope_spectrum_of_a_sine() {
    // `v`: the spectrum of a sine of 1.5 kHz, 0.5 high on both sides: a peak at 1.5 kHz, between the lines of 1k and 10k, and a peak of -6.0 dB.
    let mut h = Harness::new(scene_split());
    sound(&h, 2048, (1500.0, 0.5), (1500.0, 0.5));
    h.press(KeyCode::Char('3')).press(KeyCode::Char('v'));
    assert_golden("scope_spectrum_of_a_sine", &mut h);
}

#[test]
fn scope_peak_over_full_scale_is_red() {
    // A sine of 1.5 high clips: the peak is +3.5 dB, in red, and the wave stops at the edges.
    let mut h = Harness::new(scene_split());
    sound(&h, 2048, (468.75, 1.5), (468.75, 1.5));
    h.press(KeyCode::Char('3'));
    assert_golden("scope_peak_over_full_scale_is_red", &mut h);
}

#[test]
fn the_keys_of_the_scope_tab_choose_the_view_and_the_time_base() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('3'));
    assert_eq!((h.s.scope.kind(), scope_view::TIMES[h.s.scope.time()]), (scope_view::Kind::Wave, 1024));
    h.press(KeyCode::Right);
    assert_eq!(scope_view::TIMES[h.s.scope.time()], 2048);
    h.press(KeyCode::Left).press(KeyCode::Left);
    assert_eq!(scope_view::TIMES[h.s.scope.time()], 512);
    h.press(KeyCode::Char('v'));
    assert_eq!(h.s.scope.kind(), scope_view::Kind::Spectrum);
    h.press(KeyCode::Char('v'));
    assert_eq!(h.s.scope.kind(), scope_view::Kind::Wave);
    assert_eq!(h.commands(), Vec::<String>::new(), "the scope is nothing of the audio thread's");
    assert!(!h.s.ctl.dirty, "and nothing of the session's");
}

#[test]
fn the_keys_of_the_scope_tab_are_the_other_tabs_own_elsewhere() {
    // `v` on the Session tab is the instrument's volume, and Left/Right there are what they were: the scope's choices stay where they are.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('v'));
    assert!(h.s.gain_editing.is_some(), "the volume popup");
    h.press(KeyCode::Esc).press(KeyCode::Left).press(KeyCode::Right);
    assert_eq!((h.s.scope.kind(), scope_view::TIMES[h.s.scope.time()]), (scope_view::Kind::Wave, 1024));
}

// ---------------------------------------------------------------------------
// The scope of the selected node, on the Session tab
// ---------------------------------------------------------------------------

/// `frames` of a sine as the audio thread keeps them for the node that has probe ring `ring`, the same on both sides.
fn sound_at(h: &Harness, ring: usize, frames: usize, hertz: f64, amplitude: f64) {
    let side: Vec<f32> = (0..frames).map(|n| (amplitude * (std::f64::consts::TAU * hertz * n as f64 / f64::from(SAMPLE_RATE)).sin()) as f32).collect();
    h.s.ctl.telemetry.probes[ring].push(&[side.clone(), side]);
}

#[test]
fn o_shows_a_scope_of_the_selected_node_that_the_audio_thread_is_asked_for_and_o_again_takes_it_away() {
    let mut h = Harness::new(scene_two_effects());
    h.commands();
    assert!(!h.s.strip.on, "the scope is not there to begin with");
    h.press(KeyCode::Char('o'));
    assert!(h.s.strip.on);
    assert_eq!(h.commands(), ["ClearProbes", "SetProbe ring=0 Instrument(0)"]);
    assert_eq!(h.s.strip.watching, Some(h.s.ctl.model.instruments[0].node));
    h.press(KeyCode::Char('o'));
    assert!(!h.s.strip.on);
    assert_eq!(h.commands(), ["ClearProbes"]);
    assert_eq!(h.s.strip.watching, None);
    assert!(!h.s.ctl.dirty, "a scope is nothing of the session's");
}

#[test]
fn the_scope_follows_the_chain_cursor_and_has_no_signal_at_a_modulator() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('o'));
    h.commands();
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    assert_eq!(h.commands(), ["ClearProbes", "SetProbe ring=0 Effect { inst: 0, index: 0 }"]);
    h.goto(TreeAddress::Effect { inst: 0, index: 1 });
    assert_eq!(h.commands(), ["ClearProbes", "SetProbe ring=0 Effect { inst: 0, index: 1 }"]);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    assert_eq!(h.commands(), ["ClearProbes"], "a modulator puts out no sound");
    assert_eq!(h.s.strip.watching, None);
    h.goto(TreeAddress::Instrument(0));
    assert_eq!(h.commands().last().map(String::as_str), Some("SetProbe ring=0 Instrument(0)"));
}

#[test]
fn the_scope_follows_a_group_and_the_effects_of_its_bus() {
    let mut h = Harness::new(scene_bus_pair());
    h.press(KeyCode::Char('o'));
    h.commands();
    h.goto(TreeAddress::GroupEffect { group: 0, index: 1 });
    assert_eq!(h.commands().last().map(String::as_str), Some("SetProbe ring=0 GroupEffect { group: 0, index: 1 }"));
    h.goto(TreeAddress::Group(0));
    assert_eq!(h.commands().last().map(String::as_str), Some("SetProbe ring=0 Group(0)"));
}

#[test]
fn the_scope_is_asked_for_while_the_session_tab_is_shown_and_not_while_another_is() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('o'));
    h.commands();
    h.press(KeyCode::Char('2'));
    assert_eq!(h.commands(), ["ClearProbes"]);
    assert!(h.s.strip.on, "it is still on, and comes back");
    h.press(KeyCode::Char('3'));
    assert_eq!(h.commands(), Vec::<String>::new());
    h.press(KeyCode::Char('1'));
    assert_eq!(h.commands(), ["ClearProbes", "SetProbe ring=0 Instrument(0)"]);
}

#[test]
fn o_is_text_in_a_popup_and_in_the_filter_of_the_parameters_and_the_scope_tab_has_no_such_key() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('v'));
    assert!(h.s.gain_editing.is_some(), "the volume popup");
    h.press(KeyCode::Char('o'));
    assert!(!h.s.strip.on, "a letter in a popup is text");
    h.press(KeyCode::Esc).press(KeyCode::Enter).press(KeyCode::Char('/')).press(KeyCode::Char('o'));
    assert!(!h.s.strip.on, "and in the filter of the parameters");
    h.press(KeyCode::Esc).press(KeyCode::Esc).press(KeyCode::Char('3')).press(KeyCode::Char('o')).press(KeyCode::Char('O'));
    assert!(!h.s.strip.on, "and the Scope tab's keys are its own");
    assert_eq!(h.s.strip.scope.kind(), scope_view::Kind::Wave, "the Scope tab's keys are not the strip's");
    h.press(KeyCode::Char('5')).press(KeyCode::Char('O'));
    assert_eq!(h.s.strip.scope.kind(), scope_view::Kind::Wave, "nor are the Help tab's");
    assert_eq!(h.commands(), Vec::<String>::new());
}

#[test]
fn big_o_chooses_the_view_of_the_scope_of_a_node_and_the_scope_tabs_choice_is_not_changed() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('o'));
    assert_eq!(h.s.strip.scope.kind(), scope_view::Kind::Wave);
    h.press(KeyCode::Char('O'));
    assert_eq!(h.s.strip.scope.kind(), scope_view::Kind::Spectrum);
    assert_eq!(h.s.scope.kind(), scope_view::Kind::Wave, "the Scope tab's is its own");
    h.press(KeyCode::Char('O'));
    assert_eq!(h.s.strip.scope.kind(), scope_view::Kind::Wave);
}

#[test]
fn the_picture_of_the_node_that_was_watched_is_forgotten_when_the_cursor_goes_to_another() {
    // A spectrum falls slowly where the sound is quieter than it was: the levels of the filter would be seen under the name of the reverb for seconds.
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Down).press(KeyCode::Char('o')).press(KeyCode::Char('O'));
    sound_at(&h, 0, 2048, 1500.0, 0.5);
    h.dump();
    assert!(h.s.strip.scope.peak() > 0.4 && h.s.strip.scope.levels().iter().any(|&level| level > -20.0), "the filter's sound is taken");
    h.press(KeyCode::Down);
    assert_eq!(h.s.strip.watching, Some(h.s.ctl.model.instruments[0].effects[1].node), "the reverb is watched now");
    assert_eq!(h.s.strip.scope.peak(), 0.0);
    assert!(h.s.strip.scope.levels().iter().all(|&level| level == scope_view::FLOOR_DB), "and the picture begins from nothing");
    // (The view is the strip's own choice and is kept.)
    assert_eq!(h.s.strip.scope.kind(), scope_view::Kind::Spectrum);
}

/// The line that is the top of the strip's box: what it is of and what it shows.
fn strip_title(h: &mut Harness) -> String {
    h.dump().lines().find(|line| line.contains("Scope: ")).map(str::to_owned).expect("a strip with a title")
}

#[test]
fn the_scope_is_called_after_the_node_that_it_is_of_a_group_by_its_name_or_by_its_place() {
    let mut h = Harness::new(scene_bus_pair());
    h.press(KeyCode::Char('o'));
    for (at, name) in [
        (TreeAddress::Group(0), "Pad"),
        (TreeAddress::GroupEffect { group: 0, index: 0 }, "Filter"),
        (TreeAddress::GroupEffect { group: 0, index: 1 }, "Reverb"),
        (TreeAddress::Instrument(0), "Oscillator"),
    ] {
        h.goto(at);
        assert!(strip_title(&mut h).contains(&format!("Scope: {name} ")), "{at:?}: {}", strip_title(&mut h));
    }
    let mut unnamed = Harness::new("[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n");
    unnamed.press(KeyCode::Char('o')).goto(TreeAddress::Group(0));
    assert!(strip_title(&mut unnamed).contains("Scope: Group 1 "), "{}", strip_title(&mut unnamed));
}

#[test]
fn a_lane_that_has_no_instrument_has_no_signal_to_look_at_and_its_effects_have() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('d'));
    assert!(h.s.ctl.model.instruments[0].instrument.is_none(), "the lane is empty and keeps its effects");
    h.press(KeyCode::Char('o'));
    assert_eq!(h.s.strip.watching, None);
    assert!(h.dump().contains("No signal here"));
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    assert_eq!(h.s.strip.watching, Some(h.s.ctl.model.instruments[0].effects[0].node));
    assert!(!h.dump().contains("No signal here"));
}

#[test]
fn the_peak_of_the_scope_of_a_node_is_red_over_full_scale_and_not_under_it() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('o'));
    // (A sine of 1.5 high clips: +3.5 dB.)
    sound_at(&h, 0, 2048, 468.75, 1.5);
    let loud = h.dump();
    assert!(loud.contains("Peak: +3.5 dB") && loud.contains("fg=Rgb(235, 80, 80)"), "{loud}");
    sound_at(&h, 0, 2048, 468.75, 0.5);
    let quiet = h.dump();
    assert!(quiet.contains("Peak: -6.0 dB") && !quiet.contains("Rgb(235, 80, 80)"), "{quiet}");
}

#[test]
fn a_node_that_a_browser_takes_out_is_let_go_of_and_the_one_that_the_cursor_is_on_then_is_listened_to_at_the_next_tick() {
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Char('o'));
    let (filter, reverb) = (h.s.ctl.model.instruments[0].effects[0].node, h.s.ctl.model.instruments[0].effects[1].node);
    assert_eq!(h.s.strip.watching, Some(filter));
    h.commands();
    // A browser takes the filter out; the loop reacts to it, and its next turn begins with a tick. The reverb is where the filter was.
    let applied = h.s.ctl.apply(Intent::RemoveNode { node: filter }).unwrap();
    h.s.react_to(&applied);
    assert_eq!(h.s.selected_signal().map(|(node, _)| node), Some(reverb), "the cursor is on the reverb now");
    tick_at(&mut h.s, Instant::now());
    assert_eq!(h.s.strip.watching, Some(reverb));
    assert_eq!(h.commands().last().map(String::as_str), Some("SetProbe ring=0 Effect { inst: 0, index: 0 }"));
    assert!(h.s.ctl.probe_ring(reverb).is_some());
}

#[test]
fn the_chain_keeps_its_cursor_in_view_in_the_rows_that_the_scope_leaves_it() {
    // Thirty lanes are more than the chain has rows: the tab has 30 rows (the terminal's 32 less the tab bar and the action bar), the strip takes 10 of them and the box two.
    let mut h = Harness::new("[[instrument]]\nplugin = \"builtin:osc\"\n".repeat(30));
    h.press(KeyCode::Char('o'));
    h.goto(TreeAddress::Instrument(29));
    h.dump();
    let rows = 30 - usize::from(scope_view::STRIP_ROWS) - 2;
    let (selected, offset) = (h.s.chain_state.selected, h.s.chain_state.offset);
    assert_eq!(selected, 29);
    assert!(selected >= offset && selected < offset + rows, "the cursor ({selected}) is in the {rows} rows from {offset}");
}

#[test]
fn the_spectrum_of_the_scope_of_a_node_falls_slowly_and_a_tick_does_not_make_it_start_again() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Down).press(KeyCode::Char('o')).press(KeyCode::Char('O'));
    sound_at(&h, 0, 2048, 1500.0, 0.5);
    h.dump();
    let highest = |h: &Harness| h.s.strip.scope.levels().iter().copied().fold(f32::MIN, f32::max);
    let loud = highest(&h);
    assert!(loud > -10.0, "{loud}");
    // The sound stops, and the loop ticks before its next frame: a level falls 40 dB a second from where it was (a few thousandths of a second ago), and does not begin at the floor.
    h.s.ctl.telemetry.probes[0].push(&[vec![0.0; 2048], vec![0.0; 2048]]);
    tick_at(&mut h.s, Instant::now());
    h.dump();
    let after = highest(&h);
    assert!(after <= loud && after > loud - 20.0, "{after} from {loud}");
}

#[test]
fn scope_strip_wave_of_a_sine_at_the_filter() {
    // The filter is selected, and what the engine stores at its output is a sine of 468.75 Hz, 0.8 high: ten turns across the 21 ms, under the chain and the parameters.
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Down).press(KeyCode::Char('o'));
    sound_at(&h, 0, 2048, 468.75, 0.8);
    assert_golden("scope_strip_wave_of_a_sine_at_the_filter", &mut h);
}

#[test]
fn scope_strip_spectrum_of_a_sine_at_the_filter() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Down).press(KeyCode::Char('o')).press(KeyCode::Char('O'));
    sound_at(&h, 0, 2048, 1500.0, 0.5);
    assert_golden("scope_strip_spectrum_of_a_sine_at_the_filter", &mut h);
}

#[test]
fn scope_strip_at_a_modulator_says_that_there_is_no_signal() {
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('o'));
    assert_golden("scope_strip_at_a_modulator_says_that_there_is_no_signal", &mut h);
}

#[test]
fn scope_strip_in_silence_at_the_instrument() {
    let mut h = Harness::new(scene_two_effects());
    h.press(KeyCode::Char('o'));
    assert_golden("scope_strip_in_silence_at_the_instrument", &mut h);
}

#[test]
fn the_scope_has_the_frames_of_the_ring_that_its_node_has_and_not_another_ring_s() {
    // A client of the web interface watches the instrument, which has ring 0; the terminal's filter has ring 1. Ring 0 is loud and ring 1 is quiet.
    let mut h = Harness::new(scene_two_effects());
    h.s.ctl.set_probes(crate::app::probes::Watcher(1), &[h.s.ctl.model.instruments[0].node]);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Char('o'));
    assert_eq!(h.s.ctl.probe_ring(h.s.ctl.model.instruments[0].effects[0].node), Some(1));
    sound_at(&h, 0, 2048, 468.75, 0.9);
    sound_at(&h, 1, 2048, 468.75, 0.25);
    let frame = strip_title(&mut h);
    assert!(frame.contains("Peak: -12.0 dB"), "{frame}");
}

#[test]
fn a_node_that_has_no_ring_shows_silence_and_says_so() {
    // Eight rings are held (by clients of the web interface, say) and the node that the terminal watches has none.
    let lanes = "[[instrument]]\nplugin = \"builtin:osc\"\n".repeat(9);
    let mut h = Harness::new(&lanes);
    let held: Vec<NodeId> = (0..8).map(|lane| h.s.ctl.model.instruments[lane].node).collect();
    h.s.ctl.set_probes(crate::app::probes::Watcher(1), &held);
    h.goto(TreeAddress::Instrument(8)).press(KeyCode::Char('o'));
    assert_eq!(h.s.ctl.probe_ring(h.s.ctl.model.instruments[8].node), None);
    let frame = h.dump();
    assert!(frame.contains("No ring is free"), "{frame}");
    assert!(!strip_title(&mut h).contains("Peak"), "there is nothing to say of the peak of a signal that is not listened to");
    // (When one is let go of, the node has it, and the message is gone.)
    h.s.ctl.set_probes(crate::app::probes::Watcher(1), &held[1..]);
    assert_eq!(h.s.ctl.probe_ring(h.s.ctl.model.instruments[8].node), Some(0));
    assert!(!h.dump().contains("No ring is free"));
    assert!(strip_title(&mut h).contains("Peak"));
}

#[test]
fn choosing_a_scale_in_the_picker_is_an_intent_that_the_controller_carries_out() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('2')).press(KeyCode::Char('k'));
    h.type_chars("d dorian").press(KeyCode::Enter);
    assert_eq!(h.s.ctl.piano_filter.scale().short(), "D dorian");
    // One intent, so a front-end that watches the session hears of it, and a change to save.
    assert_eq!((h.s.ctl.rev, h.s.ctl.dirty), (1, true));
    assert_eq!(h.commands(), Vec::<String>::new(), "the scale is nothing of the audio thread's");

    // Choosing the scale that is set again is no change.
    h.press(KeyCode::Char('k')).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.rev, 1);
}

#[test]
fn the_lock_key_is_an_intent_that_the_controller_carries_out_and_a_second_press_undoes() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('2')).press(KeyCode::Char('l'));
    assert_eq!(h.s.ctl.piano_filter.mode(), PianoMode::Locked);
    assert_eq!((h.s.ctl.rev, h.s.ctl.dirty), (1, true));
    h.press(KeyCode::Char('l'));
    assert_eq!(h.s.ctl.piano_filter.mode(), PianoMode::Highlight);
    assert_eq!(h.s.ctl.rev, 2);
}

// ---------------------------------------------------------------------------
// Scenarios: key sequences → engine commands + the tree the user sees
//
// The golden renders above pin what the model *looks like*; these pin what
// the keys *do* — the commands sent to the audio thread and the index
// fix-ups that keep the TUI model and the audio graph in step.
// ---------------------------------------------------------------------------

#[test]
fn arrow_keys_adjust_a_parameter_in_model_and_engine() {
    // Param focus on `detune` (range ±24): Right = +5% of the range, Shift+Left
    // = −1% (fine).
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=1 value=2.400"]);
    h.press_with(KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=1 value=1.920"]);

    let detune = &h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params[1];
    assert_eq!(detune.name, "detune");
    assert!((detune.value - 1.92).abs() < 1e-4, "{}", detune.value);
    assert!(h.s.ctl.dirty);
}

#[test]
fn enum_parameters_step_one_value_at_a_time() {
    // `waveform` (sine → triangle) and `type` on a filter (lowpass → highpass):
    // Right moves exactly one label, whatever the modifiers.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=0 value=1.000"]);
}

#[test]
fn typing_a_value_sets_the_parameter() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Enter); // focus params, open the value popup
    h.press_n(KeyCode::Backspace, 4).type_chars("0.8").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=2 slot=1 param=0 value=0.800"]);
    assert!(h.s.editing.is_none(), "the popup closes");
    assert_eq!(h.s.ctl.model.instruments[2].effects[0].params[0].value, 0.8);
}

/// What `str::parse` takes for a number and no number can be: `NaN` (which `clamp` keeps) and
/// the infinities.
const NOT_NUMBERS: [&str; 4] = ["nan", "NaN", "inf", "-infinity"];

#[test]
fn a_number_typed_into_a_popup_is_a_finite_number_with_or_without_spaces_around_it() {
    use super::parse_number;
    assert_eq!(parse_number("0.5"), Some(0.5));
    assert_eq!(parse_number("  -12 "), Some(-12.0));
    assert_eq!(parse_number("1e3"), Some(1000.0));
    for text in NOT_NUMBERS.into_iter().chain(["", "abc", "1,5", "0.5.1"]) {
        assert_eq!(parse_number(text), None, "{text:?}");
    }
}

#[test]
fn a_tempo_that_is_not_a_number_is_not_taken() {
    for text in NOT_NUMBERS {
        let mut h = Harness::new(scene_split());
        h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars(text).press(KeyCode::Enter);
        assert!(h.s.bpm_editing.is_none(), "{text}: the popup closes");
        assert_eq!(h.s.ctl.model.bpm, 120.0, "{text}");
        assert_eq!(h.commands(), Vec::<String>::new(), "{text}");
        assert!(!h.s.ctl.dirty, "{text}");
    }
}

#[test]
fn a_value_that_is_not_a_number_is_not_taken_by_a_modulators_rate() {
    for text in NOT_NUMBERS {
        let mut h = Harness::new(scene_split());
        h.goto(TreeAddress::FreeModulator { index: 0 });
        h.press(KeyCode::Enter).press_n(KeyCode::Down, 2).press(KeyCode::Enter); // focus params, the rate row, the value popup
        assert!(h.s.editing.is_some(), "{text}: the popup opens");
        h.press_n(KeyCode::Backspace, 6).type_chars(text).press(KeyCode::Enter);
        assert!(h.s.editing.is_none(), "{text}: the popup closes");
        assert_eq!(h.commands(), Vec::<String>::new(), "{text}");
        let ModSourceSlot::Lfo { rate, .. } = h.s.ctl.model.free_modulators[0].source else { panic!("an LFO") };
        assert_eq!(rate, 4.0, "{text}");
    }
}

#[test]
fn arrow_keys_and_typing_set_a_bus_effect_parameter() {
    let mut h = Harness::new(scene_bus_pair());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Left); // the filter's cutoff, 5% down from fully open
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
    let cutoff = &h.s.ctl.model.groups[0].effects[0].params[0];
    assert_eq!((cutoff.name.as_str(), cutoff.value), ("cutoff", 0.95));
    assert!(h.s.ctl.dirty);

    h.s.ctl.dirty = false;
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("0.3").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.300"]);
    assert_eq!(h.s.ctl.model.groups[0].effects[0].params[0].value, 0.3);
    assert!(h.s.ctl.dirty);

    // Past the ends of the range (0..1), by the arrows and by typing.
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("0.99").press(KeyCode::Enter);
    h.commands();
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("-9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.000"]);
}

#[test]
fn an_enum_parameter_snaps_to_a_whole_value_when_it_was_between_two() {
    // A preset or a plugin can leave an enum at a fraction; the next step lands
    // on a label.
    let mut h = Harness::new(scene_bus_pair());
    h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[0].value = 0.4; // waveform
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=0 value=1.000"]);

    h.press(KeyCode::Esc);
    h.s.ctl.model.groups[0].effects[0].params[2].value = 0.4; // the filter's type
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=2 value=1.000"]);
}

#[test]
fn the_command_carries_the_plugins_own_parameter_index() {
    // The row and the plugin's index for it differ when a plugin hides
    // parameters; here one is renumbered to tell them apart.
    let mut h = Harness::new(scene_bus_pair());
    h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[1].index = 42; // detune
    h.s.ctl.model.groups[0].effects[0].params[0].index = 17; // the filter's cutoff
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=42 value=2.400"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("3").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=42 value=3.000"]);

    h.press(KeyCode::Esc);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Up).press(KeyCode::Left); // the parameter cursor was on row 1
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=17 value=0.950"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("0.5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=17 value=0.500"]);
}

#[test]
fn a_float_parameter_stops_at_the_ends_of_its_range() {
    // `volume` on the oscillator: 0..1, starting at 1. The command goes out
    // whether or not the value moved.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Down);
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=1.000"; 2]);

    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("99").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=1.000"]);
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("-5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=0.000"]);
    assert_eq!(h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params[2].value, 0.0);
}

#[test]
fn an_enum_parameter_stops_at_its_first_and_last_label() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter); // `waveform`: sine, triangle, square, saw, noise
    for _ in 0..6 {
        h.press(KeyCode::Right);
    }
    let values: Vec<String> = h.commands();
    assert_eq!(
        values,
        [1.0, 2.0, 3.0, 4.0, 4.0, 4.0].map(|v| format!("SetParameter inst=0 slot=0 param=0 value={v:.3}"))
    );
    for _ in 0..6 {
        h.press(KeyCode::Left);
    }
    assert_eq!(
        h.commands(),
        [3.0, 2.0, 1.0, 0.0, 0.0, 0.0].map(|v| format!("SetParameter inst=0 slot=0 param=0 value={v:.3}"))
    );
}

#[test]
fn arrow_keys_edit_the_parameter_the_filter_shows() {
    // With "res" typed into the parameter filter the first row is `resonance`
    // (parameter 1 of the filter), not `cutoff`.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('/')).type_chars("res").press(KeyCode::Enter);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetParameter inst=2 slot=1 param=1 value=1.182"]);
    assert!((h.s.ctl.model.instruments[2].effects[0].params[1].value - 1.182).abs() < 1e-3);
}

#[test]
fn a_change_of_a_modulators_setting_made_elsewhere_leaves_the_parameter_filter_being_typed_as_it_is() {
    // The TUI is filtering the filter's parameters ("cut" typed, not yet confirmed) when a browser turns the LFO's
    // rate and the depth of its cable. The tree shows both, so it is built again, and that is not a new node.
    let session = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n\
        [[instrument.modulator]]\nrate = 1.0\n[[instrument.modulator.target]]\nslot = 1\nparam = \"cutoff\"\ndepth = 0.4\n";
    let mut h = Harness::new(session);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('/')).type_chars("cut");
    assert!(h.s.param_filtering);
    assert!(!h.dump().contains("resonance"), "(only the cutoff is let through)");

    let lfo = h.s.ctl.model.free_modulators[0].node;
    let cable = h.s.ctl.model.free_modulators[0].targets[0].edge;
    for intent in [
        Intent::SetModulatorField { node: lfo, field: ModulatorField::Rate, value: 7.0 },
        Intent::SetEdgeDepth { edge: cable, depth: 0.9 },
    ] {
        let applied = h.s.ctl.apply(intent).unwrap();
        h.s.react_to(&applied);
    }
    assert!(h.tree().iter().any(|line| line.contains("7.0Hz") && line.contains("(90%)")), "the tree shows them: {:#?}", h.tree());
    assert!(h.s.param_filtering, "still being typed");
    assert_eq!(h.s.param_filter_input.value, "cut");
    assert_eq!(h.s.chain_state.selected, h.s.tree_entries.iter().position(|e| e.address == TreeAddress::Effect { inst: 0, index: 0 }).unwrap());
    // And on the screen: what is typed, and only the rows it lets through.
    let after = h.dump();
    assert!(after.contains("/ cut") && after.contains("cutoff") && !after.contains("resonance"), "{after}");
}

#[test]
fn the_parameter_filter_goes_when_the_node_under_the_cursor_is_another_one() {
    // "res" is typed and confirmed for the filter, and the cursor is in its parameters. The filter is taken out (by
    // another front-end) and the reverb is at its place in the tree: the cursor is at the same address, and on another
    // node, which is not what the filter was typed for.
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('/')).type_chars("res").press(KeyCode::Enter);
    assert_eq!(h.s.param_filter_input.value, "res");
    h.s.ctl.model.remove_effect(0, 0);
    h.s.rebuild_tree();
    assert_eq!(h.s.tree_entries.get(h.s.chain_state.selected).map(|e| e.address), Some(TreeAddress::Effect { inst: 0, index: 0 }));
    assert_eq!(h.s.ctl.model.instruments[0].effects[0].name, "Reverb");
    assert_eq!(h.s.param_filter_input.value, "", "the reverb is shown whole");
    assert!(!h.s.param_filtering);
}

const LANE_WITH_FILTER_AND_LFO_AND_A_PLAIN_ONE: &str =
    "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument.modulator]]\nrate = 2.0\n\
     [[instrument]]\nplugin = \"builtin:osc\"\nvolume = 0.5\n";

#[test]
fn shift_down_and_up_on_an_instrument_move_the_whole_lane_with_its_effects_and_modulators() {
    let mut h = Harness::new(LANE_WITH_FILTER_AND_LFO_AND_A_PLAIN_ONE);
    h.goto(TreeAddress::Instrument(0));
    assert!(h.tree()[1].contains("fx Filter") && h.tree()[2].contains("LFO"), "{:#?}", h.tree());
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SwapInstruments a=0 b=1"]);
    // The lane with the filter and the LFO is the second now, and has them under it; the cursor stays on it.
    assert!(h.tree()[0].starts_with('♪') && h.tree()[1].starts_with('♪'), "{:#?}", h.tree());
    assert!(h.tree()[2].contains("fx Filter") && h.tree()[3].contains("LFO"), "{:#?}", h.tree());
    assert_eq!(h.s.selected_address(), Some(&TreeAddress::Instrument(1)));
    assert_eq!(h.s.ctl.model.instruments[0].volume, 0.5);
    // And back.
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SwapInstruments a=1 b=0"]);
    assert!(h.tree()[1].contains("fx Filter") && h.tree()[2].contains("LFO"), "{:#?}", h.tree());
    assert_eq!(h.s.selected_address(), Some(&TreeAddress::Instrument(0)));
    // The first cannot go up and the last cannot go down.
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    h.goto(TreeAddress::Instrument(1)).press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty(), "{:?}", h.commands());
}

#[test]
fn d_on_an_instrument_empties_it_and_leaves_the_lanes_effects_and_modulators() {
    let mut h = Harness::new(LANE_WITH_FILTER_AND_LFO_AND_A_PLAIN_ONE);
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["ClearInstrument inst=0 keep"]);
    assert!(h.s.ctl.model.instruments[0].instrument.is_none());
    assert!(h.tree().iter().any(|line| line.contains("fx Filter")) && h.tree().iter().any(|line| line.contains("LFO")), "{:#?}", h.tree());
    // A second `d` removes the lane (that has no plugin now), with what it had.
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveInstrument inst=0 keep"]);
    assert_eq!(h.s.ctl.model.instruments.len(), 1);
}

#[test]
fn clicking_and_dragging_a_parameter_bar_sets_its_value() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let mut h = Harness::new(scene_split());
    h.dump(); // draws once, which lays out the panes
    let area = h.s.areas.param_inner;
    // `volume` is the third row; the bar starts 27 columns in and spans the
    // width less 36.
    let (row, bar_start, bar_width) = (area.y + 2, area.x + 27, area.width - 36);
    let mouse = |kind, column| Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });

    let left = crossterm::event::MouseButton::Left;
    process_event(&mut h.s, mouse(MouseEventKind::Down(left), bar_start + bar_width - 1));
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=1.000"], "the right end of the bar");
    process_event(&mut h.s, mouse(MouseEventKind::Drag(left), bar_start));
    assert_eq!(h.commands(), ["SetParameter inst=0 slot=0 param=2 value=0.000"], "dragged to the left end");
    process_event(&mut h.s, mouse(MouseEventKind::Drag(left), bar_start + bar_width / 2));
    let halfway = h.commands();
    let value: f32 = halfway[0].rsplit('=').next().unwrap().parse().unwrap();
    assert!((value - 0.5).abs() < 0.05, "dragged to the middle: {halfway:?}");
    process_event(&mut h.s, mouse(MouseEventKind::Up(crossterm::event::MouseButton::Left), bar_start));
    process_event(&mut h.s, mouse(MouseEventKind::Drag(crossterm::event::MouseButton::Left), bar_start));
    assert_eq!(h.commands(), Vec::<String>::new(), "the drag is over");
    assert!(h.s.ctl.dirty);
}

#[test]
fn the_gain_popups_open_on_the_current_value() {
    let mut h = Harness::new(scene_group());
    let prefill = |h: &Harness| h.s.gain_editing.as_ref().map(|g| g.edit.input.value.clone());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v'));
    assert_eq!(prefill(&h).as_deref(), Some("0.80"), "the bass's volume");
    h.press(KeyCode::Esc);
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('v'));
    assert_eq!(prefill(&h).as_deref(), Some("0.80"), "the group's volume");
    h.press(KeyCode::Esc);
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Char('x'));
    assert_eq!(prefill(&h).as_deref(), Some("1.00"), "the filter's mix");
}

#[test]
fn the_mix_popup_sets_a_lane_effects_dry_wet_within_zero_and_one() {
    let mut h = Harness::new(scene_two_effects()); // a filter, then a reverb at 0.5
    h.goto(TreeAddress::Effect { inst: 0, index: 1 });
    h.press(KeyCode::Char('x'));
    assert_eq!(h.s.gain_editing.as_ref().unwrap().edit.input.value, "0.50");
    h.press_n(KeyCode::Backspace, 6).type_chars("0.25").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetMix inst=0 slot=2 value=0.250"]);
    assert_eq!(h.s.ctl.model.instruments[0].effects[1].mix, 0.25);
    assert_eq!(h.s.ctl.model.instruments[0].effects[0].mix, 1.0, "the other effect is untouched");
    assert!(h.s.ctl.dirty);

    for (typed, command, mix) in [("5", "SetMix inst=0 slot=2 value=1.000", 1.0), ("-2", "SetMix inst=0 slot=2 value=0.000", 0.0)] {
        h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 6).type_chars(typed).press(KeyCode::Enter);
        assert_eq!(h.commands(), [command]);
        assert_eq!(h.s.ctl.model.instruments[0].effects[1].mix, mix);
    }
}

#[test]
fn the_mix_popup_sets_a_bus_effects_dry_wet_within_zero_and_one() {
    let mut h = Harness::new(scene_bus_pair()); // a filter, then a reverb at 0.5
    h.goto(TreeAddress::GroupEffect { group: 0, index: 1 });
    h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 6).type_chars("0.1").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupMix group=0 fx=1 value=0.100"]);
    assert_eq!(h.s.ctl.model.groups[0].effects[1].mix, 0.1);
    assert_eq!(h.s.ctl.model.groups[0].effects[0].mix, 1.0, "the other bus effect is untouched");
    assert!(h.s.ctl.dirty);
    h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 6).type_chars("7").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupMix group=0 fx=1 value=1.000"]);
}

#[test]
fn the_volume_popup_sets_a_groups_volume_within_zero_and_four() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("1.5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=1.500"]);
    assert_eq!(h.s.ctl.model.groups[0].volume, 1.5);
    assert!(h.s.ctl.dirty);
    for (typed, command) in [("9", "SetGroupVolume group=0 value=4.000"), ("-1", "SetGroupVolume group=0 value=0.000")] {
        h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars(typed).press(KeyCode::Enter);
        assert_eq!(h.commands(), [command]);
    }
}

#[test]
fn the_volume_popup_on_any_node_of_a_lane_sets_the_lanes_volume() {
    let mut h = Harness::new(scene_group());
    for addr in [TreeAddress::Effect { inst: 2, index: 0 }, TreeAddress::FreeModulator { index: 0 }] {
        h.goto(addr);
        h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("0.6").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["SetVolume inst=2 value=0.600"], "{addr:?}");
    }
    assert_eq!(h.s.ctl.model.instruments[2].volume, 0.6);
    assert_eq!(h.s.ctl.model.instruments[0].volume, 1.0, "the other lanes keep theirs");
}

#[test]
fn the_mix_popup_opens_only_on_an_effect() {
    let mut h = Harness::new(scene_group());
    for addr in [TreeAddress::Instrument(2), TreeAddress::Group(0), TreeAddress::FreeModulator { index: 0 }] {
        h.goto(addr);
        h.press(KeyCode::Char('x'));
        assert!(h.s.gain_editing.is_none(), "{addr:?}");
    }
}

#[test]
fn a_gain_popup_ignores_what_is_not_a_number_and_escape_cancels() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("abc").press(KeyCode::Enter);
    assert!(h.s.gain_editing.is_none(), "the popup closes");
    assert_eq!(h.commands(), Vec::<String>::new());
    assert_eq!(h.s.ctl.model.instruments[2].volume, 0.8);
    assert!(!h.s.ctl.dirty);

    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("2").press(KeyCode::Esc);
    assert!(h.s.gain_editing.is_none());
    assert_eq!(h.commands(), Vec::<String>::new());
    assert_eq!(h.s.ctl.model.instruments[2].volume, 0.8);
    assert!(!h.s.ctl.dirty);
}

#[test]
fn the_parameter_pane_sets_a_groups_volume_and_pan() {
    // The group's two rows, Volume (0..4, 0.8 to start) and Pan (-1..1, -0.3).
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=0.850"]);
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=0.950"]);
    assert!((h.s.ctl.model.groups[0].volume - 0.95).abs() < 1e-5);
    assert!(h.s.ctl.dirty);

    h.press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetGroupPan group=0 value=-0.250"]);
    assert!((h.s.ctl.model.groups[0].pan + 0.25).abs() < 1e-5);
    h.press_n(KeyCode::Right, 30);
    assert_eq!(h.commands().last().map(String::as_str), Some("SetGroupPan group=0 value=1.000"), "stops at hard right");

    // Typed values are clamped to each row's range.
    h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("-5").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupPan group=0 value=-1.000"]);
    h.press(KeyCode::Up).press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupVolume group=0 value=4.000"]);
    assert_eq!(h.s.ctl.model.groups[0].volume, 4.0);
}

#[test]
fn the_group_header_shows_the_volume_after_an_edit() {
    // The header carries the group's volume, so it has to follow the edits,
    // whichever way they are made.
    let mut h = Harness::new(scene_group());
    assert!(h.tree().contains(&"▦ Pad  vol 0.80"), "{:#?}", h.tree());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 6).type_chars("0.25").press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].volume, 0.25);
    assert!(h.tree().contains(&"▦ Pad  vol 0.25"), "after the popup: {:#?}", h.tree());

    // From the parameter pane: Volume is its first row.
    h.press(KeyCode::Enter).press(KeyCode::Right);
    assert_eq!(h.s.ctl.model.groups[0].volume, 0.3);
    assert!(h.tree().contains(&"▦ Pad  vol 0.30"), "after a nudge: {:#?}", h.tree());
}

#[test]
fn the_volume_popup_sets_instrument_volume_clamped_to_four() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('v')).press_n(KeyCode::Backspace, 4).type_chars("9").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetVolume inst=2 value=4.000"]);
    assert_eq!(h.s.ctl.model.instruments[2].volume, 4.0);
}

#[test]
fn the_mix_popup_sets_an_effects_dry_wet() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Char('x')).press_n(KeyCode::Backspace, 4).type_chars("0.25").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetGroupMix group=0 fx=0 value=0.250"]);
}

#[test]
fn the_range_popup_sets_an_instrument_range() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 5).type_chars("C2-C6").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetInstrumentRange inst=0 range=Some((36, 84))"]);
    assert_eq!(h.tree()[0], "♪ Oscillator  [Built-in]  C2-C6");
}

#[test]
fn adding_an_effect_from_the_selector_uses_the_default_mix() {
    // Reverb defaults to half wet; a filter to fully wet (a half-wet filter
    // barely filters).
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 plugin=Reverb mix=0.500"]);
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertEffect inst=0 index=1 plugin=Filter mix=1.000"]);
    assert_eq!(h.tree()[1], "├ fx Reverb  [Built-in]");
    assert_eq!(h.tree()[2], "├ fx Filter  [Built-in]");
    assert!(h.s.ctl.dirty);
}

#[test]
fn an_effect_from_the_selector_comes_with_its_presets_and_enum_parameters() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);

    let effects = &h.s.ctl.model.instruments[0].effects;
    let reverb = &effects[0];
    assert_eq!((reverb.name.as_str(), reverb.id.as_str(), reverb.is_instrument), ("Reverb", "builtin:reverb", false));
    assert!(!reverb.presets.is_empty(), "the preset list comes with the plugin");
    assert_eq!(reverb.current_preset, None);
    let filter = &effects[1];
    let kind = filter.params.iter().find(|p| p.name == "type").map(|p| &p.kind);
    assert!(matches!(kind, Some(ParamKind::Enum(labels)) if labels.len() > 4), "the filter's type is an enum");
    assert!(filter.params.iter().all(|p| p.value == p.default));
}

/// The catalog names a plugin that is not there to load (a scan found it, and it has gone since).
fn catalog_with_a_plugin_that_has_gone(h: &mut Harness) {
    h.s.ctl.catalog.add(vec![plugin::PluginInfo {
        name: "Vanished".into(),
        id: "./no-such-dir/Vanished.clap".into(),
        is_instrument: false,
        param_count: 3,
        preset_count: 0,
        path: "./no-such-dir/Vanished.clap".into(),
        scan_ms: 0,
    }]);
}

#[test]
fn an_effect_that_cannot_be_added_leaves_the_session_as_it_was() {
    let mut h = Harness::new(scene_split());
    catalog_with_a_plugin_that_has_gone(&mut h);
    let tree: Vec<String> = h.tree().iter().map(|label| label.to_string()).collect();
    h.press(KeyCode::Char('a')).type_chars("vanish").press(KeyCode::Enter);
    assert!(h.commands().is_empty(), "nothing is sent to the audio thread");
    assert_eq!(h.tree(), tree, "the tree is as it was");
    assert!(!h.s.ctl.dirty, "nothing was changed");
    assert!(h.s.selector.is_none(), "the popup is closed");
    assert!(h.s.ctl.model.instruments[0].effects.is_empty());
}

// A plugin that is not a built-in can take seconds to load, and the loop is blocked for as long as it takes, so a frame that says what is going on is drawn first.
// `process_event` is the two halves of what the loop does with a key together (the keys of a test have no frame to draw between them): `handle_event`, and then
// what it asked that waits for a frame (`run_pending`).

fn enter() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
}

#[test]
fn a_plugin_that_is_not_a_built_in_waits_for_a_frame_that_says_it_is_loading() {
    let mut h = Harness::new(scene_split());
    catalog_with_a_plugin_that_has_gone(&mut h);
    h.press(KeyCode::Char('a')).type_chars("vanish");
    handle_event(&mut h.s, enter());
    // The popup is closed, nothing has been asked of the audio thread or changed, and what is to be done waits.
    assert!(h.s.selector.is_none());
    assert_eq!(h.s.pending_load.as_ref().map(|p| p.name.as_str()), Some("Vanished"));
    assert!(h.commands().is_empty());
    assert!(h.s.ctl.model.instruments[0].effects.is_empty());
    // The frame says what it waits for.
    let frame = h.dump();
    assert!(frame.contains("Loading Vanished…"), "{frame}");
    // Then it is done: here it is refused, the plugin is not there, and the session is as it was.
    run_pending(&mut h.s);
    assert!(h.s.pending_load.is_none());
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);
    assert!(!h.dump().contains("Loading"));
}

#[test]
fn a_built_in_is_added_at_once_and_waits_for_no_frame() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let effects = h.s.ctl.model.instruments[0].effects.len();
    h.press(KeyCode::Char('a')).type_chars("filter");
    handle_event(&mut h.s, enter());
    assert!(h.s.pending_load.is_none());
    assert_eq!(h.s.ctl.model.instruments[0].effects.len(), effects + 1);
    assert_eq!(h.commands().len(), 1);
}

#[test]
fn what_waits_for_the_frame_is_asked_for_when_the_frame_has_been_drawn() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let parent = h.s.ctl.model.instruments[0].node;
    h.s.pending_load = Some(PendingLoad { name: "Filter".to_string(), intent: Intent::AddEffect { parent, plugin: "builtin:filter".to_string() } });
    assert!(h.s.ctl.model.instruments[0].effects.is_empty());
    run_pending(&mut h.s);
    assert!(h.s.pending_load.is_none());
    assert_eq!(h.s.ctl.model.instruments[0].effects.len(), 1);
    assert_eq!(h.commands().len(), 1);
}

#[test]
fn the_box_that_says_a_plugin_is_loading_is_drawn_in_the_middle_of_the_session() {
    let mut h = Harness::new(scene_split());
    catalog_with_a_plugin_that_has_gone(&mut h);
    h.press(KeyCode::Char('a')).type_chars("vanish");
    handle_event(&mut h.s, enter());
    assert_golden("loading_a_plugin", &mut h);
}

#[test]
fn the_loop_draws_the_frame_before_it_loads_and_draws_nothing_for_a_key_that_loads_nothing() {
    let mut h = Harness::new(scene_split());
    catalog_with_a_plugin_that_has_gone(&mut h);
    let blank = |h: &Harness| h.terminal.backend().buffer().content().iter().all(|cell| cell.symbol() == " ");
    // A key that loads nothing: no frame (the loop draws its own at the top of the next round).
    process_event_showing_the_load(&mut h.terminal, &mut h.s, Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))).unwrap();
    assert!(blank(&h));
    for c in "vanish".chars() {
        process_event_showing_the_load(&mut h.terminal, &mut h.s, Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))).unwrap();
    }
    assert!(blank(&h));
    // The key that asks for the plugin: the frame that is on the terminal when it has been tried (and refused, the plugin is not there) is the one drawn before.
    process_event_showing_the_load(&mut h.terminal, &mut h.s, enter()).unwrap();
    assert!(h.s.pending_load.is_none());
    assert!(h.s.ctl.model.instruments[0].effects.is_empty());
    let shown = dump_buffer(h.terminal.backend().buffer());
    assert!(shown.contains("Loading Vanished…"), "{shown}");
}

#[test]
fn a_bus_effect_that_cannot_be_added_leaves_the_session_as_it_was() {
    let mut h = Harness::new(scene_group());
    catalog_with_a_plugin_that_has_gone(&mut h);
    h.goto(TreeAddress::Group(0));
    let tree: Vec<String> = h.tree().iter().map(|label| label.to_string()).collect();
    let buses = h.s.ctl.model.groups[0].effects.len();
    h.press(KeyCode::Char('a')).type_chars("vanish").press(KeyCode::Enter);
    assert!(h.commands().is_empty(), "nothing is sent to the audio thread");
    assert_eq!(h.tree(), tree, "the tree is as it was");
    assert!(!h.s.ctl.dirty, "nothing was changed");
    assert_eq!(h.s.ctl.model.groups[0].effects.len(), buses);
}

#[test]
fn adding_a_group_effect_from_the_selector() {
    let mut h = Harness::new(scene_group());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["InsertGroupEffect group=0 index=1 plugin=Filter mix=1.000"]);
    assert!(h.tree().contains(&"  fx Filter  [Built-in]  (bus)"));
}

#[test]
fn capital_n_adds_an_effect_that_stands_by_itself_in_a_lane_of_its_own() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let lanes = h.s.ctl.model.instruments.len();
    h.press(KeyCode::Char('N'));
    // The selector lists the effects and no instrument, wherever the cursor is, and nothing is made until one is chosen.
    let selector = h.s.selector.as_ref().expect("the selector is open");
    assert_eq!(selector.mode, SelectorMode::FreeEffect);
    assert!(!selector.items.is_empty() && selector.items.iter().all(|item| item.cells[0] != "Oscillator"));
    assert_eq!(h.commands(), Vec::<String>::new());
    h.type_chars("filter").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["AddInstrument range=None", "SetLaneRoute inst=2 route=none", "InsertEffect inst=2 index=0 plugin=Filter mix=1.000"]);
    assert!(h.s.selector.is_none());
    // A lane that has no instrument and the effect for its chain, at the end of the session: a bare module, whose way out is cut (the tree says so).
    assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
    let lane = &h.s.ctl.model.instruments[lanes];
    assert!(lane.instrument.is_none());
    assert!(h.tree().iter().any(|line| line.contains("(empty)") && line.contains("[no out]")), "{:#?}", h.tree());
    assert_eq!(lane.effects.iter().map(|effect| effect.name.as_str()).collect::<Vec<_>>(), ["Filter"]);
    let tree = h.tree();
    let at = tree.iter().position(|line| line.contains("(empty)")).expect("the lane is in the tree, with no instrument");
    assert!(tree[at + 1].contains("fx Filter"), "{tree:#?}");
}

#[test]
fn capital_n_is_for_the_chain_and_the_selector_is_shut_by_escape_with_nothing_made() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let lanes = h.s.ctl.model.instruments.len();
    // (Not in a field of the parameters, where a capital is text.)
    h.press(KeyCode::Char('N')).press(KeyCode::Esc);
    assert!(h.s.selector.is_none());
    assert_eq!(h.commands(), Vec::<String>::new());
    assert_eq!(h.s.ctl.model.instruments.len(), lanes);
    h.press(KeyCode::Enter).press(KeyCode::Char('N'));
    assert!(h.s.selector.is_none(), "with the focus in the parameters the key is not the chain's");
}

#[test]
fn n_adds_a_full_range_oscillator_lane_and_opens_the_instrument_selector() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let lanes = h.s.ctl.model.instruments.len();
    h.press(KeyCode::Char('n'));

    // The lane exists on the audio thread with an oscillator in it already, so
    // that it makes sound even if the selector is dismissed.
    assert_eq!(
        h.commands(),
        ["AddInstrument range=None", "SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false"]
    );
    assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
    let lane = &h.s.ctl.model.instruments[lanes];
    assert_eq!((lane.range, lane.transpose, lane.volume, lane.group), (None, 0, 1.0, None));
    assert!(lane.effects.is_empty() && lane.pattern.is_none());
    let osc = lane.instrument.as_ref().expect("the lane defaults to the oscillator");
    assert_eq!((osc.name.as_str(), osc.id.as_str(), osc.format.as_str()), ("Oscillator", "builtin:osc", "Built-in"));
    assert!(osc.is_instrument);
    assert_eq!((osc.mix, osc.current_preset.as_deref()), (1.0, None));
    assert!(osc.params.iter().all(|p| p.value == p.default), "a new plugin starts at its defaults");
    let waveform = osc.params.iter().find(|p| p.name == "waveform").unwrap();
    assert!(matches!(&waveform.kind, ParamKind::Enum(labels) if labels.len() == 5), "labelled parameters are enums");
    assert!(h.s.ctl.dirty);
    assert_eq!(h.s.selected_address().copied(), Some(TreeAddress::Instrument(lanes)), "the cursor is on the new lane");
    assert_eq!(h.s.selector.as_ref().map(|sel| sel.mode), Some(SelectorMode::Instrument));
}

#[test]
fn cancelling_the_selector_after_n_leaves_the_oscillator() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('n'));
    h.commands();
    h.press(KeyCode::Esc);
    assert!(h.s.selector.is_none());
    assert_eq!(h.commands(), Vec::<String>::new(), "nothing more is sent");
    assert_eq!(h.s.ctl.model.instruments[2].instrument.as_ref().unwrap().name, "Oscillator");
}

#[test]
fn choosing_a_plugin_after_n_swaps_it_into_the_new_lane() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('n'));
    h.commands();
    h.type_chars("oscillator").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false keep"]);
    assert!(h.s.selector.is_none());
}

#[test]
fn the_instrument_selector_starts_on_the_instrument_that_the_lane_has_so_that_enter_keeps_it() {
    // (The audio input sorts before the oscillator, so that the first row is not the one that a hand that presses `n` and Enter, or `i` and Enter, has always chosen.)
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let chosen = |h: &Harness| {
        let sel = h.s.selector.as_ref().expect("the selector is open");
        let item = sel.filter.selected_item(&sel.items).expect("a row is selected");
        h.s.ctl.catalog.plugins()[item.index].id.clone()
    };
    h.press(KeyCode::Char('n'));
    h.commands();
    assert_eq!(chosen(&h), "builtin:osc", "the lane that `n` made has the oscillator");
    h.press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SwapInstrument inst=2 plugin=Oscillator bufs=2 remap=false keep"]);
    assert_eq!(h.s.ctl.model.instruments[2].instrument.as_ref().unwrap().id, "builtin:osc");
    assert_eq!(h.s.ctl.model.instruments[2].route, Route::Out, "the lane is heard");
    // `i` on a lane that has an instrument starts on it (the lane that `n` made: the scene's own lanes have stand-ins); on a lane that has none the list starts at its first row.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('i'));
    assert_eq!(chosen(&h), "builtin:osc");
    h.press(KeyCode::Esc);
    h.press(KeyCode::Char('d')); // takes the instrument out of the lane, which stays
    h.commands();
    h.press(KeyCode::Char('i'));
    assert_eq!(chosen(&h), "builtin:input", "an empty lane: the first row");
}

#[test]
fn an_audio_input_chosen_in_the_selector_is_in_a_lane_that_has_its_way_out_and_w_cuts_it() {
    // (The terminal makes the lane with an oscillator and the selector replaces it: the lane is the user's, and has its way out, which `w` cuts, as it does for any lane.)
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.press(KeyCode::Char('n'));
    h.commands();
    h.type_chars("audio").press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SwapInstrument inst=2 plugin=Audio Input bufs=2 remap=false keep"]);
    assert_eq!(h.s.ctl.model.instruments[2].instrument.as_ref().unwrap().id, "builtin:input");
    assert_eq!(h.s.ctl.model.instruments[2].route, Route::Out);
    h.press(KeyCode::Char('w'));
    assert_eq!(h.s.ctl.model.instruments[2].route, Route::None);
}

#[test]
fn replacing_the_instrument_loads_a_fresh_plugin() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[2].value = 0.25; // volume, edited
    // (The scene's lane has a stand-in for an instrument, which the list does not have: it starts at its first row, the audio input, so the oscillator is asked for by name.)
    h.press(KeyCode::Char('i')).type_chars("oscillator").press(KeyCode::Enter);
    // (The lane's vibrato cable into the old plugin goes first: see the next test.)
    assert_eq!(
        h.commands(),
        ["RemoveFreeModTarget mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]
    );
    let volume = &h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params[2];
    assert_eq!((volume.name.as_str(), volume.value), ("volume", 1.0), "edits don't survive a swap");
}

#[test]
fn replacing_the_instrument_takes_out_the_cables_into_the_old_one() {
    // The lane's LFO drives the old instrument's `detune` by that parameter's place in the
    // old plugin: left in place it would drive whatever sits there in the new one (f51).
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    h.goto(TreeAddress::Instrument(0));
    // (The scene's lane has a stand-in for an instrument, which the list does not have: it starts at its first row, the audio input, so the oscillator is asked for by name.)
    h.press(KeyCode::Char('i')).type_chars("oscillator").press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["RemoveFreeModTarget mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]
    );
    let lfo = &h.s.ctl.model.free_modulators[0];
    assert!(lfo.targets.is_empty(), "the cable into the old plugin is gone from the model too");
    assert_eq!(h.s.ctl.model.free_modulators[1].targets.len(), 1, "the other lane's cable stays");
}

#[test]
fn m_on_an_instrument_adds_an_lfo_of_the_sessions_own_that_hears_its_lane_and_lists_it_with_the_lane() {
    let mut h = Harness::new(scene_split());
    let before = h.tree().len();
    h.goto(TreeAddress::Instrument(1));
    let lane = h.s.ctl.model.instruments[1].node;
    h.press(KeyCode::Char('m'));
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=2 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }".to_string(),
            format!("SetModulatorEar Free index=2 ear={}", lane.0)
        ]
    );
    assert_eq!(h.s.ctl.model.free_modulators[2].ear, Some(lane));
    // It is listed with the lane that it hears, after the modulator that the lane had, which is the session's own too, and the cursor is on it.
    let tree = h.tree();
    assert_eq!(tree.len(), before + 1);
    assert_eq!(tree[3], "├ ~ LFO 7.0Hz sine → Oscillator: detune (15%)", "{tree:?}");
    assert_eq!(tree[4], "╰ ~ LFO 1.0Hz sine", "{tree:?}");
    assert_eq!(h.s.selected_address().copied(), Some(TreeAddress::FreeModulator { index: 2 }));
}

#[test]
fn the_tree_of_a_session_whose_modulators_are_moved_to_its_own_is_the_tree_it_was_with_each_under_the_lane_it_hears() {
    // Lanes only: a modulator that hears a lane is listed where the lane's own was, as it was said (a cable into the lane's own plugin by the plugin's name).
    for scene in [scene_split(), scene_two_effects(), scene_small_depths(), scene_three_kinds_of_modulator(), scene_macro(), scene_steps(), scene_synced(), scene_notes()] {
        assert_eq!(Harness::new(scene).tree(), Harness::new(scene).tree(), "{scene}");
    }
    // A session that is built with one that hears no lane has it first, in the words of every free modulator.
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Char('F'));
    assert_eq!(h.tree()[0], "~ LFO 1.0Hz sine  (free)");
    assert_eq!(h.tree().len(), Harness::new(scene_split()).tree().len() + 1);
}

#[test]
fn a_modulator_that_hears_a_group_is_listed_under_it_after_its_bus_effects_and_one_that_drives_another_lane_says_which() {
    let mut h = Harness::new(scene_group());
    let tree = h.tree();
    // The group's two, in the order of the session's own after the bass's, with what they drive by the lane or the group of the plugin.
    let at = tree.iter().position(|line| line.contains("fx Reverb  [Built-in]  (bus)")).expect("the pad's bus effect");
    assert_eq!(tree[at + 1], "  ~ LFO 0.5Hz sine → M0 Oscillator: detune (40%), Bus Reverb: damping (25%)  (bus)");
    assert_eq!(tree[at + 2], "  ~ ADSR → Mod 1 rate (30%)  (bus)");
    // The bass's own is under the bass.
    assert!(tree.contains(&"╰ ~ LFO 0.2Hz triangle → Filter: cutoff (50%)"), "{tree:#?}");
    // One that hears the bass and drives a parameter of the pad's first lane says whose it is.
    let (bass_lfo, osc) = (h.s.ctl.model.free_modulators[0].node, node_at(&h, NodePath::Instrument(0)));
    let detune = h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|row| row.name == "detune").unwrap().index;
    h.s.apply(Intent::AddModEdge { modulator: bass_lfo, to: crate::app::graph::Endpoint { node: osc, port: crate::app::graph::Port::Param { index: detune } } });
    assert!(h.tree().iter().any(|line| line.starts_with("╰ ~ LFO 0.2Hz triangle → Filter: cutoff (50%), L1 Oscillator: detune (50%)")), "{:#?}", h.tree());
}

#[test]
fn m_on_a_group_adds_an_lfo_of_the_sessions_own_that_hears_the_group() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    let group = h.s.ctl.model.groups[0].node;
    h.press(KeyCode::Char('m'));
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", group.0)
        ]
    );
    assert_eq!(h.s.ctl.model.free_modulators[3].ear, Some(group));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 4);
}

#[test]
fn deleting_a_bus_effect_drops_the_group_modulators_bus_targets() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    // (The group LFO's cable to the reverb's damping goes first, then the reverb.)
    assert_eq!(h.commands(), ["RemoveFreeModTarget mod=1 target=1", "RemoveGroupEffect group=0 index=0 keep"]);
    let tree = h.tree();
    assert!(!tree.iter().any(|l| l.contains("Reverb")), "{tree:#?}");
    assert!(tree.contains(&"  ~ LFO 0.5Hz sine → M0 Oscillator: detune (40%)  (bus)"), "{tree:#?}");
}

#[test]
fn deleting_a_group_modulator_drops_cross_mod_targets_pointing_at_it() {
    // The ADSR's only target is "Mod 0 rate"; with Mod 0 gone it has nothing
    // left to drive, and no target may keep pointing at a stale index.
    let mut h = Harness::new(scene_group());
    let group_lfo = group_modulator(&h, 0, 0);
    h.goto(group_lfo);
    h.press(KeyCode::Char('d'));
    // The ADSR's cable to its rate goes first, and then the LFO, with its own cables; what those drove, a member's
    // detune and a bus effect's parameter, has the value the user set.
    assert_eq!(
        h.commands(),
        [
            "RemoveFreeModTarget mod=2 target=0",
            "RemoveFreeModulator index=1",
            "SetParameter inst=0 slot=0 param=1 value=0.000",
            "SetGroupParameter group=0 fx=0 param=2 value=0.350"
        ]
    );
    assert_eq!(hearing_group(&h.s.ctl.model, 0).len(), 1);
    assert!(hearing_group(&h.s.ctl.model, 0)[0].targets.is_empty());
    assert!(h.tree().contains(&"  ~ ADSR  (bus)"), "{:#?}", h.tree());
}

#[test]
fn deleting_a_group_member_drops_its_modulator_targets() {
    // Lane 0 is group member 0, targeted by the group LFO's `detune` target. The
    // first `d` takes the cable into its plugin out and clears the plugin, the second
    // removes the (now empty) lane; the member-0 target goes with the plugin and the
    // bus target stays.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('d')).press(KeyCode::Char('d'));
    // (The group's cable to the member goes first, then the plugin, then the lane.)
    assert_eq!(h.commands(), ["RemoveFreeModTarget mod=1 target=0", "ClearInstrument inst=0 keep", "RemoveInstrument inst=0 keep"]);
    let lfo = &hearing_group(&h.s.ctl.model, 0)[0];
    assert_eq!(lfo.targets.len(), 1);
    assert_eq!(lfo.targets[0].param_name, "damping");
    assert!(
        h.tree().contains(&"  ~ LFO 0.5Hz sine → Bus Reverb: damping (25%)  (bus)"),
        "{:#?}",
        h.tree()
    );
}

#[test]
fn the_group_popup_takes_a_lane_out_of_its_group_and_moves_it_to_another() {
    // Lane 1 is the Pad group's second member; the group's cables are all to the first.
    let mut h = Harness::new([scene_group(), "[[group]]\nname = \"Lead\"\n".to_string()].concat());
    h.goto(TreeAddress::Instrument(1));
    // The popup lists "New group", "Ungroup" (the lane is in one), then "Pad (current)" and "Lead".
    h.press(KeyCode::Char('g')).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetLaneGroup inst=1 group=None"]);
    assert_eq!(h.s.ctl.model.instruments[1].group, None);
    assert!(h.s.ctl.dirty);
    // Now in no group, it is offered "New group", then the two groups: "Lead" is the third.
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Char('g')).press_n(KeyCode::Down, 2).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["SetLaneGroup inst=1 group=Some(1)"]);
    assert_eq!(h.s.ctl.model.instruments[1].group, Some(1));
    assert_eq!(h.s.ctl.model.instruments[0].group, Some(0), "the others are where they were");
}

#[test]
fn the_group_popup_takes_a_member_the_group_drives_out_of_it_and_the_cable_that_drives_it_stays() {
    // Lane 0 is the Pad group's first member, and the group's LFO drives its detune: the cable names the plugin by its node, so that it drives it wherever the lane goes.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('g')).press(KeyCode::Down).press(KeyCode::Enter); // "New group", then "Ungroup"
    assert_eq!(h.commands(), ["SetLaneGroup inst=0 group=None"], "no cable is taken out, and nothing is given back");
    assert_eq!(h.s.ctl.model.instruments[0].group, None);
    // (The lane is not a member of the group now, and the label of the cable says whose plugin it is by the lane.)
    assert!(h.tree().contains(&"  ~ LFO 0.5Hz sine → L1 Oscillator: detune (40%), Bus Reverb: damping (25%)  (bus)"), "{:#?}", h.tree());
}

#[test]
fn the_rename_popup_names_a_group_and_a_name_of_nothing_gives_the_default_name_back() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 3); // "Pad" is in the popup
    for ch in "Strings".chars() {
        h.press(KeyCode::Char(ch));
    }
    h.press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].name.as_deref(), Some("Strings"));
    assert!(h.tree().iter().any(|l| l.starts_with("▦ Strings")), "{:#?}", h.tree());
    assert!(h.s.ctl.dirty);
    assert!(h.commands().is_empty(), "the audio thread has no use for a name");
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 7).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].name, None);
    assert!(h.tree().iter().any(|l| l.starts_with("▦ Group 1")), "{:#?}", h.tree());
}

#[test]
fn the_rename_popup_takes_no_more_characters_than_a_name_can_have() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 3);
    h.press_n(KeyCode::Char('x'), 70).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.groups[0].name.as_deref().map(|name| name.chars().count()), Some(64));
}

#[test]
fn deleting_a_group_ungroups_its_members() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('d'));
    // The cable that drives the bus's reverb goes (a cable to a plugin goes with the plugin), then the group. The modulators that heard it stay, with the rest of their cables (they drive
    // the plugins of the members by node, and the members stay), and hear every note.
    assert_eq!(h.commands(), ["RemoveFreeModTarget mod=1 target=1", "RemoveGroup group=0 keep"]);
    assert!(h.s.ctl.model.groups.is_empty());
    assert!(h.s.ctl.model.instruments.iter().all(|n| n.group.is_none()));
    assert_eq!(h.tree().iter().filter(|l| l.starts_with("♪")).count(), 3);
    let tree = h.tree();
    assert_eq!(tree[0], "~ LFO 0.5Hz sine → L1 Oscillator: detune (40%)  (free)", "{tree:#?}");
    assert_eq!(tree[1], "~ ADSR → Mod 1 rate (30%)  (free)", "{tree:#?}");
}

#[test]
fn the_param_cursor_survives_moving_between_nodes_and_is_clamped() {
    // Moving the chain cursor swaps the parameter pane's contents but keeps the
    // parameter cursor's row, clamped to the new pane's length (a group's bus
    // pane has two rows, an oscillator twenty).
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 7).press(KeyCode::Esc);
    assert_eq!((h.s.param_state.selected, h.s.param_state.len), (7, 20));
    h.goto(TreeAddress::Instrument(1));
    assert_eq!((h.s.param_state.selected, h.s.param_state.len), (7, 20), "same row on the next oscillator");
    h.goto(TreeAddress::Group(0));
    assert_eq!((h.s.param_state.selected, h.s.param_state.len), (1, 2), "clamped to the bus pane");
}

// The engine remaps or drops a modulator's plugin targets when its effects are
// removed or reordered, so that modulation keeps following the same plugin. The
// model has to do the same, or the tree shows the wrong plugin and a saved
// session attaches the modulation to a different effect on reload.

#[test]
fn deleting_the_last_effect_keeps_the_earlier_targets() {
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 1 });
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["RemoveFreeModTarget mod=0 target=1", "RemoveEffect inst=0 index=1 keep"]);
    assert_eq!(
        h.tree(),
        ["♪ Oscillator  [Built-in]", "├ fx Filter  [Built-in]", "╰ ~ LFO 0.5Hz sine → Filter: cutoff (40%)"]
    );
}

#[test]
fn reordering_bus_effects_moves_group_modulator_targets_with_their_effects() {
    let mut h = Harness::new(scene_bus_pair());
    let label = "  ~ LFO 0.5Hz sine → Bus Filter: cutoff (40%), Bus Reverb: damping (25%)  (bus)";
    let label = label.replace("(25%)", "(30%)");
    assert!(h.tree().contains(&label.as_str()), "{:#?}", h.tree());
    let kinds_before: Vec<String> = crate::app::testutil::kinds_at(&h.s.ctl.model, ModulatorLocation::Free, 0).iter().map(|kind| format!("{kind:?}")).collect();
    assert_eq!(kinds_before.len(), 2);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    assert!(!h.s.ctl.dirty);
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderGroupEffect group=0 from=0 to=1"]);
    assert!(h.s.ctl.dirty, "a session with its bus effects in another order has something to save");
    assert_eq!(h.tree()[2], "  fx Reverb  [Built-in]  (bus)");
    assert_eq!(h.tree()[3], "  fx Filter  [Built-in]  (bus)");
    assert!(h.tree().contains(&label.as_str()), "targets follow their effects: {:#?}", h.tree());
    // (A cable names the plugin that it drives by its node: the cables are what they were while the effects change places.)
    let kinds: Vec<String> = crate::app::testutil::kinds_at(&h.s.ctl.model, ModulatorLocation::Free, 0).iter().map(|kind| format!("{kind:?}")).collect();
    assert_eq!(kinds, kinds_before);

    // The filter is the last now, and has nowhere to go: nothing is sent, and nothing is left to save.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);

    // Moving the filter back up restores the original indices.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["ReorderGroupEffect group=0 from=1 to=0"]);
    assert!(h.s.ctl.dirty);
    let kinds: Vec<String> = crate::app::testutil::kinds_at(&h.s.ctl.model, ModulatorLocation::Free, 0).iter().map(|kind| format!("{kind:?}")).collect();
    assert_eq!(kinds, kinds_before);
    assert!(h.tree().contains(&label.as_str()), "{:#?}", h.tree());
    // Nor does the first go further up.
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
    assert!(h.commands().is_empty());
    assert!(!h.s.ctl.dirty);
}

#[test]
fn a_saved_session_does_not_keep_targets_of_a_deleted_effect() {
    // The symptom the stale model produced: the old target was written out with
    // its old slot, and on reload attached to whichever effect now sits there.
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    h.s.ctl.session_path = Some(path.clone());
    h.s.ctl.save().unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("cutoff"), "the deleted filter's target was saved:\n{saved}");
    assert!(saved.contains("param = \"damping\""), "{saved}");
    assert!(saved.contains("to = \"instrument[0].effect[0]\""), "the reverb moved to the first place of the chain:\n{saved}");
    assert!(!saved.contains("effect[1]"), "no target may point past the chain:\n{saved}");
}

// A group modulator addresses a member's plugins by (member, slot) too, so the
// same has to hold for it when a member's effects go or move.

#[test]
fn the_tree_says_a_depth_of_a_fraction_of_a_percent_as_it_is_in_a_lane_and_in_a_group() {
    let h = Harness::new(scene_small_depths());
    // (A depth under ten percent that is not a whole percentage has the decimal it needs, which the sign keeps; the rest are whole, and a depth
    // within a twentieth of none is 0 and not -0.)
    let lane_line = h.tree().into_iter().find(|l| l.contains("~ LFO") && !l.contains("(bus)")).expect("the lane's LFO is in the tree");
    assert_eq!(
        lane_line.trim_start(),
        "╰ ~ LFO 0.5Hz sine → Delay: time (0.4%), Delay: feedback (2.5%), Delay: tone (26%), Delay: pingpong (-0.4%), Oscillator: detune (0%), Oscillator: volume (-25%)"
    );
    let bus_line = h.tree().into_iter().find(|l| l.contains("~ LFO") && l.contains("(bus)")).expect("the group's LFO is in the tree");
    assert_eq!(bus_line, "  ~ LFO 0.5Hz sine → Bus Delay: time (0.4%)  (bus)");
}

#[test]
fn a_saved_session_does_not_keep_group_targets_of_a_deleted_member_effect() {
    // The symptom the stale model produced: the reverb's target was written out
    // with its old slot 2, and the filter's with slot 1, so on reload the filter's
    // modulation landed on the reverb and the reverb's on nothing.
    let mut h = Harness::new(scene_member_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.toml");
    h.s.ctl.session_path = Some(path.clone());
    h.s.ctl.save().unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("effect[1]"), "no target may point past the member's chain:\n{saved}");
    assert_eq!(saved.matches("param = \"cutoff\"").count(), 1, "only member 1's cutoff is left:\n{saved}");
    assert_eq!(saved.matches("param = \"damping\"").count(), 1, "{saved}");
    assert_eq!(saved.matches("effect[0]").count(), 2, "member 0's damping and member 1's cutoff:\n{saved}");
}

// ---------------------------------------------------------------------------
// Smoke
// ---------------------------------------------------------------------------

#[test]
fn harness_sends_nothing_until_asked() {
    // Constructing and drawing a `State` must not talk to the engine
    // (`State::new` is pure; the initial transpose / pattern push lives in `run`).
    let mut h = Harness::new(scene_group());
    let _ = h.dump();
    assert!(h.commands().is_empty());
}

// ---------------------------------------------------------------------------
// The CLIP light
// ---------------------------------------------------------------------------

#[test]
fn the_status_bar_shows_clip_while_the_master_meter_is_clipping() {
    use crate::telemetry::MASTER;
    let mut h = Harness::new(scene_split());
    let t0 = Instant::now();
    tick_at(&mut h.s, t0);
    assert!(!h.dump().contains("CLIP"), "dark until the audio thread counts a clipped buffer");

    h.s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut h.s, t0 + Duration::from_millis(100));
    assert_golden("status_clip", &mut h);

    tick_at(&mut h.s, t0 + Duration::from_millis(2_099));
    assert!(h.dump().contains("CLIP"), "still on just under two seconds after the last clip");
    tick_at(&mut h.s, t0 + Duration::from_millis(2_100));
    assert!(!h.dump().contains("CLIP"), "and off after");
    assert!(h.dump().contains("120 BPM"), "the tempo is still there");
}

#[test]
fn the_tick_the_event_loop_runs_lights_the_clip_light_too() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.telemetry.bump(crate::telemetry::MASTER.clips);
    tick(&mut h.s);
    assert!(h.s.clip.is_lit());
    assert!(h.dump().contains("CLIP"));
}

#[test]
fn clips_counted_before_the_tui_started_do_not_light_it() {
    use crate::app::model::SessionModel;
    use crate::telemetry::MASTER;
    let (engine, _audio) = stub_engine();
    engine.telemetry.bump(MASTER.clips); // the engine clipped while the splash was up
    let mut s = State::new(Controller::new(SessionModel::default(), engine, None));
    tick_at(&mut s, Instant::now());
    assert!(!s.clip.is_lit());
    s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut s, Instant::now());
    assert!(s.clip.is_lit(), "a clip after it started watching does");
}

#[test]
fn on_a_narrow_terminal_the_clip_light_takes_the_right_end_of_the_tab_bar_like_the_tempo() {
    use crate::telemetry::MASTER;
    let mut h = Harness::new(scene_split());
    h.s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut h.s, Instant::now());
    let first_row = |h: &mut Harness, width: u16| {
        h.terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        // The dump starts with its size and a heading; the first row of text follows.
        h.dump().lines().nth(2).unwrap().to_string()
    };
    let row = first_row(&mut h, 30);
    assert!(row.starts_with("(1) Session"), "{row}");
    assert!(row.contains("CLIP  120 BPM"), "the light is not lost for want of room: {row}");
    // With no room for the status at all, it is left out (as the tempo always was):
    // it wants its 13 columns and two to spare.
    let row = first_row(&mut h, 15);
    assert!(!row.contains("CLIP") && !row.contains("BPM"), "{row}");
    let row = first_row(&mut h, 16);
    assert!(row.contains("CLIP  120 BPM"), "{row}");
}

/// A real audio graph playing a session of two oscillator lanes at `lane_volume`
/// each, which publishes into the controller's telemetry, with one note held for a
/// few buffers: the loudest sample it produced, and whether the status bar shows
/// CLIP afterwards. (One oscillator peaks at a quarter of full scale at volume 1.)
fn clip_light_after_playing(lane_volume: f32) -> (f32, bool) {
    clip_light_with(lane_volume, "[master]\nlimiter = false\n")
}

/// `clip_light_after_playing`, with the `[master]` table that the session has (the limiter on the output is in the sound unless a session says it is not, and no mix that it holds under its ceiling clips).
fn clip_light_with(lane_volume: f32, master: &str) -> (f32, bool) {
    let lane = format!("[[instrument]]\nplugin = \"builtin:osc\"\nvolume = {lane_volume}\n");
    let config = session::parse(&format!("{master}{lane}{lane}")).unwrap();
    let (handles, engine) = stub_engine();
    let model = load_session(
        &config,
        Path::new("."),
        SAMPLE_RATE,
        BLOCK,
        &plugin::Runtime::default(),
        &handles.cmd_tx,
        &mut |_| {},
    )
    .unwrap();
    let mut graph = crate::plugin::chain::AudioGraph::new(2, engine.cmd_rx.clone(), engine.return_tx.clone());
    graph.set_telemetry(handles.telemetry.clone());
    let mut s = State::new(Controller::new(model, handles, None));

    let mut out = vec![vec![0.0f32; BLOCK]; 2];
    let t0 = Instant::now();
    tick_at(&mut s, t0);
    graph.process(&[(0, [0x90, 60, 127])], &mut out).unwrap();
    let mut loudest = 0.0f32;
    for _ in 0..8 {
        graph.process(&[], &mut out).unwrap();
        loudest = out[0].iter().fold(loudest, |loudest, sample| loudest.max(sample.abs()));
    }
    tick_at(&mut s, t0 + Duration::from_millis(100));
    let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
    terminal.draw(|frame| draw(frame, &mut s)).unwrap();
    (loudest, dump_buffer(terminal.backend().buffer()).contains("CLIP"))
}

#[test]
fn an_engine_that_clips_lights_the_clip_light_and_one_that_does_not_leaves_it_dark() {
    let (loudest, lit) = clip_light_after_playing(4.0);
    assert!(loudest > 1.0, "the oscillator at four times the volume is beyond full scale: {loudest}");
    assert!(lit);

    let (loudest, lit) = clip_light_after_playing(0.5);
    assert!(loudest > 0.1 && loudest <= 1.0, "a note that is heard and within full scale: {loudest}");
    assert!(!lit);
}

#[test]
fn a_session_whose_limiter_is_in_the_sound_does_not_clip_though_it_is_far_over_full_scale_and_the_light_stays_dark() {
    // The same two oscillators at four times the volume, with no `[master]` table: the limiter is in the sound, as it is for a session that does not say.
    let (loudest, lit) = clip_light_with(4.0, "");
    let ceiling = 10f32.powf(-1.0 / 20.0);
    assert!(loudest > ceiling * 0.5 && loudest <= ceiling, "the output is held under the ceiling of -1 dB: {loudest}");
    assert!(!lit);
}

// ---------------------------------------------------------------------------
// The master's limiter and volume (step 102)
// ---------------------------------------------------------------------------

/// What the TUI has told the audio thread of the output mix since the last call (`SetMaster`, which `commands` leaves out). The rest of what is in the channel is let go of.
fn master_commands(h: &Harness) -> Vec<String> {
    h.cmd_rx.try_iter().filter(crate::app::testutil::is_master).map(|command| crate::app::testutil::describe(&command)).collect()
}

#[test]
fn l_puts_the_limiter_in_the_sound_and_takes_it_out_from_anywhere_in_the_chain_and_it_is_a_step_of_the_history() {
    let mut h = Harness::new(scene_group());
    master_commands(&h);
    assert!(h.s.ctl.model.master.limiter, "the limiter is in the sound from the start");
    h.press(KeyCode::Char('L'));
    assert!(!h.s.ctl.model.master.limiter && h.s.ctl.dirty);
    let told = master_commands(&h);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].starts_with("SetMaster volume=1.000 limiter=false ceiling=-1.0"), "{told:?}");
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("turn the limiter off"));
    // The master's, whatever the cursor is on: an effect, a group.
    h.goto(TreeAddress::Effect { inst: 2, index: 0 }).press(KeyCode::Char('L'));
    assert!(h.s.ctl.model.master.limiter);
    assert!(master_commands(&h)[0].contains("limiter=true"));
    h.goto(TreeAddress::Group(0)).press(KeyCode::Char('L'));
    assert!(!h.s.ctl.model.master.limiter);
    master_commands(&h);
    // Undo puts it back in the sound, and tells the audio thread.
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(h.s.ctl.model.master.limiter);
    assert!(master_commands(&h)[0].contains("limiter=true"));
    // (Not on the other tabs, where the key is something else or nothing.)
    h.press(KeyCode::Char('2')).press(KeyCode::Char('L'));
    assert!(h.s.ctl.model.master.limiter, "the Piano tab's keys are the piano's");
}

#[test]
fn c_and_v_open_on_the_ceiling_and_the_master_volume_and_set_what_is_typed_held_to_its_range() {
    let mut h = Harness::new(scene_group());
    master_commands(&h);
    let prefill = |h: &Harness| h.s.gain_editing.as_ref().map(|g| g.edit.input.value.clone());
    h.press(KeyCode::Char('C'));
    assert_eq!(prefill(&h).as_deref(), Some("-1.00"), "the ceiling that a session has without a word");
    h.press(KeyCode::Esc).press(KeyCode::Char('V'));
    assert_eq!(prefill(&h).as_deref(), Some("1.00"), "unity");
    h.press(KeyCode::Esc);
    assert!(master_commands(&h).is_empty(), "a popup that is left says nothing");
    for (key, typed, volume, ceiling) in [('C', "-6", 1.0, -6.0), ('C', "-99", 1.0, -24.0), ('C', "3", 1.0, 0.0), ('V', "0.5", 0.5, 0.0), ('V', "9", 2.0, 0.0), ('V', "-1", 0.0, 0.0)] {
        h.press(KeyCode::Char(key)).press_n(KeyCode::Backspace, 8).type_chars(typed).press(KeyCode::Enter);
        let told = master_commands(&h);
        assert_eq!(told.len(), 1, "{key} {typed}: {told:?}");
        assert!(told[0].starts_with(&format!("SetMaster volume={volume:.3} limiter=true ceiling={ceiling:.1}")), "{key} {typed}: {told:?}");
        let master = h.s.ctl.model.master;
        assert_eq!((master.volume, master.ceiling), (volume, ceiling), "{key} {typed}");
    }
    assert!(h.s.ctl.dirty);
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("set the master volume"));
    // Text that is not a number changes nothing.
    h.press(KeyCode::Char('C')).press_n(KeyCode::Backspace, 8).type_chars("loud").press(KeyCode::Enter);
    assert!(master_commands(&h).is_empty());
    assert_eq!(h.s.ctl.model.master.ceiling, 0.0);
}

#[test]
fn the_status_bar_says_how_far_the_limiter_brings_the_sound_down_while_it_is_in_the_sound_and_works() {
    use crate::telemetry::MASTER_REDUCTION;
    let mut h = Harness::new(scene_split());
    let t0 = Instant::now();
    tick_at(&mut h.s, t0);
    let first_row = |h: &mut Harness| h.dump().lines().nth(2).unwrap().to_string();
    assert!(!h.dump().contains("LIM"), "dark while the limiter has nothing to do");
    h.s.ctl.telemetry.store(MASTER_REDUCTION, 3.24);
    let row = first_row(&mut h);
    assert!(row.ends_with("LIM -3.2 dB  120 BPM"), "{row}");
    // In yellow and bold, before the tempo.
    let buffer = h.terminal.backend().buffer();
    let line: String = (0..buffer.area.width).map(|x| buffer[(x, 0)].symbol()).collect();
    let at = line.find("LIM").expect("the light is in the tab bar");
    let cell = &buffer[(line[..at].chars().count() as u16, 0)];
    assert_eq!((cell.fg, cell.modifier), (Color::Yellow, Modifier::BOLD));
    // A reduction under a tenth of a decibel is none, and a tenth is not.
    h.s.ctl.telemetry.store(MASTER_REDUCTION, 0.09);
    assert!(!h.dump().contains("LIM"));
    h.s.ctl.telemetry.store(MASTER_REDUCTION, 0.1);
    assert!(first_row(&mut h).ends_with("LIM -0.1 dB  120 BPM"));
    // Next to the clip light, which it leaves where it was.
    h.s.ctl.telemetry.store(MASTER_REDUCTION, 12.0);
    h.s.ctl.telemetry.bump(crate::telemetry::MASTER.clips);
    tick_at(&mut h.s, t0 + Duration::from_millis(100));
    assert!(first_row(&mut h).ends_with("LIM -12.0 dB  CLIP  120 BPM"));
    // A limiter that is out of the sound says nothing, whatever the cell holds.
    h.press(KeyCode::Char('L'));
    assert!(!h.s.ctl.model.master.limiter);
    assert!(!h.dump().contains("LIM"));
}

#[test]
fn on_a_narrow_terminal_the_limiters_light_gives_way_to_the_clip_light_and_the_tempo() {
    use crate::telemetry::{MASTER, MASTER_REDUCTION};
    let mut h = Harness::new(scene_split());
    h.s.ctl.telemetry.store(MASTER_REDUCTION, 3.24);
    h.s.ctl.telemetry.bump(MASTER.clips);
    tick_at(&mut h.s, Instant::now());
    let first_row = |h: &mut Harness, width: u16| {
        h.terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
        h.dump().lines().nth(2).unwrap().to_string()
    };
    assert!(first_row(&mut h, 120).ends_with("LIM -3.2 dB  CLIP  120 BPM"));
    let row = first_row(&mut h, 80);
    assert!(row.ends_with("CLIP  120 BPM") && !row.contains("LIM"), "the limiter's light is the first to go: {row}");
}

// The ways the TUI makes a modulator or binds a target, pinned command for
// command: the modulate popup (`m` on a parameter), the target selector (`t` on a
// modulator) and MIDI-learn (`l` on a parameter). It asks the controller by the
// intents a browser asks by (`AddModulator`, `AddModEdge`, `SetEdgeDepth`).

/// What the target selector lists, one line for each row: the cells the list shows, then the way the rack addresses the target. A run of
/// plugin parameters (a chain's) is one line, with its first.
fn selector_rows(h: &Harness) -> Vec<String> {
    let selector = h.s.target_selector.as_ref().expect("the selector is open");
    let mut rows: Vec<String> = Vec::new();
    for (item, entry) in selector.items.iter().zip(&selector.entries) {
        let is_param = matches!(
            entry.kind,
            crate::plugin::chain::ModTargetKind::FreeParam { .. }
        );
        let row = if is_param {
            format!("params of {}", item.cells[0])
        } else if matches!(entry.kind, crate::plugin::chain::ModTargetKind::Gain { .. }) {
            // (A volume, a mix or a pan is named by whose it is: the node's id is not for a test to say.)
            item.cells.join(" | ")
        } else {
            format!("{} | {:?}", item.cells.join(" | "), entry.kind)
        };
        if !(is_param && rows.last() == Some(&row)) {
            rows.push(row);
        }
    }
    rows
}

// -- Free modulators (`F`, P5b-2) --

#[test]
fn session_free_modulator() {
    // A free LFO selected: in no lane and no group, it is the first of the tree (magenta), and its pane is a modulator's, with a depth row for each of its four cables (three
    // parameters of plugins in two lanes and on a bus, and the attack of the free envelope).
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    assert_golden("session_free_modulator", &mut h);
}

#[test]
fn the_free_modulators_are_the_first_entries_of_the_tree_and_say_what_their_cables_drive_by_the_lane_and_the_group_of_the_plugin() {
    let h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    let tree = h.tree();
    assert!(tree[0].starts_with("~ LFO 0.5Hz sine \u{2192} "), "{tree:?}");
    assert_eq!(
        tree[0],
        "~ LFO 0.5Hz sine \u{2192} L1 Oscillator: detune (30%), L1 Reverb: damping (20%), G1 Filter (bus): cutoff (40%), Mod 1 attack (25%)  (free)"
    );
    assert_eq!(tree[1], "~ ADSR \u{2192} L2 Filter: cutoff (50%), Mod 0 depth 0 (10%)  (free)");
    assert_eq!(h.s.tree_entries[0].address, TreeAddress::FreeModulator { index: 0 });
    assert_eq!(h.s.tree_entries[1].address, TreeAddress::FreeModulator { index: 1 });
    assert!(tree[2].starts_with("\u{25a6} Group 1"), "the group comes after them: {tree:?}");
    assert_eq!(h.s.tree_entries[0].color, Some(Color::Magenta));
}

#[test]
fn f_adds_a_free_modulator_in_the_sessions_own_rack_and_puts_the_cursor_on_it() {
    let mut h = Harness::new(scene_split());
    h.commands();
    // (The two lanes of the scene have an LFO each, which are the session's own: the new one is the third.)
    assert_eq!(h.s.ctl.model.free_modulators.len(), 2);
    h.press(KeyCode::Char('F'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 3);
    assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::FreeModulator { index: 2 });
    assert_eq!(h.tree()[0], "~ LFO 1.0Hz sine  (free)");
    assert_eq!(h.commands(), ["InsertFreeModulator index=2 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }"]);
    // A second one goes after it, and `m` on a free modulator adds one as well.
    h.press(KeyCode::Char('m'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 4);
    assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::FreeModulator { index: 3 });
    assert!(h.s.ctl.dirty);
    // Undo takes the last of them out, by the history of what a browser adds too.
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.s.ctl.model.free_modulators.len(), 3);
    h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(h.s.ctl.model.free_modulators.len(), 4);
}

#[test]
fn f_in_the_parameter_pane_is_not_a_key() {
    let mut h = Harness::new(scene_split());
    h.press(KeyCode::Enter).press(KeyCode::Char('F'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 2, "the two that the lanes have, and no more");
}

// -- What a free modulator hears (`h`, step 93) --

#[test]
fn popup_ear() {
    // `h` on a free modulator: every note, each lane and each group, the one that it hears marked.
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('h'));
    assert_golden("popup_ear", &mut h);
}

#[test]
fn h_on_a_free_modulator_lists_every_note_each_lane_and_each_group_and_the_choice_is_what_it_hears() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 1 });
    h.commands();
    h.press(KeyCode::Char('h'));
    let rows = |h: &Harness| -> Vec<String> { h.s.ear_popup.as_ref().expect("the popup is open").items.iter().map(|item| item.cells.join(" | ")).collect() };
    assert_eq!(rows(&h), ["Every note (current)", "L1 Oscillator", "L2 Oscillator", "G1 Group 1"]);
    // The second lane: Down twice and Enter. The tree says whom it hears, the audio thread is told, and the history has a step.
    h.press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Enter);
    assert!(h.s.ear_popup.is_none());
    let lane1 = h.s.ctl.model.instruments[1].node;
    assert_eq!(h.s.ctl.model.free_modulators[1].ear, Some(lane1));
    // (The tree lists it with the lane it hears now, as a lane's own modulator was listed, with the plugin it drives in that lane by its name; the cursor stays on it.)
    let line = |h: &Harness| h.tree().into_iter().find(|line| line.contains("~ ADSR")).expect("the modulator is in the tree").to_string();
    assert_eq!(line(&h), "╰ ~ ADSR \u{2192} Filter: cutoff (50%), Mod 0 depth 0 (10%)");
    assert_eq!(h.s.selected_address().copied(), Some(TreeAddress::FreeModulator { index: 1 }));
    assert_eq!(h.commands(), [format!("SetModulatorEar Free index=1 ear={}", lane1.0)]);
    assert!(h.s.ctl.dirty);
    // It is marked the next time, and Esc leaves without a change.
    h.press(KeyCode::Char('h'));
    assert_eq!(rows(&h), ["Every note", "L1 Oscillator", "L2 Oscillator (current)", "G1 Group 1"]);
    h.press(KeyCode::Down).press(KeyCode::Esc);
    assert!(h.s.ear_popup.is_none());
    assert_eq!(h.s.ctl.model.free_modulators[1].ear, Some(lane1));
    assert!(h.commands().is_empty());
    // The group (the last row, which the popup starts above), which the tree says as well, and every note again (the first row).
    h.press(KeyCode::Char('h')).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Down).press(KeyCode::Enter);
    assert!(line(&h).ends_with("(bus)"), "under the group, as a group's own were: {:?}", h.tree());
    h.press(KeyCode::Char('h')).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.free_modulators[1].ear, None);
    assert!(h.tree()[1].ends_with("(free)"), "back at the top, where the ones that hear nothing are: {:?}", h.tree());
    // Undo goes back through the three.
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(line(&h).ends_with("(bus)"), "{:?}", h.tree());
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(line(&h), "╰ ~ ADSR \u{2192} Filter: cutoff (50%), Mod 0 depth 0 (10%)");
}

#[test]
fn h_is_a_key_of_a_free_modulator_and_of_nothing_else_in_the_chain() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    for address in [TreeAddress::Instrument(0), TreeAddress::Effect { inst: 0, index: 0 }, TreeAddress::Group(0)] {
        h.goto(address);
        h.press(KeyCode::Char('h'));
        assert!(h.s.ear_popup.is_none(), "{address:?}");
    }
    // (And in the parameter pane of a free modulator it is not a key either.)
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('h'));
    assert!(h.s.ear_popup.is_none());
}

#[test]
fn the_target_selector_of_a_free_modulator_lists_every_plugin_of_the_session_and_the_siblings_settings() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('t'));
    let expected: Vec<&str> = vec![
        "params of L1 Oscillator",
        "params of L1 Reverb",
        "params of L2 Oscillator",
        "params of L2 Filter",
        "params of G1 Filter",
        "L1 Oscillator | output volume",
        "L1 Reverb | dry/wet mix",
        "L2 Oscillator | output volume",
        "L2 Filter | dry/wet mix",
        "G1 Group 1 | output volume",
        "G1 Group 1 | pan",
        "G1 Filter | dry/wet mix",
        "Mod | Mod 1 attack | ModulatorAttack { mod_index: 1 }",
        "Mod | Mod 1 decay | ModulatorDecay { mod_index: 1 }",
        "Mod | Mod 1 sustain | ModulatorSustain { mod_index: 1 }",
        "Mod | Mod 1 release | ModulatorRelease { mod_index: 1 }",
        "Mod | Mod 1 cutoff depth | ModulatorDepth { mod_index: 1, target_index: 0 }",
        "Mod | Mod 1 Mod 0 depth 0 depth | ModulatorDepth { mod_index: 1, target_index: 1 }",
    ];
    assert_eq!(selector_rows(&h), expected);
}

#[test]
fn the_target_selector_binds_the_chosen_parameter_to_the_free_modulator() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 1 });
    h.commands();
    // The first row: the first parameter of the first lane's oscillator (`waveform`).
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    let osc = h.s.ctl.model.instruments[0].node;
    assert_eq!(h.commands(), [format!("AddFreeModTarget mod=1 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.000 range=[0.000, 4.000]", osc.0)]);
    let cables = &h.s.ctl.model.free_modulators[1].targets;
    assert_eq!(cables.len(), 3, "the envelope had two");
    assert_eq!(cables[2].param_name, "waveform");
    assert_eq!(h.s.ctl.model.kind_of_target(ModulatorLocation::Free, 1, 2), Some(crate::plugin::chain::ModTargetKind::FreeParam { plugin: osc, param_index: 0 }));
    assert!(h.tree()[1].contains("L1 Oscillator: waveform (50%)"), "{:?}", h.tree());
    // A parameter that it drives already is not drawn again (the controller refuses it, and nothing is sent).
    h.goto(TreeAddress::FreeModulator { index: 1 });
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.free_modulators[1].targets.len(), 3);
    assert!(h.commands().is_empty());
}

#[test]
fn the_rows_of_a_free_modulators_pane_are_edited_as_a_lanes_are_and_the_audio_thread_is_told_by_the_free_rack() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter);
    h.commands();
    // Rows: Type, Waveform, Rate, Sync, Retrigger, Fade, Targets, then the cables' depths. The rate is the third.
    h.press_n(KeyCode::Down, 2).press(KeyCode::Right);
    let commands = h.commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(commands[0].starts_with("SetFreeModulatorRate mod=0 rate="), "{commands:?}");
    assert!(matches!(h.s.ctl.model.free_modulators[0].source, ModSourceSlot::Lfo { rate, .. } if rate > 0.5));
    // A number typed for the first cable's depth (the eighth row), by Enter.
    h.press_n(KeyCode::Down, 5).press(KeyCode::Enter).press_n(KeyCode::Backspace, 6).type_chars("0.8").press(KeyCode::Enter);
    assert!(h.commands().iter().any(|c| c.starts_with("SetFreeModTargetDepth mod=0 target=0 depth=0.800")), "the depth is told by the free rack");
    assert_eq!(h.s.ctl.model.free_modulators[0].targets[0].depth, 0.8);
    // The waveform, one along.
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 1);
    h.press(KeyCode::Right);
    assert!(matches!(h.s.ctl.model.free_modulators[0].source, ModSourceSlot::Lfo { waveform, .. } if waveform != crate::plugin::chain::LfoWaveform::Sine));
}

#[test]
fn the_type_row_of_a_free_modulator_switches_it_with_the_commands_of_the_free_rack() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter);
    h.commands();
    h.press(KeyCode::Right);
    assert!(matches!(h.s.ctl.model.free_modulators[0].source, ModSourceSlot::Envelope { .. }));
    let commands = h.commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(commands[0].starts_with("SetFreeModulatorSource mod=0 source=Envelope"), "{commands:?}");
}

#[test]
fn d_on_a_free_modulator_takes_it_out_with_its_cables_and_undo_puts_it_back() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.commands();
    h.press(KeyCode::Char('d'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 1);
    assert!(h.tree()[0].starts_with("~ ADSR"), "{:?}", h.tree());
    // (The envelope's cable to the depth of the LFO's first cable went with it, and what the LFO drove has the value that was set.)
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 1);
    let commands = h.commands();
    assert!(commands.iter().any(|c| c.starts_with("RemoveFreeModulator index=0")), "{commands:?}");
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.s.ctl.model.free_modulators.len(), 2);
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 4);
    assert_eq!(h.s.ctl.model.free_modulators[1].targets.len(), 2);
}

#[test]
fn the_pane_and_the_d_of_the_second_free_modulator_are_its_own() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    // The second is the envelope: the row after its type is its attack, which a step along makes longer, and the LFO's wave is as it was.
    let wave = |h: &Harness| match h.s.ctl.model.free_modulators[0].source {
        ModSourceSlot::Lfo { waveform, .. } => waveform,
        ref other => panic!("{other:?}"),
    };
    let before = wave(&h);
    h.goto(TreeAddress::FreeModulator { index: 1 });
    h.press(KeyCode::Enter).press(KeyCode::Down);
    h.commands();
    h.press(KeyCode::Right);
    let commands = h.commands();
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(commands[0].starts_with("SetFreeModulatorEnvelopeParam mod=1"), "{commands:?}");
    assert!(matches!(h.s.ctl.model.free_modulators[1].source, ModSourceSlot::Envelope { attack, .. } if attack > 0.05), "the envelope's attack is longer");
    assert_eq!(wave(&h), before, "and the LFO is as it was");
    // `d` takes that one out and leaves the LFO.
    h.press(KeyCode::Esc);
    h.commands();
    h.press(KeyCode::Char('d'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 1);
    assert!(matches!(h.s.ctl.model.free_modulators[0].source, ModSourceSlot::Lfo { .. }), "the LFO stays");
    let commands = h.commands();
    assert!(commands.iter().any(|c| c.starts_with("RemoveFreeModulator index=1")), "{commands:?}");
}

#[test]
fn a_plugin_that_goes_takes_the_cable_of_a_free_modulator_out_of_the_tree() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    assert!(h.tree()[0].contains("L1 Reverb: damping"));
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Char('d'));
    assert!(!h.tree()[0].contains("Reverb"), "{:?}", h.tree());
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 3);
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(h.tree()[0].contains("L1 Reverb: damping (20%)"), "{:?}", h.tree());
}

#[test]
fn m_in_the_pane_of_a_free_modulators_rate_opens_the_popup_that_lists_the_other_free_modulators() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 2);
    h.press(KeyCode::Char('m'));
    let popup = h.s.modulate.as_ref().expect("the popup is open");
    assert!(popup.items.iter().any(|item| item.cells[0].starts_with("attach \u{2192} ADSR")), "the envelope, which is a sibling and no other");
    assert!(!popup.items.iter().any(|item| item.cells[0].contains("LFO") && item.cells[0].starts_with("attach")), "not itself");
}

#[test]
fn learning_the_rate_of_a_free_modulator_arms_a_midi_modulator_in_the_free_rack() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 2);
    h.commands();
    h.press(KeyCode::Char('l'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), 3);
    assert!(matches!(h.s.ctl.model.free_modulators[2].source, ModSourceSlot::MidiLearn { source: None, learning: true }));
    assert_eq!(h.s.ctl.model.kind_of_target(ModulatorLocation::Free, 2, 0), Some(crate::plugin::chain::ModTargetKind::ModulatorRate { mod_index: 0 }));
    let commands = h.commands();
    assert!(commands[0].starts_with("InsertFreeModulator index=2 source=MidiLearn"), "{commands:?}");
}

#[test]
fn a_control_that_a_free_modulator_learns_is_bound_and_shown_in_the_tree() {
    let mut h = Harness::new("[[modulator]]\ntype = \"midi\"\n[[instrument]]\nplugin = \"builtin:osc\"\n");
    assert_eq!(h.tree()[0], "~ MIDI (learning\u{2026})  (free)");
    h.learn_tx
        .send(MidiLearnedNotification { location: ModulatorLocation::Free, mod_index: 0, source: MidiBindSource::Cc(74) })
        .unwrap();
    h.press(KeyCode::Down).press(KeyCode::Up);
    h.s.ctl.poll();
    h.s.rebuild_tree();
    assert!(matches!(h.s.ctl.model.free_modulators[0].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Cc(74)), learning: false }));
    assert!(h.tree()[0].starts_with("~ CC 74"), "{:?}", h.tree());
}

#[test]
fn a_free_modulator_has_no_signal_to_look_at_and_a_lane_that_goes_does_not_leave_the_cursor_on_it() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    assert!(h.s.selected_signal().is_none());
    assert_eq!(h.s.selected_lane_or_group(), None, "no lane and no group: M, S and w have nothing to do");
    h.press(KeyCode::Char('M')).press(KeyCode::Char('a')).press(KeyCode::Char('g')).press(KeyCode::Char('x')).press(KeyCode::Char('p'));
    assert!(h.s.selector.is_none() && h.s.group_assign.is_none() && h.s.gain_editing.is_none() && h.s.preset_selector.is_none());
    assert!(h.s.ctl.model.instruments.iter().all(|lane| !lane.muted));
}

/// `(param_name, kind, depth, min, max)` of every target of a modulator.
fn target_fields(model: &crate::app::model::SessionModel, at: ModulatorLocation, modulator: usize) -> Vec<(String, String, f32, f32, f32)> {
    let kinds = crate::app::testutil::kinds_at(model, at, modulator);
    model.modulators(at).expect("the rack")[modulator]
        .targets
        .iter()
        .zip(kinds)
        .map(|(t, kind)| (t.param_name.clone(), format!("{kind:?}"), t.depth, t.param_min, t.param_max))
        .collect()
}

/// The bass lane's filter with its `cutoff` (0.35, 0 to 1) selected in the parameter pane, in a session whose modulators are the session's own (the bass's LFO is the first of three: the pad's
/// two are after it).
fn on_the_bass_filters_cutoff() -> Harness {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter);
    h
}

#[test]
fn the_modulate_popup_makes_an_lfo_of_the_sessions_own_that_hears_the_lane_and_binds_the_parameter_to_it() {
    let mut h = on_the_bass_filters_cutoff();
    let (bass, filter) = (node_at(&h, NodePath::Instrument(2)), node_at(&h, NodePath::Effect { inst: 2, index: 0 }));
    h.press(KeyCode::Char('m')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", bass.0),
            format!("AddFreeModTarget mod=3 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.350 range=[0.000, 1.000]", filter.0),
        ]
    );
    let made = &h.s.ctl.model.free_modulators[3];
    assert!(matches!(made.source, ModSourceSlot::Lfo { rate, .. } if rate == 1.0));
    assert_eq!(made.ear, Some(bass), "it hears the lane of the parameter");
    assert_eq!(
        target_fields(&h.s.ctl.model, ModulatorLocation::Free, 3),
        [("cutoff".to_string(), format!("FreeParam {{ plugin: NodeId({}), param_index: 0 }}", filter.0), 0.5, 0.0, 1.0)]
    );
    assert!(h.s.ctl.dirty);
}

#[test]
fn the_modulate_popup_can_make_an_envelope_instead() {
    let mut h = on_the_bass_filters_cutoff();
    let (bass, filter) = (node_at(&h, NodePath::Instrument(2)), node_at(&h, NodePath::Effect { inst: 2, index: 0 }));
    h.press(KeyCode::Char('m')).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", bass.0),
            format!("AddFreeModTarget mod=3 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.350 range=[0.000, 1.000]", filter.0),
        ]
    );
    let made = &h.s.ctl.model.free_modulators[3];
    assert!(
        matches!(
            made.source,
            ModSourceSlot::Envelope { attack, decay, sustain, release }
                if (attack, decay, sustain, release) == (0.01, 0.3, 0.7, 0.5)
        ),
        "{:?}",
        made.source
    );
    assert_eq!(made.targets.len(), 1);
}

#[test]
fn the_modulate_popup_can_attach_the_parameter_to_a_modulator_that_is_there() {
    let mut h = on_the_bass_filters_cutoff();
    let filter = node_at(&h, NodePath::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Down); // the filter's resonance, which the bass's LFO does not drive
    // (The popup lists a new modulator of each kind before the modulators of the session: the bass's LFO is the first, and says that it hears the bass's lane.)
    h.press(KeyCode::Char('m'));
    let popup = h.dump();
    assert!(popup.contains("attach → LFO 0.2Hz triangle (hears L3)"), "{popup}");
    h.press_n(KeyCode::Down, NEW_MODULATORS.len()).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [format!("AddFreeModTarget mod=0 aim=Param {{ plugin: NodeId({}), index: 1 }} depth=0.500 base=0.707 range=[0.500, 10.000]", filter.0)],
        "no new modulator, a second target on the bass's LFO"
    );
    assert_eq!(h.s.ctl.model.free_modulators.len(), 3);
    assert_eq!(target_fields(&h.s.ctl.model, ModulatorLocation::Free, 0).len(), 2);
    assert_eq!(target_fields(&h.s.ctl.model, ModulatorLocation::Free, 0)[1].0, "resonance");
}

#[test]
fn the_modulate_popup_attaches_a_parameter_to_a_modulator_that_hears_another_lane_or_a_group_too_since_every_modulator_reaches_every_plugin() {
    let mut h = on_the_bass_filters_cutoff();
    let filter = node_at(&h, NodePath::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Down);
    // The third of the session's modulators is the pad's envelope: it hears the group, and drives a parameter of the bass's filter all the same.
    h.press(KeyCode::Char('m'));
    let popup = h.dump();
    assert!(popup.contains("(hears G1)"), "{popup}");
    h.press_n(KeyCode::Down, NEW_MODULATORS.len() + 2).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [format!("AddFreeModTarget mod=2 aim=Param {{ plugin: NodeId({}), index: 1 }} depth=0.500 base=0.707 range=[0.500, 10.000]", filter.0)]
    );
    assert!(matches!(h.s.ctl.model.free_modulators[2].source, ModSourceSlot::Envelope { .. }));
}

#[test]
fn the_modulate_popup_does_not_attach_a_parameter_to_a_modulator_that_drives_it_already() {
    // (The controller refuses the second cable between the two, in the browser as here.)
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m')).press_n(KeyCode::Down, NEW_MODULATORS.len()).press(KeyCode::Enter);
    assert!(h.commands().is_empty());
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 1, "the LFO's cable to the cutoff, and no more");
    assert_eq!(h.s.ctl.history_state().undo, None);
}

#[test]
fn the_modulate_popup_on_a_bus_parameter_makes_a_modulator_of_the_sessions_own_that_hears_the_group() {
    let mut h = Harness::new(scene_group());
    let (pad, reverb) = (node_at(&h, NodePath::Group(0)), node_at(&h, NodePath::GroupEffect { group: 0, index: 0 }));
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", pad.0),
            format!("AddFreeModTarget mod=3 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.880 range=[0.100, 0.990]", reverb.0),
        ]
    );
    assert_eq!(
        target_fields(&h.s.ctl.model, ModulatorLocation::Free, 3),
        [("decay".to_string(), format!("FreeParam {{ plugin: NodeId({}), param_index: 0 }}", reverb.0), 0.5, 0.1, 0.99)]
    );
    assert_eq!(h.s.ctl.model.free_modulators[3].ear, Some(pad));
}

#[test]
fn the_modulate_popup_on_the_volume_and_the_pan_of_a_group_makes_a_modulator_that_hears_the_group_and_binds_the_value() {
    let mut h = Harness::new(scene_group());
    let pad = node_at(&h, NodePath::Group(0));
    let (volume, pan) = (h.s.ctl.model.groups[0].volume, h.s.ctl.model.groups[0].pan);
    h.goto(TreeAddress::Group(0));
    // The rows of a group's pane are its volume and then its pan: the first, and the second.
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", pad.0),
            format!("AddFreeModTarget mod=3 aim=Gain {{ node: NodeId({}), gain: Volume }} depth=0.500 base={volume:.3} range=[0.000, 4.000]", pad.0),
        ]
    );
    assert_eq!(h.s.ctl.model.free_modulators[3].ear, Some(pad));
    h.press(KeyCode::Esc);
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Enter).press(KeyCode::Down).press(KeyCode::Char('m')).press(KeyCode::Enter);
    assert_eq!(
        h.commands().last().cloned(),
        Some(format!("AddFreeModTarget mod=4 aim=Gain {{ node: NodeId({}), gain: Pan }} depth=0.500 base={pan:.3} range=[-1.000, 1.000]", pad.0))
    );
    // The tree says whose value each cable drives, under the group that the modulators hear.
    let tree = h.tree().join("\n");
    assert!(tree.contains("→ G1 volume (50%)") && tree.contains("→ G1 pan (50%)"), "{tree}");
    // A value that a modulator drives already is not drawn again, and a row that has no value (there are two) is no place for one.
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 5).press(KeyCode::Char('m'));
    assert!(h.s.modulate.is_none(), "the pane of a group has two rows");
}

#[test]
fn learning_the_volume_of_a_group_arms_a_midi_modulator_bound_to_it_at_full_depth() {
    let mut h = Harness::new(scene_group());
    let pad = node_at(&h, NodePath::Group(0));
    let volume = h.s.ctl.model.groups[0].volume;
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Enter).press(KeyCode::Char('l'));
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=MidiLearn { source: None, value: 0.0, learning: true }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", pad.0),
            format!("AddFreeModTarget mod=3 aim=Gain {{ node: NodeId({}), gain: Volume }} depth=0.500 base={volume:.3} range=[0.000, 4.000]", pad.0),
            "SetFreeModTargetDepth mod=3 target=0 depth=1.000".to_string(),
        ]
    );
}

#[test]
fn the_target_selector_binds_a_volume_a_mix_or_a_pan_and_the_tree_says_whose_it_is() {
    let mut h = Harness::new(crate::app::testutil::FREE_MODULATORS);
    let (filter, osc) = (node_at(&h, NodePath::GroupEffect { group: 0, index: 0 }), node_at(&h, NodePath::Instrument(0)));
    let mix = h.s.ctl.model.groups[0].effects[0].mix;
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.commands();
    // The row of the bus filter's mix, and the one of the first lane's volume: found in the list, which has every row when nothing is typed.
    let place = |h: &Harness, owner: &str, name: &str| {
        let selector = h.s.target_selector.as_ref().expect("the selector is open");
        selector.items.iter().position(|item| item.cells == [owner, name]).unwrap_or_else(|| panic!("{owner} {name}: {:?}", selector.items.iter().map(|i| i.cells.clone()).collect::<Vec<_>>()))
    };
    h.press(KeyCode::Char('t'));
    let at = place(&h, "G1 Filter", "dry/wet mix");
    h.press_n(KeyCode::Down, at).press(KeyCode::Enter);
    assert_eq!(h.commands(), [format!("AddFreeModTarget mod=0 aim=Gain {{ node: NodeId({}), gain: Mix }} depth=0.500 base={mix:.3} range=[0.000, 1.000]", filter.0)]);
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('t'));
    let at = place(&h, "L1 Oscillator", "output volume");
    h.press_n(KeyCode::Down, at).press(KeyCode::Enter);
    assert_eq!(h.commands().len(), 1);
    let tree = h.tree().join("\n");
    assert!(tree.contains("G1 Filter (bus) mix (50%)") && tree.contains("L1 Oscillator volume (50%)") && !tree.contains("L1 Oscillator: volume"), "{tree}");
    // (The modulator had four cables: the mix is the fifth and the volume the sixth.)
    assert_eq!(h.s.ctl.model.kind_of_target(ModulatorLocation::Free, 0, 4), Some(crate::plugin::chain::ModTargetKind::Gain { node: filter, gain: crate::app::model::ModGain::Mix }));
    assert_eq!(h.s.ctl.model.kind_of_target(ModulatorLocation::Free, 0, 5), Some(crate::plugin::chain::ModTargetKind::Gain { node: osc, gain: crate::app::model::ModGain::Volume }));
    // A value that the modulator drives already is not drawn again: the controller refuses it, and nothing is sent.
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('t'));
    let at = place(&h, "L1 Oscillator", "output volume");
    let before = h.s.ctl.model.free_modulators[0].targets.len();
    h.press_n(KeyCode::Down, at).press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), before);
    assert!(h.commands().is_empty());
}

#[test]
fn the_modulate_popup_on_a_bus_parameter_attaches_a_modulator_of_the_sessions_own_that_is_there() {
    let mut h = Harness::new(scene_group());
    let reverb = node_at(&h, NodePath::GroupEffect { group: 0, index: 0 });
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m'));
    // (The new ones first, and then the session's own: the first of them is the bass's, which hears the bass's lane and has no cable to the reverb.)
    h.press_n(KeyCode::Down, NEW_MODULATORS.len()).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [format!("AddFreeModTarget mod=0 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.880 range=[0.100, 0.990]", reverb.0)],
        "no new modulator: a cable from the first of the session's own"
    );
    assert_eq!(h.s.ctl.model.free_modulators.len(), 3);
}

#[test]
fn the_modulate_popup_on_the_rate_of_a_modulator_attaches_a_sibling_that_drives_it() {
    // (The two modulators of the lane are the session's own, as every modulator is since step 96: the file's `[[instrument.modulator]]` is moved when it is loaded, and each hears the lane.)
    let mut h = Harness::new("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.modulator]]\n[[instrument.modulator]]\ntype = \"envelope\"\n");
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Enter).press_n(KeyCode::Down, 2).press(KeyCode::Char('m'));
    let popup = h.s.modulate.as_ref().expect("the popup is open");
    assert!(popup.items.iter().any(|item| item.cells[0].starts_with("attach \u{2192} ADSR")), "the other modulator, the envelope");
    h.press_n(KeyCode::Down, NEW_MODULATORS.len()).press(KeyCode::Enter);
    let commands = h.commands();
    assert!(commands.iter().all(|c| !c.starts_with("InsertFreeModulator")), "no new modulator: {commands:?}");
    assert!(commands.iter().any(|c| c.starts_with("AddFreeModTarget mod=1 aim=Setting { modulator: NodeId(1), field: Rate }")), "{commands:?}");
    assert_eq!(target_fields(&h.s.ctl.model, ModulatorLocation::Free, 1).len(), 1);
}

#[test]
fn m_on_a_modulator_adds_another_that_hears_what_it_hears_and_one_that_hears_every_note_gets_one_that_does() {
    let mut h = Harness::new(scene_group());
    let before = h.s.ctl.model.free_modulators.len();
    // The first hears the bass's lane and the second the pad's group: a new one hears the same (the tree lists it with them, as the lane's and the group's modulators were).
    for (n, index) in [0, 1].into_iter().enumerate() {
        h.goto(TreeAddress::FreeModulator { index });
        let ear = h.s.ctl.model.free_modulators[index].ear;
        assert!(ear.is_some(), "the modulator at {index} hears a lane or a group");
        h.press(KeyCode::Char('m'));
        assert_eq!(h.s.ctl.model.free_modulators.len(), before + n + 1);
        assert_eq!(h.s.ctl.model.free_modulators.last().unwrap().ear, ear, "a new one hears what the one it was added from hears");
    }
    // One that hears every note: `F` makes one, and `m` on it another that does.
    h.press(KeyCode::Char('F'));
    assert_eq!(h.s.ctl.model.free_modulators.last().unwrap().ear, None);
    h.press(KeyCode::Char('m'));
    assert_eq!(h.s.ctl.model.free_modulators.len(), before + 4);
    assert_eq!(h.s.ctl.model.free_modulators.last().unwrap().ear, None, "a new one hears every note");
}

#[test]
fn the_target_selector_binds_the_chosen_parameter_to_the_modulator() {
    // The first row of the list is the first oscillator's `waveform`, whichever modulator it is opened for: every modulator reaches every plugin of the session, the first lane's first.
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        told_all(&h.s.ctl.model, &["AddFreeModTarget mod=0 kind=LaneParam { lane: 0, slot: 0, param_index: 0 } depth=0.500 base=1.000 range=[0.000, 4.000]"])
    );
    let bound = target_fields(&h.s.ctl.model, ModulatorLocation::Free, 0);
    assert_eq!(bound.len(), 2, "the LFO had one target already");
    assert_eq!(
        bound[1],
        ("waveform".to_string(), format!("FreeParam {{ plugin: NodeId({}), param_index: 0 }}", h.s.ctl.model.instruments[0].node.0), 0.5, 0.0, 4.0)
    );

    // Further down the list are the effects of the lanes: the first oscillator's twenty parameters, the second's, and then the second lane's filter's `cutoff` and its
    // `resonance` (chain slot 1).
    h.goto(TreeAddress::FreeModulator { index: 0 });
    h.press(KeyCode::Char('t')).press_n(KeyCode::Down, 41).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        told_all(&h.s.ctl.model, &["AddFreeModTarget mod=0 kind=LaneParam { lane: 1, slot: 1, param_index: 1 } depth=0.500 base=0.707 range=[0.500, 10.000]"])
    );
    let bound = target_fields(&h.s.ctl.model, ModulatorLocation::Free, 0);
    assert_eq!(bound.len(), 3);
    assert_eq!(
        bound[2],
        ("resonance".to_string(), format!("FreeParam {{ plugin: NodeId({}), param_index: 1 }}", h.s.ctl.model.instruments[1].effects[0].node.0), 0.5, 0.5, 10.0),
        "the chain slot of a filter's parameter is 1"
    );

    // The one that hears the group, which has two targets already, is given a third by the same list.
    let group_lfo = group_modulator(&h, 0, 0);
    h.goto(group_lfo);
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        told_all(&h.s.ctl.model, &["AddFreeModTarget mod=1 kind=LaneParam { lane: 0, slot: 0, param_index: 0 } depth=0.500 base=1.000 range=[0.000, 4.000]"])
    );
    let bound = target_fields(&h.s.ctl.model, ModulatorLocation::Free, 1);
    assert_eq!(bound.len(), 3, "the group LFO had two targets already");
    assert_eq!(
        bound[2],
        ("waveform".to_string(), format!("FreeParam {{ plugin: NodeId({}), param_index: 0 }}", h.s.ctl.model.instruments[0].node.0), 0.5, 0.0, 4.0)
    );
}

#[test]
fn the_target_selector_binds_to_the_modulator_it_was_opened_for_whichever_place_it_has_in_its_rack() {
    // The envelope, the second of three in the first lane of the session.
    let mut h = Harness::new(crate::app::testutil::CROSS_MOD_RACKS);
    h.goto(TreeAddress::FreeModulator { index: 1 });
    h.press(KeyCode::Char('t')).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        told_all(&h.s.ctl.model, &["AddFreeModTarget mod=1 kind=LaneParam { lane: 0, slot: 0, param_index: 0 } depth=0.500 base=0.000 range=[0.000, 4.000]"])
    );
    assert_eq!(h.s.ctl.model.free_modulators[1].targets.len(), 3);
}

#[test]
fn the_modulate_popup_attaches_the_parameter_to_the_modulator_that_was_chosen_whichever_place_it_has_in_the_sessions_own() {
    // The envelope again: the popup lists a new modulator of each kind, and then the three that are there (the envelope is the second).
    let mut h = Harness::new(crate::app::testutil::CROSS_MOD_RACKS);
    h.goto(TreeAddress::Instrument(0));
    let osc = node_at(&h, NodePath::Instrument(0));
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press_n(KeyCode::Down, NEW_MODULATORS.len() + 1).press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        [format!("AddFreeModTarget mod=1 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.000 range=[0.000, 4.000]", osc.0)]
    );
    assert_eq!(h.s.ctl.model.free_modulators[1].targets.len(), 3);
}

#[test]
fn learning_a_parameter_arms_a_midi_modulator_bound_to_it_at_full_depth() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('l'));
    let (bass, filter) = (node_at(&h, NodePath::Instrument(2)), node_at(&h, NodePath::Effect { inst: 2, index: 0 }));
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=MidiLearn { source: None, value: 0.0, learning: true }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", bass.0),
            format!("AddFreeModTarget mod=3 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.350 range=[0.000, 1.000]", filter.0),
            "SetFreeModTargetDepth mod=3 target=0 depth=1.000".to_string(),
        ]
    );
    let armed = &h.s.ctl.model.free_modulators[3];
    assert!(matches!(armed.source, ModSourceSlot::MidiLearn { source: None, learning: true }));
    assert_eq!(armed.ear, Some(bass));
    assert_eq!(
        target_fields(&h.s.ctl.model, ModulatorLocation::Free, 3),
        [("cutoff".to_string(), format!("FreeParam {{ plugin: NodeId({}), param_index: 0 }}", filter.0), 1.0, 0.0, 1.0)]
    );

    // On a group's bus effect it hears the group.
    let mut h = Harness::new(scene_group());
    let (pad, reverb) = (node_at(&h, NodePath::Group(0)), node_at(&h, NodePath::GroupEffect { group: 0, index: 0 }));
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('l'));
    assert_eq!(
        h.commands(),
        [
            "InsertFreeModulator index=3 source=MidiLearn { source: None, value: 0.0, learning: true }".to_string(),
            format!("SetModulatorEar Free index=3 ear={}", pad.0),
            format!("AddFreeModTarget mod=3 aim=Param {{ plugin: NodeId({}), index: 0 }} depth=0.500 base=0.880 range=[0.100, 0.990]", reverb.0),
            "SetFreeModTargetDepth mod=3 target=0 depth=1.000".to_string(),
        ]
    );
    assert_eq!(h.s.ctl.model.free_modulators[3].targets.len(), 1);
}

// ---------------------------------------------------------------------------
// Between frames
// ---------------------------------------------------------------------------

#[test]
fn the_tui_reaps_plugins_the_audio_thread_hands_back() {
    // Nothing read this channel while the TUI ran: removed plugins piled up in its
    // 16 slots and, once it was full, the audio thread dropped them itself.
    let mut h = Harness::new(scene_split());
    let (probe, dropped_on) = DropProbe::recording();
    h.return_tx.send(Box::new(probe)).unwrap();
    let before = *dropped_on.lock().unwrap();
    assert!(before.is_none());

    tick(&mut h.s);
    let after = *dropped_on.lock().unwrap();
    assert_eq!(after, Some(std::thread::current().id()));
}

// ---------------------------------------------------------------------------
// Modulator rows: what Left / Right and typing a value do to a modulator
// ---------------------------------------------------------------------------
//
// An LFO has the rows Type, Waveform, Rate, a separator and then one depth a target;
// an envelope Type, Attack, Decay, Sustain, Release, a separator and the depths; a MIDI
// control a row about its source, a separator and the depths. A lane's modulator and a
// group's are the same to the user and different to the engine, which is told with the
// commands of the lane or of the group, so both are pinned.

/// The cursor on the modulator at `addr`, in the parameter pane (from the chain), on row
/// `row`. (The pane keeps the row it was on, so it is moved from there.)
fn on_modulator_row(h: &mut Harness, addr: TreeAddress, row: usize) {
    h.goto(addr);
    h.press(KeyCode::Enter);
    // A press moves the selection a row, so the presses it takes are the distance to `row`, which
    // is at most `row` or the row it starts on; a pane that has no `row` would be pressed at for ever.
    for _ in 0..row.max(h.s.param_state.selected) {
        let at = h.s.param_state.selected;
        if at == row {
            break;
        }
        h.press(if at < row { KeyCode::Down } else { KeyCode::Up });
    }
    assert_eq!(h.s.param_state.selected, row, "the pane has no row {row}");
}

/// Enter a value in the popup of the row the cursor is on.
fn enter_value(h: &mut Harness, text: &str) {
    h.press(KeyCode::Enter);
    assert!(h.s.editing.is_some(), "the value popup opens");
    h.press_n(KeyCode::Backspace, 8).type_chars(text).press(KeyCode::Enter);
    assert!(h.s.editing.is_none(), "and closes");
}

// The moves above press keys until the cursor is where it was asked to be. A key that goes to
// the other pane, or a row that is not there, used to keep them pressing for ever: a test that
// asked for one never ended.

#[test]
#[should_panic(expected = "did not reach")]
fn goto_panics_with_a_message_when_the_focus_is_in_the_parameter_pane() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Enter); // the focus is in the parameter pane, where Up and Down move
    h.goto(TreeAddress::FreeModulator { index: 1 });
}

#[test]
#[should_panic(expected = "has no row 99")]
fn on_modulator_row_panics_with_a_message_when_the_pane_has_no_such_row() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 99);
}

/// A lane with an envelope (A 0.1, D 0.2, S 0.5, R 0.4) and a MIDI control bound to CC 74,
/// each with a target.
const LANE_ENVELOPE_AND_CONTROL: &str = r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument.modulator]]
type = "envelope"
attack = 0.1
decay = 0.2
sustain = 0.5
release = 0.4
[[instrument.modulator.target]]
param = "volume"
depth = 0.5

[[instrument.modulator]]
type = "midi"
midi_cc = 74
[[instrument.modulator.target]]
param = "volume"
depth = 0.5
"#;

fn lane_source(h: &Harness, _inst: usize, index: usize) -> &ModSourceSlot {
    &h.s.ctl.model.free_modulators[index].source
}

fn group_source(h: &Harness, group: usize, index: usize) -> &ModSourceSlot {
    &hearing_group(&h.s.ctl.model, group)[index].source
}

fn lane_depth(h: &Harness, _inst: usize, index: usize, target: usize) -> f32 {
    h.s.ctl.model.free_modulators[index].targets[target].depth
}

fn group_depth(h: &Harness, group: usize, index: usize, target: usize) -> f32 {
    hearing_group(&h.s.ctl.model, group)[index].targets[target].depth
}

/// The `(attack, decay, sustain, release)` of an envelope source.
fn stages(source: &ModSourceSlot) -> (f32, f32, f32, f32) {
    match *source {
        ModSourceSlot::Envelope { attack, decay, sustain, release } => (attack, decay, sustain, release),
        ref other => panic!("not an envelope: {other:?}"),
    }
}

fn lfo_rate(source: &ModSourceSlot) -> f32 {
    match *source {
        ModSourceSlot::Lfo { rate, .. } => rate,
        ref other => panic!("not an LFO: {other:?}"),
    }
}

fn lfo_waveform(source: &ModSourceSlot) -> crate::plugin::chain::LfoWaveform {
    match *source {
        ModSourceSlot::Lfo { waveform, .. } => waveform,
        ref other => panic!("not an LFO: {other:?}"),
    }
}

#[test]
fn left_and_right_step_an_lfos_rate_by_a_twentieth_of_its_range_on_a_log_scale_with_shift_a_hundredth_with_ctrl_a_tenth() {
    // (The rate runs from 0.01 to 50 Hz, a ratio of 5000, so a twentieth of it is a ratio of 5000^0.05: 1.531, 1.089 for a hundredth, 2.344 for a tenth.
    // The values are 4 x 1.5309, x 1.0889, / 2.3437 and / 1.5309, worked out apart from the program.)
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=6.124"]);
    h.press_with(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=6.668"]);
    h.press_with(KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=2.845"]);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=1.858"]);
    assert!((lfo_rate(lane_source(&h, 0, 0)) - 1.858_458).abs() < 1e-4);
    assert!(h.s.ctl.dirty);
    assert!(h.tree().iter().any(|l| l.contains("LFO 1.9Hz")), "the tree shows the new rate: {:#?}", h.tree());
    // The other lane's LFO is as it was.
    assert_eq!(lfo_rate(lane_source(&h, 1, 1)), 7.0);
}

#[test]
fn a_step_up_and_a_step_down_of_the_same_size_give_back_the_rate_and_the_time_that_were_there() {
    // (Ratios, as distances do: what a Right made, the Left of the same size takes back, for each of the three sizes.)
    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
        let mut h = Harness::new(scene_split());
        on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
        h.press_with(KeyCode::Right, modifiers).press_with(KeyCode::Left, modifiers);
        assert!((lfo_rate(lane_source(&h, 0, 0)) - 4.0).abs() < 1e-4, "{modifiers:?}: {}", lfo_rate(lane_source(&h, 0, 0)));

        let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
        on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 4);
        h.press_with(KeyCode::Left, modifiers).press_with(KeyCode::Right, modifiers);
        assert!((stages(lane_source(&h, 0, 0)).3 - 0.4).abs() < 1e-4, "{modifiers:?}: {:?}", stages(lane_source(&h, 0, 0)));
    }
}

#[test]
fn a_rate_that_a_file_gave_below_the_lowest_is_stepped_from_the_lowest_and_the_ends_hold() {
    // (A ratio of nothing is nothing: a rate of 0 would never leave it.)
    let mut h = Harness::new(scene_split());
    if let ModSourceSlot::Lfo { rate, .. } = &mut h.s.ctl.model.free_modulators[0].source {
        *rate = 0.0;
    }
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=0.015"]);
    // At the lowest, a step down sends the value that stands, as at the highest.
    enter_value(&mut h, "0.01");
    h.commands();
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=0.010"]);
    assert_eq!(lfo_rate(lane_source(&h, 0, 0)), 0.01);
}

#[test]
fn a_value_typed_for_an_lfos_rate_is_clamped_to_its_range() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
    for (text, rate) in [("12.5", 12.5), ("999", 50.0), ("0", 0.01), ("-3", 0.01)] {
        enter_value(&mut h, text);
        assert_eq!(h.commands(), [format!("SetFreeModulatorRate mod=0 rate={rate:.3}")], "{text}");
        assert_eq!(lfo_rate(lane_source(&h, 0, 0)), rate, "{text}");
    }
    // At the end of its range a step sends the value that stands.
    enter_value(&mut h, "50");
    h.commands();
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=0 rate=50.000"]);
    assert_eq!(lfo_rate(lane_source(&h, 0, 0)), 50.0);
}

#[test]
fn left_and_right_on_an_lfos_waveform_go_round_every_waveform_whatever_the_modifiers() {
    use crate::plugin::chain::LfoWaveform::{Random, Saw, Sine, Smooth, Square, Triangle};
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 1);
    let mut seen = Vec::new();
    for (code, modifiers) in [
        (KeyCode::Right, KeyModifiers::NONE),
        (KeyCode::Right, KeyModifiers::SHIFT),
        (KeyCode::Right, KeyModifiers::CONTROL),
        (KeyCode::Right, KeyModifiers::NONE),
        (KeyCode::Right, KeyModifiers::NONE),
        (KeyCode::Right, KeyModifiers::NONE), // round to the first again
        (KeyCode::Left, KeyModifiers::NONE),  // and back round to the last
        (KeyCode::Left, KeyModifiers::CONTROL),
    ] {
        h.press_with(code, modifiers);
        seen.push((h.commands(), lfo_waveform(lane_source(&h, 0, 0))));
    }
    let said = |w: &str| vec![format!("SetFreeModulatorWaveform mod=0 waveform={w}")];
    assert_eq!(
        seen,
        [
            (said("Triangle"), Triangle),
            (said("Saw"), Saw),
            (said("Square"), Square),
            (said("Random"), Random),
            (said("Smooth"), Smooth),
            (said("Sine"), Sine),
            (said("Smooth"), Smooth),
            (said("Random"), Random),
        ]
    );
    assert!(h.tree().iter().any(|l| l.contains("LFO 4.0Hz random")), "the tree shows it: {:#?}", h.tree());
}

#[test]
fn no_popup_opens_for_a_waveform_or_a_type_or_a_separator_and_a_separator_does_not_step() {
    let mut h = Harness::new(scene_split());
    // (The type, the waveform, the note length and the retrigger, which are choices, and the separator.)
    for row in [0, 1, 3, 4, 6] {
        on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, row);
        h.press(KeyCode::Enter);
        assert!(h.s.editing.is_none(), "row {row}: there is no number to type");
        assert_eq!(h.commands(), Vec::<String>::new(), "row {row}");
        h.press(KeyCode::Esc);
    }
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 6);
    h.press(KeyCode::Right).press(KeyCode::Left);
    assert_eq!(h.commands(), Vec::<String>::new(), "the separator");
    assert_eq!(format!("{:?}", lane_source(&h, 0, 0)), "Lfo { waveform: Sine, rate: 4.0, retrigger: false, fade: 0.0, sync: 0 }");
}

#[test]
fn left_and_right_step_the_depth_of_a_target_and_a_typed_depth_goes_from_minus_one_to_one() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 7);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=0 target=0 depth=0.090"]);
    h.press_with(KeyCode::Left, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=0 target=0 depth=-0.010"]);
    for (text, depth) in [("0.5", 0.5), ("5", 1.0), ("-5", -1.0), ("-0.25", -0.25)] {
        enter_value(&mut h, text);
        assert_eq!(h.commands(), [format!("SetFreeModTargetDepth mod=0 target=0 depth={depth:.3}")], "{text}");
        assert_eq!(lane_depth(&h, 0, 0, 0), depth, "{text}");
    }
    assert!(h.s.ctl.dirty);
    // At the end of its range a step sends the value that stands.
    enter_value(&mut h, "1");
    h.commands();
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=0 target=0 depth=1.000"]);
}

#[test]
fn the_tree_shows_the_depth_of_a_target_as_soon_as_it_is_edited() {
    // The tree says how deep each target goes (`(4%)`): a lane's modulator and a group's.
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 7);
    h.press(KeyCode::Right);
    assert!(h.tree().contains(&"╰ ~ LFO 4.0Hz sine → Oscillator: detune (9%)"), "{:#?}", h.tree());
    enter_value(&mut h, "0.5");
    assert!(h.tree().contains(&"╰ ~ LFO 4.0Hz sine → Oscillator: detune (50%)"), "{:#?}", h.tree());

    let mut h = Harness::new(scene_group());
    let group_lfo = group_modulator(&h, 0, 0);
    on_modulator_row(&mut h, group_lfo, 7);
    h.press(KeyCode::Right);
    assert!(h.tree().iter().any(|l| l.contains("M0 Oscillator: detune (45%)")), "{:#?}", h.tree());
    enter_value(&mut h, "0.1");
    assert!(h.tree().iter().any(|l| l.contains("M0 Oscillator: detune (10%)")), "{:#?}", h.tree());
}

#[test]
fn a_modulator_edited_in_the_tui_is_told_to_the_subscribers_as_events_and_not_again_as_a_snapshot() {
    use crate::app::intent::{Event, ModulatorField};
    let mut h = Harness::new(scene_split());
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::<crate::app::intent::Applied>::new()));
    let sink = heard.clone();
    h.s.ctl.subscribe(Box::new(move |applied: &crate::app::intent::Applied| sink.lock().unwrap().push(applied.clone())));
    let (osc_lfo, edge) = (h.s.ctl.model.free_modulators[0].node, h.s.ctl.model.free_modulators[0].targets[0].edge);

    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
    h.press(KeyCode::Right);
    enter_value(&mut h, "99");
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 1);
    h.press(KeyCode::Right);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 7);
    enter_value(&mut h, "-0.5");

    let events: Vec<(u64, Vec<Event>)> = heard.lock().unwrap().iter().map(|a| (a.rev, a.events.clone())).collect();
    assert_eq!(
        events,
        [
            (1, vec![Event::ModulatorFieldSet { node: osc_lfo, field: ModulatorField::Rate, value: 6.123_624 }]),
            (2, vec![Event::ModulatorFieldSet { node: osc_lfo, field: ModulatorField::Rate, value: 50.0 }]),
            (3, vec![Event::ModulatorFieldSet { node: osc_lfo, field: ModulatorField::Waveform, value: 1.0 }]),
            (4, vec![Event::EdgeDepthSet { edge, depth: -0.5 }]),
        ]
    );
    assert!(!h.s.ctl.resync(), "what the subscribers heard is all there is to tell: no snapshot follows");
    assert_eq!(h.s.ctl.rev, 4);
}

#[test]
fn the_key_of_an_lfo_is_turned_in_its_pane_the_switch_over_and_the_fade_by_a_twentieth_of_its_range_and_a_typed_fade_is_held_to_it() {
    let mut h = Harness::new(scene_split());
    let addr = TreeAddress::FreeModulator { index: 0 };
    // The retrigger is a choice of two: either key goes to the other.
    on_modulator_row(&mut h, addr, 4);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorKey Free index=0 retrigger=true fade=0.000"]);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorKey Free index=0 retrigger=false fade=0.000"]);
    h.press(KeyCode::Right).press(KeyCode::Left);
    assert_eq!(h.commands().len(), 2, "each press turns it over");
    // The fade goes by a twentieth of its five seconds, a hundredth with Shift and a tenth with Ctrl, and Left takes it back.
    on_modulator_row(&mut h, addr, 5);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorKey Free index=0 retrigger=false fade=0.250"]);
    h.press_with(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetModulatorKey Free index=0 retrigger=false fade=0.300"]);
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetModulatorKey Free index=0 retrigger=false fade=0.800"]);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetModulatorKey Free index=0 retrigger=false fade=0.550"]);
    // A typed fade is held to 0 to 5.
    for (text, fade) in [("9", 5.0), ("-1", 0.0), ("1.5", 1.5)] {
        enter_value(&mut h, text);
        assert_eq!(h.commands(), [format!("SetModulatorKey Free index=0 retrigger=false fade={fade:.3}")], "{text}");
    }
    assert!(h.s.ctl.dirty);
    let (retrigger, fade) = match lane_source(&h, 0, 0) {
        ModSourceSlot::Lfo { retrigger, fade, .. } => (*retrigger, *fade),
        other => panic!("{other:?}"),
    };
    assert_eq!((retrigger, fade), (false, 1.5));
}

#[test]
fn the_type_row_turns_an_lfo_into_an_envelope_and_back_with_the_defaults_of_each() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 0);
    h.press(KeyCode::Right);
    assert_eq!(
        h.commands(),
        ["SetFreeModulatorSource mod=0 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }"]
    );
    assert_eq!(stages(lane_source(&h, 0, 0)), (0.01, 0.3, 0.7, 0.5));
    assert_eq!(h.s.param_state.selected, 0);
    assert!(h.tree().iter().any(|l| l.contains("ADSR")), "{:#?}", h.tree());
    h.press(KeyCode::Left);
    assert_eq!(
        h.commands(),
        ["SetFreeModulatorSource mod=0 source=Lfo { waveform: Sine, rate: 1.0, phase: 0.0, retrigger: false, fade: 0.0, sync: 0.0, age: inf }"]
    );
    assert_eq!(format!("{:?}", lane_source(&h, 0, 0)), "Lfo { waveform: Sine, rate: 1.0, retrigger: false, fade: 0.0, sync: 0 }");
    assert!(h.s.ctl.dirty);
}

#[test]
fn left_and_right_step_one_stage_of_an_envelope_by_the_scale_of_the_stage_and_the_engine_is_given_all_four() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    let addr = TreeAddress::FreeModulator { index: 0 };
    for (row, expected, command) in [
        // (The times run from 1 ms to 10 s, a ratio of 10000, so a twentieth of it is a ratio of 1.5849; the level is stepped by 0.05.)
        (1, (0.158_489, 0.2, 0.5, 0.4), "attack=0.158 decay=0.200 sustain=0.500 release=0.400"),
        (2, (0.158_489, 0.316_979, 0.5, 0.4), "attack=0.158 decay=0.317 sustain=0.500 release=0.400"),
        (3, (0.158_489, 0.316_979, 0.55, 0.4), "attack=0.158 decay=0.317 sustain=0.550 release=0.400"),
        (4, (0.158_489, 0.316_979, 0.55, 0.633_957), "attack=0.158 decay=0.317 sustain=0.550 release=0.634"),
    ] {
        on_modulator_row(&mut h, addr, row);
        h.press(KeyCode::Right);
        assert_eq!(h.commands(), [format!("SetFreeModulatorEnvelopeParam mod=0 {command}")], "row {row}");
        let (a, d, s, r) = stages(lane_source(&h, 0, 0));
        let close = |x: f32, y: f32| (x - y).abs() < 1e-5;
        assert!(close(a, expected.0) && close(d, expected.1) && close(s, expected.2) && close(r, expected.3), "row {row}: {:?}", (a, d, s, r));
        h.press(KeyCode::Esc);
    }
    assert!(h.s.ctl.dirty);
}

#[test]
fn a_value_typed_for_an_envelope_stage_is_clamped_to_its_range() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    let addr = TreeAddress::FreeModulator { index: 0 };
    for (row, text, expected) in [
        (1, "99", (10.0, 0.2, 0.5, 0.4)),
        (1, "0", (0.001, 0.2, 0.5, 0.4)),
        (2, "2.5", (0.001, 2.5, 0.5, 0.4)),
        (3, "5", (0.001, 2.5, 1.0, 0.4)),
        (3, "-5", (0.001, 2.5, 0.0, 0.4)),
        (4, "7", (0.001, 2.5, 0.0, 7.0)),
    ] {
        on_modulator_row(&mut h, addr, row);
        enter_value(&mut h, text);
        let (a, d, s, r) = expected;
        assert_eq!(
            h.commands(),
            [format!("SetFreeModulatorEnvelopeParam mod=0 attack={a:.3} decay={d:.3} sustain={s:.3} release={r:.3}")],
            "row {row}: {text}"
        );
        assert_eq!(stages(lane_source(&h, 0, 0)), expected, "row {row}: {text}");
        h.press(KeyCode::Esc);
    }
}

#[test]
fn the_depth_rows_of_an_envelope_and_a_midi_control_are_stepped_and_typed_like_an_lfos() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    // The envelope's rows are Type, A, D, S, R, a separator and then the depth; the control's, its source, a separator and the depth.
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 6);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=0 target=0 depth=0.550"]);
    enter_value(&mut h, "9");
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=0 target=0 depth=1.000"]);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 1 }, 2);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=1 target=0 depth=0.450"]);
    enter_value(&mut h, "-9");
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=1 target=0 depth=-1.000"]);
    assert_eq!((lane_depth(&h, 0, 0, 0), lane_depth(&h, 0, 1, 0)), (1.0, -1.0));
}

#[test]
fn the_rows_of_a_midi_control_that_are_not_depths_do_nothing() {
    let mut h = Harness::new(LANE_ENVELOPE_AND_CONTROL);
    for row in [0, 1] {
        on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 1 }, row);
        h.press(KeyCode::Right).press(KeyCode::Left).press(KeyCode::Enter);
        assert!(h.s.editing.is_none(), "row {row}: there is no number to type");
        assert_eq!(h.commands(), Vec::<String>::new(), "row {row}");
        h.press(KeyCode::Esc);
    }
    assert!(matches!(lane_source(&h, 0, 1), ModSourceSlot::MidiLearn { .. }), "still a control");
    assert_eq!(lane_depth(&h, 0, 1, 0), 0.5);
}

#[test]
fn a_group_lfo_is_stepped_and_typed_with_the_commands_of_the_group() {
    use crate::plugin::chain::LfoWaveform::Triangle;
    let mut h = Harness::new(scene_group());
    // (The bass's LFO is the first of the session's own: the one that hears the group is the second.)
    let addr = group_modulator(&h, 0, 0);
    assert_eq!(addr, TreeAddress::FreeModulator { index: 1 });
    on_modulator_row(&mut h, addr, 2);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=1 rate=0.765"]);
    enter_value(&mut h, "99");
    assert_eq!(h.commands(), ["SetFreeModulatorRate mod=1 rate=50.000"]);
    assert_eq!(lfo_rate(group_source(&h, 0, 0)), 50.0);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, addr, 1);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModulatorWaveform mod=1 waveform=Triangle"]);
    assert_eq!(lfo_waveform(group_source(&h, 0, 0)), Triangle);
    h.press(KeyCode::Esc);
    // Two targets, a member's detune and the bus's damping: a depth row each.
    on_modulator_row(&mut h, addr, 7);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=1 target=0 depth=0.450"]);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, addr, 8);
    enter_value(&mut h, "-4");
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=1 target=1 depth=-1.000"]);
    assert!((group_depth(&h, 0, 0, 0) - 0.45).abs() < 1e-5);
    assert_eq!(group_depth(&h, 0, 0, 1), -1.0);
    assert!(h.s.ctl.dirty);
    // (None of it went to a lane.)
    assert_eq!(lfo_rate(lane_source(&h, 2, 0)), 0.2);
}

#[test]
fn a_group_envelope_is_stepped_and_typed_with_the_commands_of_the_group() {
    let mut h = Harness::new(scene_group());
    let addr = group_modulator(&h, 0, 1);
    assert_eq!(addr, TreeAddress::FreeModulator { index: 2 });
    on_modulator_row(&mut h, addr, 3);
    h.press(KeyCode::Left);
    assert_eq!(h.commands(), ["SetFreeModulatorEnvelopeParam mod=2 attack=0.010 decay=0.300 sustain=0.650 release=0.500"]);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, addr, 1);
    enter_value(&mut h, "99");
    assert_eq!(h.commands(), ["SetFreeModulatorEnvelopeParam mod=2 attack=10.000 decay=0.300 sustain=0.650 release=0.500"]);
    h.press(KeyCode::Esc);
    // Its only target is the LFO's rate, a modulator's field: a depth row like the others.
    on_modulator_row(&mut h, addr, 6);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetFreeModTargetDepth mod=2 target=0 depth=0.350"]);
    let (a, d, s, r) = stages(group_source(&h, 0, 1));
    assert_eq!((a, d, r), (10.0, 0.3, 0.5));
    assert!((s - 0.65).abs() < 1e-5);
}

#[test]
fn the_type_row_of_a_group_modulator_switches_it_with_the_commands_of_the_group() {
    let mut h = Harness::new(scene_group());
    let group_lfo = group_modulator(&h, 0, 0);
    on_modulator_row(&mut h, group_lfo, 0);
    h.press(KeyCode::Right);
    assert_eq!(
        h.commands(),
        ["SetFreeModulatorSource mod=1 source=Envelope { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5, state: Idle, level: 0.0, notes_held: 0 }"]
    );
    assert_eq!(stages(group_source(&h, 0, 0)), (0.01, 0.3, 0.7, 0.5));
}

// ---------------------------------------------------------------------------
// What the audio thread and the plugin scan report back
// ---------------------------------------------------------------------------
//
// `Controller::poll` folds the reports into the model and the catalog (its tests
// cover that); `tick` makes the view catch up, which is what these check.

#[test]
fn a_finished_recording_shows_in_the_tree_at_the_tempo_set_with_b() {
    let mut h = Harness::new(scene_split());
    assert!(!h.tree().iter().any(|l| l.contains("Pattern")));
    h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars("95").press(KeyCode::Enter);
    h.pattern_tx.send(recording(1)).unwrap();
    tick(&mut h.s);

    assert!(h.tree().contains(&"├ ▶ Pattern  2 beats, 1 notes"), "{:#?}", h.tree());
    assert_eq!(h.s.ctl.model.instruments[1].pattern.as_ref().unwrap().bpm, 95.0);
}

#[test]
fn a_tempo_set_with_b_is_an_intent_that_marks_the_session_changed_and_that_ctrl_z_undoes() {
    let mut h = Harness::new(scene_split());
    h.commands();
    h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars("90").press(KeyCode::Enter);
    assert_eq!((h.s.ctl.model.bpm, h.s.ctl.rev, h.s.ctl.dirty), (90.0, 1, true));
    assert_eq!(h.commands(), ["SetGlobalBpm bpm=90.0"]);
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.s.ctl.model.bpm, 120.0);
    assert_eq!(h.commands(), ["SetGlobalBpm bpm=120.0"]);
}

#[test]
fn the_popup_of_b_says_the_tempos_there_can_be() {
    let mut h = Harness::new(scene_split());
    h.type_chars("b");
    let frame = h.dump();
    assert!(frame.contains("Range: 20.00 — 300.00"), "{frame}");
}

#[test]
fn a_tempo_that_there_cannot_be_is_held_to_the_nearest_when_it_is_set_with_b() {
    for (text, tempo) in [("1000", 300.0), ("5", 20.0), ("0", 20.0), ("-40", 20.0)] {
        let mut h = Harness::new(scene_split());
        h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars(text).press(KeyCode::Enter);
        assert_eq!(h.s.ctl.model.bpm, tempo, "{text}");
    }
}

#[test]
fn a_tempo_set_with_b_leaves_the_tempo_that_a_recorded_pattern_was_recorded_at() {
    // A pattern's length in frames is its beats counted in the tempo it was recorded at, and a load counts them in the pattern's `bpm`:
    // a pattern that was saved at another tempo than that would be another length when it was loaded.
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars("60").press(KeyCode::Enter);
    assert_eq!(h.s.ctl.model.bpm, 60.0);
    assert_eq!(h.s.ctl.model.instruments[1].pattern.as_ref().unwrap().bpm, 120.0, "it was recorded at 120");
    let (lanes, _, _, _) = crate::app::save::to_save(&h.s.ctl.model);
    assert_eq!(lanes[1].pattern.as_ref().unwrap().bpm, 120.0, "and it is saved at it");
}

/// A lane with no pattern, and a lane with one.
const LANES_WITH_AND_WITHOUT_A_PATTERN: &str = r#"
[[instrument]]
plugin = "builtin:osc"

[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = true
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0
"#;

#[test]
fn ctrl_r_clears_the_pattern_of_a_lane_from_its_pattern_or_from_its_instrument_and_does_not_record() {
    // (The help says Ctrl+R clears; the key was `r`, which records, whatever the modifiers.)
    for at in [TreeAddress::Pattern(1), TreeAddress::Instrument(1)] {
        let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
        assert!(h.tree().iter().any(|l| l.contains("Pattern")), "{:#?}", h.tree());
        h.goto(at);
        h.commands();
        h.s.ctl.dirty = false;
        h.press_with(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(h.commands(), ["ClearPattern inst=1"], "{at:?}");
        assert!(h.s.ctl.model.instruments[1].pattern.is_none(), "{at:?}");
        assert!(h.s.ctl.dirty, "{at:?}");
        assert!(!h.tree().iter().any(|l| l.contains("Pattern")), "{at:?}: {:#?}", h.tree());
    }
}

#[test]
fn ctrl_r_on_a_lane_that_has_no_pattern_sends_nothing_and_does_not_record() {
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.goto(TreeAddress::Instrument(0));
    h.commands();
    h.s.ctl.dirty = false;
    h.press_with(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(h.s.ctl.model.instruments[0].pattern.is_none());
    assert!(!h.s.ctl.dirty);
}

#[test]
fn ctrl_r_is_for_the_chain_and_d_on_a_pattern_clears_it_the_same_way() {
    // With the parameter pane focused, Ctrl+R is nothing; `d` on the pattern is what it was (the same command, the same model).
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Enter);
    h.commands();
    h.press_with(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(h.s.ctl.model.instruments[1].pattern.is_some());
    h.press(KeyCode::Esc);
    h.goto(TreeAddress::Pattern(1));
    h.s.ctl.dirty = false;
    h.press(KeyCode::Char('d'));
    assert_eq!(h.commands(), ["ClearPattern inst=1"]);
    assert!(h.s.ctl.model.instruments[1].pattern.is_none());
    assert!(h.s.ctl.dirty);
    assert!(!h.tree().iter().any(|l| l.contains("Pattern")), "{:#?}", h.tree());
}

#[test]
fn r_without_control_records_the_pattern_of_the_lane_it_is_on_as_it_did() {
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.goto(TreeAddress::Instrument(0));
    h.commands();
    h.press(KeyCode::Char('r'));
    assert_eq!(
        h.commands(),
        ["SetGlobalBpm bpm=120.0", "SetPatternLength inst=0 beats=4.0", "SetPatternRecording inst=0 recording=true"]
    );
    assert!(h.s.ctl.model.instruments[0].pattern.as_ref().unwrap().recording);
}

#[test]
fn r_is_an_intent_that_starts_and_stops_a_recording_and_marks_the_session_changed() {
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.goto(TreeAddress::Instrument(0));
    h.commands();
    h.s.ctl.dirty = false;
    h.press(KeyCode::Char('r'));
    assert_eq!((h.s.ctl.rev, h.s.ctl.dirty), (1, true));
    assert!(h.tree().iter().any(|line| line.contains("Pattern  recording...")), "{:#?}", h.tree());
    h.commands();
    h.press(KeyCode::Char('r'));
    assert_eq!(h.commands(), ["SetPatternRecording inst=0 recording=false"]);
    assert_eq!(h.s.ctl.rev, 2);
    assert!(!h.s.ctl.model.instruments[0].pattern.as_ref().unwrap().recording);
}

#[test]
fn a_pattern_that_is_cleared_is_no_step_of_the_history_and_ctrl_z_undoes_the_step_before_it() {
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.type_chars("b").press_n(KeyCode::Backspace, 3).type_chars("90").press(KeyCode::Enter);
    h.goto(TreeAddress::Pattern(1));
    h.press_with(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(h.s.ctl.model.instruments[1].pattern.is_none());
    assert_eq!(h.s.ctl.rev, 2, "the tempo and the clearing are intents");
    h.commands();
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.s.ctl.model.bpm, 120.0, "the step that is undone is the tempo's");
    assert!(h.s.ctl.model.instruments[1].pattern.is_none(), "the pattern is not put back");
}

#[test]
fn the_rows_of_a_patterns_pane_turn_its_settings_by_intents_and_the_tree_shows_them_at_once() {
    let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
    h.goto(TreeAddress::Pattern(1));
    h.press(KeyCode::Enter);
    h.commands();
    // Row 0 is the length in beats: Right turns it up a step (a tenth of a beat), and ten of them make a beat, which the tree says.
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetPatternLength inst=1 beats=4.1"]);
    assert!(h.tree().iter().any(|line| line.contains("4 beats, 1 notes")), "{:#?}", h.tree());
    h.press_n(KeyCode::Right, 9);
    h.commands();
    assert!(h.tree().iter().any(|line| line.contains("5 beats, 1 notes")), "{:#?}", h.tree());
    // Row 3 is the transpose: in key is in the tree too.
    h.press_n(KeyCode::Down, 3).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetPatternInKey inst=1 in_key=true"]);
    assert!(h.tree().iter().any(|line| line.contains("5 beats, 1 notes, in-key")), "{:#?}", h.tree());
    // Row 1 is whether a held key plays it, row 2 whether it loops: each turns its own, which the one before it has already turned the other way (both were on).
    h.press_n(KeyCode::Up, 2).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetPatternEnabled inst=1 enabled=false"]);
    h.press(KeyCode::Down).press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetPatternLooping inst=1 looping=false"]);
    assert_eq!(h.s.ctl.rev, 13, "each turn is an intent: ten of the length (a tenth of a beat each), and the three switches");
    assert!(h.s.ctl.dirty);
}

#[test]
fn a_length_that_is_typed_into_a_patterns_pane_is_held_to_the_lengths_there_can_be() {
    for (text, beats) in [("8", "8.0"), ("100", "32.0"), ("0", "1.0")] {
        let mut h = Harness::new(LANES_WITH_AND_WITHOUT_A_PATTERN);
        h.goto(TreeAddress::Pattern(1));
        h.press(KeyCode::Enter);
        h.commands();
        h.press(KeyCode::Enter).press_n(KeyCode::Backspace, 8).type_chars(text).press(KeyCode::Enter);
        assert_eq!(h.commands(), [format!("SetPatternLength inst=1 beats={beats}")], "{text}");
    }
}

#[test]
fn a_finished_recording_marks_the_session_changed() {
    // `r` marks the session when the recording starts, but the events only
    // arrive when it ends, and a save in between (during the count-in, say)
    // clears the mark.
    let mut h = Harness::new(scene_split());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('r'));
    assert!(h.s.ctl.dirty, "`r` marks the session changed");
    h.s.ctl.dirty = false; // what a save does
    h.pattern_tx.send(recording(0)).unwrap();
    tick(&mut h.s);
    assert!(h.s.ctl.dirty, "the recording is part of the session");
}

#[test]
fn a_captured_control_shows_in_the_tree() {
    for (source, label) in [
        (MidiBindSource::Cc(74), "CC 74"),
        (MidiBindSource::PitchBend, "Pitch Bend"),
        (MidiBindSource::Aftertouch, "Aftertouch"),
    ] {
        let mut h = Harness::new(scene_split());
        // Arm a MIDI-learn modulator on lane 0's first parameter the way a user does.
        h.goto(TreeAddress::Instrument(0));
        h.press(KeyCode::Enter).press(KeyCode::Char('l'));
        let index = h.s.ctl.model.free_modulators.len() - 1;
        assert!(h.tree().iter().any(|l| l.contains("MIDI (learning…)")), "`l` arms a modulator");

        h.learn_tx
            .send(MidiLearnedNotification { location: ModulatorLocation::Free, mod_index: index, source })
            .unwrap();
        tick(&mut h.s);
        assert!(h.tree().iter().any(|l| l.contains(label)), "{label}: {:#?}", h.tree());
        assert!(!h.tree().iter().any(|l| l.contains("MIDI (learning…)")), "{label}: {:#?}", h.tree());
    }
}

#[test]
fn a_captured_control_shows_in_the_tree_for_a_group_modulator() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('l'));
    let index = h.s.ctl.model.free_modulators.len() - 1;

    h.learn_tx
        .send(MidiLearnedNotification {
            location: ModulatorLocation::Free,
            mod_index: index,
            source: MidiBindSource::Cc(1),
        })
        .unwrap();
    tick(&mut h.s);
    assert!(h.tree().iter().any(|l| l.contains("CC 1 →")), "{:#?}", h.tree());
}

fn plugin_info(name: &str, is_instrument: bool) -> PluginInfo {
    PluginInfo {
        name: name.to_string(),
        id: format!("builtin:{}", name.to_lowercase()),
        is_instrument,
        param_count: 3,
        preset_count: 0,
        path: String::new(),
        scan_ms: 0,
    }
}

#[test]
fn an_open_selector_gains_rows_as_the_scan_goes_on_and_keeps_its_filter() {
    let mut h = Harness::new(scene_split());
    h.start_scan();
    h.catalog_tx.as_ref().unwrap().send(vec![plugin_info("Reverb", false)]).unwrap();
    tick(&mut h.s);
    h.press(KeyCode::Char('a')).type_chars("rev");
    let sel = h.s.selector.as_ref().expect("`a` opens the effect selector");
    assert_eq!((sel.items.len(), sel.filter.filtered.len()), (1, 1));

    h.catalog_tx
        .as_ref()
        .unwrap()
        .send(vec![plugin_info("Revolver", false), plugin_info("Delay", false), plugin_info("Synth", true)])
        .unwrap();
    tick(&mut h.s);
    let sel = h.s.selector.as_ref().unwrap();
    assert_eq!(sel.items.len(), 3, "the instrument is not an effect");
    assert_eq!(sel.filter.input.value, "rev");
    assert_eq!(sel.filter.filtered.len(), 2, "Reverb and Revolver match, Delay does not");
}

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

/// A harness whose session came from `song.toml` in a fresh directory.
fn harness_with_a_file() -> (tempfile::TempDir, std::path::PathBuf, Harness) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.toml");
    let mut h = Harness::new(scene_split());
    h.s.ctl.session_path = Some(path.clone());
    (dir, path, h)
}

fn ctrl_shift() -> KeyModifiers {
    KeyModifiers::CONTROL | KeyModifiers::SHIFT
}

#[test]
fn ctrl_s_writes_the_session_to_its_file_and_clears_the_mark() {
    let (_dir, path, mut h) = harness_with_a_file();
    h.s.ctl.piano_filter.set_scale(ScaleSetting::parse("D dorian").unwrap());
    h.s.ctl.piano_filter.set_mode(PianoMode::Locked);
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL);

    assert!(!h.s.ctl.dirty);
    let config = session::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(config.instruments.len(), 2, "both lanes are in the file");
    let piano = config.piano.expect("the Piano tab's scale and mode are saved with the session");
    assert_eq!((piano.scale.as_deref(), piano.locked), (Some("D dorian"), true));
}

#[test]
fn the_tui_does_not_show_where_a_session_draws_its_nodes_but_keeps_it_through_an_edit_and_a_save() {
    use crate::app::testutil::PLACED;
    let unplaced: String = PLACED
        .lines()
        .filter(|line| !line.starts_with("ui = ") && !line.starts_with("master = ") && *line != "[ui]")
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(Harness::new(PLACED).tree(), Harness::new(&unplaced).tree(), "the tree is the same with the places and without");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("song.toml");
    let mut h = Harness::new(PLACED);
    h.s.ctl.session_path = Some(path.clone());
    // The filter goes, and its place with it; every other node is where the file put it.
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Char('d'));
    h.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL);

    let text = std::fs::read_to_string(&path).unwrap();
    let config = session::parse(&text).unwrap();
    let at = |x: f32, y: f32| Some(session::NodeUi { x, y });
    assert_eq!(config.master_ui, at(1200.0, 300.5), "{text}");
    assert_eq!(config.groups[0].ui, at(800.0, 40.0), "{text}");
    assert_eq!(config.instruments[0].ui, at(0.0, 0.0), "{text}");
    assert_eq!(config.free_modulators[0].ui, at(376.0, -200.0), "{text}");
    assert!(config.instruments[0].effects.is_empty(), "{text}");
    assert!(!text.contains("y = 0.25"), "{text}");
}

#[test]
fn ctrl_s_without_a_session_file_leaves_the_mark() {
    let mut h = Harness::new(scene_split());
    assert_eq!(h.s.ctl.session_path, None);
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert!(h.s.ctl.dirty, "nothing was saved");
}

#[test]
fn ctrl_shift_s_asks_for_a_name_starting_from_the_current_one() {
    let (_dir, _path, mut h) = harness_with_a_file();
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    let popup = h.s.save_as.as_ref().expect("the popup opens");
    assert_eq!(popup.input.value, "song.toml");
}

#[test]
fn save_as_writes_a_new_file_next_to_the_old_one_and_switches_to_it() {
    let (dir, old, mut h) = harness_with_a_file();
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).type_chars("copy").press(KeyCode::Enter);

    let new = dir.path().join("copy.toml");
    assert!(new.exists(), "`.toml` is appended");
    assert!(!old.exists(), "the old file is not written");
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(new.as_path()), "Ctrl+S now targets the new file");
    assert!(h.s.save_as.is_none(), "the popup closes");
    assert!(!h.s.ctl.dirty);
}

#[test]
fn save_as_keeps_a_toml_extension_and_trims_the_name() {
    let (dir, _old, mut h) = harness_with_a_file();
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).type_chars("  mix.toml  ").press(KeyCode::Enter);
    assert!(dir.path().join("mix.toml").exists());
    assert!(h.s.save_as.is_none());
}

#[test]
fn save_as_waits_for_a_name() {
    let (dir, old, mut h) = harness_with_a_file();
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).press(KeyCode::Enter);
    let popup = h.s.save_as.as_ref().expect("the popup stays open");
    assert_eq!(popup.error, None, "no complaint, it just waits");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing was written");
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(old.as_path()));
    assert!(h.s.ctl.dirty);
}

#[test]
fn a_save_as_that_is_refused_shows_why_in_the_popup_and_changes_nothing() {
    let (dir, old, mut h) = harness_with_a_file();
    std::fs::write(dir.path().join("taken.toml"), "another session").unwrap();
    h.s.ctl.dirty = true;
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    // A name that goes up a folder is not a file name, which the popup says in the words of the rule (the page's are the same).
    h.press_n(KeyCode::Backspace, 20).type_chars("../taken").press(KeyCode::Enter);
    let popup = h.s.save_as.as_ref().expect("the popup stays open");
    assert_eq!(popup.error.as_deref(), Some("a file name has no folder in it: `/` and `\\` are not allowed"));
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(old.as_path()), "the old file stays the session's");
    assert!(h.s.ctl.dirty);

    // Editing the name clears the error, so that it can be fixed; a name that is another file's is refused in its own words, and that file is as it was.
    h.press(KeyCode::Backspace);
    assert_eq!(h.s.save_as.as_ref().unwrap().error, None, "Backspace");
    h.press_n(KeyCode::Backspace, 20).type_chars("taken").press(KeyCode::Enter);
    assert_eq!(h.s.save_as.as_ref().unwrap().error.as_deref(), Some("a file called taken.toml is there already"));
    assert_eq!(std::fs::read_to_string(dir.path().join("taken.toml")).unwrap(), "another session");
    assert_eq!(h.s.ctl.session_path.as_deref(), Some(old.as_path()));
    h.press(KeyCode::Char('y'));
    assert_eq!(h.s.save_as.as_ref().unwrap().error, None, "typing");

    // A name that is free is written, and the popup is gone.
    h.press(KeyCode::Enter);
    assert!(h.s.save_as.is_none());
    assert!(dir.path().join("takeny.toml").exists());
    assert!(!h.s.ctl.dirty);
}

#[test]
fn a_save_as_in_the_terminal_is_told_to_the_browsers_as_a_save_is() {
    let (dir, _old, mut h) = harness_with_a_file();
    let told = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = told.clone();
    h.s.ctl.subscribe(Box::new(move |applied: &crate::app::intent::Applied| sink.lock().unwrap().push(applied.clone())));
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press_n(KeyCode::Backspace, 20).type_chars("copy").press(KeyCode::Enter);
    assert!(dir.path().join("copy.toml").exists());
    let events: Vec<_> = told.lock().unwrap().iter().flat_map(|applied| applied.events.clone()).collect();
    assert_eq!(events, [crate::app::intent::Event::Saved { file: Some("copy.toml".to_string()) }]);
}

#[test]
fn escape_closes_save_as_without_saving() {
    let (dir, _old, mut h) = harness_with_a_file();
    h.press_with(KeyCode::Char('S'), ctrl_shift());
    h.press(KeyCode::Esc);
    assert!(h.s.save_as.is_none());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------
// Node ids
// ---------------------------------------------------------------------------
//
// The harness checks after every key that each node has an id of its own; these
// check that an id stays with its node while others come, go and move.

use crate::app::model::{NodeId, NodePath};

fn node_at(h: &Harness, path: NodePath) -> NodeId {
    h.s.ctl
        .model
        .nodes()
        .find(|(_, p)| *p == path)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no node at {path:?}"))
}

fn all_nodes(h: &Harness) -> Vec<NodeId> {
    h.s.ctl.model.nodes().map(|(id, _)| id).collect()
}

#[test]
fn an_added_effect_gets_a_new_id_and_the_others_keep_theirs() {
    let mut h = Harness::new(scene_two_effects());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let before = all_nodes(&h);
    h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);

    let added = node_at(&h, NodePath::Effect { inst: 0, index: 2 });
    assert!(!before.contains(&added), "the new effect has an id of its own");
    assert!(before.iter().all(|id| all_nodes(&h).contains(id)), "the others keep theirs");
}

#[test]
fn a_new_lane_shares_one_id_between_itself_and_its_oscillator() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let before = all_nodes(&h);
    h.press(KeyCode::Char('n'));
    let lane = &h.s.ctl.model.instruments[2];
    assert!(!before.contains(&lane.node));
    assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);

    // Choosing another instrument for it keeps the lane's node.
    h.s.selector = None;
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('i')).type_chars("oscillator").press(KeyCode::Enter);
    let lane = &h.s.ctl.model.instruments[2];
    assert_eq!(lane.instrument.as_ref().unwrap().node, lane.node);
}

#[test]
fn replacing_the_instrument_keeps_the_lanes_id() {
    let mut h = Harness::new(scene_split());
    h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
    let lane = node_at(&h, NodePath::Instrument(0));
    h.goto(TreeAddress::Instrument(0));
    // (The scene's lane has a stand-in for an instrument, which the list does not have: it starts at its first row, the audio input, so the oscillator is asked for by name.)
    h.press(KeyCode::Char('i')).type_chars("oscillator").press(KeyCode::Enter);
    assert_eq!(
        h.commands(),
        ["RemoveFreeModTarget mod=0 target=0", "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false keep"]
    );
    assert_eq!(node_at(&h, NodePath::Instrument(0)), lane);
    assert_eq!(h.s.ctl.model.instruments[0].instrument.as_ref().unwrap().node, lane);
}

#[test]
fn new_modulators_and_groups_get_ids_of_their_own() {
    let mut h = Harness::new(scene_group());
    let before = all_nodes(&h);

    // A lane modulator and a group modulator, with `m` on the chain.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('m'));
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('m'));
    // A MIDI-learn modulator in each rack, with `l` on a parameter.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Enter).press(KeyCode::Char('l')).press(KeyCode::Esc);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('l')).press(KeyCode::Esc);
    // A new group, for the bass.
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('g')).press(KeyCode::Enter);

    let after = all_nodes(&h);
    let added: Vec<_> = after.iter().filter(|id| !before.contains(id)).collect();
    assert_eq!(added.len(), 5, "two `m`, two `l` and a group: {added:?}");
    assert!(before.iter().all(|id| after.contains(id)));
}

// ---------------------------------------------------------------------------
// A macro: a knob that drives what has a cable from it
// ---------------------------------------------------------------------------

/// One oscillator with a filter, and a macro at 0.25 with a cable to the filter's cutoff.
fn scene_macro() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "macro"
value = 0.25
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
"#
}

#[test]
fn a_macro_is_in_the_tree_with_its_value_and_what_it_drives() {
    let h = Harness::new(scene_macro());
    assert!(h.tree().iter().any(|line| line.contains("~ Macro 0.25 → Filter: cutoff (40%)")), "{:#?}", h.tree());
}

// ---------------------------------------------------------------------------
// A step sequencer: its settings, its sixteen steps and what it drives
// ---------------------------------------------------------------------------

/// One oscillator with a filter, and a step sequencer of four steps (at 6 steps a second, a glide of a quarter, not started again by a note) with a cable to the filter's cutoff.
fn scene_steps() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "steps"
steps = [0.5, -0.25, 1.0, -1.0]
rate = 6.0
glide = 0.25
retrigger = false
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
"#
}

fn steps_in_the_lane(h: &Harness) -> ([f32; 16], usize, f32, f32, bool) {
    match h.s.ctl.model.free_modulators[0].source {
        ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } => (values, length, rate, glide, retrigger),
        ref other => panic!("{other:?}"),
    }
}

/// The rows of a step sequencer's pane that are not its steps: the rate, the note length to follow the tempo by, the glide, the length and whether a note starts it again.
const RATE: usize = 0;
const SYNC: usize = 1;
const GLIDE: usize = 2;
const LENGTH: usize = 3;
const RETRIGGER: usize = 4;
/// The row of step `i` (counting from 0).
const fn step_row(i: usize) -> usize {
    5 + i
}

#[test]
fn a_step_sequencer_is_in_the_tree_with_how_long_it_is_and_how_fast_and_what_it_drives() {
    let h = Harness::new(scene_steps());
    assert!(h.tree().iter().any(|line| line.contains("~ Steps 4 6.0Hz → Filter: cutoff (40%)")), "{:#?}", h.tree());
}

#[test]
fn the_pane_of_a_step_sequencer_has_its_settings_its_sixteen_steps_and_its_targets() {
    let mut h = Harness::new(scene_steps());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    assert_golden("steps_pane", &mut h);
}

/// What the audio thread says of where the step sequencer of `scene_steps` is: the whole part of the number is the step it is at (`ModulatorSlot::position`).
fn publish_position(h: &Harness, position: f32) {
    let slot = h.s.ctl.model.free_modulators[0].position.expect("a step sequencer has a slot for where it is");
    h.s.ctl.telemetry.store(slot, position);
}

/// The style of row `step` (from 1) of the parameter pane, which all of the row from its name to the pane's border has (the cursor's cells before the name are not the row's own);
/// the row is found by its name.
fn step_row_style(h: &mut Harness, step: usize) -> (Color, Modifier) {
    row_style(h, &format!("Step {step} "))
}

/// The style of the row of the parameter pane whose name begins with `name` (see `step_row_style`).
fn row_style(h: &mut Harness, name: &str) -> (Color, Modifier) {
    h.dump();
    let buf = h.terminal.backend().buffer();
    // (The chain is the left 42% of the screen, and the parameter pane has a border on each side.)
    let (from, to) = (buf.area.width * 42 / 100 + 1, buf.area.width - 1);
    let text = |y: u16| (from..to).map(|x| buf[(x, y)].symbol()).collect::<String>();
    let (y, start) = (0..buf.area.height)
        .find_map(|y| text(y).find(name).map(|at| (y, from + text(y)[..at].chars().count() as u16)))
        .unwrap_or_else(|| panic!("the pane has no row {name:?}: {}", dump_buffer(buf)));
    let styles: Vec<(Color, Modifier)> = (start..to).map(|x| (buf[(x, y)].fg, buf[(x, y)].modifier)).collect();
    assert!(styles.iter().all(|style| *style == styles[0]), "the row {name:?} has one look: {styles:?}");
    styles[0]
}

/// How the pane shows each of the sixteen steps, a character each: `>` for the step the sequencer is at, `.` for one it goes through and `_` for one past its length
/// (the rows that the cursor is not on: it is drawn as the selected row is).
fn step_looks(h: &mut Harness) -> String {
    (1..=16)
        .map(|step| match step_row_style(h, step) {
            (Color::Magenta, modifier) if modifier.is_empty() => '>',
            (fg, modifier) if fg == DIM && modifier.is_empty() => '.',
            (fg, modifier) if fg == DIM && modifier == Modifier::DIM => '_',
            other => panic!("step {step} has a look that is none of the three: {other:?}"),
        })
        .collect()
}

#[test]
fn the_pane_of_a_step_sequencer_colours_the_step_it_is_at_and_dims_the_steps_past_its_length() {
    let mut h = Harness::new(scene_steps());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    // (Nothing has been published yet: it is at the beginning.)
    assert_eq!(step_looks(&mut h), ">...____________");
    for (position, looks) in [
        (0.99, ">...____________"),
        (1.0, ".>..____________"),
        (2.5, "..>.____________"),
        (3.99, "...>____________"),
        // The length brings the position round, as the engine does (`step_at`): four steps, so 4 is the first again, 5.5 the second, -0.5 the last.
        (4.0, ">...____________"),
        (5.5, ".>..____________"),
        (-0.5, "...>____________"),
        (f32::NAN, ">...____________"),
    ] {
        publish_position(&h, position);
        assert_eq!(step_looks(&mut h), looks, "at {position}");
    }
}

#[test]
fn the_step_a_step_sequencer_is_at_has_a_marker_in_its_name() {
    let mut h = Harness::new(scene_steps());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    publish_position(&h, 2.5);
    let dump = h.dump();
    assert!(dump.contains("Step 3 ▶ "), "{dump}");
    assert_eq!(dump.matches('▶').count(), 1, "one step has it: {dump}");
    publish_position(&h, 1.5);
    let dump = h.dump();
    assert!(dump.contains("Step 2 ▶ ") && !dump.contains("Step 3 ▶"), "the marker moves with it: {dump}");
}

#[test]
fn the_steps_that_are_dimmed_are_the_ones_past_the_length_the_pane_has_now() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, LENGTH);
    assert_eq!(step_looks(&mut h), ">...____________");
    h.press(KeyCode::Right);
    assert_eq!(step_looks(&mut h), ">....___________", "a step that the length brings in is not dimmed");
    h.press_n(KeyCode::Left, 2);
    assert_eq!(step_looks(&mut h), ">.._____________", "one that it takes out is");
    // The playhead is where the position is at the length that there is.
    publish_position(&h, 3.5);
    assert_eq!(step_looks(&mut h), ">.._____________", "three steps bring 3.5 round to the first");
    h.press_n(KeyCode::Right, 13);
    assert_eq!(step_looks(&mut h), "...>............", "and sixteen leave it at the fourth");
}

#[test]
fn the_row_under_the_cursor_is_bold_and_keeps_its_colour_or_its_dimming() {
    let mut h = Harness::new(scene_steps());
    publish_position(&h, 2.5);
    let lane = TreeAddress::FreeModulator { index: 0 };
    on_modulator_row(&mut h, lane, step_row(0));
    assert_eq!(step_row_style(&mut h, 1), (Color::White, Modifier::BOLD), "a step that it goes through");
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, lane, step_row(2));
    assert_eq!(step_row_style(&mut h, 3), (Color::Magenta, Modifier::BOLD), "the step that it is at");
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, lane, step_row(8));
    assert_eq!(step_row_style(&mut h, 9), (Color::White, Modifier::BOLD | Modifier::DIM), "a step past its length");
}

#[test]
fn a_step_sequencer_that_has_no_slot_for_where_it_is_has_no_marker() {
    let mut h = Harness::new(scene_steps());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    // (The harness checks the ids after every key, and a step sequencer that has no slot is one it would refuse: the slots run out only in a session that has used all there are.)
    h.s.ctl.model.free_modulators[0].position = None;
    assert_eq!(step_looks(&mut h), "....____________");
    assert!(!h.dump().contains('▶'));
}

#[test]
fn the_pane_of_a_groups_step_sequencer_has_its_playhead_too() {
    let mut h = Harness::new(
        r#"
[[group]]
[[group.modulator]]
type = "steps"
steps = [0.5, -0.25, 1.0, -1.0]
[[instrument]]
plugin = "builtin:osc"
group = 0
"#,
    );
    h.goto(TreeAddress::FreeModulator { index: 0 });
    let slot = hearing_group(&h.s.ctl.model, 0)[0].position.expect("a group's step sequencer has a slot for where it is");
    h.s.ctl.telemetry.store(slot, 1.25);
    assert_eq!(step_looks(&mut h), ".>..____________");
}

#[test]
fn the_playhead_of_another_modulator_or_node_is_not_drawn() {
    // An LFO and an oscillator, with a step sequencer whose position is published all the same: the marker is for the pane of the step sequencer that is shown.
    let mut h = Harness::new(format!("{}\n[[instrument.modulator]]\ntype = \"lfo\"\n", scene_steps()));
    publish_position(&h, 2.5);
    h.goto(TreeAddress::FreeModulator { index: 1 });
    assert!(!h.dump().contains('▶'), "{}", h.dump());
    h.goto(TreeAddress::Instrument(0));
    assert!(!h.dump().contains('▶'), "{}", h.dump());
}

#[test]
fn left_and_right_turn_the_rate_and_the_glide_of_a_step_sequencer_and_the_engine_is_given_all_it_has() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, GLIDE);
    h.press(KeyCode::Right);
    let (values, length, rate, glide, retrigger) = steps_in_the_lane(&h);
    assert!((glide - 0.30).abs() < 1e-6, "a twentieth of the glide's range: {glide}");
    assert_eq!((length, rate, retrigger), (4, 6.0, false));
    let commands = h.commands();
    assert!(commands.len() == 1 && commands[0].starts_with("SetModulatorSteps Free index=0 values=[0.5, -0.25, 1.0, -1.0, 0.0,") && commands[0].ends_with("length=4 glide=0.300 retrigger=false"), "{commands:?}");
    assert_eq!(values[..5], [0.5, -0.25, 1.0, -1.0, 0.0]);
    // The rate is turned by ratios, as an LFO's is, and is an LFO's command.
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, RATE);
    h.press(KeyCode::Right);
    assert!((steps_in_the_lane(&h).2 - 6.0 * 1.53).abs() < 0.1, "{}", steps_in_the_lane(&h).2);
    assert!(h.commands().iter().any(|c| c.starts_with("SetFreeModulatorRate mod=0 rate=9.")), "{:?}", h.commands());
    assert!(h.tree().iter().any(|line| line.contains("~ Steps 4 9.")), "the tree has the rate: {:#?}", h.tree());
}

#[test]
fn left_and_right_turn_the_length_of_a_step_sequencer_by_one_step_and_stop_at_its_ends() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, LENGTH);
    h.press(KeyCode::Right);
    assert_eq!(steps_in_the_lane(&h).1, 5, "one step, and not a twentieth of the range");
    h.press_n(KeyCode::Left, 2);
    assert_eq!(steps_in_the_lane(&h).1, 3);
    // A step with Shift is as long, since a length is a whole number of steps.
    h.press_with(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(steps_in_the_lane(&h).1, 4);
    h.press_n(KeyCode::Left, 10);
    assert_eq!(steps_in_the_lane(&h).1, 1);
    h.press_n(KeyCode::Right, 30);
    assert_eq!(steps_in_the_lane(&h).1, 16);
    assert!(h.tree().iter().any(|line| line.contains("~ Steps 16 6.0Hz")), "{:#?}", h.tree());
}

#[test]
fn left_and_right_on_the_retrigger_of_a_step_sequencer_turn_it_over_either_way() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, RETRIGGER);
    assert!(h.dump().contains("◂ off ▸"), "{}", h.dump());
    h.press(KeyCode::Right);
    assert!(steps_in_the_lane(&h).4);
    assert!(h.dump().contains("◂ on ▸"), "{}", h.dump());
    h.press(KeyCode::Left);
    assert!(!steps_in_the_lane(&h).4);
    h.press(KeyCode::Left);
    assert!(steps_in_the_lane(&h).4, "a switch has two places: either key goes to the other");
    assert!(h.commands().last().unwrap().ends_with("retrigger=true"), "{:?}", h.commands());
}

/// An oscillator with a filter, and an LFO at 3 Hz that follows the tempo by an eighth-note triplet, with a cable to the filter's cutoff.
fn scene_synced() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
rate = 3.0
sync = "1/8 triplet"
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
"#
}

/// How many places along the list of note lengths the LFO of the lane is.
fn lfo_sync(h: &Harness) -> usize {
    match lane_source(h, 0, 0) {
        ModSourceSlot::Lfo { sync, .. } => *sync,
        other => panic!("{other:?}"),
    }
}

#[test]
fn left_and_right_on_the_sync_of_an_lfo_go_one_note_length_along_the_list_and_stop_at_its_ends() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 3);
    assert!(h.dump().contains("◂ free ▸"), "{}", h.dump());
    // (Left of the first does not go anywhere, and sends nothing.)
    h.press(KeyCode::Left);
    assert_eq!((h.commands(), lfo_sync(&h)), (Vec::<String>::new(), 0));
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorSync Free index=0 beats=16.000"]);
    assert!(h.dump().contains("◂ 4 bars ▸"), "{}", h.dump());
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorSync Free index=0 beats=8.000"]);
    // A list has places and no distances: Ctrl and Shift take it one place along too.
    h.press_with(KeyCode::Right, KeyModifiers::CONTROL).press_with(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(h.commands(), ["SetModulatorSync Free index=0 beats=4.000", "SetModulatorSync Free index=0 beats=3.000"]);
    assert_eq!(lfo_sync(&h), 4);
    // To the end of it, which is the last place, and no further.
    h.press_n(KeyCode::Right, 30);
    assert_eq!(lfo_sync(&h), 16);
    assert_eq!(h.commands().last().map(String::as_str), Some("SetModulatorSync Free index=0 beats=0.125"));
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), Vec::<String>::new(), "nothing is sent for a step that goes nowhere");
    assert!(h.dump().contains("◂ 1/32 ▸"), "{}", h.dump());
    // And back to none, which is the LFO's own rate again, the one it had all the time.
    h.press_n(KeyCode::Left, 30);
    assert_eq!(lfo_sync(&h), 0);
    assert_eq!(h.commands().last().map(String::as_str), Some("SetModulatorSync Free index=0 beats=0.000"));
    assert_eq!(lfo_rate(lane_source(&h, 0, 0)), 4.0);
    assert!(h.s.ctl.dirty);
}

#[test]
fn the_sync_of_a_step_sequencer_and_of_a_group_lfo_is_stepped_with_the_commands_of_each() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, SYNC);
    h.press_n(KeyCode::Right, 14);
    assert_eq!(h.commands().last().map(String::as_str), Some("SetModulatorSync Free index=0 beats=0.250"));
    match lane_source(&h, 0, 0) {
        ModSourceSlot::Steps { sync, rate, .. } => assert_eq!((*sync, *rate), (14, 6.0), "a sixteenth note, and the rate it keeps"),
        other => panic!("{other:?}"),
    }
    assert!(h.tree().iter().any(|line| line.contains("~ Steps 4 1/16 → Filter: cutoff (40%)")), "the tree says the note length and not a rate: {:#?}", h.tree());
    let mut h = Harness::new(scene_group());
    let group_lfo = group_modulator(&h, 0, 0);
    on_modulator_row(&mut h, group_lfo, 3);
    h.press(KeyCode::Right);
    assert_eq!(h.commands(), ["SetModulatorSync Free index=1 beats=16.000"]);
    assert!(h.tree().iter().any(|line| line.contains("LFO 4 bars")), "{:#?}", h.tree());
    assert!(matches!(lane_source(&h, 2, 0), ModSourceSlot::Lfo { sync: 0, .. }), "(a lane's LFO is another modulator)");
}

#[test]
fn a_sync_that_is_stepped_is_undone_by_ctrl_z_and_the_rate_the_lfo_keeps_is_what_it_was() {
    let mut h = Harness::new(scene_split());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 3);
    h.press_n(KeyCode::Right, 3);
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("set sync"));
    h.commands();
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(lfo_sync(&h), 0, "the three steps of one thing are one step of the history");
    assert_eq!(h.commands(), ["SetModulatorSync Free index=0 beats=0.000"]);
    assert_eq!(lfo_rate(lane_source(&h, 0, 0)), 4.0);
    h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert_eq!(lfo_sync(&h), 3);
    assert_eq!(h.commands(), ["SetModulatorSync Free index=0 beats=4.000"]);
}

#[test]
fn the_rate_of_a_modulator_that_follows_the_tempo_is_dimmed_in_its_pane_and_is_as_the_others_when_it_is_free() {
    let mut h = Harness::new(scene_synced());
    let addr = TreeAddress::FreeModulator { index: 0 };
    assert!(h.tree().iter().any(|line| line.contains("~ LFO 1/8 triplet sine → Filter: cutoff (40%)")), "{:#?}", h.tree());
    on_modulator_row(&mut h, addr, 1);
    assert_eq!(row_style(&mut h, "Rate (Hz)"), (DIM, Modifier::DIM), "it goes by the tempo: the rate is not used");
    assert_eq!(row_style(&mut h, "Fade (s)"), (DIM, Modifier::empty()), "(the rows that it does use are not dimmed)");
    assert_golden("lfo_pane_synced", &mut h);
    // Free again, the rate is a row like the others, and it is what it was.
    on_modulator_row(&mut h, addr, 3);
    h.press_n(KeyCode::Left, 12);
    assert_eq!(lfo_sync(&h), 0);
    on_modulator_row(&mut h, addr, 1);
    assert_eq!(row_style(&mut h, "Rate (Hz)"), (DIM, Modifier::empty()));
    assert!(h.dump().contains("3.00"), "the rate that it kept: {}", h.dump());
}

#[test]
fn left_and_right_turn_a_step_by_a_twentieth_of_minus_one_to_one_and_it_stops_at_the_ends() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, step_row(1));
    h.press(KeyCode::Right);
    assert!((steps_in_the_lane(&h).0[1] - (-0.15)).abs() < 1e-6, "{}", steps_in_the_lane(&h).0[1]);
    assert_eq!(steps_in_the_lane(&h).0[0], 0.5, "the others stay");
    let commands = h.commands();
    assert!(commands.len() == 1 && commands[0].starts_with("SetModulatorSteps Free index=0 values=[0.5, -0.15"), "{commands:?}");
    h.press_with(KeyCode::Left, KeyModifiers::SHIFT);
    assert!((steps_in_the_lane(&h).0[1] - (-0.17)).abs() < 1e-6, "a hundredth of the range with Shift: {}", steps_in_the_lane(&h).0[1]);
    h.press_n(KeyCode::Left, 30);
    assert_eq!(steps_in_the_lane(&h).0[1], -1.0);
    h.press_n(KeyCode::Right, 30);
    assert_eq!(steps_in_the_lane(&h).0[1], 1.0);
    assert!(h.s.ctl.dirty);
    // And each is a step of the history, of one thing.
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("set steps"));
}

#[test]
fn a_value_typed_into_a_step_is_set_and_kept_between_minus_one_and_one() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, step_row(2));
    enter_value(&mut h, "-0.75");
    assert_eq!(steps_in_the_lane(&h).0[2], -0.75);
    enter_value(&mut h, "7");
    assert_eq!(steps_in_the_lane(&h).0[2], 1.0);
    // The settings are typed in as numbers too, and a length that has a fraction is rounded.
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, LENGTH);
    enter_value(&mut h, "9.4");
    assert_eq!(steps_in_the_lane(&h).1, 9);
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, RETRIGGER);
    h.press(KeyCode::Enter);
    assert!(h.s.editing.is_none(), "a switch is turned over with the arrows and not typed");
}

#[test]
fn m_on_the_rate_of_a_step_sequencer_opens_the_popup_and_on_the_steps_the_glide_and_the_length_it_does_not() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, RATE);
    h.press(KeyCode::Char('m'));
    assert!(h.s.modulate.is_some(), "the rate can be driven");
    h.press(KeyCode::Esc).press(KeyCode::Esc);
    for row in [SYNC, GLIDE, LENGTH, RETRIGGER, step_row(0), step_row(15)] {
        on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, row);
        h.press(KeyCode::Char('m'));
        assert!(h.s.modulate.is_none(), "row {row} is set by a person");
        h.press(KeyCode::Esc);
    }
}

#[test]
fn a_step_sequencer_is_taken_out_with_d_and_put_back_by_ctrl_z_with_its_steps() {
    let mut h = Harness::new(scene_steps());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, step_row(0));
    h.press(KeyCode::Right).press(KeyCode::Esc);
    h.press(KeyCode::Char('d'));
    assert!(h.s.ctl.model.free_modulators.is_empty());
    assert!(!h.tree().iter().any(|line| line.contains("Steps")), "{:#?}", h.tree());
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove a step sequencer"));
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!((steps_in_the_lane(&h).0[0] - 0.6).abs() < 1e-6, "{}", steps_in_the_lane(&h).0[0]);
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 1, "with its cable");
}

#[test]
fn the_modulate_popup_has_a_new_step_sequencer_that_is_made_as_a_staircase_with_a_cable_to_the_parameter_as_one_step() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m')).press_n(KeyCode::Down, NEW_MODULATORS.len() - 1);
    let popup = h.dump();
    assert!(popup.contains("New step sequencer → cutoff"), "{popup}");
    h.press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert_eq!(rack.len(), 4, "the three of the session, and the step sequencer");
    let ModSourceSlot::Steps { values, length, rate, glide, retrigger, .. } = rack[3].source else { panic!("{:?}", rack[3].source) };
    assert_eq!((values, length, rate, glide, retrigger), (ModSourceSlot::staircase(), 16, 8.0, 0.0, true));
    assert_eq!(rack[3].targets.len(), 1);
    let commands = h.commands();
    assert!(commands[0].starts_with("InsertFreeModulator index=3 source=Steps { values: [-1.0, -0.875,"), "{commands:?}");
    assert!(commands[1].starts_with("SetModulatorEar Free index=3 ear="), "{commands:?}");
    assert!(commands[2].starts_with("AddFreeModTarget mod=3 aim=Param"), "{commands:?}");
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("modulate cutoff"), "the step sequencer and the cable to it are one thing");
}

// ---------------------------------------------------------------------------
// Velocity and key: controls that follow the notes a lane plays
// ---------------------------------------------------------------------------

/// One oscillator with a filter, and a velocity and a key, each with a cable to the filter's cutoff.
fn scene_notes() -> &'static str {
    r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.modulator]]
type = "midi"
midi_velocity = true
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.4
[[instrument.modulator]]
type = "midi"
midi_key = true
[[instrument.modulator.target]]
slot = 1
param = "cutoff"
depth = 0.3
"#
}

#[test]
fn a_velocity_and_a_key_are_in_the_tree_with_what_they_drive() {
    let h = Harness::new(scene_notes());
    let tree = h.tree();
    assert!(tree.iter().any(|line| line.contains("~ Velocity → Filter: cutoff (40%)")), "{tree:#?}");
    assert!(tree.iter().any(|line| line.contains("~ Key → Filter: cutoff (30%)")), "{tree:#?}");
}

#[test]
fn the_modulate_popup_has_a_new_velocity_and_a_new_key_that_are_bound_at_once_with_a_cable_to_the_parameter_as_one_step() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m'));
    let popup = h.dump();
    let at = |what: &str| popup.find(what).unwrap_or_else(|| panic!("no `{what}` in:\n{popup}"));
    assert!(at("New LFO → cutoff") < at("New envelope → cutoff") && at("New envelope → cutoff") < at("New macro → cutoff"), "{popup}");
    assert!(at("New macro → cutoff") < at("New velocity → cutoff") && at("New velocity → cutoff") < at("New key → cutoff"), "in this order: {popup}");
    h.press_n(KeyCode::Down, 3).press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert_eq!(rack.len(), 4);
    assert!(matches!(rack[3].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Velocity), learning: false }), "{:?}", rack[3].source);
    assert_eq!(rack[3].targets.len(), 1);
    let commands = h.commands();
    assert_eq!(commands[0], "InsertFreeModulator index=3 source=MidiLearn { source: Some(Velocity), value: 0.0, learning: false }", "{commands:?}");
    assert!(commands[2].starts_with("AddFreeModTarget mod=3 aim=Param") && commands[2].contains("depth=0.500"), "{commands:?}");
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("modulate cutoff"), "the control and the cable to it are one thing");
    assert!(h.tree().iter().any(|line| line.contains("~ Velocity → Filter: cutoff (50%)")), "{:#?}", h.tree());

    // A key, from the popup of the same parameter again.
    h.press(KeyCode::Char('m')).press_n(KeyCode::Down, 4).press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert!(matches!(rack[4].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }), "{:?}", rack[4].source);
    assert!(h.tree().iter().any(|line| line.contains("~ Key → Filter: cutoff (50%)")), "{:#?}", h.tree());
}

#[test]
fn the_modulate_popup_on_a_bus_parameter_can_make_a_key_that_hears_the_group() {
    let mut h = Harness::new(scene_group());
    let pad = node_at(&h, NodePath::Group(0));
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press_n(KeyCode::Down, 4).press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert_eq!(rack.len(), 4, "the three of the session, and the key");
    assert!(matches!(rack[3].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }), "{:?}", rack[3].source);
    assert_eq!((rack[3].targets.len(), rack[3].ear), (1, Some(pad)));
    assert!(h.commands()[0].starts_with("InsertFreeModulator index=3 source=MidiLearn { source: Some(Key)"), "{:?}", h.commands());
}

#[test]
fn the_popup_of_a_modulators_own_setting_can_make_a_key_to_drive_it() {
    // The rate of the LFO of the first scene with three kinds of modulator, which is the third row of its pane (the type, the waveform, the rate).
    let mut h = Harness::new(scene_three_kinds_of_modulator());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
    h.press(KeyCode::Char('m'));
    let popup = h.dump();
    assert!(popup.contains("New velocity →") && popup.contains("New key →"), "{popup}");
    h.press_n(KeyCode::Down, 4).press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert_eq!(rack.len(), 4, "the three it has, and the key");
    assert!(matches!(rack[3].source, ModSourceSlot::MidiLearn { source: Some(MidiBindSource::Key), learning: false }), "{:?}", rack[3].source);
    assert_eq!(rack[3].targets.len(), 1, "it drives the rate of the LFO");
    let rate = crate::app::model::Driven::Setting { modulator: rack[0].node, field: crate::app::model::ModSetting::Rate };
    let commands = h.commands();
    // (The new one hears what the LFO hears, which is told after it is put in.)
    let cable = commands.iter().find(|c| c.starts_with("AddFreeModTarget")).expect("its cable");
    assert!(cable.contains(&format!("aim={rate:?} ")), "{commands:?}");
}

#[test]
fn a_velocity_and_a_key_are_saved_as_the_controls_that_they_are() {
    let h = Harness::new(scene_notes());
    let (instruments, groups, cables, sources) = crate::app::save::to_save(&h.s.ctl.model);
    let file = crate::session::to_toml(&instruments, &groups, &cables, &sources, None, h.s.ctl.model.master_ui(), h.s.ctl.model.bpm_to_save()).unwrap();
    assert!(file.contains("midi_velocity = true") && file.contains("midi_key = true"), "{file}");
}

#[test]
fn the_modulate_popup_has_a_new_macro_that_is_made_at_rest_with_a_cable_to_the_parameter_as_one_step() {
    let mut h = on_the_bass_filters_cutoff();
    h.press(KeyCode::Char('m')).press_n(KeyCode::Down, 2);
    let popup = h.dump();
    assert!(popup.contains("New LFO → cutoff") && popup.contains("New envelope → cutoff") && popup.contains("New macro → cutoff"), "{popup}");
    h.press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert_eq!(rack.len(), 4);
    assert!(matches!(rack[3].source, ModSourceSlot::Macro { value, .. } if value == 0.0), "{:?}", rack[3].source);
    assert_eq!(rack[3].targets.len(), 1);
    let commands = h.commands();
    assert_eq!(commands[0], "InsertFreeModulator index=3 source=Macro { value: 0.0 }", "{commands:?}");
    assert!(commands[2].starts_with("AddFreeModTarget mod=3 aim=Param"), "{commands:?}");
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("modulate cutoff"), "the macro and the cable to it are one thing");
}

#[test]
fn the_modulate_popup_on_a_bus_parameter_can_make_a_macro_that_hears_the_group() {
    let mut h = Harness::new(scene_group());
    let pad = node_at(&h, NodePath::Group(0));
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press_n(KeyCode::Down, 2).press(KeyCode::Enter);
    let rack = &h.s.ctl.model.free_modulators;
    assert_eq!(rack.len(), 4, "the three of the session, and the macro");
    assert!(matches!(rack[3].source, ModSourceSlot::Macro { value, .. } if value == 0.0), "{:?}", rack[3].source);
    assert_eq!((rack[3].targets.len(), rack[3].ear), (1, Some(pad)));
    assert_eq!(h.commands()[0], "InsertFreeModulator index=3 source=Macro { value: 0.0 }");
}

#[test]
fn the_pane_of_a_macro_has_its_value_and_its_targets() {
    let mut h = Harness::new(scene_macro());
    h.goto(TreeAddress::FreeModulator { index: 0 });
    assert_golden("macro_pane", &mut h);
}

#[test]
fn left_and_right_turn_a_macro_by_a_twentieth_and_the_engine_is_given_a_macro_of_the_value() {
    let mut h = Harness::new(scene_macro());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 0);
    h.press(KeyCode::Right);
    let commands = h.commands();
    assert!(commands.len() == 1 && commands[0].starts_with("SetFreeModulatorSource mod=0 source=Macro { value: 0.3"), "{commands:?}");
    assert!((macro_value(&h) - 0.3).abs() < 1e-6, "{}", macro_value(&h));
    assert!(h.tree().iter().any(|line| line.contains("Macro 0.30")), "{:#?}", h.tree());
    h.press(KeyCode::Left).press(KeyCode::Left);
    assert!((macro_value(&h) - 0.2).abs() < 1e-6, "{}", macro_value(&h));
    // The value stops at the ends of its range.
    h.press_n(KeyCode::Left, 10);
    assert_eq!(macro_value(&h), 0.0);
    h.press_n(KeyCode::Right, 30);
    assert_eq!(macro_value(&h), 1.0);
    assert!(h.s.ctl.dirty);
}

#[test]
fn a_value_typed_into_a_macro_is_set_and_kept_between_nothing_and_one() {
    let mut h = Harness::new(scene_macro());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 0);
    enter_value(&mut h, "0.5");
    assert_eq!(macro_value(&h), 0.5);
    assert_eq!(h.commands(), ["SetFreeModulatorSource mod=0 source=Macro { value: 0.5 }"]);
    enter_value(&mut h, "7");
    assert_eq!(macro_value(&h), 1.0);
}

fn macro_value(h: &Harness) -> f32 {
    match h.s.ctl.model.free_modulators[0].source {
        ModSourceSlot::Macro { value, .. } => value,
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn m_on_the_value_of_a_macro_opens_nothing_and_m_on_the_depth_of_its_cable_does() {
    let mut h = Harness::new(scene_macro());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 0);
    h.press(KeyCode::Char('m'));
    assert!(h.s.modulate.is_none(), "the value is turned by a person: no modulator drives it");
    h.press(KeyCode::Esc);
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 2);
    h.press(KeyCode::Char('m'));
    assert!(h.s.modulate.is_some(), "the depth of a cable can be driven");
    assert!(h.dump().contains("New macro → "), "a macro can be what drives it");
}

#[test]
fn a_macro_is_taken_out_with_d_and_put_back_by_ctrl_z_at_the_value_it_had() {
    let mut h = Harness::new(scene_macro());
    on_modulator_row(&mut h, TreeAddress::FreeModulator { index: 0 }, 0);
    h.press(KeyCode::Right).press(KeyCode::Esc);
    h.press(KeyCode::Char('d'));
    assert!(h.s.ctl.model.free_modulators.is_empty());
    assert!(!h.tree().iter().any(|line| line.contains("Macro")), "{:#?}", h.tree());
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove a macro"));
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!((macro_value(&h) - 0.3).abs() < 1e-6, "{}", macro_value(&h));
    assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 1, "with its cable");
}

#[test]
fn m_on_a_parameter_makes_a_modulator_of_the_sessions_own_that_hears_the_lane_or_the_group_and_is_bound_to_it() {
    // "New LFO" is the first row of the popup. The bass has one modulator already (the session's own since the load: it hears the bass's lane), and the pad's bus has two, that hear the group.
    let mut h = Harness::new(scene_group());
    let before = all_nodes(&h);
    let (bass, pad) = (node_at(&h, NodePath::Instrument(2)), node_at(&h, NodePath::Group(0)));
    let had = h.s.ctl.model.free_modulators.len();
    assert_eq!(had, 3);

    h.goto(TreeAddress::Effect { inst: 2, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    let mods = &h.s.ctl.model.free_modulators;
    assert_eq!(mods.len(), had + 1);
    let new = mods.last().unwrap();
    assert!(!before.contains(&new.node), "the modulator has an id of its own");
    assert_eq!((new.ear, new.targets.len()), (Some(bass), 1), "it hears the lane of the parameter, and is bound to it");
    assert!(h.commands()[0].starts_with(&format!("InsertFreeModulator index={had}")));

    h.press(KeyCode::Esc);
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Enter).press(KeyCode::Char('m')).press(KeyCode::Enter);
    let mods = &h.s.ctl.model.free_modulators;
    assert_eq!(mods.len(), had + 2);
    let new = mods.last().unwrap();
    assert!(!before.contains(&new.node), "the modulator has an id of its own");
    assert_eq!((new.ear, new.targets.len()), (Some(pad), 1), "a bus effect's modulator hears the group");
}

#[test]
fn ids_follow_their_nodes_when_effects_move_or_go() {
    let mut h = Harness::new(scene_two_effects());
    let filter = node_at(&h, NodePath::Effect { inst: 0, index: 0 });
    let reverb = node_at(&h, NodePath::Effect { inst: 0, index: 1 });
    let lfo = node_at(&h, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 });

    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
    assert_eq!(node_at(&h, NodePath::Effect { inst: 0, index: 0 }), reverb, "the reverb moved up");
    assert_eq!(node_at(&h, NodePath::Effect { inst: 0, index: 1 }), filter, "the filter moved down");

    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('d'));
    assert_eq!(node_at(&h, NodePath::Effect { inst: 0, index: 0 }), filter, "the filter is first now");
    assert!(h.s.ctl.model.locate(reverb).is_none(), "the deleted effect's id is gone, and not handed out again");
    assert_eq!(node_at(&h, NodePath::Modulator { at: ModulatorLocation::Free, index: 0 }), lfo);
}

#[test]
fn deleting_a_lane_leaves_the_others_their_ids() {
    let mut h = Harness::new(scene_split());
    let first = node_at(&h, NodePath::Instrument(0));
    let second = node_at(&h, NodePath::Instrument(1));
    h.goto(TreeAddress::Instrument(0));
    // The first `d` empties the lane, the second removes it.
    h.press(KeyCode::Char('d'));
    assert_eq!(node_at(&h, NodePath::Instrument(0)), first, "an emptied lane is still the same node");
    h.press(KeyCode::Char('d'));
    assert_eq!(h.s.ctl.model.instruments.len(), 1);
    assert_eq!(node_at(&h, NodePath::Instrument(0)), second, "the survivor is lane 0 now, with its old id");
    assert!(h.s.ctl.model.locate(first).is_none());
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

/// An oscillator (which has no presets) and a reverb on "Vintage Plate".
const PRESET_SCENE: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
preset = "Vintage Plate"
"#;

const REVERB_PRESETS: [(&str, &str); 5] = [
    ("arcadia", "Arcadia Dream Hall"),
    ("cathedral", "Cathedral"),
    ("plate", "Vintage Plate"),
    ("room", "Small Room"),
    ("ambient", "Ambient Wash"),
];

/// The value of each of the reverb's parameters, by its own index, once `preset`
/// is loaded: what the model has to mirror.
fn reverb_values_with(preset: &str) -> Vec<(u32, f32)> {
    let mut reverb = plugin::load("builtin:reverb", SAMPLE_RATE, BLOCK, &plugin::Runtime::default()).unwrap();
    session::apply_preset(&mut reverb, preset);
    let indexes: Vec<u32> = reverb.parameters().iter().map(|p| p.index).collect();
    indexes.into_iter().map(|i| (i, reverb.get_parameter(i).unwrap())).collect()
}

#[test]
fn p_lists_the_plugins_presets_and_starts_on_the_current_one() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p'));
    let ps = h.s.preset_selector.as_ref().expect("the reverb has presets");
    let listed: Vec<(&str, &str)> = ps.presets.iter().map(|p| (p.id.as_str(), p.name.as_str())).collect();
    assert_eq!(listed, REVERB_PRESETS);
    assert_eq!((ps.inst, ps.slot), (0, 1), "effect 0 is slot 1 for the audio thread");
    assert_eq!(ps.filter.selected_item(&ps.items).map(|item| item.index), Some(2), "Vintage Plate, the current one");
}

#[test]
fn p_opens_only_on_a_lane_plugin_that_has_presets() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Instrument(0)); // the oscillator has none
    h.press(KeyCode::Char('p'));
    assert!(h.s.preset_selector.is_none());

    let mut h = Harness::new(scene_bus_pair());
    for addr in [TreeAddress::Group(0), TreeAddress::GroupEffect { group: 0, index: 1 }, TreeAddress::FreeModulator { index: 0 }] {
        h.goto(addr);
        h.press(KeyCode::Char('p'));
        assert!(h.s.preset_selector.is_none(), "{addr:?}");
    }
}

#[test]
fn choosing_a_preset_loads_it_and_mirrors_its_values_into_the_model() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).type_chars("cathed").press(KeyCode::Enter);

    assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=cathedral"]);
    assert!(h.s.preset_selector.is_none(), "the popup closes");
    assert!(h.s.ctl.dirty);
    let reverb = &h.s.ctl.model.instruments[0].effects[0];
    assert_eq!(reverb.current_preset.as_deref(), Some("Cathedral"));
    let expected = reverb_values_with("Cathedral");
    assert_eq!(reverb.params.len(), expected.len());
    for row in &reverb.params {
        let want = expected.iter().find(|(i, _)| *i == row.index).unwrap().1;
        assert_eq!((row.value, row.default), (want, want), "{}: the preset's value, and the baseline", row.name);
    }
}

#[test]
fn the_arrows_choose_among_the_presets() {
    let mut h = Harness::new(PRESET_SCENE); // starts on Vintage Plate, the third
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).press(KeyCode::Down).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=room"]);
    h.press(KeyCode::Char('p')).press(KeyCode::Up).press(KeyCode::Up).press(KeyCode::Enter);
    assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=cathedral"]);
}

#[test]
fn a_loaded_preset_is_saved_by_name_with_no_parameter_overrides() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).type_chars("ambient").press(KeyCode::Enter);
    let (lanes, _, _, _) = crate::app::save::to_save(&h.s.ctl.model);
    let reverb = &lanes[0].effects[0];
    assert_eq!(reverb.preset.as_deref(), Some("Ambient Wash"));
    assert!(reverb.params.is_empty(), "the preset's values are the baseline, not edits: {:?}", reverb.params);

    // A tweak afterwards is an edit on top of the preset, and the only one.
    h.press(KeyCode::Enter).press(KeyCode::Right);
    h.commands();
    let (lanes, _, _, _) = crate::app::save::to_save(&h.s.ctl.model);
    assert_eq!(lanes[0].effects[0].params.len(), 1);
}

#[test]
fn escape_and_a_filter_with_no_match_load_nothing() {
    let mut h = Harness::new(PRESET_SCENE);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('p')).press(KeyCode::Down).press(KeyCode::Esc);
    assert!(h.s.preset_selector.is_none());
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(!h.s.ctl.dirty);

    h.press(KeyCode::Char('p')).type_chars("zzz").press(KeyCode::Enter);
    assert!(h.s.preset_selector.is_none(), "the popup closes");
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(!h.s.ctl.dirty);
    assert_eq!(h.s.ctl.model.instruments[0].effects[0].current_preset.as_deref(), Some("Vintage Plate"));
}

#[test]
fn groups_without_a_name_are_called_group_and_their_number() {
    // Three groups, none named: each place that names a group counts from one by
    // the group's own position.
    let mut h = Harness::new(
        "[[group]]\n[[group]]\n[[group]]\n\
         [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n\
         [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 1\n\
         [[instrument]]\nplugin = \"builtin:osc\"\ngroup = 2\n",
    );
    let headers: Vec<&str> = h.tree().into_iter().filter(|l| l.starts_with('▦')).collect();
    assert_eq!(headers, ["▦ Group 1", "▦ Group 2", "▦ Group 3"], "the tree");

    // The parameter pane is titled by the group under the cursor.
    h.goto(TreeAddress::Group(2));
    let screen = h.dump();
    assert!(screen.lines().any(|l| l.contains("┐┌ Group 3 ")), "the parameter pane's title: {screen}");

    // The group popup lists them all, and marks the one the lane is in.
    h.goto(TreeAddress::Instrument(1));
    h.press(KeyCode::Char('g'));
    let screen = h.dump();
    for name in ["Group 1", "Group 2 (current)", "Group 3"] {
        assert!(screen.contains(name), "the group popup lacks {name}: {screen}");
    }
}

// ---------------------------------------------------------------------------
// The loop: what wakes it, and what it does
// ---------------------------------------------------------------------------

fn terminal_key(code: KeyCode, modifiers: KeyModifiers) -> io::Result<Event> {
    Ok(Event::Key(KeyEvent::new(code, modifiers)))
}

fn terminal_quit() -> io::Result<Event> {
    terminal_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
}

#[test]
fn the_loop_handles_what_the_terminal_sends_in_order_draws_it_and_stops_on_quit() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    tx.send(terminal_quit()).unwrap();

    event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap();

    assert!(h.s.quit);
    assert_eq!(h.s.chain_state.selected, 2, "both keys were handled");
    let screen = dump_buffer(h.terminal.backend().buffer());
    assert!(screen.contains("Oscillator"), "the last frame drawn shows the session: {screen}");
}

#[test]
fn the_loop_wakes_by_itself_to_fold_in_what_the_audio_thread_reports() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    assert!(h.s.ctl.model.instruments[0].pattern.is_none());
    // The audio thread reports a recording while the loop waits for a key that does not
    // come. The loop is told to quit once it has taken the report, or if it has not within
    // three seconds: the last pass of a loop that is quitting would take it too, so only
    // that it was taken *before* the quit shows that the loop wakes by itself.
    let reported = h.pattern_tx.clone();
    let reporter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        reported.send(recording(0)).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !reported.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let taken = reported.is_empty();
        tx.send(terminal_quit()).unwrap();
        taken
    });

    event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap();
    assert!(reporter.join().unwrap(), "the loop did not take the report while it waited, with no key pressed");

    assert!(h.s.ctl.model.instruments[0].pattern.is_some(), "the recording was folded in");
    assert!(h.tree().iter().any(|l| l.contains('▶')), "and the view caught up: {:?}", h.tree());
}

#[test]
fn a_loop_whose_terminal_has_gone_ends_with_an_error() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded::<io::Result<Event>>();
    drop(tx);
    let error = event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap_err();
    assert!(error.to_string().contains("terminal"), "{error}");
}

#[test]
fn a_failure_of_the_terminal_ends_the_loop_with_it() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    tx.send(Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal went"))).unwrap();
    tx.send(terminal_quit()).unwrap();
    let error = event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap_err();
    assert_eq!((error.kind(), error.to_string().as_str()), (io::ErrorKind::BrokenPipe, "the terminal went"));
}

#[test]
fn a_failure_that_comes_behind_other_events_is_not_lost() {
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    tx.send(Err(io::Error::new(io::ErrorKind::BrokenPipe, "the terminal went"))).unwrap();
    // (A quit after it, so that a loop that carries on ends the test instead of hanging it.)
    tx.send(terminal_quit()).unwrap();
    let error = event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(h.s.chain_state.selected, 1, "what came before it was handled");
    assert!(!h.s.quit, "and what came after it was not");
}

#[test]
fn the_events_that_are_waiting_are_all_handled_before_the_next_frame_is_drawn() {
    // A paste, or a key held down: a frame for each key would fall behind.
    let mut h = Harness::new(scene_split());
    let (tx, rx) = unbounded();
    for _ in 0..40 {
        tx.send(terminal_key(KeyCode::Down, KeyModifiers::NONE)).unwrap();
    }
    tx.send(terminal_quit()).unwrap();

    event_loop(&mut h.terminal, &mut h.s, &rx, None).unwrap();

    // (The cursor wraps round the end of the tree.)
    assert_eq!(h.s.chain_state.selected, 40 % h.s.tree_entries.len(), "every key was handled");
    // (One frame before the keys and one after them; a few more if the machine stalled long enough for the tick to come.)
    let frames = h.terminal.get_frame().count();
    assert!(frames <= 4, "{frames} frames for 41 events");
}

// ---------------------------------------------------------------------------
// The web interface in the TUI
// ---------------------------------------------------------------------------

fn web_status(address: &str, clients: usize) -> WebStatus {
    WebStatus { address: address.parse().unwrap(), url: format!("http://{address}/#token=abc"), clients }
}

/// The first row of the screen: the tab bar, and the status at its right. (The dump
/// starts with the size of the screen and a heading.)
fn tab_bar(h: &mut Harness) -> String {
    h.dump().lines().nth(2).unwrap().to_string()
}

#[test]
fn the_tab_bar_says_where_the_web_interface_is_and_how_many_browsers_are_on_it() {
    let mut h = Harness::new(scene_split());
    assert!(!tab_bar(&mut h).contains("web"), "not unless it is served");
    assert!(tab_bar(&mut h).ends_with("120 BPM"));

    h.s.web = Some(web_status("127.0.0.1:8765", 0));
    let bar = tab_bar(&mut h);
    assert!(bar.ends_with("web 127.0.0.1:8765  120 BPM"), "{bar}");
    h.s.web.as_mut().unwrap().clients = 2;
    let bar = tab_bar(&mut h);
    assert!(bar.ends_with("web 127.0.0.1:8765 (2)  120 BPM"), "{bar}");
}

#[test]
fn a_click_on_a_tab_is_a_click_on_the_tab_that_is_drawn_there_with_the_mark_on_the_session_tab_or_without() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for dirty in [false, true] {
        let mut h = Harness::new(scene_split());
        h.s.ctl.dirty = dirty;
        let bar = tab_bar(&mut h);
        assert_eq!(bar.contains("Session *"), dirty, "{bar}");
        for (tab, name) in [(TAB_SESSION, "(1) Session"), (TAB_PIANO, "(2) Piano"), (TAB_SCOPE, "(3) Scope"), (TAB_PATCH, "(4) Patch"), (TAB_HELP, "(5) Help")] {
            let first = bar[..bar.find(name).unwrap()].chars().count() as u16;
            // Both ends of the word that is drawn: the first column and the last.
            for column in [first, first + name.chars().count() as u16 - 1] {
                h.s.active_tab = (tab + 1) % TAB_NAMES.len();
                let click = Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column, row: 0, modifiers: KeyModifiers::NONE });
                process_event(&mut h.s, click);
                assert_eq!(h.s.active_tab, tab, "a click at column {column} on `{name}` (the session is {}): {bar}", if dirty { "changed" } else { "as saved" });
            }
        }
    }
}

#[test]
fn the_web_label_comes_before_the_clip_light() {
    let mut h = Harness::new(scene_split());
    h.s.web = Some(web_status("127.0.0.1:8765", 1));
    h.s.clip = clip::ClipLight::new(0);
    h.s.clip.update(1, Instant::now());
    assert!(h.s.clip.is_lit());
    let bar = tab_bar(&mut h);
    assert!(bar.ends_with("web 127.0.0.1:8765 (1)  CLIP  120 BPM"), "{bar}");
}

#[test]
fn the_web_label_gives_way_before_the_tabs_the_clip_light_and_the_tempo_do() {
    let mut h = Harness::new(scene_split());
    h.s.web = Some(web_status("127.0.0.1:8765", 3));
    let tabs = "(1) Session │ (2) Piano │ (3) Scope │ (4) Patch │ (5) Help";
    for lit in [false, true] {
        h.s.clip = clip::ClipLight::new(0);
        if lit {
            h.s.clip.update(1, Instant::now());
        }
        assert_eq!(h.s.clip.is_lit(), lit);
        let tail = if lit { "CLIP  120 BPM" } else { "120 BPM" };
        let mut shown = Vec::new();
        // (From the width that the five tabs, two blanks, the clip light and the tempo take.)
        for width in 76..=130u16 {
            h.terminal = Terminal::new(TestBackend::new(width, HEIGHT)).unwrap();
            let bar = tab_bar(&mut h);
            let context = format!("{width} columns, clip light {}: {bar}", if lit { "on" } else { "off" });
            assert!(bar.starts_with(tabs), "the tabs are whole: {context}");
            assert!(bar.ends_with(tail), "the clip light and the tempo are there: {context}");
            shown.push(if bar.contains("web 127.0.0.1:8765 (3)") {
                "full"
            } else if bar.contains("web (3)") {
                "short"
            } else {
                assert!(!bar.contains("web"), "{context}");
                "none"
            });
            if shown.last() != Some(&"none") {
                assert!(bar.starts_with(&format!("{tabs}  ")), "two blanks between the tabs and the label: {context}");
                assert!(bar.ends_with(&format!("  {tail}")), "and two between the label and the rest: {context}");
            }
        }
        // The narrower the terminal, the less of it: all, the short one, none; each in one stretch of widths.
        let mut order = shown.clone();
        order.dedup();
        assert_eq!(order, ["none", "short", "full"], "clip light {lit}: {shown:?}");
    }
}

#[test]
fn the_help_tab_warns_about_an_address_beyond_this_machine_and_only_then() {
    let mut h = Harness::new(scene_split());
    h.s.set_web(web_status("127.0.0.1:8765", 0));
    assert!(!h.s.help_lines.iter().any(|l| l.contains("beyond this machine")), "{:?}", h.s.help_lines);

    h.s.set_web(web_status("0.0.0.0:8765", 0));
    assert!(h.s.help_lines.iter().any(|l| l.contains("beyond this machine")), "{:?}", h.s.help_lines);
    assert!(h.s.help_lines.iter().any(|l| l.contains("rather than 0.0.0.0")), "{:?}", h.s.help_lines);
    h.press(KeyCode::Char('5'));
    let screen = h.dump();
    assert!(screen.contains("beyond this machine"), "{screen}");
}

#[test]
fn the_help_tab_says_where_the_web_interface_is_when_there_is_one() {
    let mut h = Harness::new(scene_split());
    assert!(!h.s.help_lines.iter().any(|l| l.to_lowercase().contains("web")), "not unless it is served");

    h.s.set_web(web_status("127.0.0.1:8765", 0));
    assert!(h.s.help_lines.iter().any(|l| l.contains("http://127.0.0.1:8765/#token=abc")), "{:?}", h.s.help_lines);
    h.press(KeyCode::Char('5'));
    let screen = h.dump();
    assert!(screen.contains("Web interface"), "{screen}");
    assert!(screen.contains("http://127.0.0.1:8765/#token=abc"), "the address to open, with the token if it has one: {screen}");
}

#[cfg(feature = "web")]
mod with_web {
    //! The TUI loop and a browser on one controller.

    use std::future::Future;
    use std::net::SocketAddr;
    use std::time::Duration;

    use crossbeam_channel::Sender;
    use serde_json::{Value, json};
    use tokio::sync::oneshot;

    use super::*;
    use crate::cli::WebArgs;
    use crate::tui::web::Web;
    use crate::web::testclient::{self, said, tell};

    const TRACER: &str = include_str!("../../web/src/client/fixtures/tracer.toml");

    fn key(code: KeyCode) -> io::Result<Event> {
        terminal_key(code, KeyModifiers::NONE)
    }

    fn start(h: &mut Harness) -> Web {
        Web::start(&mut h.s.ctl, &WebArgs { web_bind: "127.0.0.1:0".parse().unwrap(), web_origins: vec![] }).expect("the web interface starts")
    }

    /// What a browser's side of a test gets: where to connect, the terminal (to press
    /// keys in the TUI as a user would: they go to the loop as the terminal's events),
    /// and a signal that comes when the test has looked at the TUI and the browser may
    /// go.
    struct Browser {
        addr: SocketAddr,
        keys: Sender<io::Result<Event>>,
        done: oneshot::Receiver<()>,
    }

    impl Browser {
        /// Connect, subscribe, and take the snapshot (and the catalog that comes after it).
        async fn join(&self) -> (testclient::Client, Value) {
            let mut client = testclient::connect(self.addr).await;
            assert_eq!(said(&mut client).await["type"], "hello");
            let snapshot = testclient::subscribe(&mut client).await;
            (client, snapshot)
        }

        /// End the loop, and keep the connection open until the test is done with the screen.
        async fn quit(self) {
            self.keys.send(terminal_quit()).unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(10), self.done).await;
        }
    }

    /// Run the loop of `h` with the web interface up, and `script` as a browser on its
    /// own thread. The loop ends when the script quits (or fails, which is reported
    /// when `after` has had its look at the screen).
    fn run<F, Fut>(h: &mut Harness, script: F, after: impl FnOnce(&mut Harness, &Web))
    where
        F: FnOnce(Browser) -> Fut + Send + 'static,
        Fut: Future<Output = ()>,
    {
        let mut web = start(h);
        let addr = web.status().address;
        let (tx, rx) = unbounded();
        let (done_tx, done) = oneshot::channel();
        let keys = tx.clone();
        let browser = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let quit = keys.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(script(Browser { addr, keys, done }))));
            // A browser that failed ends the loop too, rather than leave the test waiting for a quit.
            let _ = quit.send(terminal_quit());
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        });

        let looped = event_loop(&mut h.terminal, &mut h.s, &rx, Some(&mut web));
        // (The browser's side is still connected: it waits for this.)
        after(h, &web);
        let _ = done_tx.send(());
        browser.join().expect("the browser's side passed");
        looped.unwrap();
    }

    fn node<'a>(snapshot: &'a Value, name: &str) -> &'a Value {
        snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == name).unwrap_or_else(|| panic!("no node {name}"))
    }

    #[test]
    fn both_front_ends_share_one_controller() {
        let mut h = Harness::new(TRACER);
        // (The lane that `n` adds is by an intent, which loads from the catalog.)
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        // The TUI's cursor is on the filter.
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });

        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let (filter, cutoff) = (node(&snapshot, "Filter")["id"].as_u64().unwrap(), node(&snapshot, "Filter")["plugin"]["params"][0]["index"].as_u64().unwrap());
                assert_eq!(snapshot["snapshot"]["rev"], 0);

                // The browser turns the cutoff up; the TUI is looking at the filter.
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setParam", "node": filter, "param": cutoff, "value": 0.9}})).await;
                assert_eq!(said(&mut client).await["event"], json!({"rev": 1, "type": "paramSet", "node": filter, "param": cutoff, "value": 0.9}));
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "set cutoff", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));

                // The TUI turns it down a step (Enter takes the parameters' pane; the cutoff is the first). The
                // step starts from where the browser left it, and the browser hears it.
                browser.keys.send(key(KeyCode::Enter)).unwrap();
                browser.keys.send(key(KeyCode::Left)).unwrap();
                let event = said(&mut client).await;
                assert_eq!(event["type"], "event", "{event}");
                let (rev, value) = (event["event"]["rev"].as_u64().unwrap(), event["event"]["value"].as_f64().unwrap());
                assert_eq!((event["event"]["type"].clone(), event["event"]["node"].clone(), rev), (json!("paramSet"), json!(filter), 2));
                assert!(value < 0.9 && value > 0.8, "a step down from what the browser set, not from what the TUI had: {value}");

                // A browser that comes now is told where the session is, though no snapshot has been sent since:
                // the loop keeps it current (every 100 ms or so).
                tokio::time::sleep(Duration::from_millis(350)).await;
                let mut late = testclient::connect(browser.addr).await;
                assert_eq!(said(&mut late).await["rev"], 2, "hello");
                drop(late);

                // The TUI sets the tempo with `b`, an intent: the browser hears it as an event.
                browser.keys.send(key(KeyCode::Esc)).unwrap();
                browser.keys.send(key(KeyCode::Char('b'))).unwrap();
                // (The popup starts with 120, which goes.)
                for _ in 0..3 {
                    browser.keys.send(key(KeyCode::Backspace)).unwrap();
                }
                for c in ['9', '0'] {
                    browser.keys.send(key(KeyCode::Char(c))).unwrap();
                }
                browser.keys.send(key(KeyCode::Enter)).unwrap();
                let told = said(&mut client).await;
                assert_eq!(told["type"], "event", "{told}");
                assert_eq!(told["event"], json!({"rev": 3, "type": "bpmSet", "bpm": 90.0}));
                // (And what Undo would do now: the tempo is a step of its own, where the cutoff the TUI turned went on the browser's.)
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "set the tempo", "redo": null}));
                let seen = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().len();

                // And adds a lane with `n` (and leaves the oscillator it comes with): an intent, told as a snapshot at the revision after it.
                browser.keys.send(key(KeyCode::Char('n'))).unwrap();
                browser.keys.send(key(KeyCode::Esc)).unwrap();
                let told = said(&mut client).await;
                assert_eq!(told["type"], "snapshot", "{told}");
                assert_eq!(told["snapshot"]["rev"], 5);
                assert_eq!(told["snapshot"]["graph"]["nodes"].as_array().unwrap().len(), seen + 1, "a lane");
                // (What was told before is in it: the cutoff the TUI set.)
                assert_eq!(node(&told, "Filter")["plugin"]["params"][0]["value"].as_f64().unwrap(), value);

                browser.quit().await;
            },
            |h, web| {
                // What the browser set and the TUI changed after it, in the model and so on the screen.
                let cutoff = h.s.ctl.model.instruments[0].effects[0].params[0].value;
                assert!(cutoff < 0.9 && cutoff > 0.8, "{cutoff}");
                assert_eq!(h.s.ctl.model.bpm, 90.0);
                assert_eq!(h.s.ctl.model.instruments.len(), 2, "the lane");
                assert_eq!(h.s.ctl.rev, 5, "an intent from the browser, three from the TUI (the cutoff, the tempo and the lane, which is told with a snapshot after it)");
                let bar = tab_bar(h);
                assert!(bar.contains(&format!("web {} (1)", web.status().address)), "the browser is counted: {bar}");
            },
        );
    }

    #[test]
    fn a_group_volume_set_in_a_browser_shows_in_the_trees_group_header() {
        let mut h = Harness::new(scene_group());
        assert!(h.tree().contains(&"▦ Pad  vol 0.80"), "{:#?}", h.tree());
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let group = node(&snapshot, "Pad")["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setGain", "node": group, "gain": "groupVolume", "value": 2.5}})).await;
                assert_eq!(said(&mut client).await["type"], "event");
                assert_eq!(said(&mut client).await["type"], "history");
                assert_eq!(said(&mut client).await["type"], "ack");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.groups[0].volume, 2.5);
                assert!(h.tree().contains(&"▦ Pad  vol 2.50"), "the header follows what the browser set: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn a_range_set_in_a_browser_shows_on_the_lane_in_the_tree() {
        let mut h = Harness::new("[[instrument]]\nplugin = \"builtin:osc\"\n");
        assert!(h.tree().iter().all(|line| !line.contains("C3-B4")), "{:#?}", h.tree());
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let lane = node(&snapshot, "Oscillator")["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "setRange", "node": lane, "range": {"low": 48, "high": 71}}})).await;
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["event"]["type"].clone()), (json!("event"), json!("rangeSet")), "{told}");
                assert_eq!(said(&mut client).await["type"], "history");
                assert_eq!(said(&mut client).await["type"], "ack");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.instruments[0].range, Some((48, 71)));
                assert!(h.tree().iter().any(|line| line.contains("Oscillator") && line.contains("C3-B4")), "the lane's label follows what the browser set: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn a_preset_loaded_in_a_browser_changes_the_parameters_the_tui_shows() {
        let mut h = Harness::new("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n");
        let before: Vec<f32> = h.s.ctl.model.instruments[0].effects[0].params.iter().map(|p| p.value).collect();
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let reverb = node(&snapshot, "Reverb")["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "loadPreset", "node": reverb, "preset": "cathedral"}})).await;
                let event = said(&mut client).await;
                assert_eq!(event["event"]["type"], "presetLoaded");
                // A step of its own, in the words of the preset.
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "load preset Cathedral", "redo": null}));
                assert_eq!(said(&mut client).await["type"], "ack");
                browser.quit().await;
            },
            |h, _| {
                let slot = &h.s.ctl.model.instruments[0].effects[0];
                assert_eq!(slot.current_preset.as_deref(), Some("Cathedral"));
                let after: Vec<f32> = slot.params.iter().map(|p| p.value).collect();
                assert_ne!(before, after, "the preset's values are in what the TUI shows");
            },
        );
    }

    #[test]
    fn a_cable_a_browser_takes_out_is_gone_from_the_tui_at_once_and_the_browser_is_given_the_new_structure() {
        let mut h = Harness::new(TRACER);
        assert!(h.tree().iter().any(|line| line.contains('→')), "the LFO drives the cutoff: {:#?}", h.tree());
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let edges = snapshot["snapshot"]["graph"]["edges"].as_array().unwrap();
                let cable = edges.iter().find(|e| e["kind"] == "modulation").unwrap()["id"].as_u64().unwrap();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "removeEdge", "edge": cable}})).await;
                // The structure is told as a snapshot, at the revision after the intent's, and the answer follows it.
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(2)), "{told}");
                assert!(told["snapshot"]["graph"]["edges"].as_array().unwrap().iter().all(|e| e["kind"] != "modulation"));
                // What can be undone now (the removal is a step of the history, which puts the cable back) is told before the answer.
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "remove a cable", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));
                browser.quit().await;
            },
            |h, _| {
                assert!(h.s.ctl.model.free_modulators[0].targets.is_empty());
                assert!(h.tree().iter().all(|line| !line.contains('→')), "the tree is built again: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn an_effect_a_browser_adds_is_in_the_tuis_tree_at_once_and_the_browser_is_given_the_new_structure() {
        let mut h = Harness::new(TRACER);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        assert!(!h.tree().iter().any(|line| line.contains("fx Reverb")));
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let oscillator = snapshot["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["name"] == "Oscillator").unwrap()["id"].clone();
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addEffect", "parent": oscillator, "plugin": "builtin:reverb"}})).await;
                // The structure is told as a snapshot, at the revision after the intent's, and the answer follows it.
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(2)), "{told}");
                let names: Vec<_> = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().map(|n| n["name"].as_str().unwrap().to_string()).collect();
                assert!(names.contains(&"Reverb".to_string()), "{names:?}");
                // What can be undone now (the add is a step of the history) is told before the answer.
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "add Reverb", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));
                // A plugin that is not in the catalog is refused, with the words of the controller.
                tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "addEffect", "parent": oscillator, "plugin": "./evil.clap"}})).await;
                let refused = said(&mut client).await;
                assert_eq!((refused["type"].clone(), refused["error"]["code"].clone()), (json!("rejected"), json!("invalid")), "{refused}");
                assert!(refused["error"]["message"].as_str().unwrap().contains("not a plugin that is installed"), "{refused}");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.instruments[0].effects.len(), 2, "the filter, and the reverb the browser added");
                assert!(h.tree().iter().any(|line| line.contains("fx Reverb")), "the tree is built again: {:#?}", h.tree());
            },
        );
    }

    #[test]
    fn an_instrument_a_browser_puts_in_is_in_the_tuis_tree_at_once_and_the_browser_is_given_the_new_structure() {
        let mut h = Harness::new(TRACER);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        assert_eq!(h.s.ctl.model.instruments.len(), 1);
        run(
            &mut h,
            |browser| async move {
                let (mut client, snapshot) = browser.join().await;
                let oscillator = node(&snapshot, "Oscillator")["id"].clone();
                // A lane of its own: told as a snapshot, at the revision after the intent's, and the answer follows it.
                tell(&mut client, json!({"type": "request", "reqId": 1, "intent": {"type": "addInstrument", "plugin": "builtin:osc"}})).await;
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(2)), "{told}");
                let lanes = told["snapshot"]["graph"]["nodes"].as_array().unwrap().iter().filter(|n| n["kind"] == "instrument").count();
                assert_eq!(lanes, 2, "{told}");
                // Its meters (a peak and a clip count) are in the layout of the telemetry that the snapshot has.
                let meters = |snapshot: &Value| snapshot["snapshot"]["telemetry"]["meters"].as_array().unwrap().len();
                assert_eq!(meters(&told), meters(&snapshot) + 2, "{told}");
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "add Oscillator as an instrument", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 1, "rev": 1}));
                // The lane there was gets another instrument: a step of its own, in the words of the button that puts one in.
                tell(&mut client, json!({"type": "request", "reqId": 2, "intent": {"type": "replaceInstrument", "node": oscillator, "plugin": "builtin:osc"}})).await;
                let told = said(&mut client).await;
                assert_eq!((told["type"].clone(), told["snapshot"]["rev"].clone()), (json!("snapshot"), json!(4)), "{told}");
                assert_eq!(said(&mut client).await, json!({"type": "history", "undo": "change the instrument to Oscillator", "redo": null}));
                assert_eq!(said(&mut client).await, json!({"type": "ack", "reqId": 2, "rev": 3}));
                // An effect is not an instrument, and the controller's words say so.
                tell(&mut client, json!({"type": "request", "reqId": 3, "intent": {"type": "replaceInstrument", "node": oscillator, "plugin": "builtin:reverb"}})).await;
                let refused = said(&mut client).await;
                assert_eq!((refused["type"].clone(), refused["error"]["code"].clone()), (json!("rejected"), json!("invalid")), "{refused}");
                assert_eq!(refused["error"]["message"], "Reverb is an effect, not an instrument", "{refused}");
                browser.quit().await;
            },
            |h, _| {
                assert_eq!(h.s.ctl.model.instruments.len(), 2, "the lane the browser added");
                assert_eq!(h.tree().iter().filter(|line| line.contains("♪ Oscillator")).count(), 2, "{:#?}", h.tree());
            },
        );
    }

    #[test]
    fn a_server_that_has_stopped_does_not_make_the_loop_spin() {
        let mut h = Harness::new(TRACER);
        let mut web = start(&mut h);
        // Nothing can send a request any more, and the loop is told so by the channel.
        web.stop_server();
        let (tx, rx) = unbounded();
        let quitter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            tx.send(terminal_quit()).unwrap();
        });
        event_loop(&mut h.terminal, &mut h.s, &rx, Some(&mut web)).unwrap();
        quitter.join().unwrap();
        let frames = h.terminal.get_frame().count();
        assert!(frames < 40, "{frames} frames in half a second: the loop spins on a channel that has nothing more to give");
    }

    #[test]
    fn the_web_interface_that_is_up_is_in_the_tab_bar_from_the_first_frame() {
        let mut h = Harness::new(TRACER);
        let mut web = start(&mut h);
        let (tx, rx) = unbounded();
        tx.send(terminal_quit()).unwrap();
        event_loop(&mut h.terminal, &mut h.s, &rx, Some(&mut web)).unwrap();
        let bar = tab_bar(&mut h);
        assert!(bar.contains(&format!("web {}", web.status().address)), "{bar}");
        assert!(!bar.contains("web 127.0.0.1:0"), "the address it listens on, not the one it was asked for: {bar}");
    }
}

// -- A browser changes the structure under the TUI's cursor and popups --
//
// The tree is addressed by position, and a browser can add or take out a node above what the TUI is on:
// the cursor has to stay on the node it was on (or `d` takes out what it was never on), and a popup that
// holds a place in the tree (a modulator's, a lane's, a group's) is not for what is there now.

mod browser_changes_structure {
    use super::*;
    use crate::app::graph::{Endpoint, Port};
    use crate::app::intent::{Intent, ModulatorKind};
    use crate::app::model::ModSourceSlot;

    const TWO_LANES: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    const THREE_LFOS: &str = r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.modulator]]
rate = 1.0
[[instrument.modulator]]
rate = 2.0
[[instrument.modulator]]
rate = 3.0
"#;

    /// What a browser does: the intent is applied by the controller, and the loop of the TUI reacts to it.
    fn browser(h: &mut Harness, intent: Intent) {
        let applied = h.s.ctl.apply(intent).unwrap();
        h.s.react_to(&applied);
    }

    fn cursor_node(h: &Harness) -> NodeId {
        h.s.tree_entries[h.s.chain_state.selected].node
    }

    fn rates(h: &Harness) -> Vec<f32> {
        h.s.ctl.model.free_modulators.iter().map(|m| if let ModSourceSlot::Lfo { rate, .. } = m.source { rate } else { -1.0 }).collect()
    }

    #[test]
    fn the_cursor_stays_on_its_node_when_a_browser_adds_a_node_above_it() {
        let mut h = Harness::new(TWO_LANES);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) });
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        // And what `d` takes out is the reverb, which the person was on, and not what has come to its place.
        h.press(KeyCode::Char('d'));
        let lane = &h.s.ctl.model.instruments[1];
        assert!(lane.instrument.is_some() && lane.effects.is_empty(), "the reverb went: {:#?}", h.tree());
    }

    #[test]
    fn an_effect_a_browser_adds_is_in_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddEffect { parent: lane0, plugin: "builtin:filter".to_string() });
        assert!(h.tree().iter().any(|line| line.contains("fx Filter")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
    }

    #[test]
    fn a_lane_a_browser_adds_is_at_the_end_of_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let before = h.tree().len();
        browser(&mut h, Intent::AddInstrument { plugin: "builtin:osc".to_string() });
        assert_eq!(h.s.ctl.model.instruments.len(), 3);
        assert_eq!(h.tree().len(), before + 1, "{:#?}", h.tree());
        assert!(h.tree().last().is_some_and(|line| line.contains("Oscillator")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
        // A lane that came from a browser is a lane like any: the first `d` empties it, a second takes it out.
        h.goto(TreeAddress::Instrument(2));
        h.press(KeyCode::Char('d')).press(KeyCode::Char('d'));
        assert_eq!(h.s.ctl.model.instruments.len(), 2);
    }

    #[test]
    fn an_instrument_a_browser_puts_in_a_lane_takes_the_cable_to_the_old_one_out_of_the_tree_and_the_cursor_stays() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Instrument(0));
        let lane = cursor_node(&h);
        assert!(h.tree().iter().any(|line| line.contains("LFO 4.0Hz sine → ")), "{:#?}", h.tree());
        browser(&mut h, Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() });
        assert!(h.tree().iter().any(|line| line.contains("♪ Oscillator")), "{:#?}", h.tree());
        assert!(h.tree().iter().any(|line| line.contains("LFO 4.0Hz sine") && !line.contains('→')), "the vibrato's cable went with the old instrument: {:#?}", h.tree());
        assert!(h.tree().iter().any(|line| line.contains("LFO 7.0Hz sine → ")), "the other lane's stays: {:#?}", h.tree());
        assert_eq!(cursor_node(&h), lane, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Instrument(0));
    }

    #[test]
    fn a_row_on_the_cable_that_goes_with_a_replaced_instrument_is_not_left_selected() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        // On the vibrato's own parameters, on the row of its cable (the last).
        h.goto(TreeAddress::FreeModulator { index: 0 });
        h.press(KeyCode::Enter);
        let rows = h.s.param_state.len;
        h.press_n(KeyCode::Down, rows - 1);
        assert_eq!(h.s.param_state.selected, rows - 1, "the last row, which is the cable's");
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::ReplaceInstrument { node: lane, plugin: "builtin:osc".to_string() });
        assert!(h.s.ctl.model.free_modulators[0].targets.is_empty());
        // The pane has fewer rows, and the selection is on the last of them.
        assert!(h.s.param_state.len < rows, "{} rows of {rows}", h.s.param_state.len);
        assert_eq!(h.s.param_state.selected, h.s.param_state.len - 1);
        // The frame is drawn from what is there, and the keys still do what they do.
        assert!(!h.dump().is_empty());
        h.press(KeyCode::Down).press(KeyCode::Up).press(KeyCode::Left).press(KeyCode::Right);
        assert!(!h.dump().is_empty());
    }

    #[test]
    fn the_cursor_stays_on_its_node_when_a_browser_takes_out_an_effect_above_it() {
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#,
        );
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        browser(&mut h, Intent::RemoveNode { node: filter });
        assert!(h.tree().iter().all(|line| !line.contains("fx Filter")), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
    }

    /// A lane with a filter and then a reverb, and a group with a filter and then a reverb on its bus and a lane in it.
    const TWO_CHAINS: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"
[[group.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"

[[instrument]]
plugin = "builtin:osc"
group = 0
"#;

    /// Where the line that has `text` is in the tree.
    fn line_of(h: &Harness, text: &str) -> usize {
        h.tree().iter().position(|line| line.contains(text)).unwrap_or_else(|| panic!("no {text:?} in {:#?}", h.tree()))
    }

    #[test]
    fn the_cursor_follows_its_effect_when_a_browser_moves_it_and_the_tree_shows_the_new_order() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        let filter = cursor_node(&h);
        assert!(line_of(&h, "fx Filter  [Built-in]") < line_of(&h, "fx Reverb  [Built-in]"), "{:#?}", h.tree());
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        // (The lane's two effects are the first two of the tree's effects; the bus has its own, marked.)
        let lane_filter = h.tree().iter().position(|line| line.contains("fx Filter  [Built-in]") && !line.contains("(bus)")).unwrap();
        let lane_reverb = h.tree().iter().position(|line| line.contains("fx Reverb  [Built-in]") && !line.contains("(bus)")).unwrap();
        assert!(lane_reverb < lane_filter, "the reverb is first now: {:#?}", h.tree());
        assert_eq!(cursor_node(&h), filter, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 0, index: 1 });
        assert!(h.s.ctl.dirty);
        // What `d` takes out is the filter, which the person was on, and not what has come to its place.
        h.press(KeyCode::Char('d'));
        let chain: Vec<_> = h.s.ctl.model.instruments[0].effects.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(chain, ["Reverb"]);
    }

    #[test]
    fn what_a_browser_pins_to_a_node_leaves_the_tree_as_it_is_and_the_session_unsaved() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        let filter = cursor_node(&h);
        let before: Vec<String> = h.tree().iter().map(|line| line.to_string()).collect();
        let cutoff = h.s.ctl.model.instruments[0].effects[0].params.iter().find(|p| p.name == "cutoff").unwrap().index;
        browser(&mut h, Intent::SetPinned { node: filter, params: vec![cutoff] });
        assert_eq!(h.s.ctl.model.instruments[0].effects[0].pinned, [cutoff]);
        let after: Vec<String> = h.tree().iter().map(|line| line.to_string()).collect();
        assert_eq!(after, before, "the TUI does not draw which knobs a node has");
        assert_eq!(cursor_node(&h), filter);
        assert!(h.s.ctl.dirty, "it is in the file when the session is saved");
    }

    #[test]
    fn the_cursor_is_left_where_it_was_by_a_move_of_an_effect_it_is_not_on() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Instrument(0));
        let lane = cursor_node(&h);
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        assert_eq!(cursor_node(&h), lane);
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Instrument(0));
    }

    #[test]
    fn a_bus_effect_a_browser_moves_is_in_its_new_place_in_the_tree_and_the_cursor_follows_it() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::GroupEffect { group: 0, index: 1 });
        let reverb = cursor_node(&h);
        browser(&mut h, Intent::MoveEffect { node: reverb, index: 0 });
        assert!(line_of(&h, "fx Reverb  [Built-in]  (bus)") < line_of(&h, "fx Filter  [Built-in]  (bus)"), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb);
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::GroupEffect { group: 0, index: 0 });
        assert!(h.s.ctl.dirty);
    }

    #[test]
    fn a_value_typed_in_the_tui_goes_to_the_effect_it_was_typed_for_when_a_browser_moves_it() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press(KeyCode::Enter); // into the filter's parameters, on the first: the cutoff
        let filter = cursor_node(&h);
        h.press(KeyCode::Enter); // the value popup
        assert!(h.s.editing.is_some());
        for _ in 0..8 {
            h.press(KeyCode::Backspace);
        }
        h.type_chars("0.25");
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        assert!(h.s.editing.is_some(), "what was being typed is for a node that is still there");
        h.press(KeyCode::Enter);
        let effects = &h.s.ctl.model.instruments[0].effects;
        assert_eq!((effects[1].name.as_str(), effects[1].params[0].value), ("Filter", 0.25), "the filter, which is the second now, has it");
        assert_ne!(effects[0].params[0].value, 0.25, "and the reverb, which has come to the filter's place, does not");
    }

    #[test]
    fn a_selector_of_presets_is_closed_when_a_browser_moves_an_effect_whose_place_it_holds() {
        let mut h = Harness::new(TWO_CHAINS);
        h.goto(TreeAddress::Effect { inst: 0, index: 1 }); // the reverb, which has presets
        h.press(KeyCode::Char('p'));
        assert!(h.s.preset_selector.is_some(), "the reverb has presets");
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        browser(&mut h, Intent::MoveEffect { node: filter, index: 1 });
        assert!(h.s.preset_selector.is_none(), "it holds the place of an effect that has moved");
    }

    #[test]
    fn popups_are_closed_when_a_browser_takes_out_the_effect_the_cursor_was_on() {
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#,
        );
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press(KeyCode::Enter); // its parameters
        h.press(KeyCode::Enter); // the value popup for the first
        assert!(h.s.editing.is_some());
        let reverb = cursor_node(&h);
        browser(&mut h, Intent::RemoveNode { node: reverb });
        assert!(h.s.editing.is_none(), "what was being typed was for a node that is gone");
        assert!(h.s.ctl.model.instruments[0].effects.is_empty());
    }

    #[test]
    fn the_cursor_stays_on_its_node_when_a_browser_takes_out_a_node_above_it() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::FreeModulator { index: 1 });
        let second = cursor_node(&h);
        let first = h.s.ctl.model.free_modulators[0].node;
        browser(&mut h, Intent::RemoveNode { node: first });
        assert_eq!(cursor_node(&h), second, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::FreeModulator { index: 0 });
    }

    #[test]
    fn a_value_typed_in_the_tui_goes_to_the_node_it_was_typed_for_when_a_browser_adds_a_node_above() {
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
"#,
        );
        h.goto(TreeAddress::Instrument(1));
        h.press(KeyCode::Enter); // into the parameters of the second oscillator
        h.press(KeyCode::Down).press(KeyCode::Down); // the third row
        let before = h.s.ctl.model.instruments[1].instrument.as_ref().unwrap().params[2].value;
        h.press(KeyCode::Enter); // the value popup
        assert!(h.s.editing.is_some());
        for _ in 0..8 {
            h.press(KeyCode::Backspace);
        }
        h.type_chars("0.25");
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) });
        h.press(KeyCode::Enter);
        let after = h.s.ctl.model.instruments[1].instrument.as_ref().unwrap().params[2].value;
        assert_eq!(after, 0.25, "it was {before}: the value reached the parameter it was typed for");
        assert_eq!(rates(&h), [1.0], "and the first lane has the one LFO the browser gave it, at the rate every new one has: not what was typed");
    }

    #[test]
    fn a_selector_of_targets_is_closed_when_a_browser_takes_out_a_modulator_it_counts_by() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::FreeModulator { index: 1 }); // the 2 Hz LFO
        h.press(KeyCode::Char('t'));
        assert!(h.s.target_selector.is_some());
        let first = h.s.ctl.model.free_modulators[0].node;
        browser(&mut h, Intent::RemoveNode { node: first });
        assert!(h.s.target_selector.is_none(), "it holds the place of a modulator that has moved");
        h.press(KeyCode::Enter);
        assert_eq!(rates(&h), [2.0, 3.0]);
        assert!(h.s.ctl.model.free_modulators.iter().all(|m| m.targets.is_empty()), "no cable went to the one that was not chosen");
    }

    #[test]
    fn popups_are_closed_when_a_browser_takes_out_the_node_the_cursor_was_on() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::FreeModulator { index: 1 });
        h.press(KeyCode::Enter); // its parameters: the type, the waveform, then the rate
        h.press(KeyCode::Down).press(KeyCode::Down);
        h.press(KeyCode::Enter); // the value popup for the rate
        assert!(h.s.editing.is_some());
        let second = cursor_node(&h);
        browser(&mut h, Intent::RemoveNode { node: second });
        assert!(h.s.editing.is_none(), "what was being typed was for a node that is gone");
        assert_eq!(rates(&h), [1.0, 3.0]);
    }

    #[test]
    fn a_popup_is_left_alone_by_a_change_that_is_not_one_of_structure() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::FreeModulator { index: 1 });
        h.press(KeyCode::Char('t'));
        assert!(h.s.target_selector.is_some());
        let third = h.s.ctl.model.free_modulators[2].node;
        browser(&mut h, Intent::SetModulatorField { node: third, field: crate::app::intent::ModulatorField::Rate, value: 9.0 });
        assert!(h.s.target_selector.is_some(), "the third LFO's rate changed: nothing moved");
    }

    #[test]
    fn a_save_that_a_browser_asks_for_takes_the_mark_off_the_session_tab() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Harness::new(TWO_LANES);
        h.s.ctl.session_path = Some(dir.path().join("song.toml"));
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::SetGain { node: lane, gain: crate::app::intent::Gain::Volume, value: 0.5 });
        assert!(tab_bar(&mut h).contains("(1) Session *"), "{}", tab_bar(&mut h));
        browser(&mut h, Intent::Save);
        let bar = tab_bar(&mut h);
        assert!(bar.contains("(1) Session") && !bar.contains("Session *"), "{bar}");
        assert!(dir.path().join("song.toml").exists(), "and the file is there");
        // A save that fails says nothing to the TUI and keeps the mark.
        h.s.ctl.session_path = Some(dir.path().join("song.toml").join("not-a-directory.toml"));
        browser(&mut h, Intent::SetGain { node: lane, gain: crate::app::intent::Gain::Volume, value: 0.75 });
        assert!(h.s.ctl.apply(Intent::Save).is_err());
        assert!(tab_bar(&mut h).contains("Session *"));
    }

    #[test]
    fn a_popup_is_left_alone_by_a_cable_that_moves_no_place() {
        let mut h = Harness::new(THREE_LFOS);
        h.goto(TreeAddress::FreeModulator { index: 1 });
        h.press(KeyCode::Char('t'));
        assert!(h.s.target_selector.is_some());
        let lane = &h.s.ctl.model.instruments[0];
        let volume = lane.instrument.as_ref().unwrap().params.iter().find(|p| p.name == "volume").unwrap().index;
        let to = Endpoint { node: lane.node, port: Port::Param { index: volume } };
        let applied = h.s.ctl.apply(Intent::AddModEdge { modulator: h.s.ctl.model.free_modulators[0].node, to }).unwrap();
        assert!(applied.restructured, "a cable is a change of structure");
        let places = h.s.tree_places();
        h.s.react_to(&applied);
        assert_eq!(h.s.tree_places(), places, "but the tree has the same nodes in the same places");
        assert!(h.tree()[1].contains('→'), "and shows the cable: {:#?}", h.tree());
        assert!(h.s.target_selector.is_some(), "nothing it counts by has moved");
    }

    /// A group with a lane in it, then a lane that is in none, which has a reverb (it has presets) and an LFO.
    const GROUP_AND_LANE: &str = r#"
[[group]]
name = "Pad"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 2.0
"#;

    type IsOpen = fn(&State) -> bool;

    /// Every popup that holds a place in the tree of `GROUP_AND_LANE`: what it is, where the cursor is when it is opened, the keys that open it
    /// and how to tell that it is open.
    fn popups_that_hold_a_place() -> [(&'static str, TreeAddress, &'static [KeyCode], IsOpen); 7] {
        [
            ("the range of a lane", TreeAddress::Instrument(1), &[KeyCode::Char('R')], |s| s.range_edit.is_some()),
            ("the name of a group", TreeAddress::Group(0), &[KeyCode::Char('R')], |s| s.rename_group.is_some()),
            ("the selector of a plugin", TreeAddress::Instrument(1), &[KeyCode::Char('a')], |s| s.selector.is_some()),
            ("the selector of targets", TreeAddress::FreeModulator { index: 0 }, &[KeyCode::Char('t')], |s| s.target_selector.is_some()),
            ("the choice of a modulator", TreeAddress::Effect { inst: 1, index: 0 }, &[KeyCode::Enter, KeyCode::Char('m')], |s| s.modulate.is_some()),
            ("the group of a lane", TreeAddress::Instrument(1), &[KeyCode::Char('g')], |s| s.group_assign.is_some()),
            ("the presets of a plugin", TreeAddress::Effect { inst: 1, index: 0 }, &[KeyCode::Char('p')], |s| s.preset_selector.is_some()),
        ]
    }

    #[test]
    fn every_popup_that_holds_a_place_is_closed_when_a_browser_moves_the_places() {
        for (what, at, keys, is_open) in popups_that_hold_a_place() {
            let mut h = Harness::new(GROUP_AND_LANE);
            h.goto(at);
            for key in keys {
                h.press(*key);
            }
            assert!(is_open(&h.s), "{what} is open to begin with");
            let in_the_group = h.s.ctl.model.instruments[0].node;
            browser(&mut h, Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(in_the_group) });
            assert!(!is_open(&h.s), "{what} is for a place that has moved");
        }
    }

    #[test]
    fn every_popup_that_holds_a_place_is_closed_when_a_browser_adds_a_group_and_when_it_takes_one_out() {
        type Mover = fn(&Harness) -> Intent;
        let movers: [(&str, Mover); 3] = [
            ("adds a group", |_| Intent::AddGroup { members: vec![] }),
            ("adds a group with the lane in it", |h| Intent::AddGroup { members: vec![h.s.ctl.model.instruments[1].node] }),
            ("takes the group out", |h| Intent::RemoveNode { node: h.s.ctl.model.groups[0].node }),
        ];
        for (move_, mover) in movers {
            for (what, at, keys, is_open) in popups_that_hold_a_place() {
                let mut h = Harness::new(GROUP_AND_LANE);
                h.goto(at);
                for key in keys {
                    h.press(*key);
                }
                assert!(is_open(&h.s), "{what} is open to begin with");
                let intent = mover(&h);
                browser(&mut h, intent);
                assert!(!is_open(&h.s), "{what} is for a place that has moved when a browser {move_}");
            }
        }
    }

    #[test]
    fn the_cursor_stays_on_a_lanes_pattern_and_does_not_go_up_to_its_instrument() {
        // The pattern and the instrument of a lane are two entries of one node.
        let mut h = Harness::new(
            r#"
[[instrument]]
plugin = "builtin:osc"
[[instrument]]
plugin = "builtin:osc"
[instrument.pattern]
bpm = 120.0
length_beats = 4.0
base_note = "C4"
enabled = true
[[instrument.pattern.events]]
frame = 0
status = "on"
note = "C4"
velocity = 100
[[instrument.pattern.events]]
frame = 24000
status = "off"
note = "C4"
velocity = 0
"#,
        );
        h.goto(TreeAddress::Pattern(1));
        let lane0 = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::AddFreeModulator { kind: ModulatorKind::Lfo, hears: Some(lane0) });
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Pattern(1), "{:#?}", h.tree());
    }

    // -- what a browser does to groups, as the TUI shows it --

    /// A group with a lane in it, another that is empty, and a lane in none.
    const TWO_GROUPS_AND_LANE: &str = r#"
[[group]]
name = "Pad"

[[group]]
name = "Lead"

[[instrument]]
plugin = "builtin:osc"
group = 0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    /// The lines of the tree that are a group's header.
    fn headers(h: &Harness) -> Vec<&str> {
        h.tree().into_iter().filter(|line| line.starts_with('▦')).collect()
    }

    /// The lines of the tree that are a lane's instrument and in no group (a member of a group is indented under its header).
    fn ungrouped_lanes(h: &Harness) -> usize {
        h.tree().iter().filter(|line| line.starts_with('♪')).count()
    }

    #[test]
    fn a_group_a_browser_adds_is_in_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        assert!(headers(&h).is_empty(), "{:#?}", h.tree());
        browser(&mut h, Intent::AddGroup { members: vec![] });
        assert_eq!(h.s.ctl.model.groups.len(), 1);
        assert_eq!(headers(&h).len(), 1, "{:#?}", h.tree());
        assert!(headers(&h)[0].contains("Group 1"), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
        assert!(h.s.ctl.dirty);
        // A group that came from a browser is a group like any: `R` names it in the TUI and `d` takes it out.
        h.goto(TreeAddress::Group(0));
        h.press(KeyCode::Char('R')).type_chars("Pad").press(KeyCode::Enter);
        // (The field starts with the name the group was given, and a group that has none starts with nothing.)
        assert_eq!(h.s.ctl.model.groups[0].name.as_deref(), Some("Pad"));
        h.press(KeyCode::Char('d'));
        assert!(h.s.ctl.model.groups.is_empty(), "{:#?}", h.tree());
        assert!(headers(&h).is_empty(), "{:#?}", h.tree());
    }

    #[test]
    fn a_group_a_browser_makes_with_a_lane_in_it_has_the_lane_under_its_header_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(TWO_LANES);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        assert_eq!(ungrouped_lanes(&h), 2, "{:#?}", h.tree());
        let lane = h.s.ctl.model.instruments[1].node;
        browser(&mut h, Intent::AddGroup { members: vec![lane] });
        assert_eq!(h.s.ctl.model.instruments[1].group, Some(0));
        let tree = h.tree();
        // The header, and under it the lane with its reverb; the lane that is in no group is after.
        let header = tree.iter().position(|line| line.starts_with('▦')).expect("a header");
        assert!(tree[header + 1].contains("♪ Oscillator") && tree[header + 1].starts_with("  "), "{tree:#?}");
        assert!(tree[header + 2].contains("fx Reverb"), "{tree:#?}");
        assert_eq!(ungrouped_lanes(&h), 1, "{tree:#?}");
        assert_eq!(cursor_node(&h), reverb, "{tree:#?}");
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
    }

    #[test]
    fn a_lane_a_browser_takes_out_of_its_group_is_under_no_header_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::FreeModulator { index: 0 });
        let lfo = cursor_node(&h);
        assert_eq!(ungrouped_lanes(&h), 1, "{:#?}", h.tree());
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::SetLaneGroup { node: lane, group: None });
        assert_eq!(h.s.ctl.model.instruments[0].group, None);
        // The group is still there, empty, and both lanes are in none.
        assert_eq!(headers(&h).len(), 1, "{:#?}", h.tree());
        assert_eq!(ungrouped_lanes(&h), 2, "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), lfo, "{:#?}", h.tree());
        assert!(h.s.ctl.dirty);
    }

    #[test]
    fn a_name_a_browser_gives_a_group_is_in_the_tree_and_in_the_title_of_its_pane_and_a_name_of_nothing_gives_the_default_back() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Group(0));
        let pad = cursor_node(&h);
        assert!(h.dump().contains("┌ Pad "), "{}", h.dump());
        browser(&mut h, Intent::RenameGroup { node: pad, name: "  Strings  ".to_string() });
        assert!(headers(&h)[0].contains("Strings") && !headers(&h)[0].contains("Pad"), "{:#?}", h.tree());
        let frame = h.dump();
        assert!(frame.contains("┌ Strings ") && !frame.contains("┌ Pad "), "{frame}");
        assert!(h.s.ctl.dirty);
        browser(&mut h, Intent::RenameGroup { node: pad, name: " ".to_string() });
        assert!(headers(&h)[0].contains("Group 1"), "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), pad);
    }

    #[test]
    fn a_name_typed_in_the_tui_is_for_its_group_when_a_browser_names_another() {
        let mut h = Harness::new(TWO_GROUPS_AND_LANE);
        // The TUI is naming the second group, and a browser names the first.
        h.goto(TreeAddress::Group(1));
        h.press(KeyCode::Char('R')).type_chars("!");
        assert_eq!(h.s.rename_group.as_ref().map(|rg| rg.input.value.as_str()), Some("Lead!"));
        let pad = h.s.ctl.model.groups[0].node;
        browser(&mut h, Intent::RenameGroup { node: pad, name: "Keys".to_string() });
        // A name changes no place: what is being typed stays, and goes to the group it was typed for.
        assert_eq!(h.s.rename_group.as_ref().map(|rg| rg.input.value.as_str()), Some("Lead!"));
        h.press(KeyCode::Enter);
        let names: Vec<_> = h.s.ctl.model.groups.iter().map(|g| g.name.clone()).collect();
        assert_eq!(names, [Some("Keys".to_string()), Some("Lead!".to_string())]);
    }

    #[test]
    fn a_group_a_browser_takes_out_leaves_its_lanes_in_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let pad = h.s.ctl.model.groups[0].node;
        browser(&mut h, Intent::RemoveNode { node: pad });
        assert!(h.s.ctl.model.groups.is_empty());
        assert_eq!(h.s.ctl.model.instruments.len(), 2, "the lanes stay");
        assert!(h.s.ctl.model.instruments.iter().all(|lane| lane.group.is_none()));
        assert!(headers(&h).is_empty(), "{:#?}", h.tree());
        assert_eq!(ungrouped_lanes(&h), 2, "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 1, index: 0 });
    }

    #[test]
    fn the_cursor_is_on_a_node_of_the_tree_when_a_browser_takes_out_the_group_it_was_on() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Group(0));
        let pad = cursor_node(&h);
        browser(&mut h, Intent::RemoveNode { node: pad });
        assert!(h.s.chain_state.selected < h.s.tree_entries.len(), "{:#?}", h.tree());
        assert_ne!(cursor_node(&h), pad);
        // The frame is drawn from what is there, and the keys still do what they do.
        assert!(!h.dump().is_empty());
        h.press(KeyCode::Down).press(KeyCode::Up).press(KeyCode::Enter).press(KeyCode::Esc);
        assert!(h.s.chain_state.selected < h.s.tree_entries.len());
    }

    #[test]
    fn a_lane_a_browser_takes_out_is_gone_from_the_tree_and_the_cursor_stays_on_its_node() {
        let mut h = Harness::new(GROUP_AND_LANE);
        h.goto(TreeAddress::Effect { inst: 1, index: 0 });
        let reverb = cursor_node(&h);
        let lane = h.s.ctl.model.instruments[0].node;
        browser(&mut h, Intent::RemoveNode { node: lane });
        assert_eq!(h.s.ctl.model.instruments.len(), 1);
        // The lane that was in the group is gone, and the group is empty; the modulator that heard the lane stays, hearing every note, and is the first of the tree.
        assert_eq!(headers(&h).len(), 1, "{:#?}", h.tree());
        assert_eq!(h.tree()[0], "~ LFO 1.0Hz sine  (free)", "{:#?}", h.tree());
        assert_eq!(cursor_node(&h), reverb, "{:#?}", h.tree());
        assert_eq!(h.s.tree_entries[h.s.chain_state.selected].address, TreeAddress::Effect { inst: 0, index: 0 });
    }
}

mod panic {
    //! Ctrl+P: every note that sounds is let go of (`NoteInput::panic`), which the browser's button does too.

    use super::*;

    #[test]
    fn the_help_tab_lists_it() {
        let mut h = Harness::new(scene_bus_pair());
        h.press(KeyCode::Char('5'));
        let frame = h.dump();
        assert!(frame.contains("Ctrl+P     Panic: let go of every note"), "{frame}");
    }

    #[test]
    fn ctrl_p_lets_go_of_the_held_notes_and_sends_all_notes_off_and_all_sound_off_on_every_channel() {
        let mut h = Harness::new(scene_bus_pair());
        let held = h.s.ctl.note_input().held();
        held.note_on(60);
        h.press_with(KeyCode::Char('p'), KeyModifiers::CONTROL);
        let sent = h.notes();
        // The pedal up on 16 channels, the one note, all notes off on 16 and all sound off on 16.
        assert_eq!(sent.len(), 16 + 1 + 16 + 16, "{sent:?}");
        assert_eq!(sent[16], crate::source::MidiEvent::from_source(crate::source::Source::BROADCAST, 0, [0x80, 60, 0]), "no source's: every lane hears a panic");
        assert!(sent.iter().filter(|e| e.bytes[1] == 123).count() == 16 && sent.iter().filter(|e| e.bytes[1] == 120).count() == 16);
        assert!(held.held().is_empty(), "and nothing is held");
        assert!(h.commands().is_empty(), "it asks nothing of the engine's lanes: it is notes, and not the session");
    }

    #[test]
    fn ctrl_p_is_heard_on_every_tab_the_piano_tab_that_takes_the_letters_too() {
        let mut h = Harness::new(scene_bus_pair());
        for tab in 0..TAB_NAMES.len() {
            h.s.active_tab = tab;
            h.press_with(KeyCode::Char('p'), KeyModifiers::CONTROL);
            assert_eq!(h.notes().len(), 16 + 16 + 16, "tab {tab}");
        }
    }

    #[test]
    fn a_popup_has_the_keys_while_it_is_open_and_ctrl_p_is_not_heard_in_it() {
        let mut h = Harness::new(scene_bus_pair());
        // The tempo's popup: a letter with Control held is none of its text, and the panic is not heard under it.
        h.press(KeyCode::Char('b'));
        h.press_with(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(h.notes().is_empty());
        h.press(KeyCode::Esc);
        h.press_with(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(h.notes().len(), 16 + 16 + 16, "and it is heard when the popup is closed");
    }

    #[test]
    fn a_plain_p_is_still_the_preset_selector_and_makes_no_sound_of_its_own() {
        let mut h = Harness::new(scene_bus_pair());
        h.press(KeyCode::Char('p'));
        assert!(h.notes().is_empty());
    }
}

mod undo_and_redo {
    //! Ctrl+Z and Ctrl+Y: the controller's history (`Controller::undo` / `redo`), which the browser's buttons ask for too.

    use super::*;
    use crate::app::intent::Intent;

    fn ctrl_z(h: &mut Harness) {
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    }

    fn ctrl_y(h: &mut Harness) {
        h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
    }

    fn cutoff_of_the_bus_filter(h: &Harness) -> f32 {
        h.s.ctl.model.groups[0].effects[0].params[0].value
    }

    #[test]
    fn the_help_tab_lists_the_keys() {
        let mut h = Harness::new(scene_bus_pair());
        h.press(KeyCode::Char('5'));
        let frame = h.dump();
        assert!(frame.contains("Ctrl+Z     Undo the last thing done"), "{frame}");
        assert!(frame.contains("Ctrl+Y     Redo (Ctrl+Shift+Z too)"), "{frame}");
    }

    #[test]
    fn ctrl_z_puts_back_what_a_key_set_and_ctrl_y_does_it_again_in_the_model_and_in_the_engine() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left); // the filter's cutoff, 5% down from fully open
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);

        h.s.ctl.dirty = false;
        ctrl_z(&mut h);
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0);
        assert!(h.s.ctl.dirty, "going back is a change of the session too");

        ctrl_y(&mut h);
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);
    }

    #[test]
    fn a_drag_of_keys_is_one_step_for_ctrl_z_as_it_is_for_the_browsers_undo() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press_n(KeyCode::Left, 4); // 20% down, in four presses a moment apart
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0, "all four are put back at once");
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=1.000"]);
    }

    #[test]
    fn ctrl_shift_z_redoes_as_ctrl_y_does_and_a_capital_z_is_the_same_key() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        ctrl_z(&mut h);
        h.commands();
        h.press_with(KeyCode::Char('z'), ctrl_shift());
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
        ctrl_z(&mut h);
        h.commands();
        h.press_with(KeyCode::Char('Z'), ctrl_shift());
        assert_eq!(h.commands(), ["SetGroupParameter group=0 fx=0 param=0 value=0.950"]);
    }

    #[test]
    fn a_z_and_a_y_without_control_are_no_undo_and_no_redo() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        ctrl_z(&mut h);
        h.commands();
        // A key of the piano on its own tab, and a letter on the others: what was undone is not done again, and the other way round.
        for tab in ['1', '2'] {
            h.press(KeyCode::Char(tab));
            h.press(KeyCode::Char('y')).press(KeyCode::Char('z')).press_with(KeyCode::Char('Z'), KeyModifiers::SHIFT);
        }
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0);
        ctrl_y(&mut h);
        h.commands();
        h.press(KeyCode::Char('1')).press(KeyCode::Char('z')).press(KeyCode::Char('y'));
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);
    }

    #[test]
    fn ctrl_z_and_ctrl_y_with_nothing_to_undo_or_redo_do_nothing() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.s.ctl.dirty = false;
        ctrl_z(&mut h);
        ctrl_y(&mut h);
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 1.0);
        assert!(!h.s.ctl.dirty);
    }

    #[test]
    fn ctrl_z_puts_back_the_keys_of_a_lane_and_the_tree_says_so() {
        let mut h = Harness::new(scene_split());
        let before = h.tree()[0].to_string();
        h.press(KeyCode::Char('R')).press_n(KeyCode::Backspace, 5).type_chars("C2-C6").press(KeyCode::Enter);
        assert_eq!(h.tree()[0], "♪ Oscillator  [Built-in]  C2-C6");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(h.tree()[0], before, "the label of the lane is what it was");
        assert_eq!(h.commands(), ["SetInstrumentRange inst=0 range=Some((12, 59))"], "C0-B3, as the scene has it");
        ctrl_y(&mut h);
        assert_eq!(h.tree()[0], "♪ Oscillator  [Built-in]  C2-C6");
    }

    #[test]
    fn ctrl_z_puts_back_what_a_browser_did_and_the_tree_follows_the_effects_back_to_their_places() {
        let mut h = Harness::new(scene_two_effects());
        let order = |h: &Harness| h.tree().iter().filter(|line| line.contains("fx ")).map(|line| line.to_string()).collect::<Vec<_>>();
        let before = order(&h);
        let filter = h.s.ctl.model.instruments[0].effects[0].node;
        // What a browser does: the intent is applied by the controller, and the loop of the TUI reacts to it.
        let applied = h.s.ctl.apply(Intent::MoveEffect { node: filter, index: 1 }).unwrap();
        h.s.react_to(&applied);
        assert_ne!(order(&h), before, "the effects are in the other order");
        ctrl_z(&mut h);
        assert_eq!(order(&h), before);
        ctrl_y(&mut h);
        assert_ne!(order(&h), before);
    }

    #[test]
    fn an_effect_added_from_the_selector_is_a_step_that_ctrl_z_takes_out_and_ctrl_y_puts_back() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.press(KeyCode::Char('a')).type_chars("reverb").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 plugin=Reverb mix=0.500"]);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Reverb"));
        ctrl_z(&mut h);
        assert_eq!(h.commands(), ["RemoveEffect inst=0 index=0 keep"]);
        assert!(h.tree().iter().all(|line| !line.contains("fx Reverb")), "{:#?}", h.tree());
        assert!(h.s.ctl.model.instruments[0].effects.is_empty());
        ctrl_y(&mut h);
        assert_eq!(h.commands(), ["InsertEffect inst=0 index=0 plugin=Reverb mix=0.500"]);
        assert!(h.tree().iter().any(|line| line.contains("fx Reverb")), "{:#?}", h.tree());
    }

    #[test]
    fn an_effect_that_stands_by_itself_is_a_step_that_ctrl_z_takes_out_with_its_lane_and_ctrl_y_puts_back() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let lanes = h.s.ctl.model.instruments.len();
        h.press(KeyCode::Char('N')).type_chars("reverb").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["AddInstrument range=None", "SetLaneRoute inst=2 route=none", "InsertEffect inst=2 index=0 plugin=Reverb mix=0.500"]);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Reverb"));
        ctrl_z(&mut h);
        assert_eq!(h.commands(), ["RemoveInstrument inst=2 keep"]);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes);
        assert!(h.tree().iter().all(|line| !line.contains("(empty)")), "{:#?}", h.tree());
        ctrl_y(&mut h);
        assert_eq!(h.commands(), ["AddInstrument range=None", "SetLaneRoute inst=2 route=none", "InsertEffect inst=2 index=0 plugin=Reverb mix=0.500"]);
        assert!(h.tree().iter().any(|line| line.contains("fx Reverb")), "{:#?}", h.tree());
    }

    #[test]
    fn an_effect_on_a_bus_added_from_the_selector_is_a_step_too() {
        let mut h = Harness::new(scene_group());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        h.goto(TreeAddress::Group(0));
        let buses = h.s.ctl.model.groups[0].effects.len();
        h.press(KeyCode::Char('a')).type_chars("filter").press(KeyCode::Enter);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Filter"));
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(h.commands(), [format!("RemoveGroupEffect group=0 index={buses} keep")]);
        assert_eq!(h.s.ctl.model.groups[0].effects.len(), buses);
        ctrl_y(&mut h);
        assert_eq!(h.commands(), [format!("InsertGroupEffect group=0 index={buses} plugin=Filter mix=1.000")]);
    }

    #[test]
    fn an_effect_that_cannot_be_added_leaves_what_was_done_before_to_be_undone() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        catalog_with_a_plugin_that_has_gone(&mut h);
        h.press(KeyCode::Esc).goto(TreeAddress::Group(0));
        h.press(KeyCode::Char('a')).type_chars("vanish").press(KeyCode::Enter);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("set cutoff"), "no step for what was refused, and nothing forgotten");
    }

    #[test]
    fn n_adds_a_lane_that_ctrl_z_takes_out_and_ctrl_y_puts_back() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let lanes = h.s.ctl.model.instruments.len();
        h.press(KeyCode::Char('n')).press(KeyCode::Esc); // (the selector is dismissed, and the lane stays with its oscillator)
        assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Oscillator as an instrument"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes);
        assert!(h.tree().iter().filter(|line| line.contains("♪ Oscillator")).count() <= lanes, "{:#?}", h.tree());
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
        assert!(h.s.ctl.model.instruments[lanes].instrument.is_some(), "with its oscillator");
    }

    #[test]
    fn n_with_nothing_in_the_catalog_that_names_the_oscillator_adds_no_lane() {
        let mut h = Harness::new(scene_split());
        let lanes = h.s.ctl.model.instruments.len();
        h.press(KeyCode::Char('n'));
        assert_eq!(h.s.ctl.model.instruments.len(), lanes, "a lane with nothing in it would make no sound: the catalog has to name what goes in");
        assert!(h.s.selector.is_none());
        assert_eq!(h.s.ctl.history_state().undo, None);
    }

    #[test]
    fn the_modulate_popup_with_a_new_lfo_is_one_step_that_ctrl_z_takes_out_whole_and_ctrl_y_puts_back() {
        let mut h = on_the_bass_filters_cutoff();
        let before = h.s.ctl.model.graph();
        h.press(KeyCode::Char('m')).press(KeyCode::Enter);
        let after = h.s.ctl.model.graph();
        assert_ne!(before, after);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("modulate cutoff"), "the LFO and the cable to it are one thing");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.graph(), before);
        assert_eq!(h.s.ctl.model.free_modulators.len(), 3, "the three that the scene has");
        assert_eq!(h.commands().len(), 1, "the LFO goes, and the cable with it (the audio thread drops a modulator with its cables)");
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.graph(), after);
        assert_eq!(h.commands().len(), 3, "the LFO is made again, hearing the lane it did, and the cable to it");
    }

    #[test]
    fn the_modulate_popup_with_a_modulator_that_is_there_is_a_step_of_a_cable() {
        let mut h = on_the_bass_filters_cutoff();
        h.press(KeyCode::Down); // the filter's resonance
        h.press(KeyCode::Char('m')).press_n(KeyCode::Down, NEW_MODULATORS.len()).press(KeyCode::Enter);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("draw a cable"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), 1, "the cable of the LFO that was there");
    }

    #[test]
    fn learning_a_parameter_is_one_step_and_ctrl_z_takes_the_armed_modulator_out_with_its_cable() {
        let mut h = on_the_bass_filters_cutoff();
        let before = h.s.ctl.model.graph();
        h.press(KeyCode::Char('l'));
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("learn cutoff"));
        let after = h.s.ctl.model.graph();
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.graph(), before);
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.graph(), after);
        let armed = &h.s.ctl.model.free_modulators[3];
        assert_eq!(armed.targets[0].depth, 1.0, "at full depth, as it was learned");
    }

    #[test]
    fn the_target_selector_makes_a_step_of_the_cable_it_draws() {
        let mut h = Harness::new(scene_group());
        h.goto(TreeAddress::FreeModulator { index: 0 });
        let before = h.s.ctl.model.free_modulators[0].targets.len();
        h.press(KeyCode::Char('t')).press(KeyCode::Enter);
        assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), before + 1);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("draw a cable"));
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.free_modulators[0].targets.len(), before);
    }

    #[test]
    fn m_on_an_instrument_adds_an_lfo_that_ctrl_z_takes_out_and_ctrl_y_makes_again_hearing_the_lane() {
        let mut h = Harness::new(scene_split());
        h.goto(TreeAddress::Instrument(1));
        let (before, lane) = (h.s.ctl.model.free_modulators.len(), h.s.ctl.model.instruments[1].node);
        h.press(KeyCode::Char('m'));
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add an LFO"));
        assert_eq!(h.s.ctl.model.free_modulators.len(), before + 1);
        ctrl_z(&mut h);
        assert_eq!(h.s.ctl.model.free_modulators.len(), before);
        ctrl_y(&mut h);
        assert_eq!((h.s.ctl.model.free_modulators.len(), h.s.ctl.model.free_modulators[before].ear), (before + 1, Some(lane)));
    }

    #[test]
    fn shift_down_on_an_effect_is_a_step_that_ctrl_z_puts_back_and_ctrl_y_does_again() {
        let mut h = Harness::new(scene_two_effects());
        let names = |h: &Harness| h.s.ctl.model.instruments[0].effects.iter().map(|fx| fx.name.clone()).collect::<Vec<_>>();
        let before = names(&h);
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
        let moved = names(&h);
        assert_ne!(moved, before, "the effects are in the other order");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("move an effect"));
        ctrl_z(&mut h);
        assert_eq!(names(&h), before);
        ctrl_y(&mut h);
        assert_eq!(names(&h), moved);
    }

    #[test]
    fn shift_up_on_a_bus_effect_is_a_step_too() {
        let mut h = Harness::new(scene_bus_pair());
        // A third effect on the bus, so that one place up is not the first place.
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let group = h.s.ctl.model.groups[0].node;
        h.s.ctl.apply(Intent::AddEffect { parent: group, plugin: "builtin:reverb".to_string() }).unwrap();
        h.s.rebuild_tree();
        let order = |h: &Harness| h.s.ctl.model.groups[0].effects.iter().map(|fx| fx.node).collect::<Vec<_>>();
        let before = order(&h);
        h.goto(TreeAddress::GroupEffect { group: 0, index: 2 });
        h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
        let moved = order(&h);
        assert_eq!(moved, [before[0], before[2], before[1]], "one place up");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("move an effect"));
        ctrl_z(&mut h);
        assert_eq!(order(&h), before);
        ctrl_y(&mut h);
        assert_eq!(order(&h), moved);
    }

    #[test]
    fn shift_up_on_the_first_effect_and_shift_down_on_the_last_move_nothing_and_leave_no_step() {
        let mut h = Harness::new(scene_two_effects());
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press_with(KeyCode::Up, KeyModifiers::SHIFT);
        h.goto(TreeAddress::Effect { inst: 0, index: 1 });
        h.press_with(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(h.s.ctl.history_state().undo, None);
        assert!(h.commands().is_empty());
    }

    #[test]
    fn d_takes_a_modulator_out_as_a_step_that_ctrl_z_puts_back_with_its_cable_and_ctrl_y_takes_out_again() {
        let cases = [
            (scene_two_effects().to_string(), TreeAddress::FreeModulator { index: 0 }, "a lane's modulator", "InsertFreeModulator index=0 "),
            (scene_bus_pair().to_string(), TreeAddress::FreeModulator { index: 0 }, "a group's modulator", "InsertFreeModulator index=0 "),
        ];
        for (scene, address, what, insert) in cases {
            let mut h = Harness::new(scene);
            h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            let shape = |h: &Harness| {
                h.s.ctl.model.free_modulators.iter().map(|m| (m.node, m.targets.iter().map(|t| (t.edge, t.depth)).collect::<Vec<_>>())).collect::<Vec<_>>()
            };
            let before = shape(&h);
            h.commands();
            h.goto(address).press(KeyCode::Char('d'));
            assert_ne!(shape(&h), before, "{what} is gone");
            assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove an LFO"), "{what}");
            h.commands();
            ctrl_z(&mut h);
            let commands = h.commands();
            assert!(commands[0].starts_with(insert), "{what}: {commands:?}");
            assert_eq!(shape(&h), before, "{what} is back where it was, with its cable and the ids they had");
            ctrl_y(&mut h);
            assert_ne!(shape(&h), before, "{what} is taken out again");
            ctrl_z(&mut h);
            assert_eq!(shape(&h), before);
        }
    }

    #[test]
    fn d_takes_an_effect_out_as_a_step_that_ctrl_z_puts_back_in_its_place_with_its_cables_and_ctrl_y_takes_out_again() {
        let cases = [
            (scene_two_effects().to_string(), TreeAddress::Effect { inst: 0, index: 0 }, "an effect of a lane", "InsertEffect inst=0 index=0 plugin=Filter"),
            (scene_bus_pair().to_string(), TreeAddress::GroupEffect { group: 0, index: 0 }, "an effect of a bus", "InsertGroupEffect group=0 index=0 plugin=Filter"),
        ];
        for (scene, address, what, insert) in cases {
            let mut h = Harness::new(scene);
            h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
            // Every chain and every cable, with the ids and slots they have.
            let shape = |h: &Harness| -> String {
                let lanes = h.s.ctl.model.instruments.iter().map(|lane| format!("{:?}", lane.effects));
                let groups = h.s.ctl.model.groups.iter().map(|group| format!("{:?}", group.effects));
                lanes.chain(groups).chain(std::iter::once(format!("{:?}", h.s.ctl.model.free_modulators))).collect::<Vec<_>>().join("\n")
            };
            let before = shape(&h);
            h.commands();
            h.goto(address).press(KeyCode::Char('d'));
            assert_ne!(shape(&h), before, "{what} is gone");
            assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove Filter"), "{what}");
            h.commands();
            ctrl_z(&mut h);
            let commands = h.commands();
            assert!(commands[0].starts_with(insert), "{what}: {commands:?}");
            assert_eq!(shape(&h), before, "{what} is back where it was, with its cables and the ids and slots they had");
            ctrl_y(&mut h);
            assert_ne!(shape(&h), before, "{what} is taken out again");
            ctrl_z(&mut h);
            assert_eq!(shape(&h), before);
        }
    }

    #[test]
    fn d_takes_the_instrument_out_of_its_lane_as_a_step_that_ctrl_z_puts_back_with_its_cable_and_ctrl_y_takes_out_again() {
        let mut h = Harness::new(scene_split());
        // The first lane, with its instrument (its values among them) and the cable of its LFO to it, as they are: what an instrument that is put back has to leave as it was.
        let shape = |h: &Harness| format!("{:?}", h.s.ctl.model.instruments[0]);
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[2].value = 0.25; // volume, edited
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('d'));
        assert!(h.s.ctl.model.instruments[0].instrument.is_none(), "the lane is empty, and has the rest of what it had");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("take Oscillator out of its lane"));
        // (The cable of the LFO to the instrument goes first, and the instrument is asked for by a channel of its own: the history keeps it.)
        assert_eq!(h.commands(), ["RemoveFreeModTarget mod=0 target=0", "ClearInstrument inst=0 keep"]);
        ctrl_z(&mut h);
        let commands = h.commands();
        // (The audio thread, which the test has not, would have handed the instrument back: it is loaded again from the model, with the value that was set.)
        assert_eq!(commands[0], "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false", "{commands:?}");
        assert!(commands[1].starts_with("InsertFreeModTarget mod=0 target=0"), "the cable comes back after the instrument: {commands:?}");
        assert_eq!(shape(&h), before, "the instrument is back, with the value that was set and the cable that drove it");
        ctrl_y(&mut h);
        assert!(h.s.ctl.model.instruments[0].instrument.is_none(), "taken out again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn d_takes_a_lane_out_as_a_step_that_ctrl_z_puts_back_in_its_place_in_its_group_with_its_cables_and_ctrl_y_takes_out_again() {
        let mut h = Harness::new(scene_group());
        // Every lane and group, with the ids and slots they have: what a lane put back has to leave as it was.
        let shape = |h: &Harness| -> String {
            let lanes = h.s.ctl.model.instruments.iter().map(|lane| format!("{lane:?}"));
            let groups = h.s.ctl.model.groups.iter().map(|group| format!("{group:?}"));
            lanes.chain(groups).collect::<Vec<_>>().join("\n")
        };
        // The first key empties the lane, which nothing puts back; the second takes the lane out.
        h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('d'));
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let (lanes, before) = (h.s.ctl.model.instruments.len(), shape(&h));
        h.commands();
        h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('d'));
        assert_eq!(h.s.ctl.model.instruments.len(), lanes - 1);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove an empty lane"));
        // (The commands are read: the audio thread, which the test has not, would have handed the lane back.)
        let removed = h.commands();
        assert_eq!(removed.last().map(String::as_str), Some("RemoveInstrument inst=0 keep"), "{removed:?}");
        ctrl_z(&mut h);
        let commands = h.commands();
        assert!(commands.iter().any(|command| command.starts_with("InsertInstrument index=0 plugins=")), "{commands:?}");
        assert_eq!(shape(&h), before, "the lane is back where it was, in its group, with its cables and the ids and slots they had");
        ctrl_y(&mut h);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes - 1, "taken out again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn d_takes_a_group_out_as_a_step_that_ctrl_z_puts_back_with_its_members_and_cables_and_ctrl_y_takes_out_again() {
        let mut h = Harness::new(scene_group());
        // Every lane and group, with the ids and slots they have: what a group put back has to leave as it was.
        let shape = |h: &Harness| -> String {
            let lanes = h.s.ctl.model.instruments.iter().map(|lane| format!("{lane:?}"));
            let groups = h.s.ctl.model.groups.iter().map(|group| format!("{group:?}"));
            lanes.chain(groups).collect::<Vec<_>>().join("\n")
        };
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Group(0)).press(KeyCode::Char('d'));
        assert!(h.s.ctl.model.groups.is_empty(), "the group is gone");
        assert!(h.s.ctl.model.instruments.iter().all(|lane| lane.group.is_none()), "and its members are in none");
        let name = h.s.ctl.history_state().undo;
        assert!(name.as_deref().is_some_and(|name| name.starts_with("remove ")), "{name:?}");
        // (The commands are read: the audio thread, which the test has not, would have handed the group back.)
        let removed = h.commands();
        assert!(removed.iter().any(|command| command == "RemoveGroup group=0 keep"), "{removed:?}");
        ctrl_z(&mut h);
        let commands = h.commands();
        assert!(commands.iter().any(|command| command.starts_with("InsertGroup index=0 plugins=")), "{commands:?}");
        assert_eq!(shape(&h), before, "the group is back where it was, with its members, its cables and the ids and slots they had");
        ctrl_y(&mut h);
        assert!(h.s.ctl.model.groups.is_empty(), "taken out again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn choosing_a_preset_is_a_step_that_ctrl_z_puts_back_with_the_preset_and_the_values_the_reverb_had_and_ctrl_y_loads_again() {
        let mut h = Harness::new(PRESET_SCENE);
        // The reverb, with the preset it has and a value set over it, as they are: what a preset that is put back has to leave as it was.
        let reverb = h.s.ctl.model.instruments[0].effects[0].node;
        h.s.ctl.apply(Intent::SetParam { node: reverb, param: 2, value: 0.9 }).unwrap();
        let shape = |h: &Harness| format!("{:?}", h.s.ctl.model.instruments[0].effects[0]);
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Effect { inst: 0, index: 0 });
        h.press(KeyCode::Char('p')).type_chars("cathed").press(KeyCode::Enter);
        assert_eq!(h.commands(), ["LoadPreset inst=0 slot=1 id=cathedral"]);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("load preset Cathedral"));
        let loaded = shape(&h);
        assert_ne!(loaded, before);
        ctrl_z(&mut h);
        let commands = h.commands();
        // The preset it had is loaded again, and then the values: the ten of the reverb, the one that was set among them.
        assert_eq!(commands.first().map(String::as_str), Some("LoadPreset inst=0 slot=1 id=plate"), "{commands:?}");
        assert!(commands.contains(&"SetParameter inst=0 slot=1 param=2 value=0.900".to_string()), "{commands:?}");
        assert_eq!(shape(&h), before, "the preset and the values the reverb had, and what they are compared with");
        ctrl_y(&mut h);
        assert_eq!(shape(&h), loaded, "loaded again");
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn i_replaces_the_instrument_as_a_step_that_ctrl_z_puts_back_with_its_cable_and_ctrl_y_replaces_again() {
        let mut h = Harness::new(scene_split());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        // The first lane, with its instrument (its values among them) and the cable of its LFO to it, as they are: what a replacement that is put back has to leave as it was.
        let shape = |h: &Harness| format!("{:?}", h.s.ctl.model.instruments[0]);
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        h.s.ctl.model.instruments[0].instrument.as_mut().unwrap().params[2].value = 0.25; // volume, edited
        let before = shape(&h);
        h.commands();
        h.goto(TreeAddress::Instrument(0));
        // (The scene's lane has a stand-in for an instrument, which the list does not have: it starts at its first row, the audio input, so the oscillator is asked for by name.)
        h.press(KeyCode::Char('i')).type_chars("oscillator").press(KeyCode::Enter);
        assert_ne!(shape(&h), before, "another instrument is in the lane, and the cable is gone");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("change the instrument to Oscillator"));
        // (The commands are read: the audio thread, which the test has not, would have handed the old instrument back, so that it is loaded again from the model.)
        h.commands();
        ctrl_z(&mut h);
        let commands = h.commands();
        assert_eq!(commands[0], "SwapInstrument inst=0 plugin=Oscillator bufs=2 remap=false", "{commands:?}");
        assert!(commands[1].starts_with("InsertFreeModTarget mod=0 target=0"), "the cable comes back after the instrument: {commands:?}");
        assert_eq!(shape(&h), before, "the instrument is back, with the value that was set and the cable that drove it");
        ctrl_y(&mut h);
        assert_ne!(shape(&h), before, "replaced again");
        h.commands();
        ctrl_z(&mut h);
        assert_eq!(shape(&h), before);
    }

    #[test]
    fn a_lane_that_a_browser_takes_out_with_its_instrument_is_put_back_by_ctrl_z() {
        let mut h = Harness::new(scene_group());
        h.s.ctl.apply(Intent::SetLocked { locked: true }).unwrap();
        // The ungrouped bass, the last lane, with its instrument: taken out by an intent, as a browser asks for it.
        let bass = h.s.ctl.model.instruments[2].node;
        let before = format!("{:?}", h.s.ctl.model.instruments[2]);
        h.commands();
        h.s.ctl.apply(Intent::RemoveNode { node: bass }).unwrap();
        h.commands();
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("remove the lane of Oscillator"));
        ctrl_z(&mut h);
        assert_eq!(format!("{:?}", h.s.ctl.model.instruments[2]), before);
        assert!(h.commands().iter().any(|command| command == "InsertInstrument index=2 plugins=2"));
    }

    #[test]
    fn ctrl_z_is_not_heard_while_a_value_is_being_typed_and_does_not_undo_what_was_not_set() {
        let mut h = Harness::new(scene_bus_pair());
        h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
        h.press(KeyCode::Enter).press(KeyCode::Left);
        h.commands();
        // The value popup is open: a key goes to it, and what was set is not put back behind it.
        h.press(KeyCode::Enter);
        assert!(h.s.editing.is_some());
        ctrl_z(&mut h);
        assert!(h.commands().is_empty());
        assert_eq!(cutoff_of_the_bus_filter(&h), 0.95);
    }
}

mod control_keys_in_popups {
    //! A letter with Control held is no text (f104): a popup that takes text does not put it in as the letter (Ctrl+Z, which undoes in the chain, typed a z), and
    //! Ctrl+C and Ctrl+Q leave the program from a popup as from everywhere else.

    use super::*;

    /// A group with a lane in it, then a lane that is in none, which has a reverb (it has presets and parameters) and an LFO.
    const SCENE: &str = r#"
[[group]]
name = "Pad"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
rate = 2.0
"#;

    type Keys = &'static [(KeyCode, KeyModifiers)];
    type Text = fn(&State) -> Option<String>;
    type IsOpen = fn(&State) -> bool;

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const CTRL_SHIFT: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::SHIFT);
    /// How Windows reports AltGr.
    const ALT_GR: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

    /// Every popup that takes text: what it is, where the cursor is when it is opened, the keys that open it, and the text it holds (none if it is not open).
    fn popups_that_take_text() -> [(&'static str, TreeAddress, Keys, Text); 11] {
        let lane = TreeAddress::Instrument(1);
        let reverb = TreeAddress::Effect { inst: 1, index: 0 };
        [
            ("the range of a lane", lane, &[(KeyCode::Char('R'), NONE)], |s| s.range_edit.as_ref().map(|p| p.input.value.clone())),
            ("the name of a group", TreeAddress::Group(0), &[(KeyCode::Char('R'), NONE)], |s| s.rename_group.as_ref().map(|p| p.input.value.clone())),
            ("the selector of a plugin", lane, &[(KeyCode::Char('a'), NONE)], |s| s.selector.as_ref().map(|p| p.filter.input.value.clone())),
            ("the selector of targets", TreeAddress::FreeModulator { index: 0 }, &[(KeyCode::Char('t'), NONE)], |s| {
                s.target_selector.as_ref().map(|p| p.filter.input.value.clone())
            }),
            ("the selector of presets", reverb, &[(KeyCode::Char('p'), NONE)], |s| s.preset_selector.as_ref().map(|p| p.filter.input.value.clone())),
            ("the selector of scales", lane, &[(KeyCode::Char('2'), NONE), (KeyCode::Char('k'), NONE)], |s| {
                s.scale_selector.as_ref().map(|p| p.filter.input.value.clone())
            }),
            ("the tempo", lane, &[(KeyCode::Char('b'), NONE)], |s| s.bpm_editing.as_ref().map(|p| p.input.value.clone())),
            ("a volume", lane, &[(KeyCode::Char('v'), NONE)], |s| s.gain_editing.as_ref().map(|p| p.edit.input.value.clone())),
            ("the name of a file", lane, &[(KeyCode::Char('s'), CTRL_SHIFT)], |s| {
                s.save_as.as_ref().map(|p| p.input.value.clone())
            }),
            ("the value of a parameter", reverb, &[(KeyCode::Enter, NONE), (KeyCode::Enter, NONE)], |s| s.editing.as_ref().map(|p| p.input.value.clone())),
            ("the filter of parameters", reverb, &[(KeyCode::Enter, NONE), (KeyCode::Char('/'), NONE)], |s| {
                s.param_filtering.then(|| s.param_filter_input.value.clone())
            }),
        ]
    }

    /// The popups that take no text: the keys that open them and how to tell that they are open.
    fn popups_that_take_none() -> [(&'static str, TreeAddress, Keys, IsOpen); 2] {
        [
            ("the choice of a modulator", TreeAddress::Effect { inst: 1, index: 0 }, &[(KeyCode::Enter, NONE), (KeyCode::Char('m'), NONE)], |s| s.modulate.is_some()),
            ("the group of a lane", TreeAddress::Instrument(1), &[(KeyCode::Char('g'), NONE)], |s| s.group_assign.is_some()),
        ]
    }

    fn opened(at: TreeAddress, keys: Keys) -> Harness {
        let mut h = Harness::new(SCENE);
        h.goto(at);
        for (code, modifiers) in keys {
            h.press_with(*code, *modifiers);
        }
        h
    }

    #[test]
    fn a_letter_with_control_held_is_not_put_into_a_popup_that_takes_text_nor_does_it_reach_what_is_under_it() {
        for (what, at, keys, text) in popups_that_take_text() {
            let mut h = opened(at, keys);
            h.type_chars("ab");
            let typed = text(&h.s).unwrap_or_else(|| panic!("{what} is open"));
            assert!(typed.ends_with("ab"), "{what}: {typed:?}");
            h.commands();
            let dirty = h.s.ctl.dirty;
            // The keys of the chain under the popup (undo, redo, save) and some of a text field's own in other programs; Shift with them is a capital letter of the same key.
            for letter in ['z', 'y', 's', 'x', 'a', 'u', 'k', 'w'] {
                h.press_with(KeyCode::Char(letter), CTRL);
            }
            h.press_with(KeyCode::Char('Z'), CTRL_SHIFT);
            assert_eq!(text(&h.s).as_deref(), Some(typed.as_str()), "{what}: no chord is a letter");
            assert!(h.commands().is_empty(), "{what}: nor does a chord reach what is under it");
            assert_eq!(h.s.ctl.dirty, dirty, "{what}");
            assert!(!h.s.quit, "{what}");
        }
    }

    #[test]
    fn what_is_typed_with_shift_or_with_alt_gr_is_still_text_in_a_popup_that_takes_text() {
        // (A capital letter has Shift. Windows reports AltGr, which makes `@` and `\` on a Swedish keyboard, as Control and Alt together.)
        for (what, at, keys, text) in popups_that_take_text() {
            let mut h = opened(at, keys);
            let before = text(&h.s).unwrap_or_else(|| panic!("{what} is open"));
            h.press_with(KeyCode::Char('Q'), KeyModifiers::SHIFT);
            h.press_with(KeyCode::Char('@'), ALT_GR);
            h.press_with(KeyCode::Char('\\'), ALT_GR);
            h.press(KeyCode::Char('x'));
            assert_eq!(text(&h.s), Some(format!("{before}Q@\\x")), "{what}");
        }
    }

    #[test]
    fn ctrl_c_and_ctrl_q_leave_the_program_from_every_popup_as_they_do_from_the_chain() {
        for key in ['c', 'q'] {
            let mut h = Harness::new(SCENE);
            h.press_with(KeyCode::Char(key), CTRL);
            assert!(h.s.quit, "Ctrl+{key} from the chain");
            for (what, at, keys, _) in popups_that_take_text() {
                let mut h = opened(at, keys);
                assert!(!h.s.quit);
                h.press_with(KeyCode::Char(key), CTRL);
                assert!(h.s.quit, "Ctrl+{key} from {what}");
            }
            for (what, at, keys, is_open) in popups_that_take_none() {
                let mut h = opened(at, keys);
                assert!(is_open(&h.s), "{what} is open");
                h.press_with(KeyCode::Char(key), CTRL);
                assert!(h.s.quit, "Ctrl+{key} from {what}");
            }
        }
    }

    #[test]
    fn the_keys_of_a_text_field_that_have_control_held_but_are_not_letters_still_move_the_cursor() {
        // (Ctrl+Left and Ctrl+Backspace do what Left and Backspace do, as they did.)
        let mut h = opened(TreeAddress::Instrument(1), &[(KeyCode::Char('R'), NONE)]);
        h.press_n(KeyCode::Backspace, 20).type_chars("C2-C6");
        h.press_with(KeyCode::Left, CTRL).press_with(KeyCode::Backspace, CTRL);
        assert_eq!(h.s.range_edit.as_ref().map(|p| p.input.value.as_str()), Some("C2-6"));
    }
}

mod tree_colours {
    //! The chain tree says what an entry is by its colour (CLAUDE.md, "Chain (left pane)"): a group's header is cyan and a modulator magenta, the group's own
    //! included. Entries that are not selected are dimmed, the selected one is bold; the others have no colour of their own (f01).

    use super::*;

    const SCENE: &str = r#"
[[group]]
name = "Pad"
[[group.modulator]]
rate = 0.5

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.modulator]]
rate = 1.0

[[instrument]]
plugin = "builtin:osc"
[[instrument.effect]]
plugin = "builtin:reverb"
"#;

    /// The foreground and the modifiers of the first cell of `needle` in the chain pane (the left 42% of the screen: the other pane names the modulator it shows).
    fn style_of(h: &mut Harness, needle: &str) -> (Color, Modifier) {
        h.dump();
        let buf = h.terminal.backend().buffer();
        let chain_width = buf.area.width * 42 / 100;
        for y in 0..buf.area.height {
            let row: String = (0..chain_width).map(|x| buf[(x, y)].symbol()).collect();
            if let Some(at) = row.find(needle) {
                let cell = &buf[(row[..at].chars().count() as u16, y)];
                return (cell.fg, cell.modifier);
            }
        }
        panic!("{needle:?} is not on the screen");
    }

    /// Each kind of entry, the place of its entry in the tree, a piece of its label, and the colour it has (none for what has none of its own).
    fn entries() -> [(&'static str, TreeAddress, &'static str, Option<Color>); 5] {
        [
            ("a group's header", TreeAddress::Group(0), "▦ Pad", Some(Color::Cyan)),
            ("a lane's modulator", TreeAddress::FreeModulator { index: 0 }, "LFO 1.0Hz", Some(Color::Magenta)),
            ("a group's modulator", TreeAddress::FreeModulator { index: 1 }, "LFO 0.5Hz", Some(Color::Magenta)),
            ("an instrument", TreeAddress::Instrument(0), "♪ Oscillator  [Built-in]", None),
            ("an effect", TreeAddress::Effect { inst: 1, index: 0 }, "fx Reverb", None),
        ]
    }

    #[test]
    fn a_group_header_and_a_modulator_have_their_colour_and_are_dimmed_until_selected() {
        for (what, at, label, colour) in entries() {
            let mut h = Harness::new(SCENE);
            // (The cursor is on the last entry, the effect, unless that is the one looked at.)
            let effect = TreeAddress::Effect { inst: 1, index: 0 };
            h.goto(if at == effect { TreeAddress::Instrument(1) } else { effect });
            let (fg, modifier) = style_of(&mut h, label);
            match colour {
                Some(colour) => assert_eq!((fg, modifier), (colour, Modifier::DIM), "{what} is dimmed, not selected"),
                None => assert_eq!((fg, modifier), (DIM, Modifier::empty()), "{what} is as it was, not selected"),
            }
            h.goto(at);
            let (fg, modifier) = style_of(&mut h, label);
            match colour {
                Some(colour) => assert_eq!((fg, modifier), (colour, Modifier::BOLD), "{what} is bold, selected"),
                None => assert_eq!((fg, modifier), (Color::White, Modifier::BOLD), "{what} is as it was, selected"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mute and solo
// ---------------------------------------------------------------------------

#[test]
fn m_and_s_mute_and_solo_the_lane_that_the_cursor_is_in_and_the_tree_says_so() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('M'));
    assert_eq!(h.commands(), ["SetMute inst=2 muted=true"]);
    assert!(h.s.ctl.model.instruments[2].muted && h.s.ctl.dirty);
    h.press(KeyCode::Char('S'));
    assert_eq!(h.commands(), ["SetSolo inst=2 solo=true"]);
    assert!(h.tree().iter().any(|label| label.starts_with("♪ Oscillator  [Built-in]") && label.ends_with("[mute] [solo]")), "{:?}", h.tree());
    // The keys toggle: pressed again they bring it back.
    h.press(KeyCode::Char('M')).press(KeyCode::Char('S'));
    assert_eq!(h.commands(), ["SetMute inst=2 muted=false", "SetSolo inst=2 solo=false"]);
    assert!(h.tree().iter().all(|label| !label.contains("[mute]") && !label.contains("[solo]")), "{:?}", h.tree());
    // And from anything of the lane's: its effect, its modulator, its pattern line.
    h.goto(TreeAddress::Effect { inst: 2, index: 0 }).press(KeyCode::Char('M'));
    assert_eq!(h.commands(), ["SetMute inst=2 muted=true"]);
    h.goto(TreeAddress::FreeModulator { index: 0 }).press(KeyCode::Char('S'));
    assert_eq!(h.commands(), ["SetSolo inst=2 solo=true"]);
}

#[test]
fn m_and_s_mute_and_solo_the_group_that_the_cursor_is_in_and_the_header_says_so() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('M'));
    assert_eq!(h.commands(), ["SetGroupMute group=0 muted=true"]);
    assert!(h.tree()[0].starts_with("▦ Pad") && h.tree()[0].ends_with("[mute]"), "{:?}", h.tree()[0]);
    // From the group's bus effect and its modulator as well.
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 }).press(KeyCode::Char('S'));
    assert_eq!(h.commands(), ["SetGroupSolo group=0 solo=true"]);
    assert!(h.tree()[0].ends_with("[mute] [solo]"), "{:?}", h.tree()[0]);
    let group_lfo = group_modulator(&h, 0, 0);
    h.goto(group_lfo).press(KeyCode::Char('M'));
    assert_eq!(h.commands(), ["SetGroupMute group=0 muted=false"]);
    // (A member of the group is a lane of its own: its flags are not the group's.)
    h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('M'));
    assert_eq!(h.commands(), ["SetMute inst=0 muted=true"]);
    assert!(!h.s.ctl.model.groups[0].muted && h.s.ctl.model.groups[0].solo);
}

#[test]
fn m_and_s_are_steps_of_the_history_and_a_browsers_mute_shows_in_the_tree() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0));
    h.press(KeyCode::Char('M'));
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("mute the lane of Oscillator"));
    h.commands();
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetMute inst=0 muted=false"]);
    assert!(!h.s.ctl.model.instruments[0].muted);
    assert!(h.tree().iter().all(|label| !label.contains("[mute]")), "{:?}", h.tree());
    // What a browser asks of the controller is in the tree with the next key, as every other change of it is.
    let lane = h.s.ctl.model.instruments[0].node;
    let group = h.s.ctl.model.groups[0].node;
    let mut applied = vec![h.s.ctl.apply(Intent::SetSolo { node: lane, solo: true }).unwrap()];
    applied.push(h.s.ctl.apply(Intent::SetMute { node: group, muted: true }).unwrap());
    for one in &applied {
        h.s.react_to(one);
    }
    assert!(h.tree()[0].ends_with("[mute]"), "{:?}", h.tree()[0]);
    assert!(h.tree().iter().any(|label| label.starts_with("  ♪ Oscillator") && label.ends_with("[solo]")), "{:?}", h.tree());
}

// ---------------------------------------------------------------------------
// The way out of a lane and of a group
// ---------------------------------------------------------------------------

#[test]
fn w_cuts_and_gives_back_the_way_out_of_the_lane_that_the_cursor_is_in_and_the_tree_says_so() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2));
    h.press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetLaneRoute inst=2 route=none"]);
    assert_eq!(h.s.ctl.model.instruments[2].route, Route::None);
    assert!(h.tree().iter().any(|label| label.starts_with("♪ Oscillator  [Built-in]") && label.ends_with("  [no out]")), "{:?}", h.tree());
    // It is a flag among the others: after the mute and the solo.
    h.press(KeyCode::Char('M')).press(KeyCode::Char('S'));
    assert!(h.tree().iter().any(|label| label.starts_with("♪ Oscillator  [Built-in]") && label.ends_with("  [mute] [solo] [no out]")), "{:?}", h.tree());
    // The key toggles: pressed again it gives the way out back.
    h.commands();
    h.press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetLaneRoute inst=2 route=out"]);
    assert!(h.tree().iter().all(|label| !label.contains("[no out]")), "{:?}", h.tree());
    // And from anything of the lane's: its effect, its modulator.
    h.goto(TreeAddress::Effect { inst: 2, index: 0 }).press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetLaneRoute inst=2 route=none"]);
    h.goto(TreeAddress::FreeModulator { index: 0 }).press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetLaneRoute inst=2 route=out"]);
}

#[test]
fn w_cuts_the_way_out_of_the_group_that_the_cursor_is_in_and_the_header_says_so() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Group(0));
    h.press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetGroupRoute group=0 route=none"]);
    assert!(h.tree()[0].starts_with("▦ Pad") && h.tree()[0].ends_with("  [no out]"), "{:?}", h.tree()[0]);
    // From the group's bus effect and its modulator as well.
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 }).press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetGroupRoute group=0 route=out"]);
    let group_lfo = group_modulator(&h, 0, 0);
    h.goto(group_lfo).press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetGroupRoute group=0 route=none"]);
    // (A member of the group is a lane of its own: its way out is not the group's.)
    h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('w'));
    assert_eq!(h.commands(), ["SetLaneRoute inst=0 route=none"]);
    assert_eq!(h.s.ctl.model.groups[0].route, Route::None);
}

#[test]
fn w_is_a_step_of_the_history_and_a_way_out_that_would_make_the_audio_go_round_is_not_given_back() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(0)).press(KeyCode::Char('w'));
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("cut the way out of the lane of Oscillator"));
    h.commands();
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.commands(), ["SetLaneRoute inst=0 route=out"]);
    assert_eq!(h.s.ctl.model.instruments[0].route, Route::Out);
    // The group's output sent to the filter of a lane that is in the group is a loop (the lane feeds the group, which feeds the lane); with the lane's way out cut it is none,
    // and the key does not give the way out back.
    h.goto(TreeAddress::Instrument(1)).press(KeyCode::Char('w'));
    let (group, filter) = (h.s.ctl.model.groups[0].node, h.s.ctl.model.instruments[1].effects[0].node);
    let drawn = h.s.ctl.apply(Intent::AddCable {
        from: Endpoint { node: group, port: Port::AudioOut },
        to: Endpoint { node: filter, port: Port::AudioIn },
        gain: None,
    });
    assert!(drawn.is_ok(), "{drawn:?}");
    h.commands();
    h.press(KeyCode::Char('w'));
    assert_eq!(h.commands(), Vec::<String>::new(), "refused: the audio would go round");
    assert_eq!(h.s.ctl.model.instruments[1].route, Route::None);
}

#[test]
fn w_is_for_the_chain_and_the_patch_tab_and_a_way_out_that_a_file_cuts_is_in_the_tree() {
    let mut h = Harness::new(
        [
            "[[group]]\nname = \"Pad\"\nroute = \"none\"\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\nroute = \"none\"\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\n",
        ]
        .concat(),
    );
    assert!(h.tree()[0].starts_with("▦ Pad") && h.tree()[0].ends_with("[no out]"), "{:?}", h.tree());
    assert!(h.tree().iter().filter(|label| label.contains("[no out]")).count() == 2, "{:?}", h.tree());
    // Not on the other tabs (the Patch tab's is its own), and not in the parameter pane.
    h.goto(TreeAddress::Instrument(1)).press(KeyCode::Enter).press(KeyCode::Char('w'));
    assert_eq!(h.commands(), Vec::<String>::new());
    h.press(KeyCode::Esc);
    for tab in ['2', '3', '5'] {
        h.press(KeyCode::Char(tab)).press(KeyCode::Char('w'));
        assert_eq!(h.commands(), Vec::<String>::new(), "tab {tab}");
    }
    assert_eq!(h.s.ctl.model.instruments.iter().map(|lane| lane.route).collect::<Vec<_>>(), [Route::None, Route::Out]);
}

// ---------------------------------------------------------------------------
// The feed of an effect: the link into it from the stage before it (step 86)
// ---------------------------------------------------------------------------

#[test]
fn f_cuts_and_gives_back_the_feed_of_the_effect_that_the_cursor_is_on_and_the_tree_says_so() {
    let mut h = Harness::new(scene_two_effects());
    let (filter, reverb) = (h.s.ctl.model.instruments[0].effects[0].node.0, h.s.ctl.model.instruments[0].effects[1].node.0);
    h.goto(TreeAddress::Effect { inst: 0, index: 0 });
    h.press(KeyCode::Char('f'));
    assert_eq!(h.commands(), [format!("SetEffectFeed node={filter} fed=false")]);
    assert_eq!(h.s.ctl.model.instruments[0].effects.iter().map(|fx| fx.fed).collect::<Vec<_>>(), [false, true]);
    // The tree says it at the end of the effect's line, as it says a lane's way out is cut; the other effect has nothing.
    assert!(h.tree().iter().any(|label| label.contains("fx Filter") && label.ends_with("  [no in]")), "{:?}", h.tree());
    assert!(h.tree().iter().filter(|label| label.contains("[no in]")).count() == 1, "{:?}", h.tree());
    // The key toggles: pressed again it gives the feed back.
    h.press(KeyCode::Char('f'));
    assert_eq!(h.commands(), [format!("SetEffectFeed node={filter} fed=true")]);
    assert!(h.tree().iter().all(|label| !label.contains("[no in]")), "{:?}", h.tree());
    // The second effect is its own: its feed is the one that is cut, and the first is not touched.
    h.goto(TreeAddress::Effect { inst: 0, index: 1 }).press(KeyCode::Char('f'));
    assert_eq!(h.commands(), [format!("SetEffectFeed node={reverb} fed=false")]);
    assert!(h.tree().iter().any(|label| label.contains("fx Reverb") && label.ends_with("  [no in]")), "{:?}", h.tree());
}

#[test]
fn f_cuts_the_feed_of_a_bus_effect_and_the_line_says_so_after_what_says_it_is_the_bus() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::GroupEffect { group: 0, index: 0 });
    h.press(KeyCode::Char('f'));
    let bus = h.s.ctl.model.groups[0].effects[0].node.0;
    assert_eq!(h.commands(), [format!("SetEffectFeed node={bus} fed=false")]);
    assert!(h.tree().iter().any(|label| label.contains("fx Reverb") && label.ends_with("(bus)  [no in]")), "{:?}", h.tree());
    h.press(KeyCode::Char('f'));
    assert_eq!(h.commands(), [format!("SetEffectFeed node={bus} fed=true")]);
    assert!(h.tree().iter().all(|label| !label.contains("[no in]")), "{:?}", h.tree());
}

#[test]
fn f_is_for_an_effect_of_the_chain_only_and_a_feed_that_a_file_cuts_is_in_the_tree() {
    let mut h = Harness::new(
        [
            "[[group]]\nname = \"Pad\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\nfeed = \"none\"\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[instrument.effect]]\nplugin = \"builtin:filter\"\nfeed = \"none\"\n[[instrument.modulator]]\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\n",
        ]
        .concat(),
    );
    assert!(h.tree().iter().filter(|label| label.contains("[no in]")).count() == 2, "{:?}", h.tree());
    // A lane, a group, a modulator are no effects: they have no feed, and the key asks for nothing.
    for address in [TreeAddress::Group(0), TreeAddress::Instrument(0), TreeAddress::FreeModulator { index: 0 }, TreeAddress::Instrument(1)] {
        h.goto(address).press(KeyCode::Char('f'));
        assert_eq!(h.commands(), Vec::<String>::new(), "{address:?}");
    }
    // Not in the parameter pane, and not on the other tabs.
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Enter).press(KeyCode::Char('f'));
    assert_eq!(h.commands(), Vec::<String>::new());
    h.press(KeyCode::Esc);
    for tab in ['2', '3', '4', '5'] {
        h.press(KeyCode::Char(tab)).press(KeyCode::Char('f'));
        assert_eq!(h.commands(), Vec::<String>::new(), "tab {tab}");
    }
    assert_eq!((h.s.ctl.model.instruments[0].effects[0].fed, h.s.ctl.model.groups[0].effects[0].fed), (false, false));
}

#[test]
fn f_is_a_step_of_the_history_and_a_feed_that_would_make_the_audio_go_round_is_not_given_back() {
    let mut h = Harness::new(scene_two_effects());
    h.goto(TreeAddress::Effect { inst: 0, index: 0 }).press(KeyCode::Char('f'));
    assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("cut the input of Filter"));
    h.commands();
    h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(h.commands(), [format!("SetEffectFeed node={} fed=true", h.s.ctl.model.instruments[0].effects[0].node.0)]);
    assert!(h.s.ctl.model.instruments[0].effects[0].fed);
    // The reverb sent back to the filter before it is a loop through the link between them; with the reverb's feed cut it is none, and the key does not give the feed back.
    h.goto(TreeAddress::Effect { inst: 0, index: 1 }).press(KeyCode::Char('f'));
    let (filter, reverb) = (h.s.ctl.model.instruments[0].effects[0].node, h.s.ctl.model.instruments[0].effects[1].node);
    let drawn = h.s.ctl.apply(Intent::AddCable {
        from: Endpoint { node: reverb, port: Port::AudioOut },
        to: Endpoint { node: filter, port: Port::AudioIn },
        gain: None,
    });
    assert!(drawn.is_ok(), "{drawn:?}");
    h.commands();
    h.press(KeyCode::Char('f'));
    assert_eq!(h.commands(), Vec::<String>::new(), "refused: the audio would go round");
    assert!(!h.s.ctl.model.instruments[0].effects[1].fed);
}

#[test]
fn m_and_s_are_for_the_chain_and_not_for_the_parameter_pane() {
    let mut h = Harness::new(scene_group());
    h.goto(TreeAddress::Instrument(2)).press(KeyCode::Enter);
    assert!(h.s.focus_params);
    h.press(KeyCode::Char('M')).press(KeyCode::Char('S'));
    assert_eq!(h.commands(), Vec::<String>::new());
    assert!(!h.s.ctl.model.instruments[2].muted && !h.s.ctl.model.instruments[2].solo);
}

#[test]
fn m_and_s_are_the_session_tabs_and_the_patch_tabs_and_do_nothing_on_the_others() {
    let mut h = Harness::new(scene_group());
    for tab in ['2', '3', '5'] {
        h.press(KeyCode::Char(tab)).press(KeyCode::Char('M')).press(KeyCode::Char('S'));
        assert_eq!(h.commands(), Vec::<String>::new(), "tab {tab}");
    }
    assert!(h.s.ctl.model.instruments.iter().all(|lane| !lane.muted && !lane.solo));
}

#[test]
fn a_session_with_a_muted_lane_a_soloed_lane_and_a_muted_group_shows_it_in_the_tree_and_the_action_bar_has_the_keys() {
    let mut h = Harness::new(
        [
            "[[group]]\nname = \"Pad\"\nmute = true\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\nmute = true\nsolo = true\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\nsolo = true\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\nrange = \"-B3\"\nmute = true\n",
            "[[instrument]]\nplugin = \"builtin:osc\"\n",
        ]
        .concat(),
    );
    h.goto(TreeAddress::Instrument(2));
    assert_golden("session_muted_and_soloed", &mut h);
}

mod patch_tab {
    //! The Patch tab (P3e of `docs/patch-graph.md` §11.6): the audio cables as a matrix of outputs against inputs, drawn and turned with the keys through the intents that the page's cables are.

    use super::*;
    use crate::app::intent::Intent;
    use crate::app::testutil::{PATCHED, SENDS, Sends};
    use crate::plugin::chain::{CableFrom, CableTo};
    use crossterm::event::{MouseEvent, MouseEventKind};

    // The rows of `SENDS`: 0 the first lane's instrument, 1 its filter, 2 its delay, 3 the second lane's instrument, 4 its drive, 5 the group, 6 the reverb on its bus; and its columns: 0 the
    // filter, 1 the delay, 2 the drive, 3 the group's bus, 4 the reverb on it, 5 the mix.

    /// The Patch tab with the cursor on row `row` and column `col` of the matrix.
    fn at(h: &mut Harness, row: usize, col: usize) {
        h.press(KeyCode::Char('4'));
        h.press_n(KeyCode::Up, 60).press(KeyCode::Home);
        h.press_n(KeyCode::Down, row).press_n(KeyCode::Right, col);
    }

    /// The first line of the frame that has `needle` in it.
    fn line_of<'a>(frame: &'a str, needle: &str) -> &'a str {
        frame.lines().find(|line| line.contains(needle)).unwrap_or_else(|| panic!("no line has `{needle}`: {frame}"))
    }

    fn frame(h: &mut Harness) -> String {
        h.dump().split("── styles").next().unwrap().to_string()
    }

    fn click(h: &mut Harness, column: u16, row: u16) {
        process_event(&mut h.s, Event::Mouse(MouseEvent { kind: MouseEventKind::Down(crossterm::event::MouseButton::Left), column, row, modifiers: KeyModifiers::NONE }));
    }

    #[test]
    fn the_patch_tab_is_the_fourth_tab_and_help_the_fifth() {
        let mut h = Harness::new(SENDS);
        h.press(KeyCode::Char('4'));
        assert_eq!(h.s.active_tab, TAB_PATCH);
        assert!(frame(&mut h).contains("┌ Patch "));
        h.press(KeyCode::Char('5'));
        assert_eq!(h.s.active_tab, TAB_HELP);
        h.press(KeyCode::Char('1'));
        let mut seen = vec![h.s.active_tab];
        for _ in 0..TAB_NAMES.len() {
            h.press(KeyCode::Tab);
            seen.push(h.s.active_tab);
        }
        assert_eq!(seen, [TAB_SESSION, TAB_PIANO, TAB_SCOPE, TAB_PATCH, TAB_HELP, TAB_SESSION]);
        h.press(KeyCode::BackTab);
        assert_eq!(h.s.active_tab, TAB_HELP);
    }

    #[test]
    fn the_matrix_of_a_session_with_no_cables() {
        let mut h = Harness::new(SENDS);
        h.press(KeyCode::Char('4'));
        assert_golden("patch_tab_sends", &mut h);
    }

    #[test]
    fn the_matrix_of_a_session_with_cables_and_the_cursor_on_one_of_them() {
        let mut h = Harness::new(PATCHED);
        at(&mut h, 1, 1); // the cable from the first lane's filter to its delay, at 0.75
        assert_golden("patch_tab_patched", &mut h);
    }

    #[test]
    fn the_matrix_of_a_session_with_effects_that_have_a_second_input_has_a_column_for_each_input_and_a_cable_at_the_second_one() {
        use crate::app::testutil::{SIDED, Sided};
        let mut h = Harness::new(SIDED);
        let n = Sided::of(&h.s.ctl.model);
        h.press(KeyCode::Char('4'));
        // The file's cable goes from the second lane's instrument to the second input of lane 0's math effect: a level at the crosspoint of that row and that column, and not at the first input's.
        let drawn = frame(&mut h);
        assert!(line_of(&drawn, "L2  ♪ Oscillator").contains("1.00"), "{drawn}");
        assert!(drawn.contains("↳B"), "the second inputs are columns of their own: {drawn}");
        assert_golden("patch_tab_second_input", &mut h);
        // Enter at the second input of the group's math effect (the sixth column) from the first lane's instrument (the first row) draws a cable at that input, and not at the first, which is the
        // fifth; the foot says which of the two it joins, and Enter again takes it out.
        at(&mut h, 0, 5);
        assert!(frame(&mut h).contains("▸  G1 Math (bus, B)"), "{}", frame(&mut h));
        h.press(KeyCode::Enter);
        let cable = h.s.ctl.model.cables[1];
        assert_eq!((cable.from, cable.to, cable.gain), (CableFrom::Instrument(n.lane[0]), CableTo::EffectSide(n.bus_math), 1.0));
        assert_eq!(h.commands(), [format!("AddCable id={} from=Instrument({:?}) to=EffectSide({:?}) gain=1.000", cable.id.0, n.lane[0], n.bus_math)]);
        let drawn = frame(&mut h);
        assert!(line_of(&drawn, "L1  ♪ Oscillator").contains("1.00"), "{drawn}");
        h.press(KeyCode::Enter);
        assert_eq!(h.commands(), [format!("RemoveCable id={}", cable.id.0)]);
        assert_eq!(h.s.ctl.model.cables.len(), 1, "the file's cable is still there");
        // The first input of the same effect is another place, and takes a cable of its own beside the one at the second.
        at(&mut h, 0, 4);
        h.press(KeyCode::Enter);
        assert_eq!(h.s.ctl.model.cables[1].to, CableTo::Effect(n.bus_math));
        assert!(frame(&mut h).contains("▸  G1 Math (bus)"), "{}", frame(&mut h));
    }

    #[test]
    fn a_lane_that_is_muted_soloed_and_cut_says_so_in_the_first_columns_of_its_rows() {
        let mut h = Harness::new(SENDS);
        at(&mut h, 4, 0); // the drive of the second lane
        h.press(KeyCode::Char('w')).press(KeyCode::Char('M')).press(KeyCode::Char('S'));
        assert_eq!(h.commands(), ["SetLaneRoute inst=1 route=none", "SetMute inst=1 muted=true", "SetSolo inst=1 solo=true"]);
        assert_golden("patch_tab_muted_soloed_and_cut", &mut h);
    }

    #[test]
    fn moving_the_cursor_asks_nothing_of_the_engine() {
        let mut h = Harness::new(SENDS);
        at(&mut h, 6, 5);
        for key in [KeyCode::Up, KeyCode::Left, KeyCode::PageUp, KeyCode::Home, KeyCode::PageDown, KeyCode::End, KeyCode::Down, KeyCode::Right] {
            h.press(key);
        }
        assert_eq!(h.commands(), Vec::<String>::new());
        assert!(h.s.ctl.model.cables.is_empty() && !h.s.ctl.dirty);
    }

    #[test]
    fn enter_draws_a_cable_at_the_crosspoint_and_enter_again_takes_it_out() {
        let mut h = Harness::new(SENDS);
        let n = Sends::of(&h.s.ctl.model);
        at(&mut h, 3, 4); // the second lane's instrument, to the reverb on the group's bus
        h.press(KeyCode::Enter);
        let cable = h.s.ctl.model.cables[0];
        assert_eq!((cable.from, cable.to, cable.gain), (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus), 1.0));
        assert_eq!(h.commands(), [format!("AddCable id={} from=Instrument({:?}) to=Effect({:?}) gain=1.000", cable.id.0, n.lane[1], n.bus)]);
        assert!(h.s.ctl.dirty, "the session has a change that is not saved");
        let drawn = frame(&mut h);
        assert!(line_of(&drawn, "L2  ♪ Oscillator").contains("1.00"), "{drawn}");
        assert!(drawn.contains("A cable at level 1.00"), "{drawn}");
        h.press(KeyCode::Enter);
        assert_eq!(h.commands(), [format!("RemoveCable id={}", cable.id.0)]);
        assert!(h.s.ctl.model.cables.is_empty());
        let taken = frame(&mut h);
        assert!(!line_of(&taken, "L2  ♪ Oscillator").contains("1.00") && taken.contains("No cable: Enter draws one"), "{taken}");
        // Space does what Enter does, and d takes a cable out.
        h.press(KeyCode::Char(' '));
        assert_eq!(h.s.ctl.model.cables.len(), 1);
        h.commands();
        h.press(KeyCode::Char('d'));
        assert!(h.s.ctl.model.cables.is_empty());
        assert_eq!(h.commands().len(), 1);
    }

    #[test]
    fn two_oscillators_are_sent_to_one_reverb_and_undo_takes_the_last_cable_out_and_redo_draws_it_again() {
        let mut h = Harness::new(SENDS);
        let n = Sends::of(&h.s.ctl.model);
        at(&mut h, 0, 4);
        h.press(KeyCode::Enter);
        at(&mut h, 3, 4);
        h.press(KeyCode::Enter);
        let ends: Vec<_> = h.s.ctl.model.cables.iter().map(|cable| (cable.from, cable.to)).collect();
        assert_eq!(ends, [(CableFrom::Instrument(n.lane[0]), CableTo::Effect(n.bus)), (CableFrom::Instrument(n.lane[1]), CableTo::Effect(n.bus))]);
        let drawn = frame(&mut h);
        assert!(line_of(&drawn, "L1  ♪ Oscillator").contains("1.00") && line_of(&drawn, "L2  ♪ Oscillator").contains("1.00"), "{drawn}");
        h.commands();
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(h.s.ctl.model.cables.len(), 1, "the second cable is out");
        assert_eq!(h.commands().len(), 1);
        assert!(!line_of(&frame(&mut h), "L2  ♪ Oscillator").contains("1.00"));
        h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(h.s.ctl.model.cables.len(), 2, "and drawn again");
        assert!(h.commands()[0].starts_with("AddCable"));
    }

    #[test]
    fn plus_and_minus_turn_the_level_by_a_twentieth_and_the_frame_says_the_level_it_has() {
        let mut h = Harness::new(PATCHED);
        at(&mut h, 1, 1); // from the first lane's filter to its delay, at 0.75
        let id = h.s.ctl.model.cables[1].id;
        h.press(KeyCode::Char('-'));
        assert_eq!(h.commands(), [format!("SetCableGain id={} gain=0.700", id.0)]);
        assert!(line_of(&frame(&mut h), "L1    fx Filter").contains("0.70"));
        h.press(KeyCode::Char('+')).press(KeyCode::Char('+')).press(KeyCode::Char('+'));
        assert_eq!(h.commands(), [0.75, 0.8, 0.85].map(|level| format!("SetCableGain id={} gain={level:.3}", id.0)));
        assert!(frame(&mut h).contains("A cable at level 0.85"));
        assert!(h.s.ctl.dirty);
        // Where there is no cable they do nothing.
        at(&mut h, 0, 0);
        h.press(KeyCode::Char('+')).press(KeyCode::Char('-'));
        assert_eq!(h.commands(), Vec::<String>::new());
    }

    #[test]
    fn e_opens_a_popup_for_the_level_that_takes_a_number_and_is_the_popups_while_it_is_open() {
        let mut h = Harness::new(PATCHED);
        at(&mut h, 1, 1);
        let id = h.s.ctl.model.cables[1].id;
        h.press(KeyCode::Char('e'));
        assert!(h.s.cable_editing.is_some());
        let popup = frame(&mut h);
        assert!(popup.contains("level of the cable") && popup.contains("Range: 0.00 — 2.00") && popup.contains("Value: 0.75"), "{popup}");
        assert_golden("patch_tab_level_popup", &mut h);
        // The keys are text: the digits are not the tabs, and Ctrl+Z is not an undo.
        h.press_n(KeyCode::Backspace, 4).type_chars("0.25");
        assert_eq!(h.s.active_tab, TAB_PATCH);
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(h.s.cable_editing.is_some() && h.commands().is_empty());
        h.press(KeyCode::Enter);
        assert!(h.s.cable_editing.is_none());
        assert_eq!(h.commands(), [format!("SetCableGain id={} gain=0.250", id.0)]);
        // A number over the highest level is held to it by the controller; text that is not a number, and Escape, change nothing.
        h.press(KeyCode::Char('e')).press_n(KeyCode::Backspace, 4).type_chars("9").press(KeyCode::Enter);
        assert_eq!(h.commands(), [format!("SetCableGain id={} gain=2.000", id.0)]);
        h.press(KeyCode::Char('e')).press_n(KeyCode::Backspace, 4).type_chars("abc").press(KeyCode::Enter);
        h.press(KeyCode::Char('e')).type_chars("5").press(KeyCode::Esc);
        assert_eq!(h.commands(), Vec::<String>::new());
        assert!(h.s.cable_editing.is_none());
        // Not where there is no cable.
        at(&mut h, 0, 0);
        h.press(KeyCode::Char('e'));
        assert!(h.s.cable_editing.is_none());
    }

    #[test]
    fn a_level_that_the_controller_refuses_is_said_in_the_tab() {
        let mut h = Harness::new(PATCHED);
        at(&mut h, 1, 1);
        let edge = h.s.ctl.model.cables[1].id;
        h.press(KeyCode::Char('e'));
        // A browser takes the cable out while the popup is open.
        let taken = h.s.ctl.apply(Intent::RemoveEdge { edge }).unwrap();
        h.s.react_to(&taken);
        h.press_n(KeyCode::Backspace, 4).type_chars("0.5").press(KeyCode::Enter);
        assert!(h.s.cable_editing.is_none());
        assert_eq!(h.commands().iter().filter(|command| command.starts_with("SetCableGain")).count(), 0);
        let said = frame(&mut h);
        assert!(said.contains(&format!("there is no cable {}", edge.0)), "{said}");
    }

    #[test]
    fn a_click_off_the_level_popup_closes_it_and_changes_nothing() {
        let mut h = Harness::new(PATCHED);
        at(&mut h, 1, 1);
        h.press(KeyCode::Char('e'));
        assert!(h.s.cable_editing.is_some());
        click(&mut h, 3, 20);
        assert!(h.s.cable_editing.is_none());
        assert_eq!(h.commands(), Vec::<String>::new());
        assert_eq!(h.s.active_tab, TAB_PATCH);
    }

    #[test]
    fn the_foot_says_what_the_output_under_the_cursor_is_and_where_the_way_out_of_its_lane_or_group_goes() {
        let mut h = Harness::new(SENDS);
        let mut said = Vec::new();
        at(&mut h, 0, 0);
        for _ in 0..7 {
            let shown = frame(&mut h);
            let lines: Vec<&str> = shown.lines().collect();
            let foot = lines.iter().position(|line| line.contains("▸")).expect("the foot has what the cursor joins");
            said.push((lines[foot].trim_matches('│').trim().to_string(), lines[foot + 2].trim_matches('│').trim().to_string()));
            h.press(KeyCode::Down);
        }
        let said: Vec<(&str, &str)> = said.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(
            said,
            [
                ("L1 Oscillator (the instrument's output, after its volume)  ▸  L1 Filter", "L1's way out goes to G1 (w cuts it)"),
                ("L1 Filter (the output of the effect)  ▸  L1 Filter", "L1's way out goes to G1 (w cuts it)"),
                ("L1 Delay (the output of the effect)  ▸  L1 Filter", "L1's way out goes to G1 (w cuts it)"),
                ("L2 Oscillator (the instrument's output, after its volume)  ▸  L1 Filter", "L2's way out goes to the mix (w cuts it)"),
                ("L2 Drive (the output of the effect)  ▸  L1 Filter", "L2's way out goes to the mix (w cuts it)"),
                ("G1 Group 1 (the output of the group)  ▸  L1 Filter", "G1's way out goes to the mix (w cuts it)"),
                ("G1 Reverb (bus) (the output of the effect)  ▸  L1 Filter", "G1's way out goes to the mix (w cuts it)"),
            ]
        );
    }

    #[test]
    fn a_crosspoint_where_the_audio_would_go_round_is_a_cross_and_enter_there_says_why_and_asks_nothing() {
        let mut h = Harness::new(SENDS);
        at(&mut h, 2, 0); // the delay of the first lane, to its filter
        let shown = frame(&mut h);
        assert!(line_of(&shown, "L1    fx Delay").contains('×'), "{shown}");
        assert!(shown.contains("No cable can go here: the audio would go round"), "{shown}");
        h.press(KeyCode::Enter);
        assert_eq!(h.commands(), Vec::<String>::new());
        assert!(h.s.ctl.model.cables.is_empty());
        let said = frame(&mut h);
        assert!(said.contains("L1 Delay cannot send to L1 Filter: the audio would go round"), "{said}");
        h.press(KeyCode::Down);
        assert!(!frame(&mut h).contains("cannot send to"), "the next key clears it");
    }

    #[test]
    fn a_refusal_of_the_controller_is_said_in_the_tab_and_the_next_key_clears_it() {
        // The group's output feeds the filter of a lane that has no way out: giving it one would make the audio go round.
        let mut h = Harness::new(
            "[[group]]\n[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\nroute = \"none\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[cable]]\nfrom = \"group[0]\"\nto = \"instrument[0].effect[0]\"\n",
        );
        h.press(KeyCode::Char('4')).press(KeyCode::Char('w'));
        assert_eq!(h.commands(), Vec::<String>::new());
        assert_eq!(h.s.ctl.model.instruments[0].route, Route::None);
        let said = frame(&mut h);
        assert!(said.contains("giving that lane its way out would make the audio go round, by the cables there are"), "{said}");
        h.press(KeyCode::Down);
        assert!(!frame(&mut h).contains("by the cables there are"));
    }

    #[test]
    fn w_m_and_s_are_of_the_lane_or_the_group_of_the_row_and_toggle_what_it_has() {
        let mut h = Harness::new(SENDS);
        at(&mut h, 1, 0); // an effect of the first lane
        h.press(KeyCode::Char('w')).press(KeyCode::Char('M')).press(KeyCode::Char('S'));
        assert_eq!(h.commands(), ["SetLaneRoute inst=0 route=none", "SetMute inst=0 muted=true", "SetSolo inst=0 solo=true"]);
        let shown = frame(&mut h);
        assert!(line_of(&shown, "L1  ♪ Oscillator").starts_with("│▫    MS L1"), "{shown}");
        assert!(shown.contains("L1's way out is cut: only its cables are heard (w gives it back), muted, soloed"), "{shown}");
        h.press(KeyCode::Char('w')).press(KeyCode::Char('M')).press(KeyCode::Char('S'));
        assert_eq!(h.commands(), ["SetLaneRoute inst=0 route=out", "SetMute inst=0 muted=false", "SetSolo inst=0 solo=false"]);
        assert!(line_of(&frame(&mut h), "L1  ♪ Oscillator").starts_with("│▪G1     L1"));
        // A group, by its bus effect.
        at(&mut h, 6, 5);
        h.press(KeyCode::Char('w')).press(KeyCode::Char('M')).press(KeyCode::Char('S'));
        assert_eq!(h.commands(), ["SetGroupRoute group=0 route=none", "SetGroupMute group=0 muted=true", "SetGroupSolo group=0 solo=true"]);
    }

    #[test]
    fn what_another_front_end_does_is_on_the_tab_with_the_next_frame_and_the_cursor_stays_on_what_it_was_on() {
        let mut h = Harness::new(SENDS);
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let n = Sends::of(&h.s.ctl.model);
        at(&mut h, 6, 3); // the reverb on the group's bus, to the group's bus
        // A browser draws a cable.
        let drawn = h.s.ctl.apply(Intent::AddCable { from: Endpoint { node: n.lane[0], port: Port::AudioOut }, to: Endpoint { node: n.bus, port: Port::AudioIn }, gain: Some(0.5) }).unwrap();
        h.s.react_to(&drawn);
        assert!(line_of(&frame(&mut h), "L1  ♪ Oscillator").contains("0.50"));
        // A browser adds a lane, which has a row of its own above the cursor's.
        let added = h.s.ctl.apply(Intent::AddInstrument { plugin: "builtin:osc".to_string() }).unwrap();
        h.s.react_to(&added);
        let shown = frame(&mut h);
        assert!(shown.contains("L3  ♪ Oscillator"), "{shown}");
        assert!(shown.contains("G1 Reverb (bus) (the output of the effect)  ▸  G1 Group 1's bus"), "the cursor is on what it was on: {shown}");
        // It is taken out again: the cursor is still on its crosspoint.
        let node = h.s.ctl.model.instruments[2].node;
        let taken = h.s.ctl.apply(Intent::RemoveNode { node }).unwrap();
        h.s.react_to(&taken);
        assert!(frame(&mut h).contains("G1 Reverb (bus) (the output of the effect)  ▸  G1 Group 1's bus"));
    }

    #[test]
    fn a_click_on_a_crosspoint_puts_the_cursor_there_and_the_wheel_moves_it_by_rows() {
        let mut h = Harness::new(SENDS);
        h.press(KeyCode::Char('4'));
        let shown = frame(&mut h);
        let lines: Vec<&str> = shown.lines().collect();
        let (y, names) = (lines.iter().position(|line| line.contains("L2  ♪ Oscillator")).unwrap(), lines.iter().find(|line| line.contains("Filt Dela")).unwrap());
        let x = names.chars().position(|c| c == 'R').unwrap(); // the first letter of `Reve`
        click(&mut h, x as u16, y as u16 - 2);
        assert!(frame(&mut h).contains("L2 Oscillator (the instrument's output, after its volume)  ▸  G1 Reverb (bus)"));
        // The name of a row: the row, and the column it was on.
        click(&mut h, 14, (y + 1) as u16 - 2);
        assert!(frame(&mut h).contains("L2 Drive (the output of the effect)  ▸  G1 Reverb (bus)"));
        // The wheel: three rows.
        process_event(&mut h.s, Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp, column: 20, row: 10, modifiers: KeyModifiers::NONE }));
        assert!(frame(&mut h).contains("L1 Filter (the output of the effect)"), "{}", frame(&mut h));
        process_event(&mut h.s, Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: 20, row: 10, modifiers: KeyModifiers::NONE }));
        assert!(frame(&mut h).contains("L2 Drive (the output of the effect)"));
        assert_eq!(h.commands(), Vec::<String>::new(), "the mouse that selects asks nothing");
    }

    #[test]
    fn a_click_off_the_crosspoints_and_the_names_moves_nothing_and_a_click_on_one_takes_the_refusal_off_the_foot() {
        let mut h = Harness::new(SENDS);
        at(&mut h, 2, 0);
        h.press(KeyCode::Enter);
        assert!(frame(&mut h).contains("cannot send to"), "a refusal is said");
        let shown = frame(&mut h);
        let lines: Vec<&str> = shown.lines().collect();
        let first = lines.iter().position(|line| line.contains("L1  ♪ Oscillator")).unwrap();
        let (last, wide) = (first + 6, 100);
        // To the right of the last input, and under the last output: no crosspoint, and the foot says what it said.
        for (column, row) in [(wide, first + 2), (50, last + 3), (50, last + 4)] {
            click(&mut h, column, row as u16 - 2);
            let now = frame(&mut h);
            assert!(now.contains("L1 Delay (the output of the effect)  ▸  L1 Filter"), "a click at {column},{row}: {now}");
        }
        // On a crosspoint it goes there, and the refusal that was in the foot is gone.
        let names = lines.iter().find(|line| line.contains("Filt Dela")).unwrap();
        let x = names.chars().position(|c| c == 'D').unwrap();
        click(&mut h, x as u16, (first + 3) as u16 - 2);
        let now = frame(&mut h);
        assert!(now.contains("L2 Oscillator (the instrument's output, after its volume)  ▸  L1 Delay") && !now.contains("cannot send to"), "{now}");
    }

    #[test]
    fn a_click_on_an_item_of_the_session_tabs_action_bar_presses_its_key() {
        let mut h = Harness::new(scene_split());
        let shown = frame(&mut h);
        let bar = shown.lines().rfind(|line| line.contains("new instr")).expect("the action bar of the Session tab");
        let x = bar[..bar.find(" M  mute").unwrap()].chars().count() as u16 + 1;
        click(&mut h, x, HEIGHT - 1);
        assert_eq!(h.commands(), ["SetMute inst=0 muted=true"], "the mute of the lane the cursor is on");
        let x = bar[..bar.find(" a  add fx").unwrap()].chars().count() as u16 + 1;
        click(&mut h, x, HEIGHT - 1);
        assert!(h.s.selector.is_some(), "add fx opens the selector of effects");
    }

    #[test]
    fn the_tab_bar_and_the_action_bar_are_the_mouses_on_the_patch_tab() {
        let mut h = Harness::new(SENDS);
        let bar = tab_bar(&mut h);
        let at_name = |name: &str, bar: &str| bar[..bar.find(name).unwrap()].chars().count() as u16 + 1;
        click(&mut h, at_name("(4) Patch", &bar), 0);
        assert_eq!(h.s.active_tab, TAB_PATCH);
        at(&mut h, 3, 4);
        // The action bar: each item presses its own key.
        let shown = frame(&mut h);
        let bottom = shown.lines().rfind(|line| line.contains("pin / unpin")).unwrap();
        let on_item = |key: &str| bottom[..bottom.find(&format!(" {key} ")).unwrap()].chars().count() as u16 + 1;
        let last = HEIGHT - 1;
        click(&mut h, on_item("Enter"), last);
        assert_eq!(h.s.ctl.model.cables.len(), 1, "a click on `Enter pin / unpin` draws a cable");
        click(&mut h, on_item("-"), last);
        assert_eq!(h.s.ctl.model.cables[0].gain, 0.95, "`-` softer");
        click(&mut h, on_item("+"), last);
        click(&mut h, on_item("+"), last);
        assert_eq!(h.s.ctl.model.cables[0].gain, 1.05, "`+` louder");
        click(&mut h, on_item("e"), last);
        assert!(h.s.cable_editing.is_some(), "`e type a level` opens the popup");
        h.press(KeyCode::Esc);
        click(&mut h, on_item("M"), last);
        assert!(h.s.ctl.model.instruments[1].muted, "`M mute` is of the row's lane");
        click(&mut h, on_item("d"), last);
        assert!(h.s.ctl.model.cables.is_empty(), "and a click on `d remove` takes it out");
        // (The session has a change that is not saved now: the first tab is wider.)
        let bar = tab_bar(&mut h);
        click(&mut h, at_name("(5) Help", &bar), 0);
        assert_eq!(h.s.active_tab, TAB_HELP);
    }

    #[test]
    fn a_session_that_does_not_fit_is_scrolled_by_the_cursor_and_the_title_says_what_is_in_view() {
        let mut h = Harness::new(PATCHED);
        h.terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        h.press(KeyCode::Char('4'));
        let first = frame(&mut h);
        assert!(first.contains("┌ Patch  ·  outputs 1–6 of 10  ·  inputs 1–5 of 8 "), "{first}");
        h.press_n(KeyCode::Down, 9).press(KeyCode::End);
        let last = frame(&mut h);
        assert!(last.contains("┌ Patch  ·  outputs 5–10 of 10  ·  inputs 4–8 of 8 "), "{last}");
        assert!(line_of(&last, "G1    fx Delay (bus)").ends_with("│") && last.contains("G1 Delay (bus) (the output of the effect)  ▸  the mix"), "{last}");
        assert!(last.contains(" Mix"), "the last column is in view: {last}");
        assert!(!last.contains("L1  ♪ Oscillator"), "and the first row is not");
        // Back up: the view goes with the cursor.
        h.press_n(KeyCode::Up, 9).press(KeyCode::Home);
        let back = frame(&mut h);
        assert!(back.contains("┌ Patch  ·  outputs 1–6 of 10  ·  inputs 1–5 of 8 ") && back.contains("L1  ♪ Oscillator"), "{back}");
        // And with more room the view is not left where it was, with a blank at its end: all of it is shown.
        h.press_n(KeyCode::Down, 9).press(KeyCode::End);
        assert!(frame(&mut h).contains("outputs 5–10 of 10  ·  inputs 4–8 of 8"), "scrolled again");
        h.terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        let roomy = frame(&mut h);
        assert!(roomy.contains("┌ Patch ─") && roomy.contains("L1  ♪ Oscillator") && roomy.contains("L1    fx Filter") && roomy.contains("G1    fx Delay (bus)"), "{roomy}");
        assert!(line_of(&roomy, "outputs ↓").contains("L1") && line_of(&roomy, "outputs ↓").contains("Mix"), "every input is in view: {roomy}");
    }

    #[test]
    fn what_the_patch_tab_does_to_a_lane_is_in_the_tree_of_the_session_tab() {
        let mut h = Harness::new(SENDS);
        at(&mut h, 3, 0); // the second lane's instrument
        h.press(KeyCode::Char('w')).press(KeyCode::Char('M')).press(KeyCode::Char('S'));
        h.press(KeyCode::Char('1'));
        assert!(h.tree().iter().any(|line| line.starts_with("♪ Oscillator") && line.ends_with("[mute] [solo] [no out]")), "{:#?}", h.tree());
        assert!(tab_bar(&mut h).contains("(1) Session *"));
    }

    #[test]
    fn the_tab_is_drawn_at_every_size_down_to_nothing_with_the_cursor_anywhere() {
        let mut h = Harness::new(PATCHED);
        h.press(KeyCode::Char('4')).press(KeyCode::Down).press(KeyCode::End);
        for height in 1..=34u16 {
            for width in (1..=40u16).chain([80, 140]) {
                h.terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                h.dump();
            }
        }
        // A click where the last frame drew nothing is on nothing.
        h.terminal = Terminal::new(TestBackend::new(30, 3)).unwrap();
        h.dump();
        click(&mut h, 15, 1);
        click(&mut h, 3, 1);
        assert_eq!(h.commands(), Vec::<String>::new());
    }

    #[test]
    fn the_help_tab_lists_the_keys_of_the_patch_tab() {
        let mut h = Harness::new(SENDS);
        h.press(KeyCode::Char('5'));
        assert!(h.s.help_lines.iter().any(|line| line.trim() == "1 2 3 4 5  Switch to tab by number"), "{:?}", h.s.help_lines);
        h.s.help_offset = h.s.help_lines.iter().position(|line| line.starts_with("Patch tab")).expect("the Patch tab has a section of the help");
        let shown = frame(&mut h);
        for line in ["Patch tab (the audio cables, as a matrix of outputs against inputs):", "Enter      Draw a cable at the crosspoint, or take it out", "w          Cut / give back the way out"] {
            assert!(shown.contains(line), "{line}: {shown}");
        }
    }
}

/// Chains of effects saved as modules (step 104): `K` on the Session tab names the chain of the lane the cursor is of and saves it to the library, and `N` lists the modules of the library with the
/// effects and adds one, whole, as a lane of its own. The controller's part is in `app/apply.rs`; this is what the terminal asks of it and shows.
mod modules {
    use super::*;

    /// A harness over a lane with a filter and a reverb (and an LFO), with the built-in plugins in the catalog and a library of modules that is an empty directory of the test's own.
    fn harness() -> (tempfile::TempDir, Harness) {
        let mut h = Harness::new(scene_two_effects());
        h.s.ctl.catalog.add(plugin::builtin::enumerate_plugins());
        let dir = tempfile::tempdir().unwrap();
        h.s.ctl.modules_dir = Some(dir.path().to_path_buf());
        (dir, h)
    }

    #[test]
    fn capital_k_asks_for_a_name_and_saves_the_chain_of_the_lane_as_a_module_of_the_library_which_n_then_lists() {
        let (dir, mut h) = harness();
        h.press(KeyCode::Char('K'));
        assert!(h.s.save_module.is_some(), "the popup opens");
        assert_eq!(h.s.save_module.as_ref().map(|popup| popup.input.value.as_str()), Some(""), "with no name to begin with");
        h.type_chars("  Room ").press(KeyCode::Enter);
        assert!(h.s.save_module.is_none(), "the popup closes");
        let text = std::fs::read_to_string(dir.path().join("Room.toml")).expect("the chain is a file of the library");
        assert!(text.contains("plugin = \"builtin:filter\"") && text.contains("plugin = \"builtin:reverb\"") && !text.contains("builtin:osc"), "{text}");
        // Nothing of the session changed: not marked, nothing to undo, nothing sent to the audio thread.
        assert!(!h.s.ctl.dirty);
        assert_eq!(h.s.ctl.history_state().undo, None);
        assert_eq!(h.commands(), Vec::<String>::new());
        // The catalog lists it, and the selector of `N` has it first, with the plugins after.
        assert_eq!(h.s.ctl.catalog.modules().iter().map(|module| module.id.as_str()).collect::<Vec<_>>(), ["module:Room"]);
        h.press(KeyCode::Char('N'));
        let selector = h.s.selector.as_ref().expect("the selector is open");
        assert_eq!(selector.items.first().map(|item| item.cells.clone()), Some(vec!["Room".to_string(), "Module".to_string(), "2".to_string(), "0".to_string()]));
        assert!(selector.items.len() > 1 && selector.items.iter().skip(1).all(|item| item.cells[1] != "Module"));
    }

    #[test]
    fn n_adds_the_module_as_a_lane_of_its_own_with_the_effects_it_was_saved_with_in_one_step_of_the_history() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("Room").press(KeyCode::Enter);
        let lanes = h.s.ctl.model.instruments.len();
        h.commands();
        h.press(KeyCode::Char('N'));
        assert_eq!(h.s.selector.as_ref().map(|selector| selector.mode), Some(SelectorMode::FreeEffect));
        h.press(KeyCode::Enter);
        assert!(h.s.selector.is_none());
        assert_eq!(h.commands().iter().filter(|command| command.starts_with("InsertEffect")).count(), 2, "the filter and the reverb");
        assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
        let lane = &h.s.ctl.model.instruments[lanes];
        assert!(lane.instrument.is_none());
        assert_eq!(lane.effects.iter().map(|effect| effect.name.as_str()).collect::<Vec<_>>(), ["Filter", "Reverb"]);
        let tree = h.tree();
        let at = tree.iter().position(|line| line.contains("(empty)")).expect("the lane is in the tree, with no instrument");
        assert!(tree[at].contains("[no out]") && tree[at + 1].contains("fx Filter") && tree[at + 2].contains("fx Reverb"), "{tree:#?}");
        assert!(h.s.ctl.dirty);
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Room"));
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(h.s.ctl.model.instruments.len(), lanes, "one undo takes the lane out with both effects");
    }

    #[test]
    fn n_adds_a_module_that_is_a_graph_as_a_lane_for_each_of_its_own_with_the_instrument_and_the_cable_in_one_step_that_ctrl_z_takes_out_whole() {
        let (dir, mut h) = harness();
        // A module of two lanes, as a person saves what is chosen on the page: an oscillator with a filter, a lane of a reverb alone, and a cable from the filter into the reverb.
        let file = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[cable]]\nfrom = \"instrument[0].effect[0]\"\nto = \"instrument[1].effect[0]\"\n";
        std::fs::write(dir.path().join("Voice.toml"), file).unwrap();
        assert!(h.s.ctl.refresh_modules());
        let (lanes, cables) = (h.s.ctl.model.instruments.len(), h.s.ctl.model.cables.len());
        h.commands();
        h.press(KeyCode::Char('N'));
        let selector = h.s.selector.as_ref().expect("the selector is open");
        assert_eq!(selector.items.first().map(|item| item.cells.clone()), Some(vec!["Voice".to_string(), "Module".to_string(), "3".to_string(), "0".to_string()]), "as many plugins as parameters: the instrument and the two effects");
        h.press(KeyCode::Enter);
        assert!(h.s.selector.is_none());
        let told = h.commands();
        assert_eq!(told.iter().filter(|command| command.starts_with("InsertEffect")).count(), 2, "{told:?}");
        assert_eq!(told.iter().filter(|command| command.starts_with("SwapInstrument")).count(), 1, "the oscillator: {told:?}");
        assert_eq!((h.s.ctl.model.instruments.len(), h.s.ctl.model.cables.len()), (lanes + 2, cables + 1), "a lane for each of the module's, and the cable between them");
        let [first, second] = [&h.s.ctl.model.instruments[lanes], &h.s.ctl.model.instruments[lanes + 1]];
        assert_eq!((first.instrument.as_ref().map(|plugin| plugin.name.as_str()), first.route), (Some("Oscillator"), crate::session::Route::None), "bare, as every module is");
        assert!(second.instrument.is_none() && second.route == crate::session::Route::None);
        let tree = h.tree();
        let oscillator = tree.iter().rposition(|line| line.contains("Oscillator")).expect("the module's oscillator is in the tree");
        assert!(tree[oscillator].contains("[no out]") && tree[oscillator + 1].contains("fx Filter"), "{tree:#?}");
        let empty = tree.iter().position(|line| line.contains("(empty)")).expect("and its lane of a reverb");
        assert!(tree[empty].contains("[no out]") && tree[empty + 1].contains("fx Reverb"), "{tree:#?}");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Voice"));
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!((h.s.ctl.model.instruments.len(), h.s.ctl.model.cables.len()), (lanes, cables), "one undo takes both lanes out, with the cable");
        h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!((h.s.ctl.model.instruments.len(), h.s.ctl.model.cables.len()), (lanes + 2, cables + 1), "and one redo puts them back");
    }

    #[test]
    fn n_adds_a_module_that_has_modulators_which_the_tree_lists_under_the_lane_each_hears_and_ctrl_z_takes_out_with_the_lanes() {
        let (dir, mut h) = harness();
        // A module with two lanes and two modulators (step 106): an LFO that hears the first lane and drives the filter's cutoff, and an envelope that hears every note and drives the reverb's mix.
        let file = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:filter\"\n[[instrument]]\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n\
                    [[modulator]]\ntype = \"lfo\"\nrate = 2.0\nhears = \"instrument[0]\"\n[[modulator.target]]\nto = \"instrument[0].effect[0]\"\nparam = \"cutoff\"\ndepth = 0.25\n\
                    [[modulator]]\ntype = \"envelope\"\n[[modulator.target]]\nto = \"instrument[1].effect[0]\"\ngain = \"mix\"\ndepth = 0.5\n";
        std::fs::write(dir.path().join("Voice.toml"), file).unwrap();
        assert!(h.s.ctl.refresh_modules());
        let (lanes, modulators) = (h.s.ctl.model.instruments.len(), h.s.ctl.model.free_modulators.len());
        h.commands();
        h.press(KeyCode::Char('N'));
        assert_eq!(h.s.selector.as_ref().and_then(|selector| selector.items.first().map(|item| item.cells[2].clone())).as_deref(), Some("3"), "the plugins are counted, and the modulators are not");
        h.press(KeyCode::Enter);
        let told = h.commands();
        assert_eq!(told.iter().filter(|command| command.starts_with("InsertFreeModulator")).count(), 2, "{told:?}");
        assert_eq!(told.iter().filter(|command| command.starts_with("AddFreeModTarget")).count(), 2, "{told:?}");
        assert_eq!((h.s.ctl.model.instruments.len(), h.s.ctl.model.free_modulators.len()), (lanes + 2, modulators + 2));
        // The one that hears the module's first lane is listed under that lane, and the one that hears every note is among those that do, which come first, and ends `(free)`.
        let tree = h.tree();
        let oscillator = tree.iter().rposition(|line| line.contains("Oscillator")).expect("the module's oscillator is in the tree");
        let lfo = tree.iter().rposition(|line| line.contains("~ LFO")).expect("the LFO is in the tree");
        assert!(lfo > oscillator && tree[lfo].contains("Filter: cutoff (25%)") && !tree[lfo].contains("(free)"), "under the lane that it hears: {tree:#?}");
        let envelope = tree.iter().position(|line| line.contains("~ ADSR") && line.contains("(free)")).expect("the envelope hears every note");
        assert!(envelope < oscillator && tree[envelope].contains("mix (50%)"), "{tree:#?}");
        assert_eq!(h.s.ctl.history_state().undo.as_deref(), Some("add Voice"));
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!((h.s.ctl.model.instruments.len(), h.s.ctl.model.free_modulators.len()), (lanes, modulators), "one undo takes the lanes out, and the modulators");
        h.press_with(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!((h.s.ctl.model.instruments.len(), h.s.ctl.model.free_modulators.len()), (lanes + 2, modulators + 2), "and one redo puts them back");
    }

    #[test]
    fn a_name_that_is_refused_stays_in_the_popup_with_the_reason_and_what_is_typed_clears_it() {
        let (dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("a/b").press(KeyCode::Enter);
        let popup = h.s.save_module.as_ref().expect("the popup stays open");
        assert_eq!(popup.error.as_deref(), Some("a file name has no folder in it: `/` and `\\` are not allowed"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing was written");
        h.press(KeyCode::Backspace);
        assert_eq!(h.s.save_module.as_ref().unwrap().error, None, "Backspace");
        h.press_n(KeyCode::Backspace, 5).type_chars("a.toml.toml").press(KeyCode::Enter);
        assert_eq!(h.s.save_module.as_ref().unwrap().error.as_deref(), Some("the name of a module does not end in .toml: that is the extension of its file"));
        h.type_chars("x");
        assert_eq!(h.s.save_module.as_ref().unwrap().error, None, "typing");
        h.press_n(KeyCode::Backspace, 20).type_chars("Fixed").press(KeyCode::Enter);
        assert!(h.s.save_module.is_none());
        assert!(dir.path().join("Fixed.toml").exists());
    }

    #[test]
    fn a_name_of_nothing_waits_and_escape_leaves_with_nothing_written() {
        let (dir, mut h) = harness();
        h.press(KeyCode::Char('K')).press(KeyCode::Enter);
        let popup = h.s.save_module.as_ref().expect("the popup stays open");
        assert_eq!(popup.error, None, "no complaint, it just waits");
        h.type_chars("   ").press(KeyCode::Enter);
        assert!(h.s.save_module.is_some());
        assert_eq!(h.s.save_module.as_ref().unwrap().error, None, "blanks are no name either: nothing is asked of the controller, which would refuse it in words, and nothing is said");
        h.press(KeyCode::Esc);
        assert!(h.s.save_module.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn capital_k_is_for_a_lane_that_has_a_chain_on_the_session_tab_and_opens_nothing_else() {
        // A lane that has no effect has no chain; the other tabs have no such key; in the parameter pane a capital is text.
        let mut split = Harness::new(scene_split());
        split.press(KeyCode::Char('K'));
        assert!(split.s.save_module.is_none());
        let (_dir, mut h) = harness();
        for tab in ['2', '3', '4', '5'] {
            h.press(KeyCode::Char(tab)).press(KeyCode::Char('K'));
            assert!(h.s.save_module.is_none(), "tab {tab}");
            h.press(KeyCode::Char('1'));
        }
        h.press(KeyCode::Enter).press(KeyCode::Char('K'));
        assert!(h.s.save_module.is_none(), "in the parameter pane");
        h.press(KeyCode::Esc);
        // On the effect of the lane, and on the modulator that hears it, it is the lane's chain.
        let effect = h.s.tree_entries.iter().find(|entry| matches!(entry.address, TreeAddress::Effect { .. })).map(|entry| entry.address).expect("the lane has an effect");
        h.goto(effect).press(KeyCode::Char('K'));
        assert!(h.s.save_module.is_some());
    }

    #[test]
    fn the_selectors_of_a_chain_do_not_list_the_modules_a_chain_of_effects_does_not_go_in_a_chain() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("Room").press(KeyCode::Enter);
        for key in ['a', 'N'] {
            h.press(KeyCode::Char(key));
            let selector = h.s.selector.as_ref().expect("the selector is open");
            let listed = selector.items.iter().any(|item| item.cells[0] == "Room");
            assert_eq!(listed, key == 'N', "{key}");
            h.press(KeyCode::Esc);
        }
        h.press(KeyCode::Char('n'));
        let selector = h.s.selector.as_ref().expect("the instrument selector is open");
        assert!(selector.items.iter().all(|item| item.cells[0] != "Room"));
    }

    #[test]
    fn a_program_that_keeps_no_library_says_so_in_the_popup() {
        let mut h = Harness::new(scene_two_effects());
        h.press(KeyCode::Char('K')).type_chars("Room").press(KeyCode::Enter);
        assert_eq!(h.s.save_module.as_ref().and_then(|popup| popup.error.as_deref()), Some("this program keeps no library of modules"));
    }

    #[test]
    fn every_key_that_edits_the_name_takes_the_complaint_away_and_a_key_that_only_moves_the_cursor_leaves_it() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("a/b").press(KeyCode::Enter);
        let complaint = |h: &Harness| h.s.save_module.as_ref().expect("the popup is open").error.clone();
        assert!(complaint(&h).is_some());
        for key in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
            h.press(key);
            assert!(complaint(&h).is_some(), "{key:?} only moves the cursor");
        }
        h.press(KeyCode::Home).press(KeyCode::Delete);
        assert_eq!(complaint(&h), None, "Delete");
        assert_eq!(h.s.save_module.as_ref().map(|popup| popup.input.value.as_str()), Some("/b"), "and it took the first letter");
    }

    #[test]
    fn a_letter_with_control_held_is_no_text_of_the_name_and_asks_nothing_of_the_session_under_the_popup() {
        use crate::app::intent::{Gain, Intent};
        let (_dir, mut h) = harness();
        let lane = h.s.ctl.model.instruments[0].node;
        h.s.ctl.apply(Intent::SetGain { node: lane, gain: Gain::Volume, value: 0.5 }).unwrap();
        let name = |h: &Harness| h.s.save_module.as_ref().map(|popup| popup.input.value.clone());
        h.press(KeyCode::Char('K')).type_chars("Ro");
        h.press_with(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(name(&h).as_deref(), Some("Ro"), "Ctrl+Z is not a z");
        assert!(h.s.ctl.history_state().undo.is_some(), "and it undoes nothing under the popup");
        h.press_with(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(h.notes().is_empty(), "the panic is not heard under it");
        assert_eq!(name(&h).as_deref(), Some("Ro"));
        h.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(h.s.quit, "Ctrl+C leaves from the popup as from the chain");
    }

    #[test]
    fn a_click_outside_leaves_the_popup_as_it_leaves_the_others_and_saves_nothing() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let (dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("Room");
        process_event(&mut h.s, Event::Mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 1, row: 1, modifiers: KeyModifiers::NONE }));
        assert!(h.s.save_module.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_selector_of_an_instrument_starts_on_the_one_the_lane_has_whatever_modules_the_library_lists_before_the_plugins() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("Room").press(KeyCode::Enter);
        h.press(KeyCode::Char('i'));
        let selector = h.s.selector.as_ref().expect("the selector is open");
        let item = selector.filter.selected_item(&selector.items).expect("a row is selected");
        assert_eq!(h.s.ctl.catalog.listed()[item.index].id, "builtin:osc", "the lane has the oscillator, which Enter keeps");
    }

    #[test]
    fn a_module_waits_for_a_frame_that_says_it_is_loading_as_a_plugin_that_is_not_built_in_does() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("Room").press(KeyCode::Enter);
        let lanes = h.s.ctl.model.instruments.len();
        h.press(KeyCode::Char('N'));
        handle_event(&mut h.s, enter());
        // The popup is closed, nothing is made, and what is to be done waits for the frame that says so.
        assert!(h.s.selector.is_none());
        assert_eq!(h.s.pending_load.as_ref().map(|pending| pending.name.as_str()), Some("Room"));
        assert_eq!(h.s.ctl.model.instruments.len(), lanes);
        assert!(h.dump().contains("Loading Room…"));
        run_pending(&mut h.s);
        assert!(h.s.pending_load.is_none());
        assert_eq!(h.s.ctl.model.instruments.len(), lanes + 1);
    }

    #[test]
    fn popup_save_module() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("Room");
        assert_golden("popup_save_module", &mut h);
    }

    #[test]
    fn popup_save_module_with_a_name_that_was_refused() {
        let (_dir, mut h) = harness();
        h.press(KeyCode::Char('K')).type_chars("a/b").press(KeyCode::Enter);
        assert_golden("popup_save_module_with_a_name_that_was_refused", &mut h);
    }

    #[test]
    fn the_help_tab_lists_the_key_that_keeps_a_chain_as_a_module_and_what_n_adds() {
        let h = Harness::new(scene_two_effects());
        assert!(h.s.help_lines.iter().any(|line| line.trim_start().starts_with("K ") && line.contains("Keep the chain of effects of the lane as a module")), "{:?}", h.s.help_lines);
        assert!(h.s.help_lines.iter().any(|line| line.trim_start().starts_with("N ") && line.contains("a module of the library")), "{:?}", h.s.help_lines);
    }
}
