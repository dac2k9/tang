// A module that is added is one box on the canvas (step 108, docs/web-ui.md D108; the user's "the goal when we re-add a module is that it gets rendered like just a single box"). The session holds a module as a
// record of the nodes that came with it (`NodeKind::Module`: the lanes and the modulators, and a lane's effects are in the lane), the engine runs them as they are, and the page draws them as one node
// for as long as the module is closed: the nodes inside are not on the canvas, and a cable that has one end inside goes to the box instead, to a port of the box that stands for the port of the node inside
// (`memberHandle`). The ports that a box has are the ones that a module has to the world: the inputs of audio that nothing inside feeds, the outputs of audio that nothing inside takes, the inputs of notes of
// its instruments, and every port that a cable crosses the box's edge at. An open module is its nodes, with no box: this is a function of the graph and of which modules are open (the viewer's own choice,
// like which node shows its scope), and is what `toFlow` lays out and draws. (Step 110: a box also shows the knob of every macro that is in it and has not been told to hide it, `BoxKnob`.)
import { macroExposed } from "../../client/macros";
import { endLabel, portName } from "../../client/names";
import type { Names } from "../../client/names";
import { laneTakesNotes } from "../../client/noteWiring";
import type { Edge, Endpoint, Graph, Node, NodeId } from "../../protocol";
import { handleOf, memberHandle } from "./handles";

/** A port of a box: a port of a node inside the module, at which a cable crosses the box's edge. */
export interface BoxPort {
  /** The node inside the module whose port it is. */
  member: NodeId;
  /** The name of the handle in that node (`audioIn`, `audioInSide`, `audioOut`, `eventsIn`, `param-3`, `modOut`...). */
  handle: string;
  /** What to call it, for a person: `Reverb`, `Math: B`, `Filter: cutoff`. */
  label: string;
  /** What it carries: audio, notes, or modulation (a cable of modulation that crosses the box's edge). */
  kind: "audio" | "notes" | "modulation";
}

/** A knob that a box shows for something inside the module (step 110: the value of a macro that is not hidden from it): the macro's own knob, turned where it is. */
export interface BoxKnob {
  /** The macro, a modulator inside the module: the knob is its value. */
  member: NodeId;
  /** What to call it, for a person: the macro's name (`Brightness`, or `Macro 1` for one that has none: `displayNames` numbers the names that more than one node has). */
  label: string;
}

/** What a box has of the module it is: how much is inside it, its ports and its knobs. */
export interface Box {
  /** The module's node. */
  node: NodeId;
  /** How many instruments and effects are inside. */
  plugins: number;
  /** How many modulators are inside. */
  modulators: number;
  /** The inputs on its left edge: those of audio, then those of notes. */
  inputs: readonly BoxPort[];
  /** The outputs of audio on its right edge. */
  outputs: readonly BoxPort[];
  /** The cables of modulation that go into the module, from outside it, by the port of the node inside that each ends at (the top edge). */
  modIn: readonly BoxPort[];
  /** The modulators inside the module that drive something outside it (the bottom edge). */
  modOut: readonly BoxPort[];
  /** The knobs of the macros inside the module that show on the box, in the order of the graph (a macro that is told not to is not among them): a grid under the ports, `CONTROLS_PER_ROW` in a row. */
  knobs: readonly BoxKnob[];
}

/** How many rows the inputs and the outputs of a box take: as many as the longer of the two, and one at least (a box that has no port is as tall as one that has). */
export const boxRows = (box: Pick<Box, "inputs" | "outputs">): number => Math.max(1, box.inputs.length, box.outputs.length);

/** What is in a module, in words: `3 plugins · 1 modulator`. */
export function boxDetail(box: Pick<Box, "plugins" | "modulators">): string {
  const plugins = `${String(box.plugins)} ${box.plugins === 1 ? "plugin" : "plugins"}`;
  return box.modulators === 0 ? plugins : `${plugins} · ${String(box.modulators)} ${box.modulators === 1 ? "modulator" : "modulators"}`;
}

/** A cable of the graph as the canvas draws it: where it goes now, and where it was in the session. */
export interface Placed {
  /** The cable with the ends that are inside a closed module put at the box. */
  edge: Edge;
  /** The cable as the session has it: its ends are the nodes that it joins, and its words and its buttons are theirs. */
  real: Edge;
  /** The name of the handle that it starts at: the port's, or the box's for an end that is inside a module. */
  sourceHandle: string;
  /** The name of the handle that it ends at. */
  targetHandle: string;
}

/** The session as the canvas draws it, with the modules that are closed as boxes. */
export interface Collapsed {
  /** The nodes that are on the canvas (a closed module's nodes are not, and the module is) and the cables between them: the graph itself where no module is closed. */
  graph: Graph;
  /** Every cable that is on the canvas, as it is drawn. */
  edges: readonly Placed[];
  /** The closed modules, by their nodes. */
  boxes: ReadonlyMap<NodeId, Box>;
  /** The module that each node that is in one is in (a lane or a modulator that came with it, and the effects of its lanes), whether it is open or not. */
  moduleOf: ReadonlyMap<NodeId, NodeId>;
}

const NO_BOXES: ReadonlyMap<NodeId, Box> = new Map();
const NOT_IN_ANY: ReadonlyMap<NodeId, NodeId> = new Map();

/** The key of an end of a cable, for the ends to be looked up by. */
const keyOf = (end: Endpoint): string => `${String(end.node)}:${handleOf(end.port)}`;

/**
 * The session as the canvas draws it, given which modules are open (`open`: a module that is not is a box). A closed module's nodes (its lanes and modulators, and the effects of its lanes) are left out
 * and its node is the box; a cable between two of them is left out; a cable that has one end inside goes to the box, by a handle of it that says which port of which node it is (`memberHandle`); and a modulator that hears a lane
 * inside a box hears the box. An open module has no box: its node is left out, and its nodes are as they are. The graph is the one it was given if there is no module.
 */
export function collapse(graph: Graph, names: Names, open: ReadonlySet<NodeId>): Collapsed {
  const modules = graph.nodes.flatMap((node) => (node.kind === "module" ? [{ id: node.id, members: node.members }] : []));
  if (modules.length === 0) {
    return { graph, edges: graph.edges.map((edge) => ({ edge, real: edge, sourceHandle: handleOf(edge.from.port), targetHandle: handleOf(edge.to.port) })), boxes: NO_BOXES, moduleOf: NOT_IN_ANY };
  }
  const byId = new Map(graph.nodes.map((node) => [node.id, node]));
  // What is in each module: the lanes and the modulators that the record names (a node that the session has not, and one that another module has, are none of it), and then the effects of its lanes.
  const moduleOf = new Map<NodeId, NodeId>();
  for (const module of modules) {
    for (const member of module.members) {
      if (byId.has(member) && !moduleOf.has(member)) moduleOf.set(member, module.id);
    }
  }
  for (const node of graph.nodes) {
    const lane = node.kind === "effect" && node.parent !== null ? moduleOf.get(node.parent) : undefined;
    if (lane !== undefined && !moduleOf.has(node.id)) moduleOf.set(node.id, lane);
  }
  const inside = new Map<NodeId, Node[]>(modules.map((module) => [module.id, []]));
  for (const node of graph.nodes) {
    const module = moduleOf.get(node.id);
    if (module !== undefined) inside.get(module)?.push(node);
  }
  // The nodes that are not on the canvas: those in a module that is closed.
  const hiddenBy = new Map<NodeId, NodeId>();
  for (const module of modules) {
    if (open.has(module.id)) continue;
    for (const node of inside.get(module.id) ?? []) hiddenBy.set(node.id, module.id);
  }

  const nodes = graph.nodes.flatMap((node): Node[] => {
    if (hiddenBy.has(node.id) || (node.kind === "module" && open.has(node.id))) return [];
    const box = node.kind === "modulator" && node.hears !== undefined ? hiddenBy.get(node.hears) : undefined;
    return [box !== undefined && node.kind === "modulator" ? { ...node, hears: box } : node];
  });

  const placed: Placed[] = [];
  /** The ends of the cables that cross the edge of a box, inside it. */
  const crossing: { module: NodeId; end: Endpoint }[] = [];
  /** The inputs that something inside a module feeds, and the outputs that something inside takes: they are not the module's ports. */
  const fed = new Set<string>();
  const taken = new Set<string>();
  for (const edge of graph.edges) {
    const from = hiddenBy.get(edge.from.node);
    const to = hiddenBy.get(edge.to.node);
    const sourceHandle = handleOf(edge.from.port);
    const targetHandle = handleOf(edge.to.port);
    // (A lane that has no instrument has no input of notes on the canvas: the link by which it hears every source is not drawn, as it is not for a lane that has no card.)
    const target = byId.get(edge.to.node);
    if (edge.kind === "hears" && to !== undefined && target?.kind === "instrument" && target.plugin === null) continue;
    if (from === undefined && to === undefined) {
      placed.push({ edge, real: edge, sourceHandle, targetHandle });
      continue;
    }
    if (from === to) {
      // (The session lists a link from a lane that has no instrument to the first effect of its chain, which carries nothing: it does not feed the effect.)
      const start = byId.get(edge.from.node);
      if (!(start?.kind === "instrument" && start.plugin === null)) {
        fed.add(keyOf(edge.to));
        taken.add(keyOf(edge.from));
      }
      continue;
    }
    placed.push({
      edge: { ...edge, from: from === undefined ? edge.from : { ...edge.from, node: from }, to: to === undefined ? edge.to : { ...edge.to, node: to } },
      real: edge,
      sourceHandle: from === undefined ? sourceHandle : memberHandle(sourceHandle, edge.from.node),
      targetHandle: to === undefined ? targetHandle : memberHandle(targetHandle, edge.to.node),
    });
    if (from !== undefined) crossing.push({ module: from, end: edge.from });
    if (to !== undefined) crossing.push({ module: to, end: edge.to });
  }

  const order = new Map(graph.nodes.map((node, index) => [node.id, index]));
  const boxes = new Map<NodeId, Box>();
  for (const module of modules) {
    if (open.has(module.id)) continue;
    const audioIn = new Map<string, BoxPort>();
    const notesIn = new Map<string, BoxPort>();
    const audioOut = new Map<string, BoxPort>();
    const modIn = new Map<string, BoxPort>();
    const modOut = new Map<string, BoxPort>();
    const add = (into: Map<string, BoxPort>, end: Endpoint, kind: BoxPort["kind"], label: string): void => {
      const key = keyOf(end);
      if (!into.has(key)) into.set(key, { member: end.node, handle: handleOf(end.port), label, kind });
    };
    const called = (id: NodeId): string => names.get(id) ?? "?";
    /** What an input or an output of audio is called: the node, and what its plugin calls the input where it has two. */
    const audioLabel = (end: Endpoint): string => {
      const port = portName(graph, end.node, end.port);
      return port === null ? called(end.node) : `${called(end.node)}: ${port}`;
    };
    let plugins = 0;
    let modulators = 0;
    const knobs: BoxKnob[] = [];
    for (const node of inside.get(module.id) ?? []) {
      if (node.kind === "effect") {
        plugins += 1;
        for (const port of node.side === undefined ? ([{ type: "audioIn" }] as const) : ([{ type: "audioIn" }, { type: "audioInSide" }] as const)) {
          const end = { node: node.id, port };
          if (!fed.has(keyOf(end))) add(audioIn, end, "audio", audioLabel(end));
        }
        const out = { node: node.id, port: { type: "audioOut" } as const };
        if (!taken.has(keyOf(out))) add(audioOut, out, "audio", called(node.id));
      } else if (node.kind === "instrument" && node.plugin !== null) {
        plugins += 1;
        const out = { node: node.id, port: { type: "audioOut" } as const };
        if (!taken.has(keyOf(out))) add(audioOut, out, "audio", called(node.id));
        if (laneTakesNotes(node)) add(notesIn, { node: node.id, port: { type: "eventsIn" } }, "notes", called(node.id));
      } else if (node.kind === "modulator") {
        modulators += 1;
        if (node.source.type === "macro" && macroExposed(node.source)) knobs.push({ member: node.id, label: called(node.id) });
      }
    }
    for (const { module: of, end } of crossing) {
      if (of !== module.id) continue;
      switch (end.port.type) {
        case "audioOut":
          add(audioOut, end, "audio", called(end.node));
          break;
        case "audioIn":
        case "audioInSide":
          add(audioIn, end, "audio", audioLabel(end));
          break;
        case "eventsIn":
          add(notesIn, end, "notes", called(end.node));
          break;
        case "modOut":
          add(modOut, end, "modulation", called(end.node));
          break;
        default:
          // (A cable of modulation that ends at a parameter, a volume, a mix, a pan, a setting of a modulator or the depth of a cable inside.)
          add(modIn, end, "modulation", endLabel(graph, names, end));
      }
    }
    /** A box's ports in the order of the nodes they are the ports of (the order of the graph), and the order of the handles of one node. */
    const ordered = (ports: Map<string, BoxPort>): BoxPort[] =>
      [...ports.values()].sort((a, b) => (order.get(a.member) ?? 0) - (order.get(b.member) ?? 0) || (a.handle < b.handle ? -1 : a.handle > b.handle ? 1 : 0));
    boxes.set(module.id, { node: module.id, plugins, modulators, inputs: [...ordered(audioIn), ...ordered(notesIn)], outputs: ordered(audioOut), modIn: ordered(modIn), modOut: ordered(modOut), knobs });
  }
  return { graph: { nodes, edges: placed.map((cable) => cable.edge) }, edges: placed, boxes, moduleOf };
}
