//! A plugin that has a window of its own and a state, for the tests of what a session does with one (step 112, part C).
//!
//! `install` makes `plugin::load` give one for a name: each load makes a new plugin, with an editor of its own in the registry of the windows, and everything that is asked of either, in the order that it was asked, is in one log.
//! (Only tests have it: `plugin::load` looks here first when it is a test.)

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use super::editors::{self, EditorId, fakes::{FakeEditor, Log}};
use super::{ParameterInfo, Plugin, Preset};

/// What was asked of the plugin and of its editor, in order.
pub type Sink = Arc<Mutex<Vec<String>>>;

/// What the next plugin that is loaded is like, which a test sets before it loads one.
#[derive(Default)]
pub struct Handle {
    /// The state that its editors give: a test changes it to be a plugin whose state changed. (Shared by every plugin that is made for the name.)
    pub state: Rc<RefCell<Option<Vec<u8>>>>,
    /// Why its editors do not take a state, if they do not (read when a plugin is made).
    pub refuses_state: Cell<Option<&'static str>>,
    /// What its parameters are, as its editors say.
    pub values: Rc<RefCell<Vec<(u32, f32)>>>,
    /// What the person has done to its knobs, which its editors give once (`Editor::take_edits`).
    pub edits: Rc<RefCell<Vec<(u32, f32)>>>,
    /// Whether the person has closed the window: a test sets it, and a poll finds it.
    pub person_closed: Rc<Cell<bool>>,
    /// What the registry looks at to know that a plugin is still there, held here: the plugins go to the audio thread, which the tests' stub takes the commands of and drops, and their editors are to stay for the test.
    alive: RefCell<Vec<Arc<()>>>,
}

type Make = Rc<dyn Fn() -> Box<dyn Plugin>>;

thread_local! {
    static INSTALLED: RefCell<HashMap<String, Make>> = RefCell::default();
}

/// Make `plugin::load(source)` give a plugin called `name`, with the editor and the sink that this makes. The handle is for what the plugins are like.
pub fn install(source: &str, name: &'static str, sink: &Sink) -> Rc<Handle> {
    let handle = Rc::new(Handle::default());
    let (made_by, sink) = (handle.clone(), sink.clone());
    let make: Make = Rc::new(move || {
        let mut editor = FakeEditor::unshared(name, &Log::default());
        editor.sink = Some(sink.clone());
        editor.refuses_state = made_by.refuses_state.get();
        editor.state = made_by.state.clone();
        editor.values = made_by.values.clone();
        editor.edits = made_by.edits.clone();
        editor.person_closed = made_by.person_closed.clone();
        let alive = Arc::new(());
        let id = editors::register(Rc::new(editor), &alive);
        made_by.alive.borrow_mut().push(alive);
        Box::new(FakePlugin { name, sink: sink.clone(), editor: id })
    });
    INSTALLED.with(|installed| installed.borrow_mut().insert(source.to_string(), make));
    handle
}

/// The plugin that `install` made `source` give, if it did (what `plugin::load` asks first, in a test).
pub fn load(source: &str) -> Option<Box<dyn Plugin>> {
    let make = INSTALLED.with(|installed| installed.borrow().get(source).cloned())?;
    Some(make())
}

pub struct FakePlugin {
    name: &'static str,
    sink: Sink,
    editor: EditorId,
}

impl FakePlugin {
    fn said(&self, what: String) {
        self.sink.lock().expect("a sink").push(what);
    }
}

impl Plugin for FakePlugin {
    fn name(&self) -> &str {
        self.name
    }
    fn is_instrument(&self) -> bool {
        false
    }
    fn sample_rate(&self) -> f32 {
        48_000.0
    }
    fn audio_output_count(&self) -> usize {
        2
    }
    fn audio_input_count(&self) -> usize {
        2
    }
    fn process(&mut self, _: &[(u64, [u8; 3])], _: &[&[f32]], _: &mut [&mut [f32]]) -> anyhow::Result<()> {
        Ok(())
    }
    fn parameters(&self) -> Vec<ParameterInfo> {
        (0..2).map(|index| ParameterInfo { index, name: format!("p{index}"), min: 0.0, max: 1.0, default: 0.5, ..Default::default() }).collect()
    }
    fn get_parameter(&mut self, _: u32) -> Option<f32> {
        None
    }
    fn set_parameter(&mut self, index: u32, value: f32) -> anyhow::Result<()> {
        self.said(format!("set_parameter {index} {value}"));
        Ok(())
    }
    fn presets(&self) -> Vec<Preset> {
        vec![Preset { name: "Big".to_string(), id: "big".to_string() }]
    }
    fn load_preset(&mut self, id: &str) -> anyhow::Result<()> {
        self.said(format!("load_preset {id}"));
        Ok(())
    }
    fn editor_id(&self) -> Option<EditorId> {
        Some(self.editor)
    }
}
