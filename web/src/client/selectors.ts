// Reading the session in the client's store: the parts of it a view shows, found by the id of
// the node they are of. Pure, on the state, so that a view can hand one to a store hook.
import { WAVEFORMS } from "../protocol";
import type { EdgeId, Gain, ModulatorField, Node, NodeId, NoteRange, Param, PatternInfo, Preset } from "../protocol";
import type { ClientState } from "./connection";
import type { Stages } from "./envelope";
import { audibleOf, ownAudibleOf } from "./audible";
import { macroExposed } from "./macros";
import { displayNames } from "./names";
import { shownParams } from "./pinning";
import { unusedBecause } from "./uses";

/** The node of the session with this id, if the client has a session and it has one. */
export function nodeById(state: ClientState, id: NodeId): Node | undefined {
  return state.snapshot?.graph.nodes.find((node) => node.id === id);
}

/** Parameter `index` (the plugin's own number for it) of the plugin in node `id`. */
export function paramOf(state: ClientState, id: NodeId, index: number): Param | undefined {
  const node = nodeById(state, id);
  if (node?.kind !== "instrument" && node?.kind !== "effect") return undefined;
  return node.plugin?.params.find((param) => param.index === index);
}

/** Why parameter `index` of the plugin in node `id` has no effect now (`unusedBecause`), or null if it has one. */
export function paramUnusedBecause(state: ClientState, id: NodeId, index: number): string | null {
  const param = paramOf(state, id, index);
  const node = nodeById(state, id);
  if (!state.snapshot || !param?.usedWhen || !node) return null;
  return unusedBecause(state.snapshot.graph, node, param);
}

/**
 * The presets that can be loaded into the plugin of node `id`, and the name of the one that was loaded last: those of a lane's
 * instrument or of an effect in a lane's chain. Null for a node that has no such plugin: a bus effect has presets, but the audio thread
 * loads one by a lane's slot (the controller says "a bus effect has no presets to load"), so it is not offered one. A plugin with no
 * presets has an empty list.
 */
export function presetsOf(state: ClientState, id: NodeId): { presets: readonly Preset[]; current: string | null } | null {
  const node = nodeById(state, id);
  if (node?.kind !== "instrument" && node?.kind !== "effect") return null;
  if (!node.plugin) return null;
  if (node.kind === "effect" && state.snapshot?.graph.nodes.find((n) => n.id === node.parent)?.kind === "group") return null;
  return { presets: node.plugin.presets, current: node.plugin.currentPreset };
}

/** Where an effect is in its chain: its place, from 0 (the first, the nearest the instrument or the group), and how many effects the chain has. */
export interface ChainPlace {
  index: number;
  count: number;
}

/**
 * Where the effect `id` is in its chain, a lane's or a group's bus: the effects that have the same parent, in the order of the graph, which is the
 * order of the audio path. Null for a node that is no effect, a node that is not there, and before there is a session.
 */
export function chainPlaceOf(state: ClientState, id: NodeId): ChainPlace | null {
  const nodes = state.snapshot?.graph.nodes;
  const found = nodes?.find((node) => node.id === id);
  if (!nodes || found?.kind !== "effect") return null;
  const chain = nodes.filter((node) => node.kind === "effect" && node.parent === found.parent);
  return { index: chain.indexOf(found), count: chain.length };
}

/** Whether two answers of `chainPlaceOf` are the same place in a chain of the same length. */
export const sameChainPlace = (a: ChainPlace | null, b: ChainPlace | null): boolean => a === b || (a !== null && b !== null && a.index === b.index && a.count === b.count);

/** The parameters of a plugin to look through: what each is called and its own index, with whose they are. */
export interface ParamList {
  /** What the canvas calls the node. */
  name: string;
  /** What kind of plugin it is (`Built-in`, `CLAP`, ...). */
  format: string;
  params: readonly { index: number; name: string }[];
}

/**
 * Every parameter of the plugin of node `id`, by name and the plugin's own index, for a person to find one in: the plugin
 * of an instrument or of any effect, a group's bus included (a bus effect has parameters to turn, whatever it cannot be
 * given as a preset). Null for a node that has no plugin. It holds no values, so that it is the same list, by `sameParamList`,
 * while they change.
 */
export function paramListOf(state: ClientState, id: NodeId): ParamList | null {
  const graph = state.snapshot?.graph;
  const node = graph?.nodes.find((candidate) => candidate.id === id);
  if (!graph || (node?.kind !== "instrument" && node?.kind !== "effect") || !node.plugin) return null;
  return {
    name: displayNames(graph).get(id) ?? node.name,
    format: node.plugin.format,
    params: node.plugin.params.map((param) => ({ index: param.index, name: param.name })),
  };
}

/** How many parameters the plugin of node `id` has: none for a node that has no plugin. (Cheaper than the list, for what only asks whether there are any.) */
export function paramCountOf(state: ClientState, id: NodeId): number {
  const node = nodeById(state, id);
  return (node?.kind === "instrument" || node?.kind === "effect" ? node.plugin?.params.length : undefined) ?? 0;
}

/**
 * Whether the plugin of node `id` has a window of its own that the machine that runs Tang can open (step 112): the plugin says it has one (`hasEditor`, a VST3 plugin does) and the server says that its machine can
 * show windows (`windows` among what its hello says it can do: a machine that has no screen cannot). False for every other node.
 */
export function hasWindowOf(state: ClientState, id: NodeId): boolean {
  if (!state.hello?.features.includes("windows")) return false;
  const node = nodeById(state, id);
  return (node?.kind === "instrument" || node?.kind === "effect" ? node.plugin?.hasEditor : undefined) ?? false;
}

/**
 * Whether the page may offer to open a session file (step 115): the server says that its program can make its engine again with another session (`openSession` among what its hello says it can do: a plugin in a host; a program
 * that is started with its file cannot).
 */
export function canOpenSession(state: ClientState): boolean {
  return state.hello?.features.includes("openSession") ?? false;
}

/** Whether two answers of `paramListOf` are the same list of the same plugin. */
export function sameParamList(a: ParamList | null, b: ParamList | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.name === b.name &&
    a.format === b.format &&
    a.params.length === b.params.length &&
    a.params.every((param, position) => param.index === b.params[position]?.index && param.name === b.params[position].name)
  );
}

/** The keys a lane takes: null if every note, undefined if the node is not a lane (an empty lane is one). */
export function rangeOf(state: ClientState, id: NodeId): NoteRange | null | undefined {
  const node = nodeById(state, id);
  return node?.kind === "instrument" ? node.range : undefined;
}

/** The pattern of a lane: null while it has none, undefined if the node is not a lane (an empty lane is one). */
export function patternOf(state: ClientState, id: NodeId): PatternInfo | null | undefined {
  const node = nodeById(state, id);
  return node?.kind === "instrument" ? node.pattern : undefined;
}

/** What a lane's or a group's mute and solo are set to. */
export interface Flags {
  muted: boolean;
  solo: boolean;
}

/** The mute and the solo of a lane or a group (a lane that has no instrument has them too); undefined if the node is neither. */
export function flagsOf(state: ClientState, id: NodeId): Flags | undefined {
  const node = nodeById(state, id);
  return node?.kind === "instrument" || node?.kind === "group" ? { muted: node.muted, solo: node.solo } : undefined;
}

/** How many effects the chain of the lane `id` has: what a module of it would be made of. None for a node that is no lane (a group's bus is not saved as a module). */
export function chainLengthOf(state: ClientState, id: NodeId): number {
  const nodes = state.snapshot?.graph.nodes;
  if (nodes?.find((node) => node.id === id)?.kind !== "instrument") return 0;
  return nodes.filter((node) => node.kind === "effect" && node.parent === id).length;
}

/** Whether two answers of `flagsOf` are the same flags (the answer is made anew each time). */
export function sameFlags(a: Flags | undefined, b: Flags | undefined): boolean {
  if (a === b) return true;
  if (a === undefined || b === undefined) return false;
  return a.muted === b.muted && a.solo === b.solo;
}

/**
 * Whether what a node puts out can be heard at the master, by the mute and solo of the session and by what cables carry (`audibleOf`, `ownAudibleOf`): a lane's instrument by what the lane
 * is heard for by itself (a lane that is heard only for what is sent to it by a cable has none of its own sound in its chain), a group's bus, and an effect's by the lane or group whose chain
 * it is in, which is what carries it. Undefined for the nodes that put out no sound, a modulator and the master among them.
 */
export function audibleAt(state: ClientState, id: NodeId): boolean | undefined {
  const graph = state.snapshot?.graph;
  const node = graph?.nodes.find((n) => n.id === id);
  if (graph === undefined || node === undefined) return undefined;
  switch (node.kind) {
    case "instrument":
    case "group":
      return ownAudibleOf(graph).get(node.id);
    case "effect":
      return node.parent === null ? undefined : audibleOf(graph).get(node.parent);
    default:
      return undefined;
  }
}

/** The group a lane is in and the groups there are, for the lane's choice of group: each by the name the canvas gives it. */
export interface GroupChoices {
  /** The group the lane is in, or null for none. */
  current: NodeId | null;
  /** Every group of the session, in the order of the graph. */
  groups: readonly { id: NodeId; name: string }[];
}

/** The group of the lane `id` and the groups it can be put in; undefined if the node is not a lane (an empty lane is one). */
export function groupChoicesOf(state: ClientState, id: NodeId): GroupChoices | undefined {
  const graph = state.snapshot?.graph;
  const lane = graph?.nodes.find((node) => node.id === id);
  if (!graph || lane?.kind !== "instrument") return undefined;
  const names = displayNames(graph);
  return { current: lane.parent, groups: graph.nodes.filter((node) => node.kind === "group").map((node) => ({ id: node.id, name: names.get(node.id) ?? node.name })) };
}

/** Whether two answers of `groupChoicesOf` are the same group and the same groups by the same names. */
export function sameGroupChoices(a: GroupChoices | undefined, b: GroupChoices | undefined): boolean {
  if (a === b) return true;
  if (a === undefined || b === undefined) return false;
  return a.current === b.current && a.groups.length === b.groups.length && a.groups.every((group, i) => group.id === b.groups[i]?.id && group.name === b.groups[i].name);
}

/** What goes when a lane or a group is taken out, and what stays, for the words that ask a person to be sure of it. */
export interface Removal {
  kind: "lane" | "group";
  /** What the node is called on the canvas. */
  name: string;
  /** For a lane: whether it has an instrument, which can be taken out alone, the lane staying with its effects. (False for a group and for a lane that has none.) */
  instrument: boolean;
  /** The effects of the lane's chain, or of the group's bus: they go with it. */
  effects: number;
  /** The modulators that hear it (every modulator is the session's own, and may hear a lane or a group): they stay, and hear every note then. */
  hearing: number;
  /** For a group, the lanes that are members of it: they stay, in no group. (None for a lane.) */
  lanes: number;
  /** The only lane of the session, which a session keeps. */
  last: boolean;
}

/** What taking out the lane or the group `id` comes to; undefined if the node is neither. */
export function removalOf(state: ClientState, id: NodeId): Removal | undefined {
  const graph = state.snapshot?.graph;
  const found = graph?.nodes.find((node) => node.id === id);
  if (!graph || (found?.kind !== "instrument" && found?.kind !== "group")) return undefined;
  const children = graph.nodes.filter((node) => node.parent === id);
  return {
    kind: found.kind === "group" ? "group" : "lane",
    name: displayNames(graph).get(id) ?? found.name,
    instrument: found.kind === "instrument" && found.plugin !== null,
    effects: children.filter((node) => node.kind === "effect").length,
    hearing: graph.nodes.filter((node) => node.kind === "modulator" && node.hears === id).length,
    lanes: children.filter((node) => node.kind === "instrument").length,
    last: found.kind === "instrument" && graph.nodes.filter((node) => node.kind === "instrument").length < 2,
  };
}

/** Whether two answers of `removalOf` say the same. */
export function sameRemoval(a: Removal | undefined, b: Removal | undefined): boolean {
  if (a === b) return true;
  if (a === undefined || b === undefined) return false;
  return a.kind === b.kind && a.name === b.name && a.instrument === b.instrument && a.effects === b.effects && a.hearing === b.hearing && a.lanes === b.lanes && a.last === b.last;
}

/** The value of a host-side gain of a node, if the node has that gain (`withGain` in reducers.ts is the other way round). */
export function gainOf(state: ClientState, id: NodeId, gain: Gain): number | undefined {
  const node = nodeById(state, id);
  if (gain === "volume" && node?.kind === "instrument") return node.volume;
  if (gain === "mix" && node?.kind === "effect") return node.mix;
  if (gain === "groupVolume" && node?.kind === "group") return node.volume;
  if (gain === "groupPan" && node?.kind === "group") return node.pan;
  if (gain === "masterVolume" && node?.kind === "master") return node.volume;
  if (gain === "limiterCeiling" && node?.kind === "master") return node.ceiling;
  return undefined;
}

/** Whether the limiter on the output mix is in the sound (step 102); undefined while there is no session. */
export function limiterOf(state: ClientState): boolean | undefined {
  const master = state.snapshot?.graph.nodes.find((node) => node.kind === "master");
  return master?.kind === "master" ? master.limiter : undefined;
}

/**
 * The value of a setting of the modulator in node `id`, if it has that setting (`withSetting` in
 * reducers.ts is the other way round): an LFO's rate, or its waveform as the index the intent
 * takes, its switch (1 when a note starts it again), its fade in seconds and the place in the list of note lengths that it follows the tempo by (0 for none), a stage of an envelope, a macro's value, or a step
 * sequencer's rate, glide, length, switch and note length.
 */
export function modulatorFieldOf(state: ClientState, id: NodeId, field: ModulatorField): number | undefined {
  const node = nodeById(state, id);
  if (node?.kind !== "modulator") return undefined;
  const { source } = node;
  if (source.type === "lfo") {
    if (field === "rate") return source.rate;
    if (field === "waveform") return WAVEFORMS.indexOf(source.waveform);
    if (field === "retrigger") return source.retrigger ? 1 : 0;
    if (field === "fade") return source.fade;
    if (field === "sync") return source.sync;
  }
  if (source.type === "envelope") {
    if (field === "attack" || field === "decay" || field === "sustain" || field === "release") return source[field];
  }
  if (source.type === "macro" && field === "value") return source.value;
  if (source.type === "steps") {
    if (field === "rate" || field === "glide" || field === "length") return source[field];
    if (field === "retrigger") return source.retrigger ? 1 : 0;
    if (field === "sync") return source.sync;
  }
  return undefined;
}

/** Whether the macro in node `id` shows its knob on the box of a module that it is in (step 110); undefined for a node that is no macro. */
export function macroExposedOf(state: ClientState, id: NodeId): boolean | undefined {
  const node = nodeById(state, id);
  return node?.kind === "modulator" && node.source.type === "macro" ? macroExposed(node.source) : undefined;
}

/** The steps of the step sequencer `id`, all of them (the list that its node has, so that an unchanged one is the same list): none for a node that is not one. */
export function stepsOf(state: ClientState, id: NodeId): readonly number[] | undefined {
  const node = nodeById(state, id);
  return node?.kind === "modulator" && node.source.type === "steps" ? node.source.values : undefined;
}

/** The four settings of the envelope `id`: the times of its attack, decay and release and the level of its sustain. Null for a node that is no envelope. */
export function envelopeStagesOf(state: ClientState, id: NodeId): Stages | null {
  const node = nodeById(state, id);
  if (node?.kind !== "modulator" || node.source.type !== "envelope") return null;
  const { attack, decay, sustain, release } = node.source;
  return { attack, decay, sustain, release };
}

/** Whether two answers of `envelopeStagesOf` are the same four settings. */
export const sameStages = (a: Stages | null, b: Stages | null): boolean =>
  a === b || (a !== null && b !== null && a.attack === b.attack && a.decay === b.decay && a.sustain === b.sustain && a.release === b.release);

/** How deep the modulation cable with this id pulls, if there is one. */
export function cableDepthOf(state: ClientState, id: EdgeId): number | undefined {
  const cable = state.snapshot?.graph.edges.find((edge) => edge.kind === "modulation" && edge.id === id);
  return cable?.kind === "modulation" ? cable.amount : undefined;
}

/** How loud the audio cable with this id is (its level, 0 to 2), if there is one: the ids of the two kinds of cable are one series, and a cable of modulation has a depth and no level. */
export function cableGainOf(state: ClientState, id: EdgeId): number | undefined {
  const cable = state.snapshot?.graph.edges.find((edge) => edge.kind === "cable" && edge.id === id);
  return cable?.kind === "cable" ? cable.gain : undefined;
}

/** The modulator the modulation cable with this id starts at, if there is one: the node that the depth of the cable is a setting of (the layout of the telemetry names its value there). */
export function cableSourceOf(state: ClientState, id: EdgeId): NodeId | undefined {
  const cable = state.snapshot?.graph.edges.find((edge) => edge.kind === "modulation" && edge.id === id);
  return cable?.kind === "modulation" ? cable.from.node : undefined;
}

/** Whether the parameter `index` is one that node `id` shows as a knob for its plugin: pinned to it, or among the first few if none are pinned (`shownParams`). */
export function isKnob(state: ClientState, id: NodeId, index: number): boolean {
  const node = nodeById(state, id);
  return (node?.kind === "instrument" || node?.kind === "effect") && node.plugin ? shownParams(node.plugin).includes(index) : false;
}

/** Whether any parameter is pinned to the plugin of node `id`, which is what makes the first few no longer the ones it shows. */
export function hasPins(state: ClientState, id: NodeId): boolean {
  const node = nodeById(state, id);
  return (node?.kind === "instrument" || node?.kind === "effect") && node.plugin ? node.plugin.pinned.length > 0 : false;
}
