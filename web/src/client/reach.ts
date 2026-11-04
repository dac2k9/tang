// Where a cable from a modulator may end (docs/web-ui.md §9.3, D21). The controller decides
// (`SessionModel::param_targets` for the parameters of plugins, `SessionModel::field_targets` for the
// settings of other modulators, and `AddModEdge` refuses the rest), and a client that lets
// the user draw cables has to know before it asks, to show which parameters and settings take the cable
// that is being dragged. Every modulator is the session's own since step 96 (it is in no lane and no group), so the answer is the same for all of them: every plugin of the session, and the settings and cable depths
// of the other modulators; the page reads it from the graph alone. Fixtures that the controller writes (what it accepted, for every modulator of a session with groups and lanes in and out
// of them: `fixtures/reach.json`, `fixtures/freeReach.json`, and for the settings of modulators `fixtures/fields.json`) pin the two together.
import type { Edge, Graph, Node, NodeId, Port } from "../protocol";
import { isModulation } from "./cables";
import { endLabel, gainName } from "./names";
import type { Names } from "./names";

/** Whether a node has a plugin, whose parameters a cable can end at. */
const hasPlugin = (node: Node): boolean => (node.kind === "instrument" && node.plugin !== null) || node.kind === "effect";

/**
 * The nodes with a plugin that the modulator `modulator` can draw a cable to: every plugin of the session, an instrument's (of a lane that has one) and an effect's, a lane's or a bus's
 * (`ModulatorLocation::Free`: every modulator is the session's own, and hears one lane's or one group's notes, or every note, whatever it drives). A node that is not a modulator reaches nothing.
 */
export function reachFrom(graph: Graph, modulator: NodeId): Set<NodeId> {
  const from = graph.nodes.find((node) => node.id === modulator);
  return from?.kind === "modulator" ? new Set(graph.nodes.filter(hasPlugin).map((node) => node.id)) : new Set();
}

/** The ports of the values that the host keeps for a node, which a cable ends at beside a plugin's parameters: a lane's volume, an effect's mix, a group's volume and pan. */
export type GainPort = Extract<Port, { type: "volume" | "mix" | "pan" }>;

/** Whether a port is a host-side value (`GainPort`). */
export const isGainPort = (port: Port): port is GainPort => port.type === "volume" || port.type === "mix" || port.type === "pan";

/**
 * The host-side values of a node that a cable can end at (`SessionModel::gain_targets`): the volume of a lane that has an instrument (a lane that has none has no volume to drive), the mix of an effect,
 * a group's volume and its pan. Every modulator reaches them all, as it reaches every plugin.
 */
export function gainPorts(node: Node): GainPort[] {
  switch (node.kind) {
    case "instrument":
      return node.plugin === null ? [] : [{ type: "volume" }];
    case "effect":
      return [{ type: "mix" }];
    case "group":
      return [{ type: "volume" }, { type: "pan" }];
    default:
      return [];
  }
}

/** A setting of a modulator, or the depth of one of its cables: the node of the modulator and the port on it that a cable ends at. */
export interface FieldRef {
  node: NodeId;
  port: Port;
}

/**
 * The modulators whose settings the modulator `modulator` can be drawn to, in the order of the controller's list (`SessionModel::modulators_in_reach`): every other modulator, in the order of the graph
 * (the session's own rack). Never itself. A node that is not a modulator reaches none.
 */
export function modulatorsInReach(graph: Graph, modulator: NodeId): Node[] {
  const from = graph.nodes.find((node) => node.id === modulator);
  return from?.kind === "modulator" ? graph.nodes.filter((node) => node.kind === "modulator" && node.id !== modulator) : [];
}

/** Whether two ports are the same place on a node: of the same kind, and for a parameter the same one, for a depth the cable's. */
export function samePort(a: Port, b: Port): boolean {
  if (a.type === "param") return b.type === "param" && a.index === b.index;
  if (a.type === "depth") return b.type === "depth" && a.edge === b.edge;
  return a.type === b.type;
}

/** The ports of the settings a modulator has that a cable can end at: the rate of an LFO or of a step sequencer, an envelope's four stages, nothing for a MIDI control or a macro (its value is turned by a person). */
function settingPorts(node: Node): Port[] {
  if (node.kind !== "modulator") return [];
  switch (node.source.type) {
    case "lfo":
    case "steps":
      return [{ type: "rate" }];
    case "envelope":
      return [{ type: "attack" }, { type: "decay" }, { type: "sustain" }, { type: "release" }];
    case "midiLearn":
    case "macro":
      return [];
  }
}

/** The ports of the depths of the cables that start at the modulator `node`. */
const depthPorts = (graph: Graph, node: Node): Port[] =>
  graph.edges.flatMap((edge: Edge): Port[] => (edge.kind === "modulation" && edge.from.node === node.id ? [{ type: "depth", edge: edge.id }] : []));

/**
 * Every setting and cable depth of other modulators that `modulator` can be drawn to, as the controller lists them
 * (`SessionModel::field_targets`): for each modulator in reach (`modulatorsInReach`) its settings, then the depths of its
 * cables. Whether the modulator drives one already is not looked at here.
 */
export function fieldsOf(graph: Graph, modulator: NodeId): FieldRef[] {
  return modulatorsInReach(graph, modulator).flatMap((node) => [...settingPorts(node), ...depthPorts(graph, node)].map((port) => ({ node: node.id, port })));
}

/**
 * A node a modulator can still draw a cable to, and what it can still drive in it: the parameters of a plugin (`params`, by the
 * plugin's own index), the settings of a modulator and the depths of its cables, or the volume, the mix or the pan that the host keeps for a lane, an effect or a group (`fields`, by the port).
 */
export interface Drivable {
  node: NodeId;
  /** What the node is called on the canvas. */
  name: string;
  params: readonly { index: number; name: string }[];
  /** What each is called in a list: a setting by its name (`rate`), the depth of a cable by where the cable goes (`depth → Filter: cutoff`). */
  fields: readonly { port: Port; name: string }[];
}

/** What a setting of a modulator, or the depth of one of its cables, is called in a list: the depth of a cable is told by where the cable goes. */
function fieldName(graph: Graph, names: Names, port: Port): string {
  if (port.type !== "depth") return port.type;
  const { edge } = port;
  const cable = graph.edges.find((found) => found.kind === "modulation" && found.id === edge);
  return cable ? `depth → ${endLabel(graph, names, cable.to)}` : "depth";
}

/** The key of a setting of a modulator, or of the depth of one of its cables (a modulator has as many depths as cables), among those that a modulator drives. */
const fieldKey = (node: NodeId, port: Port): string => (port.type === "depth" ? `${String(node)}:depth:${String(port.edge)}` : `${String(node)}:${port.type}`);

/**
 * Everything `modulator` can still be drawn to, plugin by plugin in the order of the graph: the parameters of the
 * plugins it reaches (`reachFrom`) that it does not drive already (another modulator may drive the same one: the
 * two add up); then modulator by modulator, in the order of `fieldsOf`: the settings of the modulators it reaches and
 * the depths of their cables, that it does not drive already. `names` is what each node is called (`displayNames`). A node
 * with nothing left is not listed.
 */
export function drivablesOf(graph: Graph, modulator: NodeId, names: Names): Drivable[] {
  const reached = reachFrom(graph, modulator);
  const isModulator = graph.nodes.some((node) => node.id === modulator && node.kind === "modulator");
  const driven = new Set(
    graph.edges.flatMap((edge) => (edge.kind === "modulation" && edge.from.node === modulator && edge.to.port.type === "param" ? [`${String(edge.to.node)}:${String(edge.to.port.index)}`] : [])),
  );
  const drivenFields = new Set(graph.edges.flatMap((edge) => (edge.kind === "modulation" && edge.from.node === modulator && edge.to.port.type !== "param" ? [fieldKey(edge.to.node, edge.to.port)] : [])));
  const groups: Drivable[] = [];
  for (const node of graph.nodes) {
    const plugin = node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
    const params = reached.has(node.id)
      ? (plugin?.params ?? []).filter((param) => !driven.has(`${String(node.id)}:${String(param.index)}`)).map(({ index, name }) => ({ index, name }))
      : [];
    // (The volume, the mix and the pan that the host keeps: a lane's, an effect's and a group's, after the parameters of the plugin, if it has one.)
    const gains = isModulator ? gainPorts(node).filter((port) => !drivenFields.has(fieldKey(node.id, port))).map((port) => ({ port, name: gainName(node, port) })) : [];
    if (params.length > 0 || gains.length > 0) groups.push({ node: node.id, name: names.get(node.id) ?? node.name, params, fields: gains });
  }
  const byModulator = new Map<NodeId, Drivable>();
  for (const { node, port } of fieldsOf(graph, modulator)) {
    if (drivenFields.has(fieldKey(node, port))) continue;
    const group = byModulator.get(node) ?? { node, name: names.get(node) ?? graph.nodes.find((n) => n.id === node)?.name ?? "?", params: [], fields: [] };
    byModulator.set(node, { ...group, fields: [...group.fields, { port, name: fieldName(graph, names, port) }] });
  }
  return [...groups, ...byModulator.values()];
}

/**
 * The modulators that can still be given a cable to the port `port` of `node`, in the order of the graph: the ones that reach it (the parameters of
 * the plugins a modulator reaches, `reachFrom`; the settings of the other modulators in reach, and the depths of their cables, `fieldsOf`) and do not
 * drive it already (another modulator may: two can drive one). It is `drivablesOf` read from the other side, for a knob that asks what could drive it.
 * A port that no cable ends at, one the node has not, and a node that is not there have none.
 */
export function driversOf(graph: Graph, node: NodeId, port: Port): Node[] {
  const target = graph.nodes.find((n) => n.id === node);
  const plugin = target?.kind === "instrument" || target?.kind === "effect" ? target.plugin : null;
  if (port.type === "param" && !plugin?.params.some((param) => param.index === port.index)) return [];
  // (A host-side value that the node has not, a volume of an effect or a pan of a lane, is no place for a cable.)
  if (isGainPort(port) && !(target && gainPorts(target).some((own) => samePort(own, port)))) return [];
  return graph.nodes.filter((modulator) => {
    if (modulator.kind !== "modulator") return false;
    const reaches =
      port.type === "param"
        ? reachFrom(graph, modulator.id).has(node)
        : isGainPort(port) || fieldsOf(graph, modulator.id).some((field) => field.node === node && samePort(field.port, port));
    const drivesIt = graph.edges.some((edge) => isModulation(edge) && edge.from.node === modulator.id && edge.to.node === node && samePort(edge.to.port, port));
    return reaches && !drivesIt;
  });
}
