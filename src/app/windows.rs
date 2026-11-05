//! The controller's part of a plugin's own window (step 112, part B): opening it, what the person does in it, and what the session does to it.
//!
//! The windows are `plugin::editors`', by plugin (the main thread's registry), and a slot has its plugin's id (`PluginSlot::editor`). Here: `Intent::OpenEditor` opens one; `poll` applies what a knob in a window did as an
//! ordinary `SetParam` (the model, the history, the audio thread and every other front-end are told as for a change from any of them, and a drag is one step, since the history merges what comes soon after the same
//! parameter) and closes the windows of plugins that have left the session; a value that the session changes is told to the window that is open (`tell_window`); and the program's loops turn the windows' events with
//! `pump_windows`, never while the scan of the installed plugins runs (f179).

use super::apply::invalid;
use super::controller::Controller;
use super::intent::{Applied, Event, Intent, IntentError};
use super::model::{NodeId, ParamKind};
use crate::plugin::editors::{self, EditorId};
use crate::window;

impl Controller {
    /// Open the window of the plugin in `node` (`Intent::OpenEditor`), after telling its controller the values that the session has for its parameters; one that is open is brought to the front. The checks come first, in the
    /// order of what a person is likely to have got wrong: the node, whether its plugin has a window, whether the scan is over, whether the machine can show one. A revision is used (the clients are told `EditorOpened`), and
    /// the session is not marked.
    pub(super) fn open_editor_intent(&mut self, node: NodeId) -> Result<Applied, IntentError> {
        let at = self.locate_plugin(node)?;
        let slot = self.plugin_at(&at).ok_or_else(|| invalid("that instrument has no plugin"))?;
        let Some(editor) = slot.editor else {
            return Err(invalid(format!("{} has no window of its own", slot.name)));
        };
        let name = slot.name.clone();
        let values: Vec<(u32, f32)> = slot.params.iter().filter(|param| !matches!(param.kind, ParamKind::Separator)).map(|param| (param.index, param.value)).collect();
        // (The windows' events are turned on the main run loop, and a plugin of the JUCE kind keeps its message queue there: while the scan loads and lets go of such plugins the loop is not turned, f179, and a window
        // that was opened now would be frozen until it ends.)
        if self.catalog.is_scanning() {
            return Err(invalid("the installed plugins are still being looked for, and a window waits for that to end: try again in a moment"));
        }
        (self.window_check)().map_err(invalid)?;
        editors::open(editor, &name, true, &values).map_err(|why| invalid(format!("{name}: {why}")))?;
        self.rev += 1;
        let applied = Applied::of(self.rev, vec![Event::EditorOpened { node }]);
        self.publish(&applied);
        Ok(applied)
    }

    /// Apply what the person has done to the knobs in the plugins' windows since the last look, as the change of any front-end is (`Intent::SetParam`: the model, the history, the audio thread and the others are told).
    /// What a window says of a parameter that the session does not list, or of a plugin that is not in it, is let go of.
    pub(super) fn apply_window_edits(&mut self) {
        for (editor, param, value) in editors::take_edits() {
            let Some(node) = self.model.node_of_editor(editor) else { continue };
            // (The window that said it has it: it is not told it back, in the middle of a drag, in the units that Tang converted it to and back from.)
            self.editing_from = Some(editor);
            let applied = self.apply(Intent::SetParam { node, param, value });
            self.editing_from = None;
            if let Err(error) = applied {
                log::debug!("what a plugin's window said of its parameter {param} was not taken: {error}");
            }
        }
    }

    /// Close the windows of plugins that are not in the session any more: one that was taken out, replaced, or loaded over (a window is of the plugin, and the plugin is the session's). A plugin that is kept to be put back
    /// by an undo is there with its window shut, and the id that its slot has; the window is opened again by a click.
    pub(super) fn close_windows_of_plugins_that_are_gone(&mut self) {
        for editor in editors::open_ids() {
            if self.model.node_of_editor(editor).is_none() {
                editors::close(editor);
            }
        }
    }

    /// Tell the window of a plugin, if it is open, the value that the session now has for one of its parameters: not to the window that the change came from.
    pub(super) fn tell_window(&self, editor: EditorId, param: u32, value: f32) {
        if self.editing_from != Some(editor) {
            editors::set_values(editor, &[(param, value)]);
        }
    }

    /// Tell the window of the plugin in `node`, if it is open, every value that the session has for its parameters (a preset was loaded).
    pub(super) fn show_values_in_window(&self, node: NodeId) {
        let Some(slot) = self.model.plugin_slot(node) else { return };
        let Some(editor) = slot.editor else { return };
        let values: Vec<(u32, f32)> = slot.params.iter().filter(|param| !matches!(param.kind, ParamKind::Separator)).map(|param| (param.index, param.value)).collect();
        editors::set_values(editor, &values);
    }

    /// Whether the program's loops are to turn the windows' events now: a window is open, and the scan of the installed plugins is not going.
    pub fn windows_open(&self) -> bool {
        editors::any_open() && !self.catalog.is_scanning()
    }

    /// Turn the events of the plugins' windows, for as long as it takes for one to come and no longer than `window::PUMP_TURN`, and see to the windows (a size that the person made is told to the plugin, a window that the
    /// person closed is let go of). Nothing if no window is open or the scan is going, which is what the loops ask (`windows_open`) before they call it.
    pub fn pump_windows(&mut self) {
        if self.windows_open() {
            window::pump(window::PUMP_TURN);
            editors::poll();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use super::*;
    use crate::app::model::{NodePath, ParamSlot};
    use crate::app::testutil::{self, GROUPS, StubEngine, load};
    use crate::plugin::editors::fakes::{FakeEditor, Log, said};

    const SESSION: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n";

    fn controller(session: &str) -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine();
        let mut ctl = Controller::new(load(session).model, handles, None);
        ctl.catalog.add(crate::plugin::builtin::enumerate_plugins());
        ctl.window_check = || Ok(());
        (engine, ctl)
    }

    fn node(ctl: &Controller, at: NodePath) -> NodeId {
        match at {
            NodePath::Instrument(inst) => ctl.model.instruments[inst].node,
            NodePath::Effect { inst, index } => ctl.model.instruments[inst].effects[index].node,
            NodePath::GroupEffect { group, index } => ctl.model.groups[group].effects[index].node,
            other => panic!("not a plugin: {other:?}"),
        }
    }

    /// What a plugin that has a window of its own is to the session: a fake editor in the registry, whose id the slot of the plugin in `node` has. The token is what the plugin holds, and has to be held for as long as it is.
    fn give_a_window(ctl: &mut Controller, node: NodeId, log: &Log) -> (Rc<FakeEditor>, EditorId, Arc<()>) {
        let editor = FakeEditor::new("Window", log);
        let alive = Arc::new(());
        let id = editors::register(editor.clone(), &alive);
        let at = ctl.locate_plugin(node).expect("a plugin");
        ctl.plugin_at(&at).expect("a slot").editor = Some(id);
        (editor, id, alive)
    }

    fn sent_params(engine: &StubEngine) -> Vec<String> {
        engine.drain().0.into_iter().filter(|command| command.starts_with("SetParameter")).collect()
    }

    #[test]
    fn a_plugin_that_has_no_window_says_so_and_nothing_is_opened_or_counted() {
        let (_engine, mut ctl) = controller(SESSION);
        let osc = node(&ctl, NodePath::Instrument(0));
        let rev = ctl.rev;
        let refused = ctl.apply(Intent::OpenEditor { node: osc }).expect_err("refused");
        assert_eq!(refused, invalid("Oscillator has no window of its own"));
        assert_eq!((ctl.rev, ctl.dirty, editors::any_open()), (rev, false, false), "no revision, nothing unsaved, nothing open");
    }

    #[test]
    fn a_node_that_is_not_there_or_has_no_plugin_is_refused_in_words() {
        let (_engine, mut ctl) = controller(GROUPS);
        let ghost = NodeId(9999);
        assert_eq!(ctl.apply(Intent::OpenEditor { node: ghost }), Err(IntentError::NotFound(ghost)));
        let group = ctl.model.groups[0].node;
        assert_eq!(ctl.apply(Intent::OpenEditor { node: group }), Err(invalid("that node has no plugin")));
    }

    #[test]
    fn a_plugin_that_has_a_window_gets_it_opened_with_the_values_the_session_has_and_the_clients_are_told() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let (_editor, id, _alive) = give_a_window(&mut ctl, osc, &log);
        // (A plugin that files its parameters in sections has a row for the name of each, which is no parameter: the plugin is not told a value for it.)
        ctl.model.instruments[0].instrument.as_mut().unwrap().params.push(ParamSlot { name: "Envelope".into(), index: 99, kind: ParamKind::Separator, ..Default::default() });
        let rev = ctl.rev;
        let applied = ctl.apply(Intent::OpenEditor { node: osc }).expect("opens");
        assert_eq!(applied, Applied::of(rev + 1, vec![Event::EditorOpened { node: osc }]));
        assert!(editors::is_open(id) && ctl.windows_open());
        assert!(!ctl.dirty, "a window is no change of the session");
        let told = said(&log);
        let slot = ctl.model.instruments[0].instrument.as_ref().unwrap();
        let values: Vec<(u32, f32)> = slot.params.iter().filter(|param| !matches!(param.kind, ParamKind::Separator)).map(|param| (param.index, param.value)).collect();
        assert!(!values.is_empty() && !told[0].contains("(99,"), "the heading is not told: {}", told[0]);
        assert_eq!(told, [format!("Window: values {values:?}"), "Window: open \"Oscillator\" focus=true".to_string()], "the values first, so that the window shows them, and the plugin's name in its title");
    }

    #[test]
    fn a_window_that_is_open_is_brought_to_the_front_when_it_is_asked_for_again_and_one_that_is_shut_opens_again() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (_editor, id, _alive) = give_a_window(&mut ctl, reverb, &log);
        ctl.apply(Intent::OpenEditor { node: reverb }).expect("opens");
        log.borrow_mut().clear();
        ctl.apply(Intent::OpenEditor { node: reverb }).expect("raises");
        assert_eq!(said(&log), ["Window: raise focus=true"]);
        editors::close(id);
        ctl.apply(Intent::OpenEditor { node: reverb }).expect("opens again");
        assert!(editors::is_open(id));
    }

    #[test]
    fn a_machine_that_cannot_show_a_window_says_why_and_nothing_is_opened() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let (_editor, id, _alive) = give_a_window(&mut ctl, osc, &log);
        ctl.window_check = || Err("this session has no screen".to_string());
        let rev = ctl.rev;
        assert_eq!(ctl.apply(Intent::OpenEditor { node: osc }), Err(invalid("this session has no screen")));
        assert!(!editors::is_open(id) && said(&log).is_empty());
        assert_eq!(ctl.rev, rev);
    }

    #[test]
    fn a_window_waits_for_the_scan_to_end_and_the_loops_do_not_turn_while_it_goes() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let (_editor, id, _alive) = give_a_window(&mut ctl, osc, &log);
        let (scan, found) = crossbeam_channel::unbounded();
        ctl.catalog.follow(found);
        let refused = ctl.apply(Intent::OpenEditor { node: osc }).expect_err("refused");
        assert!(refused.to_string().contains("still being looked for"), "{refused}");
        assert!(!editors::is_open(id));
        // The scan ends (its sender is gone, and `poll` sees it).
        drop(scan);
        ctl.poll();
        ctl.apply(Intent::OpenEditor { node: osc }).expect("opens");
        assert!(ctl.windows_open());
        // A scan that starts again while a window is open stops the loops from turning: the window waits.
        let (_scan, found) = crossbeam_channel::unbounded::<Vec<crate::plugin::PluginInfo>>();
        ctl.catalog.follow(found);
        assert!(editors::any_open() && !ctl.windows_open(), "open, and not to be turned");
    }

    #[test]
    fn the_loops_have_nothing_to_turn_while_no_window_is_open() {
        let (_engine, mut ctl) = controller(SESSION);
        assert!(!ctl.windows_open());
        ctl.pump_windows();
    }

    #[test]
    fn the_windows_are_seen_to_when_the_loops_turn_them_and_not_while_the_scan_goes() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let (_editor, _id, _alive) = give_a_window(&mut ctl, osc, &log);
        ctl.apply(Intent::OpenEditor { node: osc }).expect("opens");
        log.borrow_mut().clear();
        ctl.pump_windows();
        assert_eq!(said(&log), ["Window: poll"], "the window is open and the scan is not going: it is seen to");
        // The scan starts again: the loops do not turn, and the window waits, not seen to.
        let (_scan, found) = crossbeam_channel::unbounded::<Vec<crate::plugin::PluginInfo>>();
        ctl.catalog.follow(found);
        log.borrow_mut().clear();
        ctl.pump_windows();
        assert!(said(&log).is_empty(), "{:?}", said(&log));
    }

    #[test]
    fn a_knob_in_the_window_is_applied_as_a_change_of_a_parameter_that_one_undo_takes_back_and_the_window_is_not_told_it_back() {
        let (engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let (editor, _id, _alive) = give_a_window(&mut ctl, osc, &log);
        ctl.apply(Intent::OpenEditor { node: osc }).expect("opens");
        let index = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.name == "detune").expect("a parameter").index;
        let value_of = |ctl: &Controller| ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.index == index).unwrap().value;
        let before = value_of(&ctl);
        engine.drain();
        log.borrow_mut().clear();
        // The person drags the knob: three values, one after the other, and a poll between.
        for value in [1.0, 2.0, 3.0] {
            editor.edits.borrow_mut().push((index, value));
        }
        ctl.poll();
        assert_eq!(value_of(&ctl), 3.0, "the model has the last of them");
        assert!(ctl.dirty, "a knob that was turned is a change of the session");
        assert_eq!(sent_params(&engine).len(), 3, "and the audio thread was told of each");
        assert!(said(&log).is_empty(), "the window that made them is not told them back: {:?}", said(&log));
        // One drag is one step of the history.
        ctl.apply(Intent::Undo).expect("undoes");
        assert_eq!(value_of(&ctl), before, "what it was before the drag");
        assert!(ctl.apply(Intent::Undo).is_err(), "and there was no other step: it was one");
        // A change that something else makes after the drag is told to the window: it is not left deaf by the drag that came before.
        log.borrow_mut().clear();
        ctl.apply(Intent::SetParam { node: osc, param: index, value: 1.5 }).expect("set from the page");
        assert_eq!(said(&log), [format!("Window: values [({index}, 1.5)]")], "{:?}", said(&log));
    }

    #[test]
    fn what_a_window_says_of_a_plugin_that_is_not_in_the_session_or_of_a_parameter_that_it_does_not_have_is_let_go_of() {
        let (engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        // A window whose plugin the session does not have.
        let loose = FakeEditor::new("Loose", &log);
        let alive = Arc::new(());
        editors::register(loose.clone(), &alive);
        loose.edits.borrow_mut().push((0, 1.0));
        // A parameter that the plugin has not.
        let osc = node(&ctl, NodePath::Instrument(0));
        let (editor, _id, _alive) = give_a_window(&mut ctl, osc, &log);
        editor.edits.borrow_mut().push((9999, 1.0));
        engine.drain();
        let rev = ctl.rev;
        ctl.poll();
        assert_eq!((ctl.rev, sent_params(&engine)), (rev, vec![]), "nothing was applied");
        assert!(loose.edits.borrow().is_empty() && editor.edits.borrow().is_empty(), "and they are gone, not kept for ever");
    }

    #[test]
    fn what_a_window_says_of_a_plugin_that_is_not_in_the_session_does_not_stop_what_the_windows_after_it_said() {
        let (engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        // A plugin that is not in the session, made first, so that what its window said comes first (the edits are taken in the order that the plugins were made).
        let loose = FakeEditor::new("Loose", &log);
        let loose_alive = Arc::new(());
        let _loose_id = editors::register(loose.clone(), &loose_alive);
        loose.edits.borrow_mut().push((0, 1.0));
        let osc = node(&ctl, NodePath::Instrument(0));
        let (editor, _id, _alive) = give_a_window(&mut ctl, osc, &log);
        let index = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.name == "detune").expect("a parameter").index;
        editor.edits.borrow_mut().push((index, 2.0));
        engine.drain();
        ctl.poll();
        let value = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.index == index).unwrap().value;
        assert_eq!(value, 2.0, "what the plugin that is in the session said was applied, after what the one that is not said");
        assert_eq!(sent_params(&engine).len(), 1, "and the other was let go of");
    }

    #[test]
    fn a_change_that_the_session_makes_is_told_to_the_window_that_is_open_and_not_to_one_that_is_shut() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let (_editor, id, _alive) = give_a_window(&mut ctl, osc, &log);
        let index = ctl.model.instruments[0].instrument.as_ref().unwrap().params.iter().find(|p| p.name == "detune").unwrap().index;
        ctl.apply(Intent::SetParam { node: osc, param: index, value: 1.0 }).unwrap();
        assert!(said(&log).is_empty(), "a window that is shut has nothing to show it in");
        ctl.apply(Intent::OpenEditor { node: osc }).unwrap();
        log.borrow_mut().clear();
        ctl.apply(Intent::SetParam { node: osc, param: index, value: 2.0 }).unwrap();
        assert_eq!(said(&log), [format!("Window: values [({index}, 2.0)]")], "the value that the model has, as a page or an undo made it");
        // (The two changes came together, so they are one step of the history, and undo goes back to what the parameter was before the first.)
        ctl.apply(Intent::Undo).unwrap();
        assert_eq!(said(&log)[1], format!("Window: values [({index}, 0.0)]"), "undo is the same");
        editors::close(id);
        log.borrow_mut().clear();
        ctl.apply(Intent::SetParam { node: osc, param: index, value: 0.5 }).unwrap();
        assert!(said(&log).is_empty());
    }

    #[test]
    fn a_preset_that_is_loaded_is_shown_in_the_open_window_whole() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (_editor, _id, _alive) = give_a_window(&mut ctl, reverb, &log);
        ctl.model.instruments[0].effects[0].params.push(ParamSlot { name: "Tail".into(), index: 99, kind: ParamKind::Separator, ..Default::default() });
        ctl.apply(Intent::OpenEditor { node: reverb }).unwrap();
        log.borrow_mut().clear();
        ctl.apply(Intent::LoadPreset { node: reverb, preset: "cathedral".to_string() }).unwrap();
        let told = said(&log);
        assert_eq!(told.len(), 1, "{told:?}");
        assert!(!told[0].contains("(99,"), "a heading is not a parameter: {}", told[0]);
        let slot = &ctl.model.instruments[0].effects[0];
        let values: Vec<(u32, f32)> = slot.params.iter().filter(|param| !matches!(param.kind, ParamKind::Separator)).map(|param| (param.index, param.value)).collect();
        assert_eq!(told[0], format!("Window: values {values:?}"), "every value, as the preset made them");
    }

    #[test]
    fn the_window_of_a_plugin_that_leaves_the_session_is_closed_and_one_that_stays_is_not() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let osc = node(&ctl, NodePath::Instrument(0));
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (_editor_a, osc_id, _alive_a) = give_a_window(&mut ctl, osc, &log);
        let (_editor_b, reverb_id, _alive_b) = give_a_window(&mut ctl, reverb, &log);
        ctl.apply(Intent::OpenEditor { node: osc }).unwrap();
        ctl.apply(Intent::OpenEditor { node: reverb }).unwrap();
        ctl.poll();
        assert!(editors::is_open(osc_id) && editors::is_open(reverb_id));
        ctl.apply(Intent::RemoveNode { node: reverb }).expect("the effect is taken out");
        ctl.poll();
        assert!(!editors::is_open(reverb_id), "its plugin is out of the session");
        assert!(editors::is_open(osc_id), "the other is still there");
    }

    #[test]
    fn a_plugin_that_an_undo_loads_again_is_another_instance_and_its_slot_has_that_ones_window_and_not_the_old_ones() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (_editor, old, _alive) = give_a_window(&mut ctl, reverb, &log);
        ctl.apply(Intent::RemoveNode { node: reverb }).expect("taken out");
        // (The stub engine hands nothing back, so the effect is loaded again from the catalog, as one that the history gave up is.)
        ctl.undo().expect("put back");
        assert_eq!(node(&ctl, NodePath::Effect { inst: 0, index: 0 }), reverb, "the same node");
        assert_eq!(ctl.model.node_of_editor(old), None, "the window of the plugin that went is not the one that came (a built-in plugin has none)");
        let refused = ctl.apply(Intent::OpenEditor { node: reverb }).expect_err("refused");
        assert_eq!(refused, invalid("Reverb has no window of its own"));
    }

    #[test]
    fn a_slot_is_found_by_its_window_and_a_window_that_no_slot_has_is_found_by_none() {
        let (_engine, mut ctl) = controller(SESSION);
        let log = Log::default();
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        let (_editor, id, _alive) = give_a_window(&mut ctl, reverb, &log);
        assert_eq!(ctl.model.node_of_editor(id), Some(reverb));
        let loose = FakeEditor::new("Loose", &log);
        let alive = Arc::new(());
        let loose_id = editors::register(loose, &alive);
        assert_eq!(ctl.model.node_of_editor(loose_id), None);
    }
}
