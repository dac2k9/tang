// The session the mock engine serves and the tests read: the snapshot of
// `fixtures/tracer.toml` (a saw oscillator through a low-pass filter whose cutoff an LFO
// sweeps: what `examples/tracer.toml` was, kept as the tests' own since a person who runs the
// program on the example can save over it) exactly as the server makes it. The JSON is generated from the Rust types and
// kept current by a Rust test (`the_web_clients_fixture_of_the_tracer_is_current`), so
// the cast below is as good as the generated types.
import type { CatalogEntry, Event, Feed, Graph, Intent, Node, NodeId, NoteRange, ParamValue, Plugin, Port, Route, SideInfo, Snapshot, StampedEvent, TelemetryLayout } from "../protocol";
import { isLink } from "./cables";
import builtinEffectNodes from "./fixtures/builtins.effects.json";
import builtins from "./fixtures/builtins.catalog.json";
import builtinInstrumentNodes from "./fixtures/builtins.instruments.json";
import fields from "./fixtures/fields.json";
import filenames from "./fixtures/filenames.json";
import freeReach from "./fixtures/freeReach.json";
import groups from "./fixtures/groups.json";
import history from "./fixtures/history.json";
import inputs from "./fixtures/inputs.json";
import audible from "./fixtures/audible.json";
import audibleCables from "./fixtures/audibleCables.json";
import audioCables from "./fixtures/audioCables.json";
import sideCables from "./fixtures/sideCables.json";
import uses from "./fixtures/uses.json";
import builtinPresetValues from "./fixtures/builtins.presets.json";
import moduleBoxes from "./fixtures/moduleBoxes.json";
import moduleCollapse from "./fixtures/moduleCollapse.json";
import moduleGraphs from "./fixtures/moduleGraphs.json";
import modules from "./fixtures/modules.json";
import moves from "./fixtures/moves.json";
import noise from "./fixtures/noise.json";
import patterns from "./fixtures/patterns.json";
import pins from "./fixtures/pins.json";
import reach from "./fixtures/reach.json";
import sources from "./fixtures/sources.json";
import steps from "./fixtures/steps.json";
import sync from "./fixtures/sync.json";
import tracer from "./fixtures/tracer.snapshot.json";

export const tracerSnapshot = tracer as unknown as Snapshot;

/**
 * What the engine makes of every combination of mute and solo of two groups and four lanes (the first two in the first group, the third in the second, the
 * fourth in none), read from a run of the audio graph by `the_web_clients_fixture_of_what_is_heard_is_current`: a case is the state of each node, two bits
 * each (the first muted and the second soloed), the groups first and then the lanes, and a sum whose bits are which lanes reach the master (1, 2, 4 and 8) and
 * which buses are heard (16 and 32).
 */
export const audibleFixture = audible as unknown as { groups: number; lanes: (number | null)[]; cases: [number, number][] };

/** What the engine makes of every combination of mute and solo of two groups and four lanes that have cables between them (`the_web_clients_fixture_of_what_is_heard_with_cables_is_current`): the cables, each as [what it starts at, which, what it ends at, which]. */
export const audibleCablesFixture = audibleCables as unknown as { groups: number; lanes: (number | null)[]; cables: [string, number, string, number][]; cases: [number, number][] };

/**
 * A session with two groups, lanes in them and out of them, an empty lane and a modulator in a few of each,
 * and, for each modulator (by its node id), the plugin nodes the controller let a cable be drawn to: written by
 * the controller (`the_web_clients_fixture_of_what_a_modulator_reaches_is_current`).
 */
export const reachFixture = reach as unknown as { snapshot: Snapshot; reach: Record<string, number[]> };

/**
 * The session of `reachFixture` with the session's own rack in it: two modulators that are in no lane and no group (an LFO and an envelope, the LFO with a cable to the first lane's detune and the envelope with one to
 * its filter's resonance), and, for each modulator (by its node id), the plugin nodes (`reach`) and the settings and cable depths of other modulators (`fields`) that the controller let a cable be drawn to: written by the
 * controller (`the_web_clients_fixture_of_what_a_modulator_that_is_in_no_lane_reaches_is_current`).
 */
export const freeReachFixture = freeReach as unknown as {
  snapshot: Snapshot;
  reach: Record<string, number[]>;
  fields: Record<string, { node: NodeId; port: Port }[]>;
  gains: Record<string, { node: NodeId; port: Port }[]>;
};

/**
 * A session with a group of three modulators whose first member has three of its own (an envelope with two cables among them), a
 * second member, a lane in no group and a lane in a second group, each with a modulator, and, for each modulator (by its node id),
 * the settings and cable depths of other modulators that the controller let a cable be drawn to, as a node and the port on it:
 * written by the controller (`the_web_clients_fixture_of_what_a_modulator_reaches_of_the_others_is_current`).
 */
export const fieldsFixture = fields as unknown as { snapshot: Snapshot; fields: Record<string, { node: NodeId; port: Port }[]> };

/**
 * A session with a group whose bus has four effects, a lane in it with four of its own and a lane in no group with three (a modulator in each
 * rack, with a cable to every plugin), and what the controller makes of asking it to move each of its nodes to each of some places: for a move
 * that is done the order of the nodes of the graph afterwards and the links of the audio path in the order of the graph's edges (the node each
 * leaves, the node it goes to, and its kind), for one that is refused its words. Written by the controller
 * (`the_web_clients_fixture_of_what_moving_an_effect_comes_to_is_current`).
 */
export const movesFixture = moves as unknown as {
  snapshot: Snapshot;
  moves: ({ node: NodeId; index: number } & ({ error: string } | { nodes: NodeId[]; audio: [NodeId, NodeId, "audio" | "member"][] }))[];
};

/**
 * A session with a group whose bus has an effect, a lane in it with an instrument and an effect, and an empty lane, and what the controller
 * makes of pinning some parameters to each of its nodes (the master and one that is not there too): for a list that is taken the
 * parameters pinned to the node's plugin afterwards, for one that is refused its words. Written by the controller
 * (`the_web_clients_fixture_of_what_pinning_comes_to_is_current`).
 */
export const pinsFixture = pins as unknown as {
  snapshot: Snapshot;
  pins: ({ node: NodeId; params: number[] } & ({ error: string } | { pinned: number[] }))[];
};

/**
 * A session with a lane that has an oscillator and a chain of three effects (a filter, a reverb that has a preset, a mix of its own and the link into it cut, and a math effect), three audio cables and a group, a modulator and an empty
 * lane, and what the controller makes of saving the chain as a module, adding it, undoing that and redoing it, saving the chain that came of it under another name and adding that: steps, each an intent that was asked in turn, with the
 * events it was answered with, whether it changed the structure, what Undo and Redo would do after it, the modules that the catalog lists, and the nodes and cables and the layout of the telemetry that the session has then. The
 * refusals are the controller's words for what it turned away, each on a session that had no module. Written by the controller (`the_web_clients_fixture_of_what_a_module_comes_to_is_current`).
 */
export const modulesFixture = modules as unknown as ModulesFixture;

/** What the controller wrote of a script of saving and adding modules: see `modulesFixture`. */
export interface ModulesFixture {
  snapshot: Snapshot;
  steps: {
    intent: Intent;
    events: StampedEvent[];
    restructured: boolean;
    history: { undo: string | null; redo: string | null };
    modules: { id: string; name: string; params: number }[];
    graph: Graph;
    telemetry: TelemetryLayout;
  }[];
  refusals: { intent: Intent; error: string }[];
  /** A module that a hand wrote (`file`, with what a plugin cannot take in it) added to the same session, and what the controller made of it. */
  odd: {
    file: string;
    step: { intent: Intent; events: StampedEvent[]; restructured: boolean; history: { undo: string | null; redo: string | null }; graph: Graph; telemetry: TelemetryLayout };
  };
}

/**
 * A session with an oscillator in a lane with a chain of three effects, a lane that holds a math effect and a drive with cables between the lanes, an empty lane, a group, six modulators (an LFO whose cable drives the depth of the
 * envelope's cable, an LFO, an envelope, a step sequencer, a macro and a control bound to CC 74, with cables to the plugins, the volume of a lane, the mix of an effect and each other) and the keys, and what the controller
 * makes of a script of saving a selection of its plugins as a module (`SaveSelectionAsModule`), adding it, undoing that and redoing it, and of what it refuses: as `modulesFixture` is for a chain. `modulatorSteps` is a second
 * script, on a controller of its own with a library of its own, for the modulators that go with the plugins: all of them at once, added, undone and done again, and a choice that leaves out most of what they drive. Written by the
 * controller (`the_web_clients_fixture_of_what_a_graph_module_comes_to_is_current`).
 */
export const moduleGraphsFixture = moduleGraphs as unknown as ModuleGraphsFixture;

/** What the controller wrote of a script of saving and adding modules that are graphs: see `moduleGraphsFixture`. */
export interface ModuleGraphsFixture extends ModulesFixture {
  modulatorSteps: ModulesFixture["steps"];
}

/**
 * A small session (an oscillator with a filter, a lane that has a reverb and no instrument, and an LFO that hears the first lane and drives the filter's cutoff) and what the controller makes of a module as one box (step 108): the filter, the reverb
 * and the LFO saved as a module, added (the lanes, the modulator and the box that is their record), placed, taken out whole, put back, taken out again, taken out one node after the other and put back as the module was, and a second one added with
 * the first taken out and put back. Steps, as `modulesFixture` has them (with no list of what the catalog says of modules). Written by the controller (`the_web_clients_fixture_of_what_a_module_comes_to_as_one_box_is_current`).
 */
export const moduleBoxesFixture = moduleBoxes as unknown as { snapshot: Snapshot; steps: Omit<ModulesFixture["steps"][number], "modules">[] };

/**
 * A small session (a group with a bus effect, the keys, a lane with an oscillator and two effects, a lane that has a reverb and no instrument, an empty lane, an LFO that hears the first lane and a macro) and what the controller
 * makes of collapsing nodes into one module where they are, and of taking the box away (step 109): the filter and the LFO collapsed as `Room` with a place, undone and redone, ungrouped and undone, the lane of the reverb collapsed under a
 * name of nothing, the empty lane and the macro collapsed as `Tail`, the first box ungrouped and undone, taken out whole and undone and redone and undone. `refusals` are what it refuses, each tried after the first `after` steps
 * of the script (a name and a place are looked at last, so those are tried before any box is made). Written by the controller (`the_web_clients_fixture_of_what_collapsing_into_a_module_comes_to_is_current`).
 */
export const moduleCollapseFixture = moduleCollapse as unknown as {
  snapshot: Snapshot;
  steps: Omit<ModulesFixture["steps"][number], "modules">[];
  refusals: { after: number; intent: Intent; error: string }[];
};

/**
 * A session with a group (a bus effect and an LFO in it), a member of it with a reverb and an LFO of its own, a lane with a pattern that was recorded at 90 beats a minute and a
 * lane with no instrument, at 100, and what the controller makes of intents about patterns: scripts, each a list of intents asked of one session in turn, with the event each is
 * answered with or the words it is refused with. They are every kind of intent asked of every node (the master and one that is not there too), with values that are held to their
 * range, rounded and taken as they are, and runs on each lane that go on from one intent to the next. Written by the controller
 * (`the_web_clients_fixture_of_what_a_pattern_comes_to_is_current`).
 */
export const patternsFixture = patterns as unknown as {
  snapshot: Snapshot;
  scripts: ({ intent: Intent } & ({ error: string } | { event: Event }))[][];
};

/**
 * A session with two groups, each with a modulator whose cables go to its members (a parameter, and the rate of a member's own modulator;
 * the first group has an envelope too, whose cable drives the depth of one of them), four lanes (two in the first group, one in the second, one
 * in none), and what the controller makes of the intents that add a group, put a
 * lane in one, name one and take a lane or a group out: for one that is refused its words, for a name the name the group has, and for the others what the session comes
 * to: its nodes (the id, the parent and the name of each), the audio path, the modulation cables and the two lists of the layout of the
 * telemetry, a line of text for each entry, made by the rules of `shapeOf` (below); and, for the tracer's one lane, what the
 * controller says to taking it out, since a session keeps a lane. Written by the controller
 * (`the_web_clients_fixture_of_what_the_intents_of_groups_come_to_is_current`).
 */
export const groupsFixture = groups as unknown as {
  snapshot: Snapshot;
  rows: ({ intent: Intent } & ({ error: string } | { name: string } | { after: Shape }))[];
  lastLane: { node: NodeId; error: string };
};

/**
 * A session with audio cables that make the audio go round when a lane is put in a group or an effect is moved (a bus of two effects, a lane of two effects with an LFO, a lane of one
 * effect in no group, an empty lane, and six audio cables between them: `testutil::PATCHED`), and what the controller makes of the intents about audio cables: every start tried with every
 * end (the master, a node that is not there and a modulator among them), the ports that are not audio's, levels, the level and the depth of each kind of cable, the cables taken out,
 * every node taken out, every lane put in the group and out of it and every effect moved to every place of its chain. For one that is refused its words; for a cable that is drawn and a
 * level that is set the audio cables the session has after it (`sends`, in the lines of `shapeOf`: nothing else changes); for the others what the session comes to (`shapeOf`). `cutSnapshot` is the
 * same session with every link of the audio path cut, and `cutRows` what the controller makes of an `addCable` between every start and every end of it, each on a session of its own: a cable between the
 * two ends of a link that is cut gives the link back (the links, what is cut and the cables after it are kept of the session it comes to). Written by the controller
 * (`the_web_clients_fixture_of_what_audio_cables_come_to_is_current`).
 */
export const audioCablesFixture = audioCables as unknown as {
  snapshot: Snapshot;
  rows: ({ intent: Intent } & ({ error: string } | { sends: string[] } | { after: Shape }))[];
  /** The same session with every link of the audio path cut (step 88), and what the controller makes of a cable asked for between every start and every end, each on a session of its own. */
  cutSnapshot: Snapshot;
  cutRows: ({ intent: Intent } & ({ error: string } | { links: { audio: string[]; routes: string[]; feeds: string[]; sends: string[] } }))[];
};

/**
 * A session with effects that have a second input of audio, and cables at it (a math effect on a bus, one in a lane of two effects after a filter, a lane whose way out is cut, a free LFO, and three
 * audio cables at the second input of the lane's math effect: one from the other lane, one from the group's output and one from the filter before it: `testutil::SIDED`, with the group's
 * cable and the filter's drawn and the LFO made), and what the controller makes of the intents about the second input: every start tried with both inputs of every end, the ports that are not
 * audio's, levels, the cables' levels and removals, every node taken out, every lane put in the group and out of it, every effect moved to every place of its chain, and a math effect added. For one that is
 * refused its words; for a cable that is drawn and a level that is set the audio cables the session has after it (`sends`, in the lines of `shapeOf`, which say `side` of a cable at a second input:
 * nothing else changes); for the others what the session comes to (`shapeOf`). Written by the controller (`the_web_clients_fixture_of_what_a_second_input_comes_to_is_current`).
 */
export const sideCablesFixture = sideCables as unknown as {
  snapshot: Snapshot;
  rows: ({ intent: Intent } & ({ error: string } | { sends: string[] } | { after: Shape }))[];
};

/**
 * When each parameter of the two math effects of the session of `sideCablesFixture` has an effect, as the model says (`PluginSlot::unused_because`): a row is one of the effects (node 4 is the lane's, which has
 * three cables at its second input, and node 1 the group's, which has none), with the cables as they are in that snapshot or taken out, set to one of its twelve modes, and for each parameter by its name why it
 * has no effect then, in the plugin's words, or null if it has one. Written by the controller (`the_web_clients_fixture_of_when_a_parameter_has_an_effect_is_current`).
 */
export const usesFixture = uses as unknown as {
  rows: { node: NodeId; cables: "as they were" | "taken out"; mode: number; unused: Record<string, string | null> }[];
};

/**
 * Two sessions, one with the keys, a device, a group, three lanes and a cable of notes and an audio cable among its cables ("wired", `testutil::NOTES`) and one with two lanes and no source ("bare"),
 * and what the controller makes of intents about sources and cables of notes tried on each, one on each fresh session: every start of a cable of notes with every end (the master, a node that is not
 * there, an effect, a group and a modulator among them), the ports that are not events', levels, the names of devices, cables and nodes taken out, and every intent that names a node, with a
 * source as the node. For one that is refused its words; else the events it made (stamped), whether it changed the structure and what the session comes to after it (`shapeOf`). Written by the
 * controller (`the_web_clients_fixture_of_what_sources_and_cables_of_notes_come_to_is_current`).
 */
export const sourcesFixture = sources as unknown as {
  cases: { name: string; snapshot: Snapshot; rows: ({ intent: Intent } & ({ error: string } | { events: StampedEvent[]; restructured: boolean; after: Shape }))[] }[];
};

/**
 * One op of a script of the history fixture: time going by (in milliseconds), or an intent and what the controller made of it: its words if it
 * was refused, else the events it made, whether it changed the structure and, if it did, what the session comes to then (`shapeOf`).
 */
export type HistoryRow =
  | { wait: number }
  | ({ intent: Intent; history: { undo: string | null; redo: string | null } } & (
      | { error: string }
      | { events: StampedEvent[]; restructured: false }
      | { events: StampedEvent[]; restructured: true; shape: Shape }
    ));

/**
 * A session (a group with a bus filter, another with nothing and no name, a lane with a reverb, a filter, an LFO and an envelope with a cable
 * each, and a lane of its own with an LFO) and what the controller makes of scripts of intents and of time going by (a change is a step of its own when a second has gone by
 * since the last of its kind): for each intent the events it made, stamped, whether it changed the structure, what Undo and Redo would do after it,
 * or the words of its refusal. Written by the controller (`the_web_clients_fixture_of_what_a_history_comes_to_is_current`). A script that needs
 * what that session has not has a session of its own (`snapshot`).
 */
export const historyFixture = history as unknown as { snapshot: Snapshot; scripts: { name: string; snapshot?: Snapshot; rows: HistoryRow[] }[] };

/**
 * What each of some names comes to when the session is saved as it: the file it is saved as, or the controller's words for what is wrong with it. Written by
 * the controller (`the_web_clients_fixture_of_file_names_is_current`, `session::plain_file_name`).
 */
export const filenamesFixture = filenames;

/**
 * Which of the MIDI inputs that are open a device of a session stands for, for some sets of inputs and some names: a row is the inputs, a name and the inputs that name stands for, in the order
 * they are open, as `midi::matching` finds them (`the_web_clients_fixture_of_what_a_device_stands_for_is_current`).
 */
export const inputsFixture = inputs;

/**
 * The rate, in cycles or steps a second, that the engine goes at for each of the note lengths that a modulator can follow the tempo by, at five tempos (`plugin::chain::rate_at_tempo`): a row
 * is the place in the list, its name, the tempo and the rate, each as the 64-bit number that the engine's 32-bit one widens to. Written by the model (`the_web_clients_fixture_of_what_a_note_length_comes_to_is_current`).
 */
export const syncFixture = sync;

/**
 * The numbers that the engine's `noise` makes for a spread of seeds and cycles (the random numbers of a random LFO, `plugin::chain::noise`), the ends of the 32 bits among them: each as the
 * 64-bit number that the engine's 32-bit one widens to. Written by the engine (`the_web_clients_fixture_of_the_noise_is_current`).
 */
export const noiseFixture = noise;

/**
 * What the engine's `steps_output` makes of a spread of patterns (three lists of sixteen steps), lengths, places and glides, written by `the_web_clients_fixture_of_what_a_step_sequencer_puts_out_is_current`:
 * the page's law of a step sequencer (`stepsOutput`) is pinned to it.
 */
export const stepsFixture = steps;

/** What a session comes to, for the fixture of the group intents: a line of text for each entry of each list. */
export interface Shape {
  nodes: string[];
  audio: string[];
  cables: string[];
  /** The audio cables that have been drawn between two places, a line each (`#id from>to @level`); none at all for a session that has none, as the controller's fixtures write it. */
  sends?: string[];
  /** The lanes and groups whose way out is cut, a line each (`id none`); none at all when none is, as the controller's fixtures write it. */
  routes?: string[];
  /** The effects whose feed is cut (the link into them from the stage before is not there), a line each (`id none`); none at all when none is, as the controller's fixtures write it. */
  feeds?: string[];
  /** The modulators that are in no lane and no group and hear a lane or a group, a line each (`modulator lane`); none at all when none does, as the controller's fixtures write it. */
  ears?: string[];
  /** The sources of notes, a line each (`id keys` or `id device Name`); none at all for a session that has none, as the controller's fixtures write it. */
  sources?: string[];
  /** The cables of notes that have been drawn from a source to a lane, a line each (`#id from>to`); none at all for a session that has none. */
  notes?: string[];
  /** The links from each source to each lane that hears every source, a line each (`from>to`); none at all for a session that has none. */
  hears?: string[];
  /** The order that the kinds of edge come in, each run of one kind an entry (`hears×4`), for a session that has a source: none at all for a session that has none. */
  order?: string[];
  meters: string[];
  modulation: string[];
}

const portKey = (port: Port): string => `${port.type}${"index" in port ? String(port.index) : ""}${"edge" in port ? `#${String(port.edge)}` : ""}`;

/**
 * What a session comes to, as the fixtures write it (`shape_of` in `app/testutil.rs`, by the same rules): a line for each node (its id, its
 * parent, its name), each link of the audio path (the node it leaves, the node it goes to, its kind), each cable (its id, the node it leaves
 * and the one it goes to, the port, its depth), each audio cable that has been drawn between two places (its id, the nodes, its level: `sends`, a list only when there are some), each lane
 * and group whose way out is cut (`routes`, likewise), each effect whose feed is cut (`feeds`, likewise) and each
 * entry of the two lists of the layout of the telemetry (the slot, the node, the port, the kind); the fingerprint of the layout is not part of it.
 */
export function shapeOf(snapshot: Snapshot): Shape {
  const { nodes, edges } = snapshot.graph;
  // (The second input of an effect has its word after the nodes, so that the line of every other cable is what it was.)
  const sends = edges.flatMap((e) =>
    e.kind === "cable" ? [`#${String(e.id)} ${String(e.from.node)}>${String(e.to.node)}${e.to.port.type === "audioInSide" ? " side" : ""} @${String(e.gain)}`] : [],
  );
  const routes = nodes.flatMap((n) => ((n.kind === "instrument" || n.kind === "group") && n.route === "none" ? [`${String(n.id)} none`] : []));
  const feeds = nodes.flatMap((n) => (n.kind === "effect" && n.feed === "none" ? [`${String(n.id)} none`] : []));
  const ears = nodes.flatMap((n) => (n.kind === "modulator" && n.hears !== undefined ? [`${String(n.id)} ${String(n.hears)}`] : []));
  const sources = nodes.flatMap((n) => (n.kind === "source" ? [n.source.type === "keys" ? `${String(n.id)} keys` : `${String(n.id)} device ${n.source.name}`] : []));
  const notes = edges.flatMap((e) => (e.kind === "events" ? [`#${String(e.id)} ${String(e.from.node)}>${String(e.to.node)}`] : []));
  const hears = edges.flatMap((e) => (e.kind === "hears" ? [`${String(e.from.node)}>${String(e.to.node)}`] : []));
  // The kinds of edge in the order they come in, a run of one kind as one entry.
  const runs: { kind: string; count: number }[] = [];
  for (const e of edges) {
    const last = runs.at(-1);
    if (last?.kind === e.kind) last.count += 1;
    else runs.push({ kind: e.kind, count: 1 });
  }
  const order = runs.map(({ kind, count }) => `${kind}×${String(count)}`);
  return {
    nodes: nodes.map((n) => `${String(n.id)} ${n.parent === null ? "-" : String(n.parent)} ${n.name}`),
    audio: edges.flatMap((e) => (isLink(e) ? [`${String(e.from.node)}>${String(e.to.node)} ${e.kind}`] : [])),
    cables: edges.flatMap((e) => (e.kind === "modulation" ? [`#${String(e.id)} ${String(e.from.node)}>${String(e.to.node)} ${portKey(e.to.port)} @${String(e.amount)}`] : [])),
    ...(sends.length > 0 ? { sends } : {}),
    ...(routes.length > 0 ? { routes } : {}),
    ...(feeds.length > 0 ? { feeds } : {}),
    ...(ears.length > 0 ? { ears } : {}),
    ...(sources.length > 0 ? { sources } : {}),
    ...(notes.length > 0 ? { notes } : {}),
    ...(hears.length > 0 ? { hears } : {}),
    ...(sources.length > 0 || notes.length > 0 || hears.length > 0 ? { order } : {}),
    meters: snapshot.telemetry.meters.map((m) => `${String(m.slot)} ${String(m.node)} ${portKey(m.port)} ${m.kind}`),
    modulation: snapshot.telemetry.modulation.map((m) => `${String(m.slot)} ${String(m.node)} ${portKey(m.port)} ${m.kind}`),
  };
}

/** The built-in plugins as the server lists them in its catalog, which the mock engine serves: written by Rust (`the_web_clients_fixture_of_the_builtins_is_current`). */
export const builtinCatalog = builtins as CatalogEntry[];

/**
 * What the controller makes of each built-in effect that is added to a lane or a group, by the plugin's id: the node's name,
 * the plugin with the parameters it has when it is new, the mix it starts at, its feed (`chain`: the stage before it feeds it) and, for the one that has a second input of audio (the
 * Math), what the two are called (`side`); the ids are the mock's to hand out. Written by Rust (`the_web_clients_fixture_of_the_builtin_effects_is_current`).
 */
export const builtinEffects = builtinEffectNodes as unknown as Record<string, { name: string; kind: "effect"; plugin: Plugin; mix: number; feed: Feed; side?: SideInfo }>;

/**
 * What the controller makes of each built-in instrument that is added in a lane of its own, by the plugin's id: the node's
 * name, the plugin with the parameters it has when it is new, the lane's range, transposition and volume (full range,
 * none, 1) and its way out (`out`, except for an instrument that starts bare, as the audio input does: `none`, so that it is heard
 * only through the cables that are drawn from it); the ids are the mock's to hand out. Written by Rust (`the_web_clients_fixture_of_the_builtin_instruments_is_current`).
 */
export const builtinInstruments = builtinInstrumentNodes as unknown as Record<
  string,
  { name: string; kind: "instrument"; plugin: Plugin; range: NoteRange | null; transpose: number; volume: number; route: Route }
>;

/**
 * What the controller says each preset of each built-in plugin does (`PresetLoaded`'s values: what the plugin has for its
 * parameters after it), by the plugin's id and the preset's id. Written by Rust (`the_web_clients_fixture_of_the_builtin_presets_is_current`).
 */
export const builtinPresets = builtinPresetValues as unknown as Record<string, Record<string, ParamValue[]>>;

/** The master's node id, as the server numbers it (`NodeId::MASTER`). */
export const MASTER: NodeId = 4294967295;

/** The node of a snapshot called `name`; throws if there is none, since a fixture that lacks it is wrong. */
export function nodeNamed(snapshot: Snapshot, name: string): Node {
  const node = snapshot.graph.nodes.find((n) => n.name.startsWith(name));
  if (!node) throw new Error(`the snapshot has no node called ${name}`);
  return node;
}
