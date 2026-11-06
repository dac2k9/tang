//! What a CLAP plugin asks its host to do on the main thread (step 115): `request_callback`, which a plugin makes from any thread to have the host call its `on_main_thread` on the thread that the host calls main. The Tang plugin
//! makes it when a page opens another session (the engine is made again by the main thread) and when the switch that opens its page is pressed in a block; other plugins make it for what they have to do away from the audio thread
//! (a file that is loaded, a window that is to be drawn again). Tang as a host used to ignore it, which left every one of those undone.
//!
//! **The plugin is on the audio thread, and the call is the main thread's.** As for its window (`clap_editor`), the call is made through the main-thread handle that was made when the plugin was loaded, with its lifetime made
//! `'static`, kept in a table of the main thread's (below) beside a token that says whether the plugin is still there. The table is read by whoever turns the controller (`Controller::poll`, and the loop of `tang window`),
//! which is on the thread that loaded the plugins, and a plugin that is let go of takes its entry out first (`ClapPlugin::drop`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use clack_host::prelude::PluginMainThreadHandle;

use super::clap_editor::Asked;

/// The call into a plugin's `on_main_thread`. The plugin's own is `RealCall`; a test's write it down.
pub trait MainThreadCall {
    fn call(&self);
}

/// A plugin's `on_main_thread`, reached through its main-thread handle.
pub struct RealCall {
    /// The handle that was made when the plugin was loaded, made `'static`: good for as long as the plugin is.
    handle: RefCell<PluginMainThreadHandle<'static>>,
}

impl RealCall {
    /// # Safety
    ///
    /// `handle` must be the handle of an instance that is not destroyed while this is used: the entry is taken out of the table first thing when the plugin is let go of (`ClapPlugin::drop`), and `run` calls nothing in a plugin
    /// whose token is gone. The call is the main thread's.
    pub unsafe fn new(handle: PluginMainThreadHandle<'_>) -> RealCall {
        // SAFETY: only the lifetime is made another; the caller keeps the instance for as long as this is used.
        let handle = unsafe { std::mem::transmute::<PluginMainThreadHandle<'_>, PluginMainThreadHandle<'static>>(handle) };
        RealCall { handle: RefCell::new(handle) }
    }
}

impl MainThreadCall for RealCall {
    fn call(&self) {
        let handle = self.handle.borrow_mut();
        let raw = handle.as_raw();
        if let Some(on_main_thread) = raw.on_main_thread {
            // SAFETY: the function is the plugin's own, called with the plugin it belongs to, on the main thread, which is where the plugin asked for it to be called.
            unsafe { on_main_thread(raw) }
        }
    }
}

/// One plugin in the table: how to call it, what it has asked, and whether it is still there.
struct Entry {
    call: Rc<dyn MainThreadCall>,
    asked: Arc<Asked>,
    alive: Weak<()>,
}

/// A plugin's place in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CallbackId(u64);

#[derive(Default)]
struct Table {
    next: u64,
    entries: BTreeMap<CallbackId, Entry>,
}

thread_local! {
    /// The plugins whose calls back this thread makes: the ones that it loaded.
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
}

/// Put a plugin in the table: `run` calls it when `asked` says that it asked, for as long as `alive` is held by the plugin.
pub fn register(call: Rc<dyn MainThreadCall>, asked: Arc<Asked>, alive: &Arc<()>) -> CallbackId {
    TABLE.with(|table| {
        let mut table = table.borrow_mut();
        let id = CallbackId(table.next);
        table.next += 1;
        table.entries.insert(id, Entry { call, asked, alive: Arc::downgrade(alive) });
        id
    })
}

/// The plugin is going: take it out of the table. Called first in the plugin's `Drop`, before it terminates anything.
pub fn forget(id: CallbackId) {
    // (Dropped after the borrow is let go of, as the registry of the windows does.)
    let entry = TABLE.with(|table| table.borrow_mut().entries.remove(&id));
    drop(entry);
}

/// Make the calls that plugins have asked for since the last time, once for each: the main thread's turn, which whoever turns the controller gives every time it goes round. A plugin that has been let go of on another thread is
/// found by its token and let go of here, and is not called.
pub fn run() {
    // (The calls are made after the borrow is let go of: a plugin that is called may load another plugin, or let one go, and so change the table.)
    let due: Vec<Rc<dyn MainThreadCall>> = TABLE.with(|table| {
        let mut table = table.borrow_mut();
        table.entries.retain(|_, entry| entry.alive.strong_count() > 0);
        table.entries.values().filter(|entry| entry.asked.take_callback()).map(|entry| entry.call.clone()).collect()
    });
    for call in due {
        call.call();
    }
}

/// How many plugins are in the table (for a test).
#[cfg(test)]
fn len() -> usize {
    TABLE.with(|table| table.borrow().entries.len())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A plugin that counts the calls it is given, and may do something in one.
    struct Counting {
        calls: Cell<usize>,
        inside: RefCell<Option<Box<dyn Fn()>>>,
    }

    impl Counting {
        fn new() -> Rc<Counting> {
            Rc::new(Counting { calls: Cell::new(0), inside: RefCell::new(None) })
        }
    }

    impl MainThreadCall for Counting {
        fn call(&self) {
            self.calls.set(self.calls.get() + 1);
            if let Some(inside) = self.inside.borrow().as_ref() {
                inside();
            }
        }
    }

    /// A plugin as `load` has it: what is asked, the token that it holds, and the entry.
    fn plugin(call: &Rc<Counting>) -> (Arc<Asked>, Arc<()>, CallbackId) {
        let asked = Arc::new(Asked::default());
        let alive = Arc::new(());
        let id = register(call.clone(), asked.clone(), &alive);
        (asked, alive, id)
    }

    #[test]
    fn a_plugin_that_asked_is_called_once_for_each_ask_and_one_that_did_not_is_not_called() {
        let (asking, quiet) = (Counting::new(), Counting::new());
        let (asked, _alive, asking_id) = plugin(&asking);
        let (_, _alive_too, quiet_id) = plugin(&quiet);
        run();
        assert_eq!((asking.calls.get(), quiet.calls.get()), (0, 0), "nobody asked");
        asked.request_callback();
        run();
        assert_eq!((asking.calls.get(), quiet.calls.get()), (1, 0), "the one that asked");
        run();
        assert_eq!(asking.calls.get(), 1, "once for an ask: it is not called again until it asks again");
        asked.request_callback();
        asked.request_callback();
        run();
        assert_eq!(asking.calls.get(), 2, "asks that come together are one call: it is what a host does with the flag");
        forget(asking_id);
        forget(quiet_id);
    }

    #[test]
    fn a_plugin_that_has_been_forgotten_or_let_go_of_is_not_called() {
        let (forgotten, gone) = (Counting::new(), Counting::new());
        let (asked, _alive, id) = plugin(&forgotten);
        let (asked_gone, alive_gone, id_gone) = plugin(&gone);
        let before = len();
        asked.request_callback();
        forget(id);
        assert_eq!(len(), before - 1, "taken out of the table");
        asked_gone.request_callback();
        // (A plugin that is let go of on another thread cannot take its entry out of this thread's table: its token is what says so.)
        drop(alive_gone);
        run();
        assert_eq!((forgotten.calls.get(), gone.calls.get()), (0, 0));
        assert_eq!(len(), before - 2, "the one that was found gone is let go of");
        forget(id_gone);
    }

    #[test]
    fn a_plugin_that_loads_another_or_lets_one_go_in_its_call_changes_the_table_without_a_panic() {
        let (first, second, third) = (Counting::new(), Counting::new(), Counting::new());
        let (asked_first, _alive_first, id_first) = plugin(&first);
        let (asked_second, _alive_second, id_second) = plugin(&second);
        // The first, in its call, lets the second go and loads a third: what a plugin that is called does when it makes a session over.
        let (gone, loaded) = (id_second, third.clone());
        *first.inside.borrow_mut() = Some(Box::new(move || {
            forget(gone);
            let (asked, alive, id) = plugin(&loaded);
            asked.request_callback();
            // (Kept, so that its token lasts for the rest of the test.)
            std::mem::forget((alive, id));
        }));
        asked_first.request_callback();
        asked_second.request_callback();
        run();
        // (Both were asked for when the turn began, and are called in it, the one that was let go of included: it was due.)
        assert_eq!((first.calls.get(), second.calls.get()), (1, 1));
        run();
        assert_eq!(third.calls.get(), 1, "the plugin that was loaded in a call is called in the next turn, for what it asked");
        assert_eq!(second.calls.get(), 1, "and the one that was let go of is not called again");
        forget(id_first);
    }

    #[test]
    fn the_call_is_the_main_threads_and_a_table_is_the_threads_own() {
        let call = Counting::new();
        let (asked, _alive, id) = plugin(&call);
        asked.request_callback();
        // Another thread has a table of its own, in which this plugin is not: its turn does nothing for it.
        std::thread::spawn(run).join().unwrap();
        assert_eq!(call.calls.get(), 0);
        run();
        assert_eq!(call.calls.get(), 1);
        forget(id);
    }
}
