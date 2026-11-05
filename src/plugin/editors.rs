//! The windows of the plugins' own editors, kept by the plugin that they belong to (step 112, part B).
//!
//! A plugin's window is the main thread's, and a plugin is not: it is made on the main thread, goes to the audio thread, may come back to be kept for an undo, and is let go of on the thread of the controller. So the
//! editors are not kept in the model, or by node: a plugin that has one registers it here when it is loaded, under an [`EditorId`] that it keeps (`Plugin::editor_id`), and forgets it when it is let go of, which
//! closes the window first (a window that is open when the plugin is terminated is a view of a plugin that is gone). A node's slot has the id, and the controller opens the window by it.
//!
//! The table is the main thread's own (a thread local): what is registered on another thread is not found from this one, and says so. Nothing that calls into a plugin is done with the table borrowed: a plugin's
//! callbacks (the window's, the frame's, a parameter's) must find it as it is. The windows' events are turned by [`window::pump`](crate::window::pump), by whoever has a window open (`Controller::pump_windows`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use super::{Editor, OpenEditor};

/// Which plugin's window: given out when the plugin registers its editor, and not twice in a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EditorId(u64);

struct Entry {
    // (The window goes before the editor that made it: it holds the plugin's view, and the editor the library that the view is from.)
    open: Option<Box<dyn OpenEditor>>,
    editor: Rc<dyn Editor>,
    /// The other end of what the plugin holds (`register`): once it is gone, so is the plugin, and a window that is still open is a view of nothing.
    alive: Weak<()>,
}

#[derive(Default)]
struct Table {
    next: u64,
    entries: HashMap<EditorId, Entry>,
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
}

/// Keep the editor of a plugin that is loaded, and give the plugin its id. `alive` is what the plugin holds for as long as it is: the table looks at it (`poll`) to find a plugin that was let go of on another thread,
/// whose `forget` could not reach this table.
pub fn register(editor: Rc<dyn Editor>, alive: &Arc<()>) -> EditorId {
    TABLE.with(|table| {
        let mut table = table.borrow_mut();
        let id = EditorId(table.next);
        table.next += 1;
        table.entries.insert(id, Entry { open: None, editor, alive: Arc::downgrade(alive) });
        id
    })
}

/// The plugin is going: close its window, and let go of its editor. Called first in the plugin's `Drop`, before it terminates anything.
pub fn forget(id: EditorId) {
    // (Dropped after the borrow is let go of: dropping an open window calls into the plugin.)
    let entry = TABLE.with(|table| table.borrow_mut().entries.remove(&id));
    drop(entry);
}

/// Open the window of the plugin `id`, called `title`, after telling its controller the `values` that the session has for its parameters (so that the window shows them); one that is open is brought to the front
/// instead. `focus` is whether the window takes the keyboard. The reason is in the words when it cannot be opened (the plugin has no window of its own, the machine has no screen).
pub fn open(id: EditorId, title: &str, focus: bool, values: &[(u32, f32)]) -> Result<(), String> {
    enum Found {
        Missing,
        Open,
        Closed(Rc<dyn Editor>),
    }
    let found = TABLE.with(|table| match table.borrow().entries.get(&id) {
        None => Found::Missing,
        Some(Entry { open: Some(_), .. }) => Found::Open,
        Some(entry) => Found::Closed(entry.editor.clone()),
    });
    match found {
        Found::Missing => Err("this plugin's window is not to be had here (the plugin is gone, or it was made on another thread)".to_string()),
        Found::Open => {
            with_open(id, |open| open.raise(focus));
            Ok(())
        }
        Found::Closed(editor) => {
            editor.set_values(values);
            let open = editor.open(title, focus)?;
            // (An entry that went in the meantime leaves the window to be dropped, which closes it.)
            TABLE.with(|table| {
                if let Some(entry) = table.borrow_mut().entries.get_mut(&id) {
                    entry.open = Some(open);
                }
            });
            Ok(())
        }
    }
}

/// Close the window of the plugin `id`, if it is open; its editor is kept, and the window can be opened again.
pub fn close(id: EditorId) {
    let open = TABLE.with(|table| table.borrow_mut().entries.get_mut(&id).and_then(|entry| entry.open.take()));
    drop(open);
}

/// Whether the plugin `id` has a window open.
pub fn is_open(id: EditorId) -> bool {
    TABLE.with(|table| table.borrow().entries.get(&id).is_some_and(|entry| entry.open.is_some()))
}

/// Whether any window is open: what the loops that turn the windows' events ask, to know whether to.
pub fn any_open() -> bool {
    TABLE.with(|table| table.borrow().entries.values().any(|entry| entry.open.is_some()))
}

/// The plugins that have a window open.
pub fn open_ids() -> Vec<EditorId> {
    TABLE.with(|table| table.borrow().entries.iter().filter(|(_, entry)| entry.open.is_some()).map(|(id, _)| *id).collect())
}

/// Look at `id`'s window while it is open: `with` is given the open editor (its size, a picture of it). `None` if it is not open.
pub fn with_open<R>(id: EditorId, with: impl FnOnce(&mut dyn OpenEditor) -> R) -> Option<R> {
    // (Taken out of the table for the call and put back after it, so that what `with` calls into a plugin does not find the table borrowed.)
    let mut open = TABLE.with(|table| table.borrow_mut().entries.get_mut(&id).and_then(|entry| entry.open.take()))?;
    let result = with(&mut *open);
    TABLE.with(|table| {
        if let Some(entry) = table.borrow_mut().entries.get_mut(&id).filter(|entry| entry.open.is_none()) {
            entry.open = Some(open);
        }
    });
    Some(result)
}

/// See to every open window (`OpenEditor::poll`: a size that the person made is told to the plugin) and drop those that the person closed. A plugin that was let go of on another thread, which could not forget its entry, is
/// found here, and its window is closed (too late for the plugin, which has been terminated: it is said).
pub fn poll() {
    let gone: Vec<EditorId> = TABLE.with(|table| table.borrow().entries.iter().filter(|(_, entry)| entry.alive.strong_count() == 0).map(|(id, _)| *id).collect());
    for id in gone {
        if is_open(id) {
            log::error!("a plugin was let go of on a thread that is not the main one while its window was open: the window is closed now");
        }
        forget(id);
    }
    for id in open_ids() {
        // (A window that `with_open` finds is still open, and is put back; one that says it is not is not.)
        if with_open(id, |open| open.poll()) == Some(false) {
            close(id);
        }
    }
}

/// What the plugins' windows have changed since they were last asked, for every plugin that has an editor, whether its window is open or not: the plugin's id, its parameter's index and the value
/// (`Editor::take_edits`).
pub fn take_edits() -> Vec<(EditorId, u32, f32)> {
    let mut editors: Vec<(EditorId, Rc<dyn Editor>)> = TABLE.with(|table| table.borrow().entries.iter().map(|(id, entry)| (*id, entry.editor.clone())).collect());
    // (In the order that the plugins were made, so that what several windows said is applied in the same order every time.)
    editors.sort_by_key(|(id, _)| id.0);
    let mut edits = Vec::new();
    for (id, editor) in editors {
        edits.extend(editor.take_edits().into_iter().map(|(index, value)| (id, index, value)));
    }
    edits
}

/// Tell the plugin `id`'s controller the values that the session now has, if its window is open (a window that is not has nothing to show them in: it is told them when it opens).
pub fn set_values(id: EditorId, values: &[(u32, f32)]) {
    let editor = TABLE.with(|table| table.borrow().entries.get(&id).filter(|entry| entry.open.is_some()).map(|entry| entry.editor.clone()));
    if let Some(editor) = editor {
        editor.set_values(values);
    }
}

/// Editors that are not a plugin's, for the tests of the registry and of what uses it (the controller's).
#[cfg(test)]
pub mod fakes {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::{Editor, OpenEditor};

    /// What the fakes were asked, in order: one log for all of them.
    pub type Log = Rc<RefCell<Vec<String>>>;

    pub fn said(log: &Log) -> Vec<String> {
        log.borrow().clone()
    }

    pub struct FakeEditor {
        pub name: &'static str,
        pub log: Log,
        /// Why it cannot be opened, if it cannot.
        pub refuses: Option<&'static str>,
        /// Whether the person has closed the window that it made last.
        pub person_closed: Rc<Cell<bool>>,
        /// What the person has done to the plugin's knobs, as `take_edits` gives it.
        pub edits: RefCell<Vec<(u32, f32)>>,
    }

    impl FakeEditor {
        pub fn new(name: &'static str, log: &Log) -> Rc<FakeEditor> {
            Rc::new(FakeEditor { name, log: log.clone(), refuses: None, person_closed: Rc::new(Cell::new(false)), edits: RefCell::default() })
        }
    }

    impl Editor for FakeEditor {
        fn open(&self, title: &str, focus: bool) -> Result<Box<dyn OpenEditor>, String> {
            if let Some(why) = self.refuses {
                return Err(why.to_string());
            }
            self.person_closed.set(false);
            self.log.borrow_mut().push(format!("{}: open {title:?} focus={focus}", self.name));
            Ok(Box::new(FakeOpen { name: self.name, log: self.log.clone(), person_closed: self.person_closed.clone() }))
        }
        fn take_edits(&self) -> Vec<(u32, f32)> {
            std::mem::take(&mut *self.edits.borrow_mut())
        }
        fn set_values(&self, values: &[(u32, f32)]) {
            self.log.borrow_mut().push(format!("{}: values {values:?}", self.name));
        }
    }

    impl Drop for FakeEditor {
        fn drop(&mut self) {
            self.log.borrow_mut().push(format!("{}: editor dropped", self.name));
        }
    }

    struct FakeOpen {
        name: &'static str,
        log: Log,
        person_closed: Rc<Cell<bool>>,
    }

    impl OpenEditor for FakeOpen {
        fn poll(&mut self) -> bool {
            self.log.borrow_mut().push(format!("{}: poll", self.name));
            !self.person_closed.get()
        }
        fn raise(&mut self, focus: bool) {
            self.log.borrow_mut().push(format!("{}: raise focus={focus}", self.name));
        }
        fn close(&mut self) {}
        fn size(&self) -> (i32, i32) {
            (300, 200)
        }
        fn snapshot_png(&self) -> Option<Vec<u8>> {
            None
        }
        fn describe(&self) -> String {
            String::new()
        }
    }

    impl Drop for FakeOpen {
        fn drop(&mut self) {
            self.log.borrow_mut().push(format!("{}: window closed", self.name));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::fakes::{FakeEditor, Log, said};
    use super::*;

    /// A plugin's editor in the table, and the token that the plugin holds.
    fn registered(name: &'static str, log: &Log) -> (EditorId, Rc<FakeEditor>, Arc<()>) {
        let editor = FakeEditor::new(name, log);
        let alive = Arc::new(());
        let id = register(editor.clone(), &alive);
        (id, editor, alive)
    }

    #[test]
    fn a_window_opens_once_and_is_brought_to_the_front_when_it_is_asked_for_again() {
        let log = Log::default();
        let (id, _editor, _alive) = registered("A", &log);
        assert!(!is_open(id) && !any_open());
        open(id, "Pianoteq", false, &[(3, 0.5)]).expect("opens");
        assert!(is_open(id) && any_open());
        assert_eq!(said(&log), ["A: values [(3, 0.5)]", "A: open \"Pianoteq\" focus=false"], "the controller is told the values first, so that the window shows them, and whether it takes the keyboard");
        open(id, "Pianoteq", true, &[(3, 0.9)]).expect("raises");
        assert_eq!(said(&log)[2..], ["A: raise focus=true"], "no second window and no second set of values, and the window is raised as asked");
    }

    #[test]
    fn a_window_that_is_asked_for_by_an_id_that_is_not_there_is_refused_in_words() {
        let log = Log::default();
        let (id, _editor, _alive) = registered("A", &log);
        forget(id);
        let why = open(id, "x", true, &[]).expect_err("refused");
        assert!(why.contains("not to be had here"), "{why}");
        assert!(!is_open(id));
    }

    #[test]
    fn forgetting_a_plugin_closes_its_window_before_it_lets_go_of_its_editor() {
        let log = Log::default();
        let (id, editor, _alive) = registered("A", &log);
        open(id, "A", true, &[]).expect("opens");
        drop(editor);
        log.borrow_mut().clear();
        forget(id);
        assert_eq!(said(&log), ["A: window closed", "A: editor dropped"], "the view goes before the library that it is from");
        assert!(!is_open(id) && !any_open());
        assert!(open(id, "A", true, &[]).is_err(), "and the entry is gone");
    }

    #[test]
    fn closing_a_window_keeps_the_editor_so_that_it_opens_again() {
        let log = Log::default();
        let (id, _editor, _alive) = registered("A", &log);
        open(id, "A", true, &[]).expect("opens");
        close(id);
        assert!(!is_open(id));
        close(id);
        open(id, "A", true, &[]).expect("opens again");
        assert!(is_open(id));
        let log = said(&log);
        assert_eq!(log.iter().filter(|line| line.contains("open \"A\"")).count(), 2, "{log:?}");
        assert_eq!(log.iter().filter(|line| line.ends_with("window closed")).count(), 1, "{log:?}");
    }

    #[test]
    fn polling_drops_the_window_that_the_person_closed_and_keeps_the_others() {
        let log = Log::default();
        let (a, editor_a, _alive_a) = registered("A", &log);
        let (b, _editor_b, _alive_b) = registered("B", &log);
        open(a, "A", true, &[]).expect("opens");
        open(b, "B", true, &[]).expect("opens");
        poll();
        assert!(is_open(a) && is_open(b), "both are open");
        editor_a.person_closed.set(true);
        poll();
        assert!(!is_open(a) && is_open(b));
        assert_eq!(open_ids(), [b]);
        assert!(said(&log).contains(&"A: window closed".to_string()));
        open(a, "A", true, &[]).expect("and it can be opened again");
        assert!(is_open(a));
    }

    #[test]
    fn a_plugin_that_was_let_go_of_somewhere_else_is_found_by_polling_and_its_window_closed() {
        let log = Log::default();
        let (id, editor, alive) = registered("A", &log);
        // (The table has the only hold of the editor, as a plugin's does.)
        drop(editor);
        open(id, "A", true, &[]).expect("opens");
        poll();
        assert!(is_open(id), "the plugin is there");
        drop(alive);
        poll();
        assert!(!is_open(id) && !any_open(), "the plugin is gone, and so is its window");
        assert!(said(&log).contains(&"A: editor dropped".to_string()), "and the entry");
        assert!(open(id, "A", true, &[]).is_err());
    }

    #[test]
    fn the_edits_of_every_plugin_are_taken_once_with_its_id_whether_its_window_is_open_or_not() {
        let log = Log::default();
        let (a, editor_a, _alive_a) = registered("A", &log);
        let (b, editor_b, _alive_b) = registered("B", &log);
        open(b, "B", true, &[]).expect("opens");
        editor_a.edits.borrow_mut().extend([(1, 0.25), (1, 0.5)]);
        editor_b.edits.borrow_mut().push((7, 1.0));
        let mut taken = take_edits();
        taken.sort_by_key(|(_, index, _)| *index);
        assert_eq!(taken, [(a, 1, 0.25), (a, 1, 0.5), (b, 7, 1.0)], "in the order that each plugin did them");
        assert!(take_edits().is_empty(), "each is told once");
    }

    #[test]
    fn values_reach_a_plugin_while_its_window_is_open_and_not_before_or_after() {
        let log = Log::default();
        let (id, _editor, _alive) = registered("A", &log);
        set_values(id, &[(1, 0.1)]);
        assert!(said(&log).is_empty(), "a window that is not open has nothing to show them in");
        open(id, "A", true, &[]).expect("opens");
        set_values(id, &[(2, 0.2)]);
        close(id);
        set_values(id, &[(3, 0.3)]);
        let values: Vec<String> = said(&log).into_iter().filter(|line| line.contains("values")).collect();
        assert_eq!(values, ["A: values []", "A: values [(2, 0.2)]"]);
    }

    #[test]
    fn a_window_that_cannot_be_opened_says_why_and_nothing_is_kept() {
        let log = Log::default();
        let editor = Rc::new(FakeEditor { name: "A", log: log.clone(), refuses: Some("this plugin has no window of its own"), person_closed: Rc::default(), edits: RefCell::default() });
        let alive = Arc::new(());
        let id = register(editor, &alive);
        assert_eq!(open(id, "A", true, &[]), Err("this plugin has no window of its own".to_string()));
        assert!(!is_open(id) && !any_open());
    }

    #[test]
    fn an_id_is_not_given_twice() {
        let log = Log::default();
        let (a, _ea, _aa) = registered("A", &log);
        let (b, _eb, _ab) = registered("B", &log);
        forget(a);
        let (c, _ec, _ac) = registered("C", &log);
        assert!(a != b && b != c && a != c, "{a:?} {b:?} {c:?}: not even one that was forgotten");
    }

    #[test]
    fn the_table_is_the_threads_own() {
        let log = Log::default();
        let (id, _editor, _alive) = registered("A", &log);
        let from_another = std::thread::spawn(move || (open(id, "A", true, &[]), any_open(), is_open(id))).join().expect("ends");
        assert!(from_another.0.is_err() && !from_another.1 && !from_another.2, "{from_another:?}");
        assert!(open(id, "A", true, &[]).is_ok(), "here it is");
    }

    #[test]
    fn with_open_looks_at_a_window_that_is_open_and_leaves_it_so() {
        let log = Log::default();
        let (id, _editor, _alive) = registered("A", &log);
        assert_eq!(with_open(id, |open| open.size()), None, "nothing is open");
        open(id, "A", true, &[]).expect("opens");
        assert_eq!(with_open(id, |open| open.size()), Some((300, 200)));
        assert!(is_open(id), "it is put back");
    }
}
