// Graphs made by hand for the tests of the boxes (step 108): the nodes and the cables of a session that has modules in it, as the server lists them. (Only tests import this.)
import { MASTER } from "../../client/fixtures";
import type { Edge, Node, NodeId, Param, Plugin } from "../../protocol";

export { MASTER };

export const params = (count: number): Param[] =>
  Array.from({ length: count }, (_, index) => ({ index: index * 2 + 1, name: `p${String(index)}`, min: 0, max: 1, default: 0, value: 0.5, labels: null, unit: null, group: [], stepCount: 0 }));

export const plugin = (count: number, id: string, takesNotes?: false): Plugin => ({
  id,
  format: "Built-in",
  isInstrument: id === "builtin:osc",
  params: params(count),
  presets: [],
  currentPreset: null,
  pinned: [],
  ...(takesNotes === false ? { takesNotes } : {}),
});

/** A lane: an instrument node, with a plugin or without (a lane that has effects only). Its way out is cut, as the lanes of a module that is added are. */
export const lane = (id: NodeId, name: string, withPlugin: Plugin | null = plugin(3, "builtin:osc")): Node => ({
  id,
  parent: null,
  name,
  kind: "instrument",
  plugin: withPlugin,
  range: null,
  transpose: 0,
  volume: 1,
  muted: false,
  solo: false,
  route: "none",
  pattern: null,
});

export const effect = (id: NodeId, parent: NodeId, name: string, side?: { main: string; side: string }): Node => ({
  id,
  parent,
  name,
  kind: "effect",
  plugin: plugin(3, "builtin:filter"),
  mix: 1,
  feed: "chain",
  ...(side ? { side } : {}),
});

export const modulator = (id: NodeId, name: string, hears?: NodeId): Node => ({
  id,
  parent: null,
  name,
  kind: "modulator",
  source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 },
  ...(hears === undefined ? {} : { hears }),
});

/** A macro (step 110): a modulator that is a knob, with the name that a person gave it and whether a box shows its knob, as the wire says them (left out while they are what a macro is to begin with). */
export const macro = (id: NodeId, name: string, given: { name?: string; expose?: boolean } = {}): Node => ({
  id,
  parent: null,
  name,
  kind: "modulator",
  source: { type: "macro", value: 0.5, ...(given.name === undefined ? {} : { name: given.name }), ...(given.expose === undefined ? {} : { expose: given.expose }) },
});

export const moduleNode = (id: NodeId, name: string, members: NodeId[]): Node => ({ id, parent: null, name, kind: "module", members });
export const master: Node = { id: MASTER, parent: null, name: "Master", kind: "master", volume: 1, limiter: true, ceiling: -1 };
export const keys: Node = { id: 99, parent: null, name: "Keys", kind: "source", source: { type: "keys" } };

export const out = (node: NodeId): { node: NodeId; port: { type: "audioOut" } } => ({ node, port: { type: "audioOut" } });
export const into = (node: NodeId, side = false): { node: NodeId; port: { type: "audioIn" } | { type: "audioInSide" } } => ({ node, port: side ? { type: "audioInSide" } : { type: "audioIn" } });
/** A link of the audio path inside a chain. */
export const link = (from: NodeId, to: NodeId): Edge => ({ kind: "audio", from: out(from), to: into(to) });
/** The way out of a lane's chain, to the mix. */
export const wayOut = (from: NodeId): Edge => ({ kind: "member", from: out(from), to: into(MASTER) });
/** An audio cable that a person drew. */
export const send = (id: number, from: NodeId, to: NodeId, side = false): Edge => ({ kind: "cable", id, gain: 1, from: out(from), to: into(to, side) });
/** A cable of modulation to the plugin parameter `param`. */
export const drives = (id: number, from: NodeId, to: NodeId, param: number): Edge => ({
  kind: "modulation",
  id,
  amount: 0.5,
  from: { node: from, port: { type: "modOut" } },
  to: { node: to, port: { type: "param", index: param } },
});
/** The link by which a lane hears every source. */
export const hears = (from: NodeId, to: NodeId): Edge => ({ kind: "hears", from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } } });
/** A cable of notes that a person drew. */
export const notes = (id: number, from: NodeId, to: NodeId): Edge => ({ kind: "events", id, from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } } });
