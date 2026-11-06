//! What a session keeps of what is in a plugin that is not a parameter (step 112, part C): the sound that a person picked in a plugin's own window, a model that it loaded.
//!
//! A plugin gives it as bytes that only it reads (`Editor::state`), and `PluginSlot::state` has the last that it gave: the session file keeps it (`state`, base64), and gives it back to the plugin when it is loaded, before its
//! preset and its values. It is read again when the plugin's window is closed, which is when what a person did in it is done, and when the session is saved or a module is, for the plugins that have one or whose window has been
//! opened: a plugin that has none and has not been looked into is not asked. A state that changed makes the session unsaved, and the plugin may have changed some of its parameters itself, which are read back. No step of the
//! history has a state: an Undo does not take back what was picked in a window.

use super::controller::Controller;
use super::model::{NodeId, ParamKind};
use crate::plugin::editors::{self, EditorId};

impl Controller {
    /// The plugins whose state is read when the session is written: those that have a window in the registry and a state already (read from the session file, or from the plugin before), or have had their windows opened.
    fn plugins_whose_state_is_kept(&self) -> Vec<(NodeId, EditorId)> {
        let lanes = self.model.instruments.iter().flat_map(|lane| lane.instrument.iter().chain(&lane.effects));
        let buses = self.model.groups.iter().flat_map(|group| &group.effects);
        lanes.chain(buses).filter_map(|slot| slot.editor.map(|editor| (slot, editor))).filter(|(slot, editor)| slot.state.is_some() || editors::was_opened(*editor)).map(|(slot, editor)| (slot.node, editor)).collect()
    }

    /// Read the state of the plugin in `node` (whose window is `editor`) into its slot, if it is not the state that the slot has; and then the values of the plugin's parameters, which a state may have changed (a sound that
    /// is picked sets some of them): the session shows what the plugin has, and the audio thread is told (a cable that drives one adds to what it was). A plugin that gives no state this time keeps the one it had.
    /// True if the state changed.
    fn keep_the_state_of(&mut self, node: NodeId, editor: EditorId) -> bool {
        let Some(state) = editors::state(editor) else { return false };
        let Some(slot) = self.model.plugin_slot(node) else { return false };
        if slot.state.as_ref() == Some(&state) {
            return false;
        }
        let Ok(at) = self.locate_plugin(node) else { return false };
        if let Some(slot) = self.plugin_at(&at) {
            slot.state = Some(state);
        }
        for (param, value) in editors::values(editor) {
            let differs = self.model.plugin_slot(node).and_then(|slot| slot.params.iter().find(|row| row.index == param && !matches!(row.kind, ParamKind::Separator))).is_some_and(|row| (row.value - value).abs() > 1e-6);
            if differs {
                if let Err(error) = self.set_param(node, param, value) {
                    log::debug!("the value that a plugin has for its parameter {param} was not taken: {error}");
                }
            }
        }
        true
    }

    /// Read the state of every plugin that keeps one (`plugins_whose_state_is_kept`) into its slot: what the session is about to write. The clients are told if the session shows it (the values that were read back).
    /// True if any state changed.
    pub(super) fn keep_the_states(&mut self) -> bool {
        let mut changed = false;
        for (node, editor) in self.plugins_whose_state_is_kept() {
            changed |= self.keep_the_state_of(node, editor);
        }
        #[cfg(any(test, feature = "web"))]
        if changed {
            self.resync();
        }
        changed
    }

    /// Read the state of the plugins whose windows were closed since the last look (`editors::take_closed`), which is when what a person did in them is done: the session is unsaved if any changed. A window of a plugin that
    /// has left the session is not one of these. True if any state changed.
    pub(super) fn keep_the_states_of_closed_windows(&mut self) -> bool {
        let mut changed = false;
        for editor in editors::take_closed() {
            if let Some(node) = self.model.node_of_editor(editor) {
                changed |= self.keep_the_state_of(node, editor);
            }
        }
        if changed {
            self.dirty = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::app::intent::{Applied, Intent};
    use crate::app::model::NodePath;
    use crate::app::snapshot::Snapshot;
    use crate::app::subscribers::EventSink;
    use crate::app::testutil::{self, StubEngine};
    use crate::plugin::fake::{self, Sink};
    use crate::plugin::PluginInfo;

    /// A synth with a state, a preset and two parameters, and an effect with a state (each a fake, `plugin::fake`).
    const LANE: &str = "[[instrument]]\nplugin = \"fake:synth\"\nstate = \"AQID\"\npreset = \"Big\"\n[[instrument.effect]]\nplugin = \"fake:verb\"\nstate = \"BAUG\"\n";

    fn said(sink: &Sink) -> Vec<String> {
        sink.lock().unwrap().clone()
    }

    /// A controller over a session whose plugins are fakes. (The engine is kept: what it has taken of the commands holds on to the plugins.)
    fn controller(session: &str) -> (StubEngine, Controller) {
        let (handles, engine) = testutil::stub_engine_of(256);
        let model = testutil::load_onto(session, &handles.cmd_tx);
        let mut ctl = Controller::new(model, handles, None);
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

    fn state_of(ctl: &Controller, at: NodePath) -> Option<Vec<u8>> {
        ctl.model.plugin_slot(node(ctl, at)).expect("a plugin").state.clone()
    }

    #[test]
    fn a_state_in_the_file_is_given_to_each_plugin_when_it_is_loaded_before_its_preset_and_is_kept_in_its_slot() {
        let sink = Sink::default();
        let _synth = fake::install("fake:synth", "Synth", &sink);
        let _verb = fake::install("fake:verb", "Verb", &sink);
        let session = format!("[[group]]\n[[group.effect]]\nplugin = \"fake:verb\"\nstate = \"BwgJ\"\n{LANE}");
        let (_engine, ctl) = controller(&session);
        // The group's effect first, then the lane's instrument (its state, then its preset) and its effect.
        assert_eq!(said(&sink), ["set_state [7, 8, 9]", "set_state [1, 2, 3]", "load_preset big", "set_state [4, 5, 6]"]);
        assert_eq!(ctl.model.groups[0].effects[0].state, Some(vec![7, 8, 9]));
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![1, 2, 3]));
        assert_eq!(state_of(&ctl, NodePath::Effect { inst: 0, index: 0 }), Some(vec![4, 5, 6]));
    }

    #[test]
    fn a_plugin_with_no_state_in_the_file_is_given_none_and_one_that_does_not_take_its_state_keeps_it_all_the_same() {
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let (_engine, ctl) = controller("[[instrument]]\nplugin = \"fake:synth\"\n");
        assert!(said(&sink).is_empty(), "{:?}", said(&sink));
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), None);
        // A plugin that says no to it (it was kept by another version of the plugin): the session loads, the plugin is as it was loaded, and the session still has the state, to be written again.
        synth.refuses_state.set(Some("a state from another version"));
        let (_engine, ctl) = controller("[[instrument]]\nplugin = \"fake:synth\"\nstate = \"AQID\"\n");
        assert_eq!(said(&sink), ["set_state [1, 2, 3]"]);
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![1, 2, 3]));
    }

    #[test]
    fn a_plugin_that_an_undo_loads_again_is_given_its_state_before_its_preset_and_its_values() {
        let sink = Sink::default();
        let _synth = fake::install("fake:synth", "Synth", &sink);
        let _verb = fake::install("fake:verb", "Verb", &sink);
        let (_engine, mut ctl) = controller(LANE);
        let lane = node(&ctl, NodePath::Instrument(0));
        ctl.apply(Intent::ClearInstrument { node: lane }).expect("taken out");
        sink.lock().unwrap().clear();
        // (The stub engine hands nothing back, so the instrument is loaded again from what the session has, as one that the history gave up is.)
        ctl.undo().expect("put back");
        assert_eq!(said(&sink), ["set_state [1, 2, 3]", "load_preset big", "set_parameter 0 0.5", "set_parameter 1 0.5"]);
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![1, 2, 3]));
        assert!(ctl.model.instruments[0].instrument.as_ref().unwrap().editor.is_some(), "and it has a window of its own again");
    }

    #[test]
    fn what_a_person_did_in_a_window_is_kept_when_it_is_closed_the_parameters_it_changed_are_read_back_and_the_session_is_unsaved() {
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let _verb = fake::install("fake:verb", "Verb", &sink);
        let (engine, mut ctl) = controller(LANE);
        let lane = node(&ctl, NodePath::Instrument(0));
        ctl.apply(Intent::OpenEditor { node: lane }).expect("opens");
        ctl.dirty = false;
        engine.drain();
        // The person picks a sound in the window (the plugin's state is another, and it changed a parameter of its own), and closes the window.
        synth.state.replace(Some(vec![9, 9]));
        synth.values.replace(vec![(0, 0.9), (1, 0.5)]);
        synth.person_closed.set(true);
        ctl.pump_windows();
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![1, 2, 3]), "not before the session looks");
        let polled = ctl.poll();
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![9, 9]));
        assert!(ctl.dirty && polled.model_changed, "what was picked is a change of the session");
        let slot = ctl.model.plugin_slot(lane).unwrap();
        assert_eq!(slot.params.iter().map(|row| row.value).collect::<Vec<_>>(), [0.9, 0.5], "the parameter that the plugin changed is as it has it, and the other is as it was");
        let commands = engine.drain().0;
        assert_eq!(commands.iter().filter(|command| command.starts_with("SetParameter")).count(), 1, "the audio thread was told of the one that changed: {commands:?}");
        // No step of the history has it: an Undo does not take back what was picked.
        assert!(ctl.apply(Intent::Undo).is_err(), "nothing to undo");
        // And the window is read once: a second poll has nothing new.
        ctl.dirty = false;
        assert!(!ctl.poll().model_changed && !ctl.dirty);
    }

    #[test]
    fn a_window_that_is_closed_with_the_state_the_plugin_had_leaves_the_session_as_it_was_and_so_does_a_plugin_that_gives_none() {
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let _verb = fake::install("fake:verb", "Verb", &sink);
        let (_engine, mut ctl) = controller(LANE);
        let lane = node(&ctl, NodePath::Instrument(0));
        for gives in [Some(vec![1, 2, 3]), None] {
            ctl.apply(Intent::OpenEditor { node: lane }).expect("opens");
            ctl.dirty = false;
            synth.state.replace(gives);
            synth.person_closed.set(true);
            ctl.pump_windows();
            assert!(!ctl.poll().model_changed && !ctl.dirty);
            assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![1, 2, 3]), "the state that it had is kept");
        }
    }

    #[test]
    fn the_state_of_a_plugin_that_has_none_and_whose_window_was_never_opened_is_not_asked_for_and_that_of_one_that_has_either_is() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let (_engine, mut ctl) = controller("[[instrument]]\nplugin = \"fake:synth\"\n");
        let path = dir.path().join("s.toml");
        ctl.session_path = Some(path.clone());
        synth.state.replace(Some(vec![5, 6]));
        ctl.save().expect("saved");
        assert!(said(&sink).is_empty(), "a plugin that has no state and has not been looked into is not asked: {:?}", said(&sink));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("state"));
        // Its window is opened: it is asked at the next save, and what it gives is in the file, and in the slot.
        let lane = node(&ctl, NodePath::Instrument(0));
        ctl.apply(Intent::OpenEditor { node: lane }).expect("opens");
        ctl.save().expect("saved");
        assert_eq!(said(&sink), ["state asked"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.lines().any(|line| line == "state = \"BQY=\""), "{text}");
        assert_eq!(state_of(&ctl, NodePath::Instrument(0)), Some(vec![5, 6]));
        // The file gives it back to a plugin that is loaded from it, before anything else is done to it.
        sink.lock().unwrap().clear();
        let (_engine, again) = controller(&text);
        assert_eq!(said(&sink), ["set_state [5, 6]"]);
        assert_eq!(state_of(&again, NodePath::Instrument(0)), Some(vec![5, 6]));
        // A plugin that had a state from the file is asked though its window has not been opened.
        sink.lock().unwrap().clear();
        let (_engine, mut kept) = controller(&text);
        kept.session_path = Some(dir.path().join("t.toml"));
        sink.lock().unwrap().clear();
        kept.save().expect("saved");
        assert_eq!(said(&sink), ["state asked"]);
    }

    /// A client that keeps the snapshots that it is told to start again from.
    #[derive(Clone, Default)]
    struct Resynced(Arc<Mutex<Vec<Snapshot>>>);

    impl EventSink for Resynced {
        fn applied(&mut self, _: &Applied) {}

        fn resync(&mut self, snapshot: &Snapshot) {
            self.0.lock().unwrap().push(snapshot.clone());
        }
    }

    #[test]
    fn the_clients_are_told_when_what_a_save_read_back_is_a_change_that_they_can_see_and_not_when_nothing_changed() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let _verb = fake::install("fake:verb", "Verb", &sink);
        let (_engine, mut ctl) = controller(LANE);
        ctl.session_path = Some(dir.path().join("s.toml"));
        let heard = Resynced::default();
        ctl.subscribe(Box::new(heard.clone()));
        let lane = node(&ctl, NodePath::Instrument(0));
        // The plugin's state and one of its values are not what they were when the session was loaded.
        synth.state.replace(Some(vec![9, 9]));
        synth.values.replace(vec![(0, 0.9)]);
        ctl.save().expect("saved");
        let snapshots = heard.0.lock().unwrap().clone();
        assert_eq!(snapshots.len(), 1, "told once");
        let json = serde_json::to_value(&snapshots[0]).unwrap();
        let plugin = json["graph"]["nodes"].as_array().unwrap().iter().find(|n| n["id"] == serde_json::json!(lane)).unwrap()["plugin"].clone();
        assert_eq!(plugin["params"][0]["value"].as_f64().map(|v| (v * 10.0).round()), Some(9.0), "with the value that was read back: {plugin}");
        // Nothing has changed since: a second save tells nobody.
        ctl.save().expect("saved again");
        assert_eq!(heard.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_selection_saved_as_a_module_keeps_the_state_of_an_instrument_and_an_effect_as_they_are_and_gives_both_back() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let verb = fake::install("fake:verb", "Verb", &sink);
        let (_engine, mut ctl) = controller(LANE);
        ctl.modules_dir = Some(dir.path().to_path_buf());
        let entry = |id: &str, name: &str, is_instrument: bool| PluginInfo { name: name.to_string(), id: id.to_string(), is_instrument, param_count: 2, preset_count: 1, path: String::new(), scan_ms: 0 };
        ctl.catalog.add(vec![entry("fake:synth", "Synth", true), entry("fake:verb", "Verb", false)]);
        let (lane, effect) = (node(&ctl, NodePath::Instrument(0)), node(&ctl, NodePath::Effect { inst: 0, index: 0 }));
        synth.state.replace(Some(vec![8]));
        verb.state.replace(Some(vec![9]));
        ctl.apply(Intent::SaveSelectionAsModule { nodes: vec![lane, effect], name: "Both".to_string() }).expect("saved");
        let text = std::fs::read_to_string(dir.path().join("Both.toml")).unwrap();
        for line in ["state = \"CA==\"", "state = \"CQ==\""] {
            assert_eq!(text.lines().filter(|l| *l == line).count(), 1, "{line} in\n{text}");
        }
        // Added, each plugin has its own state before anything else is done to it, in the order of the module (and the instrument's preset comes after both), and each slot has it.
        sink.lock().unwrap().clear();
        ctl.apply(Intent::AddFreeEffect { plugin: "module:Both".to_string() }).expect("added");
        assert_eq!(said(&sink), ["set_state [8]", "set_state [9]", "load_preset big"]);
        let added = ctl.model.instruments.len() - 1;
        assert_eq!(state_of(&ctl, NodePath::Instrument(added)), Some(vec![8]));
        assert_eq!(state_of(&ctl, NodePath::Effect { inst: added, index: 0 }), Some(vec![9]));
    }

    #[test]
    fn a_module_keeps_the_state_of_its_plugins_as_they_are_and_gives_it_back_when_it_is_added() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink::default();
        let verb = fake::install("fake:verb", "Verb", &sink);
        let (_engine, mut ctl) = controller("[[instrument]]\n[[instrument.effect]]\nplugin = \"fake:verb\"\nstate = \"BAUG\"\n");
        ctl.modules_dir = Some(dir.path().to_path_buf());
        ctl.catalog.add(vec![PluginInfo { name: "Verb".to_string(), id: "fake:verb".to_string(), is_instrument: false, param_count: 2, preset_count: 1, path: String::new(), scan_ms: 0 }]);
        let effect = node(&ctl, NodePath::Effect { inst: 0, index: 0 });
        // The plugin's state changed since it was loaded: the module has it as it is.
        verb.state.replace(Some(vec![8]));
        ctl.apply(Intent::SaveModule { node: effect, name: "Room".to_string() }).expect("saved");
        let text = std::fs::read_to_string(dir.path().join("Room.toml")).unwrap();
        assert!(text.lines().any(|line| line == "state = \"CA==\""), "{text}");
        // Added, the plugin has it before anything else is done to it, and so has its slot.
        sink.lock().unwrap().clear();
        ctl.apply(Intent::AddFreeEffect { plugin: "module:Room".to_string() }).expect("added");
        assert_eq!(said(&sink), ["set_state [8]"]);
        let added = ctl.model.instruments.len() - 1;
        assert_eq!(state_of(&ctl, NodePath::Effect { inst: added, index: 0 }), Some(vec![8]));
    }

    #[test]
    fn the_session_as_text_is_what_a_save_writes_with_the_state_that_the_plugins_have_now() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Sink::default();
        let synth = fake::install("fake:synth", "Synth", &sink);
        let _verb = fake::install("fake:verb", "Verb", &sink);
        let (_engine, mut ctl) = controller(LANE);
        // The plugin's state is another by the time the text is asked for: the text has it (`Bwc=` is [7, 7]), as the file of a save does.
        synth.state.replace(Some(vec![7, 7]));
        let text = ctl.session_text().expect("the text");
        assert!(text.contains("state = \"Bwc=\""), "the state that the plugin has now:\n{text}");
        assert!(text.contains("state = \"BAUG\""), "and the state of the other, which it still has:\n{text}");
        let path = dir.path().join("s.toml");
        ctl.session_path = Some(path.clone());
        ctl.apply(Intent::Save).expect("saved");
        assert_eq!(text, std::fs::read_to_string(path).unwrap(), "the file of a save is that text");
    }
}
