//! Watching the signal at the output of a node: the pool of rings, which node has which, and what the audio thread is told to listen to
//! (`docs/web-ui.md` D47). The front-ends say what they watch (`Controller::set_probes`); this keeps the engine's taps where the nodes are.

use std::collections::BTreeMap;

use crate::app::controller::Controller;
use crate::app::model::{NodeId, NodePath, SessionModel};
use crate::plugin::chain::{GraphCommand, ProbeTarget};
use crate::telemetry::PROBES;

/// Who is watching: a client of the web interface, by the number of its connection (from one up), or the terminal, which has a number of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Watcher(pub u64);

impl Watcher {
    /// The terminal: the highest number there is, which no connection of the web interface has.
    pub const TERMINAL: Watcher = Watcher(u64::MAX);
}

/// What is watched, by whom, and which ring of the engine's pool each node has: the controller's (`Controller::probes`).
#[derive(Default)]
pub struct Probes {
    /// The nodes each watcher asked for, in the order it asked, each once.
    watching: BTreeMap<Watcher, Vec<NodeId>>,
    /// The node that each ring of the pool is for.
    rings: [Option<NodeId>; PROBES],
    /// What the audio thread was last told to listen to: a ring, and where in the graph its node is.
    told: Vec<(usize, ProbeTarget)>,
}

impl Probes {
    /// Nobody watches anything, and the audio thread listens to nothing (so that no ring is a node's, since the rings are only given and taken in `reconcile_probes`, which tells it): there
    /// is nothing to keep in step with the session.
    fn is_idle(&self) -> bool {
        self.watching.is_empty() && self.told.is_empty()
    }
}

/// Where the engine listens for `node`: none for what has no signal of its own (a modulator, a lane that has no instrument), for the master (it has the scope that
/// every front-end has), and for a node that is not there.
fn target_of(model: &SessionModel, node: NodeId) -> Option<ProbeTarget> {
    match model.locate(node)? {
        NodePath::Instrument(inst) => model.instruments.get(inst)?.instrument.as_ref().map(|_| ProbeTarget::Instrument(inst)),
        NodePath::Effect { inst, index } => Some(ProbeTarget::Effect { inst, index }),
        NodePath::Group(group) => Some(ProbeTarget::Group(group)),
        NodePath::GroupEffect { group, index } => Some(ProbeTarget::GroupEffect { group, index }),
        NodePath::Modulator { .. } | NodePath::Source(_) | NodePath::Module(_) => None,
    }
}

impl Controller {
    /// `watcher` watches the signal at the output of `nodes` (what it watched before it does not): returns the nodes of them that are watched, in the order they were asked for, which are those that
    /// have a signal and a ring (the pool has `PROBES`, shared by every watcher: a node that no ring is left for is watched when one is free, and is not among those returned until then). Two
    /// watchers of one node share its ring.
    pub fn set_probes(&mut self, watcher: Watcher, nodes: &[NodeId]) -> Vec<NodeId> {
        let mut asked = Vec::new();
        for &node in nodes {
            if !asked.contains(&node) {
                asked.push(node);
            }
        }
        self.probes.watching.insert(watcher, asked.clone());
        self.reconcile_probes();
        asked.into_iter().filter(|node| self.probes.rings.contains(&Some(*node))).collect()
    }

    /// The ring of the pool (`Telemetry::probes`) that `node` has now, if it has one: a reader that has the node and not the ring (the terminal's scope) asks for it here.
    pub fn probe_ring(&self, node: NodeId) -> Option<usize> {
        self.probes.rings.iter().position(|ring| *ring == Some(node))
    }

    /// Let go of the watchers that `keep` says are not there any more (a client of the web interface that has closed).
    #[cfg(any(test, feature = "web"))]
    pub fn keep_probes_of(&mut self, keep: impl Fn(Watcher) -> bool) {
        let before = self.probes.watching.len();
        self.probes.watching.retain(|watcher, _| keep(*watcher));
        if self.probes.watching.len() != before {
            self.reconcile_probes();
        }
    }

    /// Keep what the audio thread listens to, and what readers are told, where the nodes that are watched are now: a ring that its node has no use for (nobody watches it, it is gone or has
    /// no signal) is free, a node that is watched and has no ring takes a free one if there is one, and when what the audio thread listens to differs from what it was told it is told
    /// again, whole (`ClearProbes`, then a `SetProbe` for each). Called when the watchers change, when an intent has changed the structure and at every poll (the terminal changes
    /// lanes with no intent).
    pub(super) fn reconcile_probes(&mut self) {
        if self.probes.is_idle() {
            return;
        }
        let mut wanted: Vec<NodeId> = Vec::new();
        for node in self.probes.watching.values().flatten() {
            if !wanted.contains(node) && target_of(&self.model, *node).is_some() {
                wanted.push(*node);
            }
        }
        for ring in &mut self.probes.rings {
            if ring.is_some_and(|node| !wanted.contains(&node)) {
                *ring = None;
            }
        }
        for node in wanted {
            if !self.probes.rings.contains(&Some(node)) {
                if let Some(free) = self.probes.rings.iter().position(Option::is_none) {
                    self.probes.rings[free] = Some(node);
                }
            }
        }
        let listening: Vec<(usize, ProbeTarget)> =
            self.probes.rings.iter().enumerate().filter_map(|(ring, node)| Some((ring, target_of(&self.model, (*node)?)?))).collect();
        if listening != self.probes.told {
            let _ = self.cmd_tx.send(GraphCommand::ClearProbes);
            for &(ring, target) in &listening {
                let _ = self.cmd_tx.send(GraphCommand::SetProbe { target, ring });
            }
            self.probes.told = listening;
        }
        for (ring, node) in self.probes.rings.iter().enumerate() {
            self.telemetry.set_probe_node(ring, node.map(|node| node.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::intent::Intent;
    use crate::app::model::{NodeId, NodePath};
    use crate::app::testutil::{StubEngine, load, stub_engine};
    use crate::app::controller::Controller;
    use crate::telemetry::PROBES;

    /// A group with a bus filter, a lane in it with a filter and a reverb and an LFO, a lane of its own, and an empty lane.
    const SESSION: &str = r#"
[[group]]
[[group.effect]]
plugin = "builtin:filter"

[[instrument]]
plugin = "builtin:osc"
group = 0
[[instrument.effect]]
plugin = "builtin:filter"
[[instrument.effect]]
plugin = "builtin:reverb"
[[instrument.modulator]]
type = "lfo"

[[instrument]]
plugin = "builtin:osc"

[[instrument]]
"#;

    fn controller(text: &str) -> (StubEngine, Controller) {
        let model = load(text).model;
        let (handles, engine) = stub_engine();
        (engine, Controller::new(model, handles, None))
    }

    fn node(ctl: &Controller, path: NodePath) -> NodeId {
        ctl.model.nodes().find(|(_, p)| *p == path).map(|(id, _)| id).unwrap_or_else(|| panic!("no node at {path:?}"))
    }

    fn sent(engine: &StubEngine) -> Vec<String> {
        engine.drain().0
    }

    /// The node that each ring of the pool is listened for, as the readers see it.
    fn rings(ctl: &Controller) -> Vec<Option<u32>> {
        (0..PROBES).map(|ring| ctl.telemetry.probe_node(ring)).collect()
    }

    fn only(ring: usize, node: NodeId) -> Vec<Option<u32>> {
        (0..PROBES).map(|r| (r == ring).then_some(node.0)).collect()
    }

    #[test]
    fn watching_an_effect_has_the_audio_thread_store_its_signal_and_tells_readers_which_ring_is_its() {
        let (engine, mut ctl) = controller(SESSION);
        sent(&engine);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        let watched = ctl.set_probes(Watcher(1), &[reverb]);
        assert_eq!(watched, [reverb]);
        assert_eq!(sent(&engine), ["ClearProbes", "SetProbe ring=0 Effect { inst: 0, index: 1 }"]);
        assert_eq!(rings(&ctl), only(0, reverb));
    }

    #[test]
    fn each_kind_of_node_that_has_a_signal_is_listened_to_where_it_is() {
        let (engine, mut ctl) = controller(SESSION);
        sent(&engine);
        let nodes = [
            node(&ctl, NodePath::Instrument(0)),
            node(&ctl, NodePath::Effect { inst: 0, index: 0 }),
            node(&ctl, NodePath::Group(0)),
            node(&ctl, NodePath::GroupEffect { group: 0, index: 0 }),
            node(&ctl, NodePath::Instrument(1)),
        ];
        assert_eq!(ctl.set_probes(Watcher(1), &nodes), nodes);
        assert_eq!(
            sent(&engine),
            [
                "ClearProbes",
                "SetProbe ring=0 Instrument(0)",
                "SetProbe ring=1 Effect { inst: 0, index: 0 }",
                "SetProbe ring=2 Group(0)",
                "SetProbe ring=3 GroupEffect { group: 0, index: 0 }",
                "SetProbe ring=4 Instrument(1)",
            ]
        );
        assert_eq!(rings(&ctl)[..6], [Some(nodes[0].0), Some(nodes[1].0), Some(nodes[2].0), Some(nodes[3].0), Some(nodes[4].0), None]);
    }

    #[test]
    fn a_node_that_has_no_signal_to_watch_is_not_watched() {
        let (engine, mut ctl) = controller(SESSION);
        sent(&engine);
        let lfo = ctl.model.nodes().find(|(_, path)| matches!(path, NodePath::Modulator { .. })).map(|(id, _)| id).unwrap();
        let empty_lane = node(&ctl, NodePath::Instrument(2));
        // The master has a scope of its own, a modulator puts out no sound, a lane that has no instrument puts out none, and there is no node 9999.
        assert_eq!(ctl.set_probes(Watcher(1), &[NodeId::MASTER, lfo, empty_lane, NodeId(9999)]), []);
        assert_eq!(sent(&engine), Vec::<String>::new());
        assert_eq!(rings(&ctl), vec![None; PROBES]);
    }

    #[test]
    fn a_node_that_two_watch_has_one_ring_that_is_kept_until_both_have_stopped() {
        let (engine, mut ctl) = controller(SESSION);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.set_probes(Watcher(1), &[reverb]);
        assert_eq!(ctl.set_probes(Watcher(2), &[reverb]), [reverb]);
        sent(&engine);
        ctl.keep_probes_of(|watcher| watcher != Watcher(1));
        assert_eq!(sent(&engine), Vec::<String>::new(), "the other still watches it");
        assert_eq!(rings(&ctl), only(0, reverb));
        ctl.keep_probes_of(|watcher| watcher != Watcher(2));
        assert_eq!(sent(&engine), ["ClearProbes"]);
        assert_eq!(rings(&ctl), vec![None; PROBES]);
    }

    #[test]
    fn a_watcher_that_asks_again_replaces_what_it_watched_and_what_stays_keeps_its_ring() {
        let (engine, mut ctl) = controller(SESSION);
        let (filter, reverb) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        ctl.set_probes(Watcher(1), &[filter, reverb]);
        sent(&engine);
        assert_eq!(ctl.set_probes(Watcher(1), &[reverb]), [reverb]);
        assert_eq!(sent(&engine), ["ClearProbes", "SetProbe ring=1 Effect { inst: 0, index: 1 }"]);
        assert_eq!(rings(&ctl), only(1, reverb));
        // And a node asked for twice is watched once.
        assert_eq!(ctl.set_probes(Watcher(1), &[reverb, reverb]), [reverb]);
    }

    #[test]
    fn asking_for_what_is_watched_already_sends_nothing() {
        let (engine, mut ctl) = controller(SESSION);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.set_probes(Watcher(1), &[reverb]);
        sent(&engine);
        ctl.set_probes(Watcher(1), &[reverb]);
        ctl.set_probes(Watcher(2), &[reverb]);
        assert_eq!(sent(&engine), Vec::<String>::new());
    }

    #[test]
    fn the_pool_has_eight_rings_and_a_ninth_node_has_none_until_one_is_free() {
        let lanes: String = "[[instrument]]\nplugin = \"builtin:osc\"\n".repeat(PROBES + 1);
        let (engine, mut ctl) = controller(&lanes);
        let ids: Vec<NodeId> = (0..=PROBES).map(|lane| node(&ctl, NodePath::Instrument(lane))).collect();
        assert_eq!(ctl.set_probes(Watcher(1), &ids[..PROBES]), ids[..PROBES]);
        sent(&engine);
        assert_eq!(ctl.set_probes(Watcher(2), &ids[PROBES..]), [], "no ring is left");
        assert_eq!(sent(&engine), Vec::<String>::new());
        // When the first watcher lets go of one, the second's node takes the ring.
        ctl.set_probes(Watcher(1), &ids[1..PROBES]);
        assert_eq!(rings(&ctl)[0], Some(ids[PROBES].0));
        let told = sent(&engine);
        assert_eq!(told.len(), PROBES + 1, "everything is told again, whole: {told:?}");
        assert_eq!(told[..2], ["ClearProbes", "SetProbe ring=0 Instrument(8)"]);
        assert!(!told.iter().any(|line| line.ends_with("Instrument(0)")), "the lane that was let go of is not listened to: {told:?}");
    }

    #[test]
    fn when_a_ring_is_free_for_two_nodes_that_had_none_the_first_watcher_has_it() {
        let lanes: String = "[[instrument]]\nplugin = \"builtin:osc\"\n".repeat(PROBES + 2);
        let (_engine, mut ctl) = controller(&lanes);
        let ids: Vec<NodeId> = (0..PROBES + 2).map(|lane| node(&ctl, NodePath::Instrument(lane))).collect();
        ctl.set_probes(Watcher(1), &ids[..PROBES]);
        // Both ask when no ring is left, the later watcher first.
        assert_eq!(ctl.set_probes(Watcher(3), &ids[PROBES + 1..]), []);
        assert_eq!(ctl.set_probes(Watcher(2), &ids[PROBES..PROBES + 1]), []);
        ctl.set_probes(Watcher(1), &ids[1..PROBES]);
        assert_eq!(rings(&ctl)[0], Some(ids[PROBES].0), "the watcher with the lower number has the ring");
        // (And the one that has none is not among what its watcher is told it has.)
        assert_eq!(ctl.set_probes(Watcher(3), &ids[PROBES + 1..]), []);
    }

    #[test]
    fn a_ring_follows_its_node_when_the_chain_changes_under_it() {
        let (engine, mut ctl) = controller(SESSION);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.set_probes(Watcher(1), &[reverb]);
        sent(&engine);
        ctl.apply(Intent::MoveEffect { node: reverb, index: 0 }).unwrap();
        let told = sent(&engine);
        assert_eq!(told[told.len() - 2..], ["ClearProbes", "SetProbe ring=0 Effect { inst: 0, index: 0 }"], "{told:?}");
        assert_eq!(rings(&ctl), only(0, reverb));
    }

    #[test]
    fn a_node_that_is_taken_out_gives_up_its_ring_and_has_one_again_when_undo_puts_it_back() {
        let (engine, mut ctl) = controller(SESSION);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.set_probes(Watcher(1), &[reverb]);
        sent(&engine);
        ctl.apply(Intent::RemoveNode { node: reverb }).unwrap();
        assert!(sent(&engine).contains(&"ClearProbes".to_string()));
        assert_eq!(rings(&ctl), vec![None; PROBES]);
        // (The watcher still asked for it: when the node is back, so is its ring.)
        ctl.apply(Intent::Undo).unwrap();
        let told = sent(&engine);
        assert_eq!(told[told.len() - 2..], ["ClearProbes", "SetProbe ring=0 Effect { inst: 0, index: 1 }"], "{told:?}");
        assert_eq!(rings(&ctl), only(0, reverb));
    }

    #[test]
    fn what_the_terminal_does_to_the_lanes_without_an_intent_is_followed_at_the_next_poll() {
        let (engine, mut ctl) = controller(SESSION);
        let osc = node(&ctl, NodePath::Instrument(1));
        ctl.set_probes(Watcher(1), &[osc]);
        sent(&engine);
        ctl.swap_lanes(0, 1);
        ctl.poll();
        let told = sent(&engine);
        assert_eq!(told[told.len() - 2..], ["ClearProbes", "SetProbe ring=0 Instrument(0)"], "{told:?}");
    }

    #[test]
    fn the_ring_that_a_node_has_is_asked_of_the_controller_by_whoever_reads_it() {
        let (_engine, mut ctl) = controller(SESSION);
        let (filter, reverb) = (node(&ctl, NodePath::Effect { inst: 0, index: 0 }), node(&ctl, NodePath::Effect { inst: 0, index: 1 }));
        assert_eq!(ctl.probe_ring(filter), None, "nobody watches it");
        ctl.set_probes(Watcher::TERMINAL, &[reverb]);
        assert_eq!((ctl.probe_ring(reverb), ctl.probe_ring(filter)), (Some(0), None));
        // What the terminal watches next replaces that, and the ring goes to the node that is watched.
        ctl.set_probes(Watcher::TERMINAL, &[filter]);
        assert_eq!((ctl.probe_ring(filter), ctl.probe_ring(reverb)), (Some(0), None));
        // A node that is not there, or has no signal, has none.
        let lfo = ctl.model.nodes().find(|(_, path)| matches!(path, NodePath::Modulator { .. })).map(|(id, _)| id).unwrap();
        ctl.set_probes(Watcher::TERMINAL, &[lfo, NodeId(9999)]);
        assert_eq!((ctl.probe_ring(lfo), ctl.probe_ring(NodeId(9999))), (None, None));
    }

    #[test]
    fn the_terminal_and_a_client_of_the_web_interface_share_the_ring_of_a_node_and_the_terminal_is_not_a_client_that_goes() {
        let (engine, mut ctl) = controller(SESSION);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.set_probes(Watcher::TERMINAL, &[reverb]);
        ctl.set_probes(Watcher(1), &[reverb]);
        assert_eq!(rings(&ctl), only(0, reverb));
        sent(&engine);
        // The client closes: what the owner of the web interface does for the numbers of the connections that are gone, which are the only numbers it knows.
        ctl.keep_probes_of(|watcher| watcher == Watcher::TERMINAL);
        assert_eq!(sent(&engine), Vec::<String>::new(), "the terminal still watches it");
        assert_eq!(rings(&ctl), only(0, reverb));
        assert!(Watcher::TERMINAL > Watcher(u64::from(u32::MAX)), "the connections are numbered from one: none is the terminal's");
    }

    #[test]
    fn with_nobody_watching_a_change_of_the_session_sends_no_probes() {
        let (engine, mut ctl) = controller(SESSION);
        sent(&engine);
        let reverb = node(&ctl, NodePath::Effect { inst: 0, index: 1 });
        ctl.apply(Intent::MoveEffect { node: reverb, index: 0 }).unwrap();
        ctl.poll();
        assert!(sent(&engine).iter().all(|line| !line.contains("Probe")));
    }
}
