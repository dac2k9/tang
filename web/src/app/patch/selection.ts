// What a module can hold of the nodes that are chosen on the canvas (steps 105 and 106): the plugins, which are the instruments of the lanes that have one and the effects that are in a lane, and the modulators that go with them.
// The server takes nothing else (`module::to_text_of`: a group's bus, a source of notes, an empty lane and the master are none of a module's), so that what is chosen with them in the box is left out here and the
// page says how many, and a module can still be made of the rest. A box (a module that is closed, step 108) that is chosen is what is in it: its instruments, its effects and its modulators.
import type { Graph, Node, NodeId, NodeUi } from "../../protocol";

/** What a module holds of what is chosen. */
export interface Held {
  /** The nodes that a module can hold, in the order they were chosen in. */
  readonly nodes: readonly NodeId[];
  /** How many of them are plugins (an instrument or an effect): a module needs one at least, which the modulators that are chosen go with. */
  readonly plugins: number;
  /** How many of the chosen it cannot hold. */
  readonly left: number;
}

/** What a module has in it that a module can hold, in the order of the graph: the instruments of its lanes and their effects, and its modulators. A lane that has no instrument holds nothing but its effects. */
function contentsOf(graph: Graph, module: Extract<Node, { kind: "module" }>): NodeId[] {
  const members = new Set(module.members);
  return graph.nodes.flatMap((node) =>
    (node.kind === "instrument" && members.has(node.id) && node.plugin !== null) ||
    (node.kind === "effect" && node.parent !== null && members.has(node.parent)) ||
    (node.kind === "modulator" && members.has(node.id))
      ? [node.id]
      : [],
  );
}

/** What a module holds of the nodes in `chosen`: see the top of this file. A node that the session does not have is one that cannot be held. */
export function modulePluginsOf(graph: Graph | null, chosen: readonly NodeId[]): Held {
  const nodes: NodeId[] = [];
  let plugins = 0;
  // (A box that is chosen is what is in it, each once, whatever else is chosen with it.)
  const wanted = new Set(
    chosen.flatMap((id) => {
      const found = graph?.nodes.find((candidate) => candidate.id === id);
      return graph && found?.kind === "module" ? contentsOf(graph, found) : [id];
    }),
  );
  for (const id of wanted) {
    const node = graph?.nodes.find((candidate) => candidate.id === id);
    const holdsAPlugin =
      node?.kind === "instrument"
        ? node.plugin !== null
        : node?.kind === "effect" && graph?.nodes.some((owner) => owner.id === node.parent && owner.kind === "instrument") === true;
    if (holdsAPlugin || node?.kind === "modulator") nodes.push(id);
    if (holdsAPlugin) plugins += 1;
  }
  return { nodes, plugins, left: wanted.size - nodes.length };
}

/** What collapsing the nodes that are chosen into one module takes (step 109). */
export interface Collapsible {
  /** The nodes to ask the server to collapse: those of the chosen that a module can take, each once, in the order they were chosen in. */
  readonly nodes: readonly NodeId[];
  /** How many lanes the module comes to, each whole (a lane that has any of its plugins chosen is in it with all of them), and how many modulators. */
  readonly lanes: number;
  readonly modulators: number;
  /** How many plugins of those lanes were not chosen and come along with them. */
  readonly brought: number;
  /** How many of the chosen it cannot take: a box, a node that is in a module already, a source of notes, a group (and its bus) and the master. */
  readonly left: number;
  /** Every node that is in the module once it is made, and so is not on the canvas while the module is closed: the lanes, the effects in them and the modulators. */
  readonly hides: ReadonlySet<NodeId>;
}

/** Nothing is collapsed. */
const NOTHING_TO_COLLAPSE: Collapsible = { nodes: [], lanes: 0, modulators: 0, brought: 0, left: 0, hides: new Set() };

/**
 * What collapsing the nodes in `chosen` into one module takes, as the controller works it out (`collapse_into_module_intent`, step 109): the lane is the unit, so that an instrument or an effect that is chosen is its lane, with its instrument and
 * every effect it has, and a modulator is itself. A node that a module cannot take is left out and counted (`left`): a box (a module cannot hold a module yet), a node that is in a module already (a lane, an effect of its chain or a modulator of
 * one that is open, which has no box), a source of notes, a group, an effect on a group's bus and the master. A node that the session does not have is one that cannot be taken.
 */
export function collapsibleOf(graph: Graph | null, chosen: readonly NodeId[]): Collapsible {
  if (graph === null) return NOTHING_TO_COLLAPSE;
  const byId = new Map(graph.nodes.map((node) => [node.id, node]));
  const inModule = new Set(graph.nodes.flatMap((node) => (node.kind === "module" ? node.members : [])));
  const lanes = new Set<NodeId>();
  const modulators = new Set<NodeId>();
  const nodes: NodeId[] = [];
  let left = 0;
  for (const id of new Set(chosen)) {
    const node = byId.get(id);
    const parent = node?.kind === "effect" && node.parent !== null ? byId.get(node.parent) : undefined;
    const lane = node?.kind === "instrument" ? node : parent?.kind === "instrument" ? parent : undefined;
    const member = lane ?? (node?.kind === "modulator" ? node : undefined);
    if (member === undefined || inModule.has(member.id)) {
      left += 1;
      continue;
    }
    (member.kind === "modulator" ? modulators : lanes).add(member.id);
    nodes.push(id);
  }
  // (Every plugin that a lane has, and the lane's own node: the nodes that are not on the canvas once the module is closed.)
  const hides = new Set<NodeId>([...lanes, ...modulators]);
  let brought = 0;
  for (const node of graph.nodes) {
    const inside = (node.kind === "effect" && node.parent !== null && lanes.has(node.parent)) || (node.kind === "instrument" && lanes.has(node.id) && node.plugin !== null);
    if (!inside) continue;
    hides.add(node.id);
    if (!nodes.includes(node.id)) brought += 1;
  }
  return { nodes, lanes: lanes.size, modulators: modulators.size, brought, left, hides };
}

/** What collapsing takes, in words: `2 lanes and 1 modulator`, `1 lane`, `3 modulators`. */
export function collapseWords(collapsible: Pick<Collapsible, "lanes" | "modulators">): string {
  const lanes = `${String(collapsible.lanes)} ${collapsible.lanes === 1 ? "lane" : "lanes"}`;
  const modulators = `${String(collapsible.modulators)} ${collapsible.modulators === 1 ? "modulator" : "modulators"}`;
  if (collapsible.modulators === 0) return lanes;
  return collapsible.lanes === 0 ? modulators : `${lanes} and ${modulators}`;
}

/** Why the button that collapses has none to collapse, or null if it has: in words for its title. */
export function collapseWhyNot(collapsible: Pick<Collapsible, "lanes" | "modulators">, chosen: number): string | null {
  if (collapsible.lanes + collapsible.modulators > 0) return null;
  return chosen === 0 ? "Choose the nodes to collapse into a module" : "None of what is chosen can be collapsed: a module holds lanes and modulators that are in no module";
}

/** What the button that collapses says it does, for its title: what it takes (and how many more plugins of those lanes come along) and that it can be opened and ungrouped, or why it cannot be pressed. */
export function collapseTitle(collapsible: Pick<Collapsible, "lanes" | "modulators" | "brought">, chosen: number): string {
  const why = collapseWhyNot(collapsible, chosen);
  if (why !== null) return why;
  const more = collapsible.brought > 0 ? `, with ${String(collapsible.brought)} more ${collapsible.brought === 1 ? "plugin" : "plugins"} from their lanes` : "";
  return `Collapse ${collapseWords(collapsible)} into one box where they are${more}: open it to see them again, or ungroup it`;
}

/**
 * Where the box of a module that is made of what is chosen is drawn, so that it takes the place of what it hides: the top left corner of the nodes that go into it, which are on the canvas at `positions` (the nodes that are not, such as a
 * lane that has no card, are not asked for). None if none of them is on the canvas.
 */
export function collapsePlace(hides: ReadonlySet<NodeId>, positions: ReadonlyMap<NodeId, NodeUi>): NodeUi | undefined {
  let place: NodeUi | undefined;
  for (const [node, at] of positions) {
    if (!hides.has(node)) continue;
    place = place === undefined ? { x: at.x, y: at.y } : { x: Math.min(place.x, at.x), y: Math.min(place.y, at.y) };
  }
  return place;
}
