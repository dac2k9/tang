// The session as a canvas: which node goes where, and which cable joins which two ports
// (docs/web-ui.md §9.3). A pure function of the graph the server sends: the layout is a
// lane strip for each instrument (the instrument, then its effects, left to right), its
// modulators above the strip, over the plugin each one drives (and a modulator that drives
// the settings of another above that one), the groups to the right of the lanes that feed
// them, the master at the end, and the sources of notes in a column of their own before the lanes (level with the middle of
// them, as the master is with the middle of the lanes and the groups). A lane that only holds effects (`holdersOf`: an effect that stands by itself is one) has no card: its strip is its effects, from the
// first column, and the lane's cell (mute, solo, way out) is on the first of them. That is where a node is unless the
// session says otherwise: a node that has been moved has a place (`Node.ui`, kept in the
// session file, §4.4), and is drawn there; the layout of the others does not leave room for it,
// but a node that the layout would put on one that was placed (or too near it) is moved clear of it (`clearOfPlaced`).
// The canvas is not rearranged under a person's hand: given the places that the last layout put the nodes at, a node stays
// where it was while the slot it was put for is the same, and only what is new (or is in another slot: an effect that was
// moved along its chain) goes where the layout puts it, level with its strip and clear of the nodes that stayed. Room is made where
// it is needed: what has grown, or a modulator that is added to a strip that has no room above its row, pushes the rows below it
// down, strips and all (`makeRoom`). Every node goes where the layout puts it when nothing is remembered, which is what arranging
// the nodes again does: from nothing, with no node placed by a person, and when `FlowOptions.arrange` says so, the places of the layout are the start of an arrangement that makes the cables
// short (`arrange.ts`, step 98), and those are the places that every node goes to, and that the next layout leaves them at.
import type { Edge as FlowEdge, Node as FlowNode } from "@xyflow/react";

import { holdersOf } from "../../client/holders";
import { cableWords, displayNames, endLabel } from "../../client/names";
import { laneTakesNotes } from "../../client/noteWiring";
import { knobsOf } from "../../client/pinning";
import { drivablesOf } from "../../client/reach";
import type { Drivable } from "../../client/reach";
import type { Edge, EdgeId, Graph, Node, NodeId, NodeUi, Plugin, Port, SideInfo, SourceInfo } from "../../protocol";
import { arrange } from "./arrange";
import type { Block, Wire } from "./arrange";
import { boxRows, collapse } from "./boxes";
import type { Box as ModuleBox } from "./boxes";
import { handleOf, SETTING_HANDLES } from "./handles";
import type { SettingHandle } from "./handles";

// (The names of the handles are in `handles.ts`, which the boxes need too: they are said here as they always were said.)
export { depthHandle, handleOf, paramHandle, SETTING_HANDLES } from "./handles";
export type { SettingHandle } from "./handles";

/** The width of every node, so that the columns of the canvas are known without measuring anything. */
export const NODE_WIDTH = 304;
/** The distance from one column of the canvas to the next. */
export const COLUMN = NODE_WIDTH + 72;
/** Room for a cable to arrive, between the modulators above a strip and the strip. */
export const CABLE_GAP = 72;
export const ROW_GAP = 56;

// What the layout assumes about the size of a node: the components' CSS is built to these.
const HEADER_HEIGHT = 44;
const CONTROL_ROW_HEIGHT = 108;
const CONTROLS_PER_ROW = 4;
/** The width of a control's cell in a node's grid (`.tang-node__controls`: four columns of 72 px, in the middle of the node). */
const CELL_WIDTH = 72;
const NODE_PADDING = 16;
/** The list at the foot of a modulator, with which it draws a cable (measured in a browser: 29). */
const DRIVES_HEIGHT = 30;
/** The trace of what a modulator puts out is this much taller than the bar it was (44 against 8: the bar's nodes were 119 and 213). */
const TRACE_EXTRA = 36;
/** The shape of an envelope's settings (a drawing a little under 60 px tall, and the gap above it) makes its node this much taller than an LFO's (measured in a browser: 315 against 249). */
const ENVELOPE_EXTRA = 66;
/** A modulator that has no settings (a MIDI control) is this tall (measured in a browser: 155)... */
export const MODULATOR_HEIGHT = 128 + TRACE_EXTRA;
/** ...and one that has them has a row of controls more (measured in a browser: 249). */
export const MODULATOR_SETTINGS_HEIGHT = HEADER_HEIGHT + CONTROL_ROW_HEIGHT + NODE_PADDING + 20 + DRIVES_HEIGHT + TRACE_EXTRA;
/** ...and an envelope's, which draws its shape, is taller by that. */
export const ENVELOPE_NODE_HEIGHT = MODULATOR_SETTINGS_HEIGHT + ENVELOPE_EXTRA;
/** The bars of a step sequencer (`StepBars.css`: 64 px tall) and the gap before them in the body of the node (8) make its node this much taller than an LFO's (measured in a browser: 321 against 249). */
export const STEPS_EXTRA = 64 + 8;
/** ...and a step sequencer's node is as tall as an LFO's with the bars. */
export const STEPS_NODE_HEIGHT = MODULATOR_SETTINGS_HEIGHT + STEPS_EXTRA;
/** The master: its name and its meter, the scope under them and the choices of the scope's view, a row of three cells (the volume, the limiter's ceiling and the limiter's button) and how far the limiter works (measured in a browser: 373; 223 before step 102 put the last two in). */
export const MASTER_HEIGHT = 378;
/** A source of notes: its header, the line that says what it is and where it is plugged in (18) and the room at its foot (12) (measured in a browser: 74). */
export const SOURCE_HEIGHT = HEADER_HEIGHT + 18 + 12;
/** A scope that an instrument, an effect or a group shows below its controls, with the choices of its view and its time: this much taller (measured in a browser: 157). */
export const NODE_SCOPE_HEIGHT = 157;
/** The pattern of a lane, under its controls (record, clear and its length, its three switches, the line that says what it is), as tall whatever it shows: a lane's node is this much taller (measured in a browser: 97, in both states of the record button). */
export const PATTERN_HEIGHT = 97;
/** A box that stands for a module has a row for each of its inputs and outputs, this tall (the style says so: `.tang-box__ports`)... */
export const BOX_ROW_HEIGHT = 28;
/** ...and the line under them that says what is in it, this tall. */
export const BOX_DETAIL_HEIGHT = 20;
/** A cable that a modulator's node lists is a row this tall (the style says so: `.tang-cable-row`)... */
export const CABLE_ROW_HEIGHT = 20;
/** ...and the list has this much room before it, as much as the rest of what is in the body of the node (`.tang-node__body`'s gap). */
export const CABLES_GAP = 8;

export type PatchNodeKind = "instrument" | "effect" | "modulator" | "group" | "master" | "source" | "module";

export interface PatchNodeData extends Record<string, unknown> {
  /** The node of the session this one is the picture of. */
  node: NodeId;
  /** What to call it: the server's name, with a number after it when another node has the same (`displayNames`). */
  name: string;
  /** What kind of plugin it has (`Built-in`, `LV2`, `CLAP`, `VST3`), if it has one. */
  format: string | null;
  /** The parameters of its plugin it shows as knobs, by the plugin's own index: the ones pinned to it or the first few (`knobsOf`), and every one a cable ends at. */
  knobs: readonly number[];
  /** Those of them that a modulator drives (a cable ends at them). */
  modulated: readonly number[];
  /** For a modulator: what it can still be drawn to, plugin by plugin and modulator by modulator (`drivablesOf`); nothing for the other nodes. */
  targets: readonly Drivable[];
  /** For a modulator: the settings of it that a cable ends at, as the names of their handles, in the order a modulator has them; nothing for the other nodes. */
  drivenSettings: readonly SettingHandle[];
  /** For a modulator: the cables that start at it, in the order of the graph, as its node lists them (`CableRow`); nothing for the other nodes. */
  cables: readonly CableRow[];
  /** For a lane: whether it takes notes (`laneTakesNotes`), which its card has the keys, the pattern and the input of events for; an audio input does not. False for the other nodes. */
  notes: boolean;
  /** For a lane: whether its card has the choice of group, which it has only while the session has a group (the page offers no way to make one: step 103). False for the other nodes. */
  groupCell: boolean;
  /** Whether it shows a scope of what comes out of it (the viewer's own choice, `toFlow`'s `scopes`, for a node that has a signal: `canScope`), which it is taller by. */
  scope: boolean;
  /** For a source of notes: what it is (the keys, or a device by the name it stands for); nothing for the other nodes. */
  source: SourceInfo | null;
  /** For an effect in a lane that only holds effects (the canvas draws no card for such a lane): which lane, and what the module has of the lane; nothing for the other nodes. */
  heldIn: HeldIn | null;
  /** For an effect that has a second input of audio (a Math): what its plugin calls its two inputs, which the node has a port each for; nothing for the other nodes. */
  side: SideInfo | null;
  /** For a module that is closed, which is a box on the canvas (step 108): its ports and what is in it (`collapse`); nothing for the other nodes. */
  box: ModuleBox | null;
  /** For a lane of a module that is open (a lane that has no card: the first of its effects): the module, which the tag over the card closes; nothing for the other nodes. */
  module: ModuleTag | null;
}

/** The module that a lane on the canvas is part of, while the module is open: its node, and its name, for the tag over the card that closes it again. */
export interface ModuleTag {
  node: NodeId;
  name: string;
}

/** What the module of an effect has of the lane that holds it, a lane the canvas draws no card for (`holdersOf`). */
export interface HeldIn {
  /** The lane that holds it: the node that the mute, the solo, the way out and the removal of the effect are of. */
  lane: NodeId;
  /** Whether it is the first of the lane's effects, which has what the card said of the lane as a whole: the mute, the solo and the way out, and the "+" that adds a modulator to the lane. */
  first: boolean;
  /**
   * Whether it is on its own in the lane (its only effect): taking it out takes the lane out with it, and the modulators that hear the lane stay, hearing every note. Not the last lane of the session,
   * which a session keeps.
   */
  alone: boolean;
}

/**
 * A cable that starts at a modulator, as the modulator's node lists it: a row that says what the cable goes to, with a place on its
 * edge where another modulator's cable can end, to drive how deep this one goes.
 */
export interface CableRow {
  /** The cable. */
  edge: EdgeId;
  /** What the cable ends at, in words (`Filter: cutoff`, `depth of LFO → Filter: cutoff`: `endLabel`). */
  label: string;
  /** Whether a cable ends at its depth: another modulator drives how deep it goes. */
  driven: boolean;
}

export type PatchNode = FlowNode<PatchNodeData, PatchNodeKind>;

export type PatchEdgeKind = "audio" | "modulation" | "send" | "notes" | "hears";

export interface PatchEdgeData extends Record<string, unknown> {
  /**
   * The id of a modulation cable, by which the cable reads how deep it goes, of an audio cable (a `send`), by which it reads how loud it is (a value, which is
   * turned at the pace of the frames, and so not part of what the canvas is laid out by), or of a cable of notes, by which it is taken out.
   */
  cable?: EdgeId;
  /** The modulator that drives the cable, whose list the focus goes to when the cable is taken out. */
  modulator?: NodeId;
  /** What the cable is, in words: `LFO → Filter: cutoff`, `Oscillator → Reverb`, `Keys → Oscillator`. */
  label?: string;
  /** The lane or the group whose way out the link is, on a link of the audio path that is one (the end of its chain): the node that the button on the link cuts the way out of. */
  wayOut?: NodeId;
  /** The effect that a link of the audio path goes into, on a link that is one (a link of a chain: the stage before an effect feeding it): the node that the button on the link cuts the feed of. */
  feedOf?: NodeId;
}

export type PatchEdge = FlowEdge<PatchEdgeData, PatchEdgeKind>;

/** A place that the layout put a node at, and the slot that the node was in then (`slotsOf`): the next layout keeps it there while the slot is the same. */
export interface Remembered {
  x: number;
  y: number;
  slot: string;
}

/** The places that a layout put the nodes that nobody has placed at, by node: what `toFlow` is given, to leave them where they are. */
export type Places = ReadonlyMap<NodeId, Remembered>;

export interface Flow {
  nodes: PatchNode[];
  edges: PatchEdge[];
  /** Where the nodes that the layout placed are, for the next layout to leave them there (the nodes that a person placed have the session's place). */
  places: Places;
  /** Whether every node is where a layout that remembered nothing puts it: false when the places that were remembered keep a node away from it. */
  arranged: boolean;
}

/** The parameters that a modulation cable ends at, for each node. */
function modulatedParams(graph: Graph): Map<NodeId, Set<number>> {
  const modulated = new Map<NodeId, Set<number>>();
  for (const edge of graph.edges) {
    if (edge.to.port.type !== "param") continue;
    const params = modulated.get(edge.to.node) ?? new Set<number>();
    params.add(edge.to.port.index);
    modulated.set(edge.to.node, params);
  }
  return modulated;
}

/** The settings of each modulator that a cable ends at: the names of their handles, in the order a modulator has them. */
function drivenSettings(graph: Graph): Map<NodeId, SettingHandle[]> {
  const driven = new Map<NodeId, Set<string>>();
  for (const edge of graph.edges) {
    const settings = driven.get(edge.to.node) ?? new Set<string>();
    settings.add(edge.to.port.type);
    driven.set(edge.to.node, settings);
  }
  return new Map([...driven].map(([node, settings]) => [node, SETTING_HANDLES.filter((handle) => settings.has(handle))]));
}

/**
 * How many controls a node shows: its parameters, and its own gain (a volume or a mix; a lane's keys, its group and its mute and solo too) or a group's three. `carriesLane` is whether an effect has
 * the cell of the lane that holds it (the mute, the solo and the way out of a lane the canvas draws no card for): one control more; it is nothing to the other nodes. `groupCell` is whether a lane has the
 * choice of group, which it has only while the session has a group: one control more; it is nothing to the other nodes.
 */
export function controlCount(node: Node, knobs: number, carriesLane = false, groupCell = false): number {
  switch (node.kind) {
    case "instrument":
      // (Its gain, its keys, its group and its mute and solo: a lane that takes no notes has no keys, and one has no choice of group while the session has no group.)
      return knobs + 2 + (laneTakesNotes(node) ? 1 : 0) + (groupCell ? 1 : 0);
    case "effect":
      return knobs + 1 + (carriesLane ? 1 : 0);
    case "group":
      return 3;
    default:
      return 0;
  }
}

/**
 * Whether a node has a signal that it can show a scope of: an instrument that has a plugin (a lane that has none has no sound), an effect and a group. A modulator has no audio,
 * and the master shows its scope always.
 */
export function canScope(node: Node): boolean {
  return node.kind === "effect" || node.kind === "group" || (node.kind === "instrument" && node.plugin !== null);
}

/**
 * How tall a node is, as the layout assumes it (the components' styles keep to it). `cables` is how many a modulator lists (`CableRow`); it is nothing to the other nodes.
 * `scope` is whether an instrument, an effect or a group shows its scope, which it is taller by (`NODE_SCOPE_HEIGHT`); it is nothing to a modulator, and to the master.
 * `carriesLane` is whether an effect has the cell of the lane that holds it, and `groupCell` whether a lane has the choice of group (`controlCount`): they are nothing to the other nodes.
 * `rows` is how many rows of ports a box that stands for a module has (`boxRows`): it is nothing to the other nodes.
 */
export function nodeHeight(node: Node, knobs: number, cables = 0, scope = false, carriesLane = false, groupCell = false, rows = 1): number {
  switch (node.kind) {
    case "module":
      return HEADER_HEIGHT + Math.max(1, rows) * BOX_ROW_HEIGHT + BOX_DETAIL_HEIGHT + NODE_PADDING;
    case "modulator": {
      const listed = cables > 0 ? CABLES_GAP + cables * CABLE_ROW_HEIGHT : 0;
      if (node.source.type === "midiLearn") return MODULATOR_HEIGHT + listed;
      if (node.source.type === "steps") return STEPS_NODE_HEIGHT + listed;
      return (node.source.type === "envelope" ? ENVELOPE_NODE_HEIGHT : MODULATOR_SETTINGS_HEIGHT) + listed;
    }
    case "master":
      return MASTER_HEIGHT;
    case "source":
      return SOURCE_HEIGHT;
    default: {
      const rows = Math.max(1, Math.ceil(controlCount(node, knobs, carriesLane, groupCell) / CONTROLS_PER_ROW));
      return HEADER_HEIGHT + rows * CONTROL_ROW_HEIGHT + NODE_PADDING + (laneTakesNotes(node) ? PATTERN_HEIGHT : 0) + (scope ? NODE_SCOPE_HEIGHT : 0);
    }
  }
}

/** A strip: nodes in a row, left to right, from a column, with the modulators that sit above them. */
interface Strip {
  /** The node the strip is of: an instrument or a group. */
  head: Node;
  /** The nodes in the row, the head first (a lane that only holds effects has no card on the canvas: its row is its effects). */
  row: Node[];
  /** The rows of modulators above the strip's row, the nearest first (`tiersOf`). */
  tiers: Node[][];
  /** The column of each modulator, from the strip's first. */
  columns: Map<Node, number>;
}

/**
 * Whose strip a node is in: the lane or group that an effect is in and, for a modulator, the lane or group that it hears (every modulator is the session's own and has no parent since step 96: it is laid out above the row of
 * the lane or group it hears, as a lane's modulator was when it was in the lane). None for a modulator that hears every note, which is in a row of its own above the first lane, and for the nodes that are strips themselves.
 * An ear that is not a lane or a group of the graph is no ear (the server tells none, but a graph can be anything). (A modulator that has a parent, the old form that no server makes any more, is still laid out in its parent's strip.)
 */
type OwnerOf = (node: Node) => NodeId | null;

function ownersOf(graph: Graph): OwnerOf {
  const heads = new Set(graph.nodes.flatMap((node) => (node.kind === "instrument" || node.kind === "group" || node.kind === "module" ? [node.id] : [])));
  return (node) => (node.kind === "modulator" && node.parent === null && node.hears !== undefined && heads.has(node.hears) ? node.hears : node.parent);
}

/**
 * A strip of the instrument or group `head`: its nodes, and the modulators that belong to it, those in its rack and those that hear it. A lane that only holds effects has no card (`drawn` is false): its row is its effects, from the first
 * column, and its modulators are above them as for any lane.
 */
function stripOf(graph: Graph, head: Node, ownerOf: OwnerOf, drawn = true): Strip {
  const children = graph.nodes.filter((node) => ownerOf(node) === head.id);
  const row = [...(drawn ? [head] : []), ...children.filter((node) => node.kind === "effect")];
  const tiers = tiersOf(graph, children.filter((node) => node.kind === "modulator"));
  return { head, row, tiers, columns: modulatorColumns(graph, row, tiers) };
}

/**
 * The modulators of a rack in rows above its strip, the nearest the strip first. A cable runs from the bottom of a node to the
 * top of what it drives, so a modulator that drives a setting of another (that the canvas draws a cable to) is a row above it:
 * a modulator's row is as many above the strip's as the longest chain of modulators it drives (none for one that drives no
 * other). A rack in which none drives another is one row. A cycle is cut where it is first met, in the order of the graph.
 */
function tiersOf(graph: Graph, modulators: readonly Node[]): Node[][] {
  const drives = new Map<NodeId, NodeId[]>(modulators.map((node) => [node.id, []]));
  for (const edge of graph.edges) {
    // (Only the cables of the rack, from one of its modulators to another.)
    if (drives.has(edge.to.node)) drives.get(edge.from.node)?.push(edge.to.node);
  }
  const heights = new Map<NodeId, number>();
  const visiting = new Set<NodeId>();
  const heightOf = (id: NodeId): number => {
    const known = heights.get(id);
    if (known !== undefined) return known;
    visiting.add(id);
    let height = 0;
    for (const driven of drives.get(id) ?? []) {
      if (!visiting.has(driven)) height = Math.max(height, heightOf(driven) + 1);
    }
    visiting.delete(id);
    heights.set(id, height);
    return height;
  };
  const tiers: Node[][] = [];
  for (const modulator of modulators) {
    const tier = heightOf(modulator.id);
    tiers[tier] = [...(tiers[tier] ?? []), modulator];
  }
  // (A modulator's row is one above the highest of those it drives, so no row between the first and the last is empty.)
  return tiers;
}

/**
 * How many columns a strip takes: its row, or as far as its modulators go if they go past the row (a
 * lane with more modulators than nodes). What comes after it, groups and the master, starts after that.
 */
function widthOf(strip: Strip): number {
  return Math.max(strip.row.length, ...[...strip.columns.values()].map((column) => column + 1));
}

/**
 * Where the modulators of a strip go, as columns from its first: above the node each drives (the first of them, if it drives
 * more than one), or the first column that is free in its row. The rows are placed from the nearest the strip, so that what
 * a modulator drives of the others is placed before it.
 */
function modulatorColumns(graph: Graph, row: readonly Node[], tiers: readonly Node[][]): Map<Node, number> {
  const columnOf = new Map<NodeId, number>(row.map((node, column) => [node.id, column]));
  const placed = new Map<Node, number>();
  for (const tier of tiers) {
    const taken = new Set<number>();
    for (const modulator of tier) {
      const aimed = graph.edges
        .filter((edge) => edge.from.node === modulator.id)
        .map((edge) => columnOf.get(edge.to.node))
        .filter((column): column is number => column !== undefined);
      let column = aimed.length > 0 ? Math.min(...aimed) : 0;
      while (taken.has(column)) column += 1;
      taken.add(column);
      columnOf.set(modulator.id, column);
      placed.set(modulator, column);
    }
  }
  return placed;
}

/** Where each node of the graph goes on the canvas, as tall as `heightOfNode` says each is. The lanes that only hold effects (`holders`) have a strip and no card: nothing is placed for them. */
function layout(graph: Graph, heightOfNode: (node: Node) => number, holders: ReadonlyMap<NodeId, readonly Node[]>, ownerOf: OwnerOf): Map<NodeId, { x: number; y: number }> {
  const placed = new Map<NodeId, { x: number; y: number }>();
  const place = (node: Node, column: number, y: number): void => {
    placed.set(node.id, { x: column * COLUMN, y });
  };

  /** The room a row of modulators needs: the tallest of them, and the room for a cable to arrive below it. */
  const roomOf = (tier: readonly Node[]): number => Math.max(...tier.map(heightOfNode)) + CABLE_GAP;

  /** The room a strip's modulators need above its row: a row of them for each step of what drives what. */
  const aboveOf = (strip: Strip): number => strip.tiers.reduce((room, tier) => room + roomOf(tier), 0);

  /** Put a strip's row from `column` on, its top at `rowTop`, its modulators above it, the rows of them from the nearest; the y at the bottom of it. */
  const placeStrip = (strip: Strip, column: number, rowTop: number): number => {
    strip.row.forEach((node, offset) => {
      place(node, column + offset, rowTop);
    });
    let below = rowTop;
    for (const tier of strip.tiers) {
      below -= roomOf(tier);
      for (const modulator of tier) place(modulator, column + (strip.columns.get(modulator) ?? 0), below);
    }
    return rowTop + Math.max(...strip.row.map(heightOfNode)) + ROW_GAP;
  };

  // The session's own modulators that hear every note (a modulator that has no parent is in no lane and no group: step 91; one that hears a lane or a group is in that strip, step 94) in rows at the top, above the first lane's,
  // the nearest the lanes first: what a cable from one drives is in the lanes, below it, so that the cable runs down to it. A session that has none has no room for them.
  const free = graph.nodes.filter((node) => node.kind === "modulator" && ownerOf(node) === null);
  const freeTiers = tiersOf(graph, free);
  const freeColumns = modulatorColumns(graph, [], freeTiers);
  let y = freeTiers.reduce((room, tier) => room + roomOf(tier), 0);
  const freeRoom = y;
  const freeColumnsTaken = new Set(freeColumns.values());
  let freeBelow = y;
  for (const tier of freeTiers) {
    freeBelow -= roomOf(tier);
    for (const modulator of tier) place(modulator, freeColumns.get(modulator) ?? 0, freeBelow);
  }

  // One strip for each instrument, from there: modulators above, the row below them.
  const lanes = graph.nodes.filter((node) => node.kind === "instrument").map((lane) => stripOf(graph, lane, ownerOf, !holders.has(lane.id)));
  const rowTops = new Map<NodeId, number>();
  for (const lane of lanes) {
    const rowTop = y + aboveOf(lane);
    rowTops.set(lane.head.id, rowTop);
    y = placeStrip(lane, 0, rowTop);
  }

  // A strip for each module that is a box (step 108), under the lanes: the box, with the modulators that hear what is in it above it.
  const boxes = graph.nodes.filter((node) => node.kind === "module").map((module) => stripOf(graph, module, ownerOf));
  for (const box of boxes) {
    const rowTop = y + aboveOf(box);
    rowTops.set(box.head.id, rowTop);
    y = placeStrip(box, 0, rowTop);
  }

  // A strip for each group, to the right of the longest lane, its row level with the middle of its members'.
  const groups = graph.nodes.filter((node) => node.kind === "group").map((group) => stripOf(graph, group, ownerOf));
  const lanesWidth = Math.max(0, ...lanes.map(widthOf), ...boxes.map(widthOf));
  const wanted = groups.map((group) => {
    const tops = lanes.filter((lane) => lane.head.parent === group.head.id).map((lane) => rowTops.get(lane.head.id) ?? 0);
    return { group, rowTop: tops.length > 0 ? tops.reduce((sum, top) => sum + top, 0) / tops.length : y + aboveOf(group) };
  });
  wanted.sort((a, b) => a.rowTop - b.rowTop);
  let groupsBottom = 0;
  for (const { group, rowTop } of wanted) {
    // (The modulators above a group's row keep below the row of the session's own that hear every note, where they are in the same column as one of them: a group is level with its members, which can be as high as the first lane.)
    const floor = [...group.columns.values()].some((column) => freeColumnsTaken.has(lanesWidth + column)) ? freeRoom + aboveOf(group) : 0;
    groupsBottom = placeStrip(group, lanesWidth, Math.max(rowTop, groupsBottom + aboveOf(group), floor));
  }

  // The master at the end, in the middle of everything.
  const groupsWidth = Math.max(0, ...groups.map(widthOf));
  const middle = (Math.max(y, groupsBottom) - ROW_GAP) / 2;
  for (const node of graph.nodes) {
    if (node.kind === "master") place(node, lanesWidth + groupsWidth, Math.max(0, middle - heightOfNode(node) / 2));
  }

  // The sources of notes in a column of their own before the lanes, one under the other, level with the middle of the lanes as a whole and not above the top of the canvas.
  const sources = graph.nodes.filter((node) => node.kind === "source");
  const stack = sources.reduce((sum, source) => sum + heightOfNode(source) + ROW_GAP, -ROW_GAP);
  let sourceTop = Math.max(0, (y - ROW_GAP) / 2 - stack / 2);
  for (const source of sources) {
    place(source, -1, sourceTop);
    sourceTop += heightOfNode(source) + ROW_GAP;
  }

  // Whatever the layout did not reach (a node whose parent is not there) goes in a row below (a lane that only holds effects has no card to place).
  let stray = 0;
  for (const node of graph.nodes) {
    if (placed.has(node.id) || holders.has(node.id)) continue;
    place(node, stray, Math.max(y, groupsBottom));
    stray += 1;
  }
  return placed;
}

/** How near a node that the layout places may be to one that is placed by a person, or one that stayed where it was, before it is moved: the sizes the layout assumes are the components' to a few pixels. */
const CLEARANCE = 16;

interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

interface Position {
  x: number;
  y: number;
}

const ORIGIN: Position = { x: 0, y: 0 };

/** Whether two boxes are over each other or nearer than `CLEARANCE`. */
const near = (a: Box, b: Box): boolean => a.left < b.right + CLEARANCE && b.left < a.right + CLEARANCE && a.top < b.bottom + CLEARANCE && b.top < a.bottom + CLEARANCE;

/** Whether two boxes are over each other. */
const over = (a: Box, b: Box): boolean => a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;

/**
 * What a node is in, as far as its place goes: the slot that its place was made for, so that it stays where it is until it is in another. An effect is in the place of
 * its chain that it is at (a chain that has an effect taken out closes up, and one that has two moved past each other has them change places); a modulator is in its
 * rack, and not at a place in it (nothing that is drawn to or from it moves it, and nothing that is taken out of the rack does); both are in the slot of the node they are
 * of, so that what is in a group's strip goes along with the group when it changes column; a modulator of the session's own that hears a lane or a group is in the slot of the one it hears, by its id too, so that it is
 * laid out again above the other strip when it is made to hear another (and stays where it was while it hears the same); a lane and a source are in the session; and a group and the master are in the column that the
 * layout puts them in, which is to the right of the longest lane (a lane that grows, with an effect added to it, moves them along).
 */
function slotsOf(graph: Graph, fresh: ReadonlyMap<NodeId, Position>, ownerOf: OwnerOf): Map<NodeId, string> {
  const slots = new Map<NodeId, string>();
  for (const node of graph.nodes) {
    if (node.kind === "group" || node.kind === "master") slots.set(node.id, `${node.kind}:${String(fresh.get(node.id)?.x ?? 0)}`);
    else if (node.kind === "instrument" || node.kind === "source" || node.kind === "module") slots.set(node.id, node.kind);
  }
  const effects = new Map<NodeId | null, number>();
  for (const node of graph.nodes) {
    const owner = ownerOf(node);
    const of = owner !== null ? (slots.get(owner) ?? String(owner)) : "";
    if (node.kind === "effect") {
      const index = effects.get(node.parent) ?? 0;
      effects.set(node.parent, index + 1);
      slots.set(node.id, `effect ${String(index)} of ${of}`);
    } else if (node.kind === "modulator") {
      // (One of the session's own that hears a lane or a group has the id of what it hears in its slot, since a lane's slot is the same for every lane.)
      slots.set(node.id, node.parent === null && owner !== null ? `modulator hearing ${of} ${String(owner)}` : `modulator of ${of}`);
    }
  }
  return slots;
}

/**
 * The places to start from: a node that was laid out before and is in the slot it was laid out for is where it was. A group and the master that are in another column
 * (a lane has grown past them) go along to it and stay at the height they were at. The others are where the layout puts them now, moved down or up as far as the strip they are
 * of has moved from where the layout would put it (an effect added to a lane that is not where the layout would have it any more is added level with the lane, and not level
 * with where the lane would be; what is in a group's strip, when the group has gone along to another column, goes along to it, at the group's height). A strip is moved
 * in height only by the rows' layout (its row is in the column that the layout gives it) and in width as well when an arrangement has put it somewhere else: what is added to it goes where
 * the strip's head has gone from where the layout would have it. `kept` is the nodes that stay exactly where they were. A lane that only holds effects has no card, and its strip goes by
 * its first effect (`anchors`: the lane by the first of its effects), as the others go by their cards.
 */
function carryOver(
  graph: Graph,
  fresh: ReadonlyMap<NodeId, Position>,
  slots: ReadonlyMap<NodeId, string>,
  remembered: Places,
  anchors: ReadonlyMap<NodeId, NodeId>,
  ownerOf: OwnerOf,
): { positions: Map<NodeId, Position>; kept: Set<NodeId> } {
  const positions = new Map<NodeId, Position>();
  const kept = new Set<NodeId>();
  for (const node of graph.nodes) {
    const before = node.ui ? undefined : remembered.get(node.id);
    if (before && before.slot === slots.get(node.id)) {
      positions.set(node.id, { x: before.x, y: before.y });
      kept.add(node.id);
    }
  }
  const isChild = (node: Node): boolean => node.kind === "effect" || node.kind === "modulator";
  // (The effect before an effect in its chain: a new effect goes by the one that it follows, which is level with it, and not by the strip's head, which an arrangement may have put at another height.)
  const previous = new Map<NodeId, NodeId>();
  const lastOf = new Map<NodeId | null, NodeId>();
  for (const node of graph.nodes) {
    if (node.kind !== "effect") continue;
    const before = lastOf.get(node.parent);
    if (before !== undefined) previous.set(node.id, before);
    lastOf.set(node.parent, node.id);
  }
  // (What has no strip, the session's own modulators that hear every note, goes by the first lane, above which its rows are: one that is new is above the first lane where it stayed, and not where the layout would have the lane.
  // One that hears a lane or a group goes by that strip, as a lane's modulator does.)
  const firstLane = graph.nodes.find((node) => node.kind === "instrument")?.id ?? null;
  // The strips' heads and the rest first, and then what is in a strip, which goes by where its head is.
  for (const node of graph.nodes.filter((candidate) => !isChild(candidate))) {
    if (node.ui || kept.has(node.id)) continue;
    const at = fresh.get(node.id) ?? ORIGIN;
    const before = remembered.get(node.id);
    positions.set(node.id, before && (node.kind === "group" || node.kind === "master") ? { x: at.x, y: before.y } : at);
  }
  for (const node of graph.nodes.filter(isChild)) {
    if (node.ui || kept.has(node.id)) continue;
    const at = fresh.get(node.id) ?? ORIGIN;
    // (What the strip goes by: its card, or for a lane that has none its first effect, which is where the strip's row is.)
    const parent = ownerOf(node) ?? firstLane;
    const before = previous.get(node.id);
    const anchor = before !== undefined && positions.has(before) ? before : parent !== null ? (anchors.get(parent) ?? parent) : null;
    const where = anchor !== null ? positions.get(anchor) : undefined;
    const would = anchor !== null ? fresh.get(anchor) : undefined;
    positions.set(node.id, where && would ? { x: at.x + where.x - would.x, y: at.y + where.y - would.y } : at);
  }
  return { positions, kept };
}

/** The box that a node takes where it is, by the sizes the layout assumes. */
const boxAt = (node: Node, at: Position, heightOf: (node: Node) => number): Box => ({ left: at.x, top: at.y, right: at.x + NODE_WIDTH, bottom: at.y + heightOf(node) });

/** A node that others keep clear of, and the box it takes. */
interface Obstacle {
  node: Node;
  box: Box;
}

/**
 * Move each of `moving` that is on one of the `fixed` obstacles, or nearer to one than `CLEARANCE`, clear of them: below it, with the gap of a row, and above it for a modulator, which
 * belongs above its strip; and clear of the others that it then comes to, one after the other in the order of the graph, so that two that were on the same node do not come to be on
 * each other. A node that is clear stays where it is: what is moved is what would have been on one. `matters` says which obstacles a node has to keep clear of.
 */
function moveClear(
  moving: readonly Node[],
  fixed: readonly Obstacle[],
  positions: Map<NodeId, Position>,
  heightOf: (node: Node) => number,
  matters: (mover: Node, obstacle: Obstacle) => boolean = () => true,
): void {
  const boxOf = (node: Node): Box => boxAt(node, positions.get(node.id) ?? ORIGIN, heightOf);
  const onOne = new Set(moving.filter((node) => fixed.some((other) => matters(node, other) && near(boxOf(node), other.box))));
  if (onOne.size === 0) return;
  // What a moved node has to keep clear of: the fixed ones, the others that were not on one (which stay), and the ones moved before it.
  const obstacles: Obstacle[] = [...fixed, ...moving.filter((node) => !onOne.has(node)).map((node) => ({ node, box: boxOf(node) }))];
  for (const node of moving.filter((candidate) => onOne.has(candidate))) {
    let at = positions.get(node.id) ?? ORIGIN;
    // (Each step passes an obstacle for good, so it takes no more steps than there are obstacles.)
    for (let step = 0; step <= obstacles.length; step += 1) {
      const here = boxAt(node, at, heightOf);
      const hit = obstacles.find((obstacle) => matters(node, obstacle) && near(here, obstacle.box));
      if (!hit) break;
      at = { x: at.x, y: node.kind === "modulator" ? hit.box.top - ROW_GAP - heightOf(node) : hit.box.bottom + ROW_GAP };
    }
    positions.set(node.id, at);
    obstacles.push({ node, box: boxAt(node, at, heightOf) });
  }
}

/**
 * Move the nodes that the layout places off the nodes that a person has placed (`Node.ui`). A place is a pin: the layout makes no room around a node that was moved, so a node that is
 * added later (an effect at the end of a chain, a lane, a modulator) goes where the layout puts it, which can be on a node that was placed there, and hide it, and so does a node that
 * stays where it was when the one next to it is put down on it. Every node that is not placed is moved clear of it, one that stayed too: what is moved is what would have been on one,
 * and a node that is dragged does not move the rest unless it is dropped on them.
 */
function clearOfPlaced(graph: Graph, positions: Map<NodeId, Position>, heightOf: (node: Node) => number): void {
  const placed = graph.nodes.flatMap((node) => (node.ui ? [{ node, box: boxAt(node, node.ui, heightOf) }] : []));
  if (placed.length === 0) return;
  moveClear(
    graph.nodes.filter((node) => !node.ui),
    placed,
    positions,
    heightOf,
  );
}

/**
 * Move the nodes that are new off the nodes that stayed where they were: a lane that is added is where the layout puts it, which is on what stayed if something was taken out above it,
 * and goes below that. A modulator keeps clear of the modulators that stayed (above them) and not of the rest: it goes in the room above its strip, which is where the strip above it has its
 * row if there is none, and `makeRoom` makes it.
 */
function clearOfKept(graph: Graph, positions: Map<NodeId, Position>, kept: ReadonlySet<NodeId>, heightOf: (node: Node) => number): void {
  const stayed = graph.nodes.flatMap((node) => (kept.has(node.id) ? [{ node, box: boxAt(node, positions.get(node.id) ?? ORIGIN, heightOf) }] : []));
  if (stayed.length === 0) return;
  moveClear(
    graph.nodes.filter((node) => !node.ui && !kept.has(node.id)),
    stayed,
    positions,
    heightOf,
    (mover, obstacle) => mover.kind !== "modulator" || obstacle.node.kind === "modulator",
  );
}

/**
 * Make room for what has grown or is new: a node that shows more knobs (one was pinned to it, or a cable ends at a parameter it did not show), or lists more cables, is taller than the
 * place it was left in, and a modulator that is added goes where its strip's room above its row is, which the strip above may have. A row of nodes (those that are level) that is over
 * what is above it is moved down, below it, with the gap of a row, and so is every row below it, by as much: room is made where it is needed, the strips below it keep their shape and
 * the rows do not come nearer to each other. The rows are taken from the top, so that what is not in the way stays where it is. (The nodes that a person has placed are not in it:
 * a node that comes to be on one is moved clear of it by itself, `clearOfPlaced`.)
 */
function makeRoom(graph: Graph, positions: Map<NodeId, Position>, heightOf: (node: Node) => number): void {
  const free = graph.nodes.filter((node) => !node.ui);
  const rows = new Map<number, Node[]>();
  for (const node of free) {
    const top = positions.get(node.id)?.y ?? 0;
    rows.set(top, [...(rows.get(top) ?? []), node]);
  }
  const settled: Box[] = [];
  let shift = 0;
  for (const top of [...rows.keys()].sort((a, b) => a - b)) {
    const row = rows.get(top) ?? [];
    let y = top + shift;
    // (Each step passes a box for good, so it takes no more steps than there are boxes.)
    for (let step = 0; step <= settled.length; step += 1) {
      const here = y;
      const hit = settled.find((other) => row.some((node) => over(boxAt(node, { x: positions.get(node.id)?.x ?? 0, y: here }, heightOf), other)));
      if (!hit) break;
      y = hit.bottom + ROW_GAP;
    }
    shift = y - top;
    for (const node of row) {
      const at = { x: positions.get(node.id)?.x ?? 0, y };
      positions.set(node.id, at);
      settled.push(boxAt(node, at, heightOf));
    }
  }
}

/** The cables that start at each modulator, as its node lists them, in the order of the graph. */
function cableRows(graph: Graph, names: ReadonlyMap<NodeId, string>): Map<NodeId, CableRow[]> {
  const drivenDepths = new Set<EdgeId>(graph.edges.flatMap((edge) => (edge.to.port.type === "depth" ? [edge.to.port.edge] : [])));
  const rows = new Map<NodeId, CableRow[]>();
  for (const edge of graph.edges) {
    if (edge.kind !== "modulation") continue;
    rows.set(edge.from.node, [...(rows.get(edge.from.node) ?? []), { edge: edge.id, label: endLabel(graph, names, edge.to), driven: drivenDepths.has(edge.id) }]);
  }
  return rows;
}

/** Nothing remembered: every node goes where the layout puts it. */
const NOTHING_REMEMBERED: Places = new Map();

/** No module is open: every module is a box. */
const NONE_OPEN: ReadonlySet<NodeId> = new Set();

/** What else `toFlow` is asked for. */
export interface FlowOptions {
  /**
   * Put the nodes where `arrange` puts them (the cables short, with room for each), and not where the layout of the rows puts them. It is for a layout from nothing (the nodes asked to be arranged): a node that a person has placed
   * (`Node.ui`) is a pin that the arrangement would have to keep, and it is left out of the question: with one, the layout is the rows'.
   */
  arrange?: boolean;
  /** The modules that are open (the viewer's own choice, like the scopes): their nodes are on the canvas and they are no box; a module that is not here is a box. */
  open?: ReadonlySet<NodeId>;
}

/** How much a pixel of a cable matters to the arrangement: the faint links by which a lane hears every source are the least of the cables. */
const HEARS_WEIGHT = 0.25;

/** How much a pixel of the tie that keeps a modulator that has no cable by the lane or the group that it hears matters to the arrangement. */
const TIE_WEIGHT = 0.3;

/** How far from its node's left edge the middle of the cell that is `index`th among a node's controls is (the cell's port, where a cable of modulation arrives, is at the top of it). */
const cellX = (index: number): number => (NODE_WIDTH - CONTROLS_PER_ROW * CELL_WIDTH) / 2 + CELL_WIDTH * (index % CONTROLS_PER_ROW) + CELL_WIDTH / 2;

/**
 * Which control of its node a cable ends at, as its place among the node's controls, if that is told by the port: a knob by its place among the node's knobs, the volume of a lane and the mix of an effect
 * by the cell that comes after the knobs, and a group's volume and pan by its first two cells. The ports of a modulator's settings and of the depths of cables are not told (their cells are the node's own).
 */
function controlOf(node: Node | undefined, port: Port, knobs: readonly number[]): number | null {
  if (!node) return null;
  switch (port.type) {
    case "param": {
      const place = knobs.indexOf(port.index);
      return place >= 0 ? place : null;
    }
    case "volume":
      return node.kind === "instrument" ? knobs.length : node.kind === "group" ? 0 : null;
    case "mix":
      return node.kind === "effect" ? knobs.length : null;
    case "pan":
      return node.kind === "group" ? 1 : null;
    default:
      return null;
  }
}

/**
 * The cables of `graph` as the arrangement sees them (`Wire`): the two nodes and the handles of each, where along the top of its node a cable of modulation ends (the column of the knob it drives, from
 * `knobs`: the parameters that each node shows), and how much it matters that it is short. A modulator that hears a lane or a group and has no cable of its own is tied to what it hears, as the layout of the
 * rows has it (above it), so that it is not left where the rest has moved away from.
 */
function wiresOf(graph: Graph, knobs: ReadonlyMap<NodeId, readonly number[]>): Wire[] {
  const nodes = new Map(graph.nodes.map((node) => [node.id, node]));
  const wires: Wire[] = graph.edges.map((edge) => {
    const target = nodes.get(edge.to.node);
    // (An effect that has two inputs has the first at a third of its height and not in the middle.)
    const pair = edge.to.port.type === "audioIn" && target?.kind === "effect" && target.side !== undefined;
    const control = controlOf(target, edge.to.port, knobs.get(edge.to.node) ?? []);
    return {
      from: edge.from.node,
      fromHandle: handleOf(edge.from.port),
      to: edge.to.node,
      toHandle: pair ? "audioIn:pair" : handleOf(edge.to.port),
      weight: edge.kind === "hears" ? HEARS_WEIGHT : 1,
      ...(control !== null ? { toX: cellX(control) } : {}),
    };
  });
  const driving = new Set(graph.edges.filter((edge) => edge.kind === "modulation").map((edge) => edge.from.node));
  for (const node of graph.nodes) {
    if (node.kind === "modulator" && node.hears !== undefined && !driving.has(node.id) && nodes.has(node.hears)) wires.push({ from: node.id, fromHandle: "modOut", to: node.hears, toHandle: "param", weight: TIE_WEIGHT });
  }
  return wires;
}

/** The places that `arrange` gives the nodes of `graph`, from where the layout of the rows has them (`fresh`): its boxes and its cables, as the arrangement sees them. */
function arrangedFrom(graph: Graph, fresh: ReadonlyMap<NodeId, Position>, heightOf: (node: Node) => number, knobs: ReadonlyMap<NodeId, readonly number[]>): Map<NodeId, Position> {
  const blocks: Block[] = graph.nodes.map((node) => ({ id: node.id, ...(fresh.get(node.id) ?? ORIGIN), width: NODE_WIDTH, height: heightOf(node) }));
  return new Map([...fresh, ...arrange(blocks, wiresOf(graph, knobs))]);
}

/**
 * The nodes of a layout and the cables of the session as the arrangement sees them, for `measure` to say what the layout is worth: the boxes are where `flow` has the nodes, as tall as the layout
 * assumes (`nodeHeight`, from what the node says it shows). The nodes that the canvas draws no card for (a lane that only holds effects) are not among them, and the cables at them are dropped.
 */
export function blocksOf(graph: Graph, flow: Flow, open: ReadonlySet<NodeId> = NONE_OPEN): { blocks: Block[]; wires: Wire[] } {
  // (What is on the canvas: a closed module's nodes are not, and the cables at them are at its box.)
  const seen = collapse(graph, displayNames(graph), open).graph;
  const known = new Map(seen.nodes.map((node) => [node.id, node]));
  const blocks = flow.nodes.flatMap((placed): Block[] => {
    const node = known.get(placed.data.node);
    if (!node) return [];
    const height = nodeHeight(node, placed.data.knobs.length, placed.data.cables.length, placed.data.scope, placed.data.heldIn?.first ?? false, placed.data.groupCell, placed.data.box ? boxRows(placed.data.box) : 1);
    return [{ id: node.id, x: placed.position.x, y: placed.position.y, width: NODE_WIDTH, height }];
  });
  return { blocks, wires: wiresOf(seen, new Map(flow.nodes.map((placed) => [placed.data.node, placed.data.knobs]))) };
}

/** No node shows a scope. */
const NO_SCOPES: ReadonlySet<NodeId> = new Set();

/**
 * The nodes of the session on the canvas, and the cables between them. `remembered` is the places of the last layout (`Flow.places`): a node stays where it was while it is in
 * the slot it was put for, so that what is drawn or added or taken out does not rearrange the canvas; without it every node goes where the layout puts it. `scopes` are the
 * nodes that the viewer shows a scope in (a scope is not part of the session): those of them that have a signal are taller by it, and room is made for it as for a knob that is
 * pinned. A lane that only holds effects (`holdersOf`) has no card on the canvas: it is laid out, remembered and drawn without it and without the links at it, and its first effect has
 * what the card said of the lane (`heldIn`). A module that is closed is one box (`collapse`, step 108): its nodes are not on the canvas, the cables at them are at the box, and the nodes of one that is
 * in `options.open` are on the canvas as any are, with a tag that closes the module again.
 */
export function toFlow(graph: Graph, remembered: Places = NOTHING_REMEMBERED, scopes: ReadonlySet<NodeId> = NO_SCOPES, options: FlowOptions = {}): Flow {
  const holders = holdersOf(graph);
  // (What is said of a node, its name and the words of a cable, is read from the whole graph: the effects of a held lane are called what they are called with the lane in it.)
  const names = displayNames(graph);
  const open = options.open ?? NONE_OPEN;
  // (What is on the canvas: a closed module's nodes are left out, and it is a box that the cables at them go to.)
  const collapsed = collapse(graph, names, open);
  const seen = collapsed.graph;
  const shown: Graph = holders.size === 0 ? seen : { nodes: seen.nodes.filter((node) => !holders.has(node.id)), edges: seen.edges.filter((edge) => !holders.has(edge.from.node) && !holders.has(edge.to.node)) };
  const carriers = new Set([...holders.values()].flatMap(([first]) => (first ? [first.id] : [])));
  const anchors = new Map([...holders].flatMap(([lane, [first]]) => (first ? [[lane, first.id] as const] : [])));
  const modulated = modulatedParams(graph);
  const settingsDriven = drivenSettings(graph);
  const listed = cableRows(graph, names);
  const knobs = new Map<NodeId, number[]>();
  for (const node of graph.nodes) {
    const plugin = node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
    knobs.set(node.id, knobsOf(plugin, modulated.get(node.id) ?? new Set()));
  }
  const knobCounts = new Map([...knobs].map(([id, params]) => [id, params.length]));
  const cableCounts = new Map(graph.nodes.map((node) => [node.id, node.kind === "modulator" ? (listed.get(node.id)?.length ?? 0) : 0]));
  const withScope = new Set(graph.nodes.filter((node) => scopes.has(node.id) && canScope(node)).map((node) => node.id));
  // (The page offers no way to make a group: a lane has the choice of one only while the session has a group.)
  const groups = graph.nodes.some((node) => node.kind === "group");
  const heightOf = (node: Node): number => {
    const box = collapsed.boxes.get(node.id);
    return nodeHeight(node, knobCounts.get(node.id) ?? 0, cableCounts.get(node.id) ?? 0, withScope.has(node.id), carriers.has(node.id), groups, box ? boxRows(box) : 1);
  };
  const ownerOf = ownersOf(seen);
  const baseline = layout(seen, heightOf, holders, ownerOf);
  // (The slots are those of the rows' layout, whatever the places are: a layout that goes on from an arrangement finds the nodes in the slots that were remembered.)
  const slots = slotsOf(shown, baseline, ownerOf);
  const fresh = options.arrange === true && shown.nodes.every((node) => node.ui === undefined) ? arrangedFrom(shown, baseline, heightOf, knobs) : baseline;
  // Where a layout that remembers nothing puts the nodes, to say whether these are arranged.
  const arrangedAt = new Map(fresh);
  clearOfPlaced(shown, arrangedAt, heightOf);
  const { positions, kept } = carryOver(shown, fresh, slots, remembered, anchors, ownerOf);
  clearOfKept(shown, positions, kept, heightOf);
  makeRoom(shown, positions, heightOf);
  clearOfPlaced(shown, positions, heightOf);
  const arranged = shown.nodes.every((node) => {
    const at = positions.get(node.id) ?? ORIGIN;
    const would = arrangedAt.get(node.id) ?? ORIGIN;
    return node.ui !== undefined || (at.x === would.x && at.y === would.y);
  });
  const places = new Map<NodeId, Remembered>(
    shown.nodes.flatMap((node) => (node.ui ? [] : [[node.id, { ...(positions.get(node.id) ?? ORIGIN), slot: slots.get(node.id) ?? node.kind }] as const])),
  );
  /** What the module of an effect has of the lane that holds it, if the canvas draws no card for that lane. */
  const lanes = graph.nodes.filter((node) => node.kind === "instrument").length;
  const heldIn = (node: Node): HeldIn | null => {
    const held = node.kind === "effect" && node.parent !== null ? holders.get(node.parent) : undefined;
    return held === undefined || node.parent === null ? null : { lane: node.parent, first: held[0]?.id === node.id, alone: held.length === 1 && lanes > 1 };
  };

  /** The module that a lane on the canvas is part of while the module is open, for its tag: a lane that has a card, or the first effect of one that has none. */
  const moduleOfLane = (node: Node): ModuleTag | null => {
    const lane = node.kind === "instrument" ? node.id : node.kind === "effect" && node.parent !== null && holders.get(node.parent)?.[0]?.id === node.id ? node.parent : null;
    const module = lane === null ? undefined : collapsed.moduleOf.get(lane);
    return module === undefined ? null : { node: module, name: names.get(module) ?? "module" };
  };

  const nodes: PatchNode[] = shown.nodes.map((node) => ({
    id: String(node.id),
    type: node.kind,
    position: node.ui ? { x: node.ui.x, y: node.ui.y } : (positions.get(node.id) ?? { x: 0, y: 0 }),
    width: NODE_WIDTH,
    // (React Flow makes a node a group of this name for a screen reader.)
    ariaLabel: names.get(node.id) ?? node.name,
    data: {
      node: node.id,
      name: names.get(node.id) ?? node.name,
      format: node.kind === "instrument" || node.kind === "effect" ? (node.plugin?.format ?? null) : null,
      knobs: knobs.get(node.id) ?? [],
      modulated: [...(modulated.get(node.id) ?? [])].sort((a, b) => a - b),
      targets: node.kind === "modulator" ? drivablesOf(graph, node.id, names) : [],
      drivenSettings: node.kind === "modulator" ? (settingsDriven.get(node.id) ?? []) : [],
      cables: node.kind === "modulator" ? (listed.get(node.id) ?? []) : [],
      notes: laneTakesNotes(node),
      groupCell: node.kind === "instrument" && groups,
      scope: withScope.has(node.id),
      source: node.kind === "source" ? node.source : null,
      heldIn: heldIn(node),
      side: node.kind === "effect" ? (node.side ?? null) : null,
      box: collapsed.boxes.get(node.id) ?? null,
      module: moduleOfLane(node),
    },
    draggable: true,
    deletable: false,
  }));

  const edges: PatchEdge[] = [];
  const nodesById = new Map(graph.nodes.map((node) => [node.id, node]));
  // (What each cable is, in words and by its buttons, is the session's: the cable as it is there, and not as it is drawn, with an end at the box. The ends and the handles are the canvas's.)
  for (const { edge: drawn, real: edge, sourceHandle, targetHandle } of collapsed.edges) {
    if (holders.has(drawn.from.node) || holders.has(drawn.to.node)) continue;
    const common = { source: String(drawn.from.node), target: String(drawn.to.node), sourceHandle, targetHandle, deletable: false };
    if (edge.kind === "modulation") {
      edges.push({ ...common, id: `mod:${String(edge.id)}`, type: "modulation", data: { cable: edge.id, modulator: edge.from.node, label: cableWords(graph, names, edge) }, zIndex: 10 });
    } else if (edge.kind === "cable") {
      // (An audio cable that has been drawn between two places: over the links of the audio path and under the cables of modulation. It moves no node.)
      edges.push({ ...common, id: `send:${String(edge.id)}`, type: "send", data: { cable: edge.id, label: cableWords(graph, names, edge) }, zIndex: 5 });
    } else if (edge.kind === "events") {
      // (A cable of notes that has been drawn from a source to a lane: over the links by which a lane hears a source and under the audio cables. It moves no node.)
      edges.push({ ...common, id: `notes:${String(edge.id)}`, type: "notes", data: { cable: edge.id, label: cableWords(graph, names, edge) }, zIndex: 4 });
    } else if (edge.kind === "hears") {
      // (The link that the graph derives for a lane that no cable of notes ends at, which hears every source: nothing is drawn or taken out by it. The lowest of the cables, over the links of the audio path.)
      edges.push({ ...common, id: `hears:${String(edge.from.node)}>${String(edge.to.node)}`, type: "hears", data: {}, zIndex: 1 });
    } else {
      // (A link of the audio path, derived. The end of a chain is the way out of its lane or group: it says whose, for the button that cuts it. A link into an effect is the feed of that effect, which the
      // button on it cuts as well: it says whose. A chain is a row of links that can each be taken apart.)
      const owner = ownerOfWayOut(edge, nodesById);
      const fed = nodesById.get(edge.to.node)?.kind === "effect" ? edge.to.node : undefined;
      const label = cableWords(graph, names, edge);
      edges.push({ ...common, id: `audio:${String(edge.from.node)}>${String(edge.to.node)}`, type: "audio", data: owner !== undefined ? { wayOut: owner, label } : fed !== undefined ? { feedOf: fed, label } : {} });
    }
  }
  return { nodes, edges, places, arranged };
}

/**
 * The lane or the group whose way out a link of the audio path is, if it is one. A link into a group or the master is the end of a chain (a link inside a chain goes into an effect), of the chain of the
 * lane or the group that it starts at, or that the effect it starts at is in. None for any other link, and for one whose lane or group is not in the graph, which the controller would not find to cut.
 */
function ownerOfWayOut(edge: Edge, nodes: ReadonlyMap<NodeId, Node>): NodeId | undefined {
  const into = nodes.get(edge.to.node)?.kind;
  if (into !== "group" && into !== "master") return undefined;
  const start = nodes.get(edge.from.node);
  const owner = start?.kind === "effect" ? (start.parent === null ? undefined : nodes.get(start.parent)) : start;
  return owner?.kind === "instrument" || owner?.kind === "group" ? owner.id : undefined;
}

/** A cable as the canvas depends on it: where it goes, and not how deep or at what level (which are values). */
const cableStructure = (edge: Edge): string => JSON.stringify(edge.kind === "modulation" ? { ...edge, amount: 0 } : edge.kind === "cable" ? { ...edge, gain: 0 } : edge);

/**
 * Whether two graphs are laid out the same: the same nodes, in the same places in the
 * tree, with the same names, the same plugins' parameters, and the same cables. What is
 * not part of it is what the values in them are, which change all the time: a parameter's,
 * a gain, a modulator's rate or stages, the depth of a cable.
 */
export function sameStructure(a: Graph, b: Graph): boolean {
  if (a === b) return true;
  if (a.nodes.length !== b.nodes.length || a.edges.length !== b.edges.length) return false;
  for (let i = 0; i < a.nodes.length; i += 1) {
    const x = a.nodes[i];
    const y = b.nodes[i];
    if (!x || !y || !sameNodeStructure(x, y)) return false;
  }
  if (a.edges === b.edges) return true;
  for (let i = 0; i < a.edges.length; i += 1) {
    const x = a.edges[i];
    const y = b.edges[i];
    if (!x || !y || cableStructure(x) !== cableStructure(y)) return false;
  }
  return true;
}

/** Whether two graphs (or the lack of one) are laid out the same: what the canvas depends on, which is not the values. */
export const sameGraphStructure = (a: Graph | null, b: Graph | null): boolean => (a && b ? sameStructure(a, b) : a === b);

/** Whether two nodes are placed alike: both left to the layout, or at the same place. */
function samePlace(x: NodeUi | undefined, y: NodeUi | undefined): boolean {
  // (A place that is missing is neither coordinate: it equals no place that there is.)
  return x === y || (x?.x === y?.x && x?.y === y?.y);
}

function sameNodeStructure(x: Node, y: Node): boolean {
  if (x === y) return true;
  if (x.id !== y.id || x.kind !== y.kind || x.parent !== y.parent || x.name !== y.name || !samePlace(x.ui, y.ui)) return false;
  // (Whether a lane has a key range or a pattern is part of it: with neither, and no instrument, a lane that has effects has no card on the canvas.)
  if (x.kind === "instrument" && y.kind === "instrument" && ((x.range === null) !== (y.range === null) || (x.pattern === null) !== (y.pattern === null))) return false;
  if ((x.kind === "instrument" || x.kind === "effect") && (y.kind === "instrument" || y.kind === "effect")) {
    return samePluginStructure(x.plugin, y.plugin);
  }
  // (Whom a modulator hears is part of it: the menu on its node has it, and the structure is what the menus are given.)
  if (x.kind === "modulator" && y.kind === "modulator") return x.source.type === y.source.type && x.hears === y.hears;
  // (What is in a module is part of it: the box has a port for what a cable crosses its edge at, and what is inside is on the canvas while it is open.)
  if (x.kind === "module" && y.kind === "module") return x.members.length === y.members.length && x.members.every((member, i) => member === y.members[i]);
  if (x.kind === "source" && y.kind === "source") return sameSource(x.source, y.source);
  return true;
}

/** Whether two sources of notes are the same: both the keys, or both a device that stands for the same name. */
const sameSource = (x: SourceInfo, y: SourceInfo): boolean => x.type === y.type && (x.type !== "device" || (y.type === "device" && x.name === y.name));

function samePluginStructure(x: Plugin | null, y: Plugin | null): boolean {
  if (x === y) return true;
  if (x === null || y === null) return false;
  return (
    x.id === y.id &&
    x.params.length === y.params.length &&
    x.params.every((param, i) => param.index === y.params[i]?.index) &&
    x.pinned.length === y.pinned.length &&
    x.pinned.every((index, i) => index === y.pinned[i])
  );
}
