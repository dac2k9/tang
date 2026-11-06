//! The plugin catalog: what is installed on this machine, as the selector
//! popups list it.
//!
//! A full scan instantiates every installed plugin and can take seconds, so it
//! runs on its own thread and delivers one batch per format: the built-ins
//! arrive at once, VST3, which loads each plugin, last. The owner of the
//! catalog calls `poll` to fold in what has arrived; the scan is over when the
//! thread has exited and `poll` has seen its channel close.

use crossbeam_channel::{Receiver, TryRecvError};
use serde::{Deserialize, Serialize};

use crate::app::model::format_from_id;
use crate::plugin::{self, PluginInfo};

/// What a client is told of an installed plugin: enough to choose by. Never where it is
/// installed (`PluginInfo::path`): a client names a plugin by its `id`, and the program
/// loads only what is in its own catalog (docs/web-ui.md §8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends the catalog"))]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    /// What the plugin is loaded by: `builtin:osc`, a CLAP id, an LV2 URI, a VST3 name.
    pub id: String,
    pub name: String,
    /// `Built-in`, `LV2`, `CLAP`, `VST3` or `Module` (effects and instruments with the cables between them, that the library has, which is added as an effect that stands by itself and not loaded as a plugin).
    pub format: String,
    pub is_instrument: bool,
    /// How many parameters it has.
    pub params: usize,
    /// How many presets it has.
    pub presets: usize,
}

#[derive(Default)]
pub struct Catalog {
    /// Everything found so far, sorted by name without regard to case.
    plugins: Vec<PluginInfo>,
    /// The modules of the library (`app::module`): effects and instruments with the cables between them, that are saved in files, as `module:<name>`, sorted by name without regard to case. They are listed with the plugins to
    /// a client (`entries`) and are not among `plugins`, which is what a plugin is loaded from and what tells whether a scan has been made.
    modules: Vec<PluginInfo>,
    /// Where the scan thread delivers its batches; `None` when no scan is
    /// running.
    batches: Option<Receiver<Vec<PluginInfo>>>,
    /// Goes up whenever plugins are added: a client that was told of an earlier one has to be told again.
    rev: u64,
}

impl Catalog {
    /// Start scanning on a background thread. Results arrive through `poll`.
    pub fn scan(&mut self) {
        self.start(true);
    }

    /// List the built-in plugins and nothing else, at once: no plugin is loaded for that. What a program that is a guest in another's process does until it is told to look further (Tang inside a host, `embed`: the host's own
    /// threads turn the run loop, which a plugin of the JUCE kind that is loaded and let go of on another thread does not survive, f179).
    pub fn builtins(&mut self) {
        self.add(plugin::builtin::enumerate_plugins());
    }

    /// Start scanning the plugins that are installed (LV2, CLAP and VST3: each is loaded to be asked what it is), on a background thread, and not the built-ins (`builtins` has them). Results arrive through `poll`.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the page of a plugin in a host asks for the scan, when it is told to"))]
    pub fn scan_installed(&mut self) {
        self.start(false);
    }

    fn start(&mut self, builtins: bool) {
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::spawn(move || {
            if builtins {
                let _ = tx.send(plugin::builtin::enumerate_plugins());
            }

            #[cfg(feature = "lv2")]
            let _ = tx.send(plugin::lv2::enumerate_plugins());

            let _ = tx.send(plugin::clap::enumerate_plugins());

            #[cfg(feature = "vst3")]
            let _ = tx.send(plugin::vst3::enumerate_plugins());
        });
        self.follow(rx);
    }

    /// Start scanning unless a scan is under way or has been done: for a front-end that needs the
    /// catalog without having started the scan itself (`tang serve`).
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only `tang serve` scans for a front-end that has not"))]
    pub fn ensure_scan(&mut self) {
        if self.batches.is_none() && self.plugins.is_empty() {
            self.scan();
        }
    }

    /// Take batches from `batches` the way a scan delivers them; the scan is
    /// over when the sending side is dropped.
    pub fn follow(&mut self, batches: Receiver<Vec<PluginInfo>>) {
        self.batches = Some(batches);
    }

    /// Everything found so far, sorted by name without regard to case.
    pub fn plugins(&self) -> &[PluginInfo] {
        &self.plugins
    }

    /// The modules of the library, sorted by name without regard to case.
    pub fn modules(&self) -> &[PluginInfo] {
        &self.modules
    }

    /// What the terminal's selectors list: the modules of the library, and then the plugins, each by name; a row of a selector names what it is by its place in this list. (Only the selector of an
    /// effect that stands by itself shows the modules: a module goes in lanes of its own, whole.)
    pub fn listed(&self) -> Vec<&PluginInfo> {
        self.modules.iter().chain(&self.plugins).collect()
    }

    /// Put these in the place of the modules the catalog lists: the library was read again. True, and the revision up, if they are not the modules that it listed (a client has to be told again).
    pub fn set_modules(&mut self, modules: Vec<PluginInfo>) -> bool {
        let key = |list: &[PluginInfo]| list.iter().map(|module| (module.id.clone(), module.param_count)).collect::<Vec<_>>();
        if key(&self.modules) == key(&modules) {
            return false;
        }
        self.modules = modules;
        self.rev += 1;
        true
    }

    /// True until the scan has finished (or if none was ever started: false).
    pub fn is_scanning(&self) -> bool {
        self.batches.is_some()
    }

    /// How many times plugins have been added: it changes when what `entries` says does.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends the catalog"))]
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// The plugins and the modules as a client is told of them: by name without regard to case, a plugin before a module of the same name.
    #[cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime sends the catalog"))]
    pub fn entries(&self) -> Vec<CatalogEntry> {
        let mut all: Vec<&PluginInfo> = self.plugins.iter().chain(&self.modules).collect();
        all.sort_by_key(|p| p.name.to_lowercase());
        all.into_iter()
            .map(|p| CatalogEntry {
                id: p.id.clone(),
                name: p.name.clone(),
                format: format_from_id(&p.id),
                is_instrument: p.is_instrument,
                params: p.param_count,
                presets: p.preset_count,
            })
            .collect()
    }

    /// Add plugins to the catalog: those that it does not list already, by `id` (a batch that comes twice, and a plugin that is found in two places, are listed once: a plugin is loaded by its id, and the first is the one
    /// that is found). True if any was added.
    pub fn add(&mut self, batch: Vec<PluginInfo>) -> bool {
        let mut known: std::collections::HashSet<String> = self.plugins.iter().map(|plugin| plugin.id.clone()).collect();
        let batch: Vec<PluginInfo> = batch.into_iter().filter(|plugin| known.insert(plugin.id.clone())).collect();
        if batch.is_empty() {
            return false;
        }
        self.plugins.extend(batch);
        // Stable, so plugins with the same name stay in the order they arrived
        // in: built-in, LV2, CLAP, VST3.
        self.plugins.sort_by_key(|p| p.name.to_lowercase());
        self.rev += 1;
        true
    }

    /// Fold in what the scan has delivered since the last call. True if
    /// plugins were added.
    pub fn poll(&mut self) -> bool {
        let mut added = false;
        while let Some(batches) = &self.batches {
            match batches.try_recv() {
                Ok(batch) => added |= self.add(batch),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.batches = None,
            }
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_ins_are_listed_at_once_by_a_catalog_that_has_started_no_scan_and_loads_no_plugin() {
        let mut catalog = Catalog::default();
        catalog.builtins();
        assert!(!catalog.is_scanning(), "no scan is under way: nothing is loaded for the built-ins");
        assert!(catalog.rev() > 0, "a client is told that there is a list");
        let ids: Vec<&str> = catalog.plugins().iter().map(|plugin| plugin.id.as_str()).collect();
        assert!(ids.contains(&"builtin:osc") && ids.contains(&"builtin:input"), "{ids:?}");
        assert!(ids.iter().all(|id| id.starts_with("builtin:")), "only the built-ins: {ids:?}");
        let (rev, listed) = (catalog.rev(), catalog.plugins().len());
        catalog.builtins();
        assert_eq!((catalog.rev(), catalog.plugins().len()), (rev, listed), "asked again, nothing is listed twice");
    }

    #[test]
    fn a_plugin_that_is_listed_already_is_not_listed_again_whichever_batch_it_comes_in() {
        let mut catalog = Catalog::default();
        assert!(catalog.add(vec![info("Reverb", "clap:reverb"), info("Reverb again", "clap:reverb"), info("Delay", "clap:delay")]), "the first of two with the same id is kept, and the others are added");
        assert_eq!(catalog.plugins().iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Delay", "Reverb"], "once within a batch");
        let rev = catalog.rev();
        assert!(!catalog.add(vec![info("Reverb from another folder", "clap:reverb")]), "and once across batches: nothing was added");
        assert_eq!(catalog.rev(), rev, "so a client is not told again");
        assert!(catalog.add(vec![info("Reverb", "vst3:Reverb")]), "a plugin of another format with the same name has an id of its own");
        assert_eq!(catalog.plugins().len(), 3);
    }

    fn info(name: &str, id: &str) -> PluginInfo {
        PluginInfo {
            name: name.to_string(),
            id: id.to_string(),
            is_instrument: false,
            param_count: 0,
            preset_count: 0,
            path: String::new(),
            scan_ms: 0,
        }
    }

    fn names(catalog: &Catalog) -> Vec<&str> {
        catalog.plugins().iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn plugins_are_sorted_by_name_without_regard_to_case() {
        let mut catalog = Catalog::default();
        assert!(catalog.add(vec![info("Zeta", "a"), info("alpha", "b")]));
        assert!(catalog.add(vec![info("Beta", "c")]));
        assert_eq!(names(&catalog), ["alpha", "Beta", "Zeta"]);
    }

    #[test]
    fn plugins_with_the_same_name_keep_the_order_they_arrived_in() {
        let mut catalog = Catalog::default();
        catalog.add(vec![info("Reverb", "builtin:reverb")]);
        catalog.add(vec![info("Other", "x")]);
        catalog.add(vec![info("reverb", "clap:reverb")]);
        catalog.add(vec![info("REVERB", "vst3:reverb")]);
        let ids: Vec<&str> = catalog.plugins().iter().filter(|p| p.name.eq_ignore_ascii_case("reverb")).map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["builtin:reverb", "clap:reverb", "vst3:reverb"]);
    }

    #[test]
    fn an_entry_says_what_a_client_chooses_by_and_nothing_of_where_the_plugin_is_installed() {
        let mut catalog = Catalog::default();
        catalog.add(vec![
            PluginInfo { name: "Reverb".into(), id: "builtin:reverb".into(), is_instrument: false, param_count: 12, preset_count: 5, path: "/secret/place".into(), scan_ms: 3 },
            PluginInfo { name: "Pianoteq".into(), id: "vst3:Pianoteq 8".into(), is_instrument: true, param_count: 300, preset_count: 0, path: "/another/secret".into(), scan_ms: 900 },
        ]);
        let json = serde_json::to_value(catalog.entries()).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {"id": "vst3:Pianoteq 8", "name": "Pianoteq", "format": "VST3", "isInstrument": true, "params": 300, "presets": 0},
                {"id": "builtin:reverb", "name": "Reverb", "format": "Built-in", "isInstrument": false, "params": 12, "presets": 5},
            ]),
            "in the order of the catalog"
        );
        assert!(!json.to_string().contains("secret"), "{json}");
        assert_eq!(serde_json::from_value::<Vec<CatalogEntry>>(json).unwrap(), catalog.entries());
    }

    #[test]
    fn the_revision_goes_up_when_plugins_are_added_and_not_when_nothing_is() {
        let mut catalog = Catalog::default();
        assert_eq!(catalog.rev(), 0);
        catalog.add(vec![]);
        assert_eq!(catalog.rev(), 0, "an empty batch is not news");
        catalog.add(vec![info("A", "a")]);
        assert_eq!(catalog.rev(), 1);
        catalog.add(vec![info("B", "b"), info("C", "c")]);
        assert_eq!(catalog.rev(), 2, "a batch, however big, is one");
    }

    #[test]
    fn a_scan_that_is_under_way_or_done_is_not_started_again() {
        // Under way: the scan that is, is the one that is followed (what it finds still arrives).
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut catalog = Catalog::default();
        catalog.follow(rx);
        catalog.ensure_scan();
        assert!(catalog.is_scanning());
        tx.send(vec![info("A", "a")]).expect("nothing took the place of the scan that was under way");
        assert!(catalog.poll());
        assert_eq!(names(&catalog), ["A"]);
        // Done: there are plugins, and no scan is followed.
        let mut catalog = Catalog::default();
        catalog.add(vec![info("A", "a")]);
        catalog.ensure_scan();
        assert!(!catalog.is_scanning(), "the catalog has what a scan found: nothing starts");
    }

    fn module(name: &str, effects: usize) -> PluginInfo {
        PluginInfo { param_count: effects, ..info(name, &format!("module:{name}")) }
    }

    #[test]
    fn the_modules_of_the_library_are_listed_with_the_plugins_by_name_and_are_none_of_the_plugins() {
        let mut catalog = Catalog::default();
        catalog.add(vec![info("Zeta", "clap:zeta"), info("Space", "builtin:space")]);
        assert!(catalog.set_modules(vec![module("hall", 2), module("Space", 3)]));
        let listed: Vec<(String, String, String, usize)> = catalog.entries().into_iter().map(|entry| (entry.id, entry.name, entry.format, entry.params)).collect();
        let words = |id: &str, name: &str, format: &str, params: usize| (id.to_string(), name.to_string(), format.to_string(), params);
        assert_eq!(
            listed,
            [
                words("module:hall", "hall", "Module", 2),
                // (A plugin before a module that has its name.)
                words("builtin:space", "Space", "Built-in", 0),
                words("module:Space", "Space", "Module", 3),
                words("clap:zeta", "Zeta", "CLAP", 0),
            ]
        );
        // What a plugin is loaded from, and what says that a scan has been made, are the plugins alone.
        assert_eq!(names(&catalog), ["Space", "Zeta"]);
        assert_eq!(catalog.modules().iter().map(|module| module.name.as_str()).collect::<Vec<_>>(), ["hall", "Space"]);
        assert!(catalog.entries().iter().filter(|entry| entry.format == "Module").all(|entry| !entry.is_instrument && entry.presets == 0));
        let mut only_modules = Catalog::default();
        only_modules.set_modules(vec![module("hall", 2)]);
        assert!(only_modules.plugins().is_empty() && !only_modules.is_scanning(), "a library is no scan: a program that has modules and no plugins still has plugins to find");
    }

    #[test]
    fn the_terminals_selectors_list_the_modules_first_and_then_the_plugins() {
        let mut catalog = Catalog::default();
        catalog.add(vec![info("Zeta", "clap:zeta"), info("Alpha", "builtin:alpha")]);
        catalog.set_modules(vec![module("hall", 2), module("Space", 3)]);
        assert_eq!(catalog.listed().iter().map(|plugin| plugin.id.as_str()).collect::<Vec<_>>(), ["module:hall", "module:Space", "builtin:alpha", "clap:zeta"]);
        assert_eq!(Catalog::default().listed().len(), 0);
    }

    #[test]
    fn the_revision_goes_up_when_the_modules_that_are_listed_change_and_not_when_the_library_is_read_again_and_is_the_same() {
        let mut catalog = Catalog::default();
        assert!(!catalog.set_modules(vec![]), "no library is the library there was");
        assert_eq!(catalog.rev(), 0);
        assert!(catalog.set_modules(vec![module("Space", 2)]));
        assert_eq!(catalog.rev(), 1);
        assert!(!catalog.set_modules(vec![module("Space", 2)]), "the same module is not news");
        assert_eq!(catalog.rev(), 1);
        // A module that is saved again with another number of plugins is the same name and another module; one that is taken away or put in is one more.
        assert!(catalog.set_modules(vec![module("Space", 3)]));
        assert!(catalog.set_modules(vec![module("Hall", 3)]), "another module with as many plugins is another listing");
        assert!(catalog.set_modules(vec![module("Hall", 3), module("Space", 3)]));
        assert!(catalog.set_modules(vec![]));
        assert_eq!(catalog.rev(), 5);
    }

    /// The built-ins as the web client's mock engine serves them for a catalog (and its tests read
    /// them): the file `web/src/client/fixtures/builtins.catalog.json`, which this test keeps
    /// current. `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` rewrites it.
    #[test]
    fn the_web_clients_fixture_of_the_builtins_is_current() {
        let mut catalog = Catalog::default();
        catalog.add(plugin::builtin::enumerate_plugins());
        let fresh = serde_json::to_string_pretty(&catalog.entries()).unwrap() + "\n";

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/src/client/fixtures/builtins.catalog.json");
        if std::env::var_os("TANG_UPDATE_TYPES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == fresh,
            "{} is not what the built-in plugins are now: run `TANG_UPDATE_TYPES=1 cargo test the_web_clients_fixture` and commit the result",
            path.display()
        );
    }

    #[test]
    fn an_empty_batch_adds_nothing() {
        let mut catalog = Catalog::default();
        assert!(!catalog.add(vec![]));
        assert!(catalog.plugins().is_empty());
    }

    #[test]
    fn a_catalog_that_was_never_scanned_is_not_scanning() {
        let mut catalog = Catalog::default();
        assert!(!catalog.is_scanning());
        assert!(!catalog.poll());
    }

    #[test]
    fn poll_folds_in_the_batches_and_notices_the_end_of_the_scan() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut catalog = Catalog::default();
        catalog.follow(rx);
        assert!(catalog.is_scanning());
        assert!(!catalog.poll(), "nothing has arrived");
        assert!(catalog.is_scanning(), "the scan thread is still running");

        tx.send(vec![info("Zeta", "a")]).unwrap();
        tx.send(vec![info("alpha", "b")]).unwrap();
        tx.send(vec![]).unwrap(); // an empty batch last must not hide the others
        assert!(catalog.poll());
        assert_eq!(names(&catalog), ["alpha", "Zeta"]);
        assert!(!catalog.poll(), "what arrived has been folded in");

        tx.send(vec![]).unwrap();
        assert!(!catalog.poll(), "an empty batch is not news");

        tx.send(vec![info("Beta", "c")]).unwrap();
        drop(tx); // the scan thread exits, its last batch still in the channel
        assert!(catalog.poll());
        assert_eq!(names(&catalog), ["alpha", "Beta", "Zeta"]);
        assert!(!catalog.is_scanning());
        assert!(!catalog.poll());
    }
}
