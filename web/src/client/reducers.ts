// How a client keeps its copy of the session in step with the server's: apply the
// events that come after a snapshot. Pure: a snapshot in, a snapshot out, with only
// what changed replaced (so an unchanged node is the same object, which is what a
// store's subscribers compare). The rules are the controller's (`src/app/apply.rs`),
// read from the other side.
import { MODULATOR_FIELD_RANGES, WAVEFORMS } from "../protocol";
import type { Gain, Graph, ModulatorField, Node, NodeUi, ParamValue, PianoMeta, Plugin, Snapshot, StampedEvent } from "../protocol";

/** The snapshot after an event, or why the event does not fit the one it was applied to. */
export type Applied = { ok: true; snapshot: Snapshot } | { ok: false; reason: string };

/**
 * Apply `event` to `snapshot`. An event is the next revision, or one this snapshot
 * already has (nothing to do: a client that asked for a second snapshot may see
 * events from before it). Anything else means events were missed, or the session is
 * not the one the event is about, and the client has to start again from a snapshot.
 */
export function applyEvent(snapshot: Snapshot, event: StampedEvent): Applied {
  if (event.rev <= snapshot.rev) return { ok: true, snapshot };
  if (event.rev !== snapshot.rev + 1) {
    return { ok: false, reason: `the session is at revision ${snapshot.rev} and the next event is ${event.rev}` };
  }
  // A module that is saved is a file of the library and no change of the session: the session is as saved, or as unsaved, as it was. Nor is a window of a plugin that is opened on the server's machine (step 112).
  if (event.type === "moduleSaved" || event.type === "editorOpened") return { ok: true, snapshot: { ...snapshot, rev: event.rev } };
  // A save changes no node: the session is as it was, and nothing in it is unsaved now.
  if (event.type === "saved") return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta: { ...snapshot.meta, dirty: false, file: event.file } } };
  // The piano's scale and lock are the session's, not a node's: no node changes.
  if (event.type === "scaleSet" || event.type === "lockedSet") {
    return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta: { ...snapshot.meta, dirty: true, piano: pianoAfter(snapshot.meta.piano, event) } } };
  }
  // So is the tempo: it changes no node either.
  if (event.type === "bpmSet") return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta: { ...snapshot.meta, dirty: true, bpm: event.bpm } } };
  const graph = changeGraph(snapshot.graph, event);
  if (typeof graph === "string") return { ok: false, reason: graph };
  // An intent, from whichever front-end, is a change the session has not saved (`Controller::apply` marks it so).
  const meta = snapshot.meta.dirty ? snapshot.meta : { ...snapshot.meta, dirty: true };
  return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta, graph } };
}

/** The piano's scale and lock after an event about them. */
function pianoAfter(piano: PianoMeta, event: Extract<StampedEvent, { type: "scaleSet" | "lockedSet" }>): PianoMeta {
  if (event.type === "lockedSet") return { ...piano, locked: event.locked };
  return { ...piano, scale: event.scale, root: event.root, kind: event.kind, notes: event.notes };
}

/** The graph after the event, with only what it changed replaced; or the reason it cannot be. */
function changeGraph(graph: Graph, event: Exclude<StampedEvent, { type: "saved" | "moduleSaved" | "editorOpened" | "scaleSet" | "lockedSet" | "bpmSet" }>): Graph | string {
  if (event.type === "limiterSet") return withLimiter(graph, event.on);
  if (event.type === "edgeDepthSet") return withDepth(graph, event.edge, event.depth);
  if (event.type === "cableGainSet") return withCableGain(graph, event.edge, event.gain);
  const node = graph.nodes.find((n) => n.id === event.node);
  if (!node) return `there is no node ${event.node}`;
  const changed = changeNode(node, event);
  if (typeof changed === "string") return changed;
  return { ...graph, nodes: graph.nodes.map((n) => (n === node ? changed : n)) };
}

/** The node the event is about, as it is after it; or the reason it cannot be. */
function changeNode(node: Node, event: Exclude<StampedEvent, { type: "edgeDepthSet" | "cableGainSet" | "saved" | "moduleSaved" | "editorOpened" | "scaleSet" | "lockedSet" | "bpmSet" | "limiterSet" }>): Node | string {
  switch (event.type) {
    case "paramSet": {
      const plugin = pluginOf(node);
      if (!plugin) return `node ${node.id} has no plugin`;
      const changed = withValues(plugin, [{ param: event.param, value: event.value }]);
      return changed ? withPlugin(node, changed) : `node ${node.id} has no parameter ${event.param}`;
    }
    case "gainSet":
      return withGain(node, event.gain, event.value) ?? `${event.gain} does not apply to node ${node.id}`;
    case "presetLoaded": {
      const plugin = pluginOf(node);
      if (!plugin) return `node ${node.id} has no plugin`;
      // The controller keeps a preset's values as the parameters' baseline too, and a snapshot shows the baseline as the default.
      const changed = withValues({ ...plugin, currentPreset: event.preset }, event.values, true);
      return changed ? withPlugin(node, changed) : `the preset names a parameter node ${node.id} does not have`;
    }
    case "rangeSet":
      return node.kind === "instrument" ? { ...node, range: event.range } : `node ${node.id} is not a lane`;
    case "muteSet":
      return node.kind === "instrument" || node.kind === "group" ? { ...node, muted: event.muted } : `node ${node.id} is neither a lane nor a group`;
    case "soloSet":
      return node.kind === "instrument" || node.kind === "group" ? { ...node, solo: event.solo } : `node ${node.id} is neither a lane nor a group`;
    case "patternSet":
      return node.kind === "instrument" ? { ...node, pattern: event.pattern } : `node ${node.id} is not a lane`;
    case "nodeUiSet":
      return withPlace(node, event.ui);
    case "pinnedSet": {
      const plugin = pluginOf(node);
      if (!plugin) return `node ${node.id} has no plugin`;
      const missing = event.params.find((index) => plugin.params.every((param) => param.index !== index));
      if (missing !== undefined) return `node ${node.id} has no parameter ${missing}`;
      return withPlugin(node, { ...plugin, pinned: event.params });
    }
    case "groupRenamed":
      return node.kind === "group" ? { ...node, name: event.name } : `node ${node.id} is not a group`;
    case "modulatorFieldSet":
      return withSetting(node, event.field, event.value);
    case "stepsSet": {
      if (node.kind !== "modulator" || node.source.type !== "steps") return `node ${node.id} is not a step sequencer`;
      const { values } = node.source;
      if (event.values.length !== values.length) return `a step sequencer has ${values.length} steps, and ${event.values.length} were told`;
      return { ...node, source: { ...node.source, values: event.values } };
    }
  }
}

/** The node at a place, or, for none, as a node the server has not placed is: with no `ui` at all. */
function withPlace(node: Node, ui: NodeUi | null): Node {
  if (ui !== null) return { ...node, ui };
  const unplaced = { ...node };
  delete unplaced.ui;
  return unplaced;
}

function pluginOf(node: Node): Plugin | null {
  return node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
}

/** The node with its plugin replaced (only nodes that have one get here). */
function withPlugin(node: Node, plugin: Plugin): Node {
  return node.kind === "instrument" || node.kind === "effect" ? { ...node, plugin } : node;
}

/** The plugin with these parameters at these values (and, if `baseline`, with them as their defaults); null if it lacks one of them. */
export function withValues(plugin: Plugin, values: readonly ParamValue[], baseline = false): Plugin | null {
  const wanted = new Map(values.map((v) => [v.param, v.value]));
  let found = 0;
  const params = plugin.params.map((param) => {
    const value = wanted.get(param.index);
    if (value === undefined) return param;
    found += 1;
    return baseline ? { ...param, value, default: value } : { ...param, value };
  });
  return found === wanted.size ? { ...plugin, params } : null;
}

/** The node with its host-side gain set; null if that gain is not one this node has. */
/** The graph with the limiter on the output mix in the sound or out of it (the master is the one node that has it). */
function withLimiter(graph: Graph, on: boolean): Graph | string {
  const master = graph.nodes.find((n) => n.kind === "master");
  if (master?.kind !== "master") return "the output mix is not where it was";
  return { ...graph, nodes: graph.nodes.map((n) => (n === master ? { ...master, limiter: on } : n)) };
}

function withGain(node: Node, gain: Gain, value: number): Node | null {
  if (gain === "masterVolume" && node.kind === "master") return { ...node, volume: value };
  if (gain === "limiterCeiling" && node.kind === "master") return { ...node, ceiling: value };
  if (gain === "volume" && node.kind === "instrument") return { ...node, volume: value };
  if (gain === "mix" && node.kind === "effect") return { ...node, mix: value };
  if (gain === "groupVolume" && node.kind === "group") return { ...node, volume: value };
  if (gain === "groupPan" && node.kind === "group") return { ...node, pan: value };
  return null;
}

/** Whether `value` is a place in the list of note lengths that a modulator can follow the tempo by (0 is none, and the last place is the last note length): a whole number of those. */
function isNoteLength(value: number): boolean {
  const [lowest, highest] = MODULATOR_FIELD_RANGES.sync;
  return Number.isInteger(value) && value >= lowest && value <= highest;
}

/** The node with a setting of its modulator set; a reason if it is not a modulator, or not one that has that setting. */
function withSetting(node: Node, field: ModulatorField, value: number): Node | string {
  if (node.kind !== "modulator") return `node ${node.id} is not a modulator`;
  const { source } = node;
  if (source.type === "lfo") {
    if (field === "rate") return { ...node, source: { ...source, rate: value } };
    if (field === "waveform") {
      // (Only a whole number, 0 to 3, is an index: the rest, a half or a minus one or NaN, finds nothing.)
      const waveform = WAVEFORMS[value];
      return waveform === undefined ? `${value} is not a waveform` : { ...node, source: { ...source, waveform } };
    }
    // (Its key: whether a note starts it again at the start of its wave, and the seconds it takes to come in after one.)
    if (field === "retrigger") return { ...node, source: { ...source, retrigger: value !== 0 } };
    if (field === "fade") return { ...node, source: { ...source, fade: value } };
    // (The note length that its rate follows the tempo by: a place in the list, from 0 for none.)
    if (field === "sync") return isNoteLength(value) ? { ...node, source: { ...source, sync: value } } : `${String(value)} is not a note length`;
  }
  if (source.type === "envelope" && (field === "attack" || field === "decay" || field === "sustain" || field === "release")) {
    return { ...node, source: { ...source, [field]: value } };
  }
  if (source.type === "macro" && field === "value") return { ...node, source: { ...source, value } };
  if (source.type === "steps") {
    if (field === "rate" || field === "glide") return { ...node, source: { ...source, [field]: value } };
    if (field === "length") {
      // (Only a whole number of the steps there are: the rest, a half or a hundred or NaN, is no length.)
      const [lowest, highest] = MODULATOR_FIELD_RANGES.length;
      return Number.isInteger(value) && value >= lowest && value <= highest ? { ...node, source: { ...source, length: value } } : `${String(value)} is not a length`;
    }
    if (field === "retrigger") return { ...node, source: { ...source, retrigger: value !== 0 } };
    if (field === "sync") return isNoteLength(value) ? { ...node, source: { ...source, sync: value } } : `${String(value)} is not a note length`;
  }
  return `${field} does not apply to node ${node.id}`;
}

/** The graph with the cable `edge` pulling `depth`; a reason if there is no such cable. */
function withDepth(graph: Graph, edge: number, depth: number): Graph | string {
  if (!graph.edges.some((e) => e.kind === "modulation" && e.id === edge)) return `there is no cable ${edge}`;
  return { ...graph, edges: graph.edges.map((e) => (e.kind === "modulation" && e.id === edge ? { ...e, amount: depth } : e)) };
}

/** The graph with the audio cable `edge` at the level `gain`; a reason if there is no such cable. */
function withCableGain(graph: Graph, edge: number, gain: number): Graph | string {
  if (!graph.edges.some((e) => e.kind === "cable" && e.id === edge)) return `there is no audio cable ${edge}`;
  return { ...graph, edges: graph.edges.map((e) => (e.kind === "cable" && e.id === edge ? { ...e, gain } : e)) };
}
