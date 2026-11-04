import { describe, expect, test } from "vitest";

import type { Graph, Node, NodeId } from "../../protocol";
import { measure } from "./arrange";
import { boxRows } from "./boxes";
import { drives, effect, hears, keys, lane, link, MASTER, master, modulator, moduleNode, plugin, send, wayOut } from "./boxesKit";
import { blocksOf, CABLE_GAP, COLUMN, nodeHeight, NODE_WIDTH, ROW_GAP, sameStructure, toFlow } from "./toFlow";
import type { PatchNode } from "./toFlow";

// A module that is closed is one box on the canvas, and one that is open is its nodes (step 108): what `toFlow` makes of it.

/** An outside lane (10) with a reverb (11) to the mix, and the module "Room" (1): a lane (2) that holds a filter (3) and a delay (4), which has no instrument and so no card. */
const room: Graph = {
  nodes: [lane(10, "Oscillator"), effect(11, 10, "Reverb"), lane(2, "Chain", null), effect(3, 2, "Filter"), effect(4, 2, "Delay"), moduleNode(1, "Room", [2]), master],
  // (The session lists a link from a lane that has no instrument to the first effect of its chain: it carries nothing.)
  edges: [link(10, 11), wayOut(11), link(2, 3), link(3, 4)],
};

/** The module "Voice" (1): a lane (2) that has an instrument, a filter (3) after it and a modulator (20) that hears it. */
const voice: Graph = {
  nodes: [lane(2, "Oscillator"), effect(3, 2, "Filter"), modulator(20, "LFO", 2), moduleNode(1, "Voice", [2, 20]), master],
  edges: [link(2, 3), drives(5, 20, 3, 1)],
};

const byId = (nodes: readonly PatchNode[], id: NodeId): PatchNode => {
  const found = nodes.find((node) => node.id === String(id));
  if (!found) throw new Error(`no node ${String(id)} on the canvas`);
  return found;
};

const open = (...ids: NodeId[]): ReadonlySet<NodeId> => new Set(ids);

describe("a module that is closed", () => {
  test("is one node of the kind module with its ports, and what is in it is not on the canvas", () => {
    const flow = toFlow(room);
    expect(flow.nodes.map((node) => [node.id, node.type])).toEqual([
      ["10", "instrument"],
      ["11", "effect"],
      ["1", "module"],
      [String(MASTER), "master"],
    ]);
    const box = byId(flow.nodes, 1);
    expect(box.data.name).toBe("Room");
    expect(box.data.box?.inputs.map((port) => port.label)).toEqual(["Filter"]);
    expect(box.data.box?.outputs.map((port) => port.label)).toEqual(["Delay"]);
    expect(box.data.module).toBeNull();
    // (No other node has a box.)
    expect(flow.nodes.filter((node) => node.data.box !== null).map((node) => node.id)).toEqual(["1"]);
  });

  test("has a place that is remembered with the session's others, and the place that a person gave it when it has one", () => {
    const flow = toFlow(room);
    expect(flow.places.get(1)?.slot).toBe("module");
    const placed: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.kind === "module" ? { ...node, ui: { x: 700, y: -90 } } : node)) };
    expect(byId(toFlow(placed).nodes, 1).position).toEqual({ x: 700, y: -90 });
  });

  test("is as tall as the layout says and has a row for each port, and no node is on it", () => {
    const flow = toFlow(room);
    const { blocks } = blocksOf(room, flow);
    const box = blocks.find((block) => block.id === 1);
    expect(box?.width).toBe(NODE_WIDTH);
    // (The header, a row, the line that says what is in it, and the room under it: the stylesheet's numbers, 28 and 20 and 16, which the browser spec measures on the page.)
    expect(box?.height).toBe(44 + 28 + 20 + 16);
    for (const [index, a] of blocks.entries()) {
      for (const b of blocks.slice(index + 1)) {
        const apart = a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y;
        expect(apart, `${String(a.id)} and ${String(b.id)}`).toBe(true);
      }
    }
  });

  test("is taller for the ports it has, as many rows as the longer of its inputs and its outputs", () => {
    const three: Graph = {
      nodes: [lane(2, "A", null), effect(3, 2, "X"), lane(4, "B", null), effect(5, 4, "Y"), lane(6, "C", null), effect(7, 6, "Z"), moduleNode(1, "Three", [2, 4, 6]), master],
      edges: [],
    };
    const flow = toFlow(three);
    const box = byId(flow.nodes, 1);
    expect(box.data.box).not.toBeNull();
    expect(boxRows(box.data.box ?? { inputs: [], outputs: [] })).toBe(3);
    const found = blocksOf(three, flow).blocks.find((block) => block.id === 1);
    expect(found?.height).toBe(44 + 3 * 28 + 20 + 16);
    const module = three.nodes.find((node) => node.kind === "module");
    if (!module) throw new Error("the graph has the module");
    expect(nodeHeight(module, 0, 0, false, false, false, 3)).toBe(found?.height);
    // (A box that has no port is as tall as one that has one.)
    expect(nodeHeight(module, 0, 0, false, false, false, 0)).toBe(44 + 28 + 20 + 16);
  });

  test("is below the lanes, one under the other where there are several, in a column of its own", () => {
    const two: Graph = {
      nodes: [lane(10, "Oscillator"), lane(2, "Chain", null), effect(3, 2, "Filter"), lane(4, "Other", null), effect(5, 4, "Delay"), moduleNode(1, "First", [2]), moduleNode(6, "Second", [4]), master],
      edges: [],
    };
    const flow = toFlow(two);
    const lanes = byId(flow.nodes, 10);
    const first = byId(flow.nodes, 1);
    const second = byId(flow.nodes, 6);
    expect(first.position.y).toBeGreaterThan(lanes.position.y);
    expect(first.position.x).toBe(lanes.position.x);
    expect(second.position.x).toBe(first.position.x);
    expect(second.position.y).toBe(first.position.y + 44 + 28 + 20 + 16 + ROW_GAP);
  });

  test("is in the first place below the lanes: where the strip of the lane above it ends, with the gap of a row, and what is hidden in it has no room of its own", () => {
    const flow = toFlow(room);
    const { blocks } = blocksOf(room, flow);
    const above = blocks.filter((block) => block.id === 10 || block.id === 11);
    expect(above).toHaveLength(2);
    expect(byId(flow.nodes, 1).position.y).toBe(Math.max(...above.map((block) => block.y + block.height)) + ROW_GAP);
    expect(byId(flow.nodes, 1).position.x).toBe(byId(flow.nodes, 10).position.x);
  });

  test("stays where it was remembered while it is the module's place, and goes where the layout puts it when it was remembered for another kind of node", () => {
    const kept = toFlow(room, new Map([[1, { x: 999, y: 888, slot: "module" }]]));
    expect(byId(kept.nodes, 1).position).toEqual({ x: 999, y: 888 });
    const moved = toFlow(room, new Map([[1, { x: 999, y: 888, slot: "instrument" }]]));
    expect(byId(moved.nodes, 1).position).not.toEqual({ x: 999, y: 888 });
  });

  test("has the cables that cross its edge, drawn to its ports, and in the words of the nodes inside that they join", () => {
    const graph: Graph = { ...room, edges: [...room.edges, send(7, 11, 3), wayOut(4)] };
    const flow = toFlow(graph);
    const cable = flow.edges.find((edge) => edge.id === "send:7");
    expect(cable).toMatchObject({ source: "11", target: "1", sourceHandle: "audioOut", targetHandle: "audioIn@3", type: "send" });
    expect(cable?.data).toMatchObject({ cable: 7, label: "Reverb → Filter" });
    // The way out of the chain is the lane's, whose button cuts it, and starts at the box by the port of the delay.
    const out = flow.edges.find((edge) => edge.id === `audio:4>${String(MASTER)}`);
    expect(out).toMatchObject({ source: "1", target: String(MASTER), sourceHandle: "audioOut@4", targetHandle: "audioIn" });
    expect(out?.data).toMatchObject({ wayOut: 2, label: "Delay → Master" });
    // (What is between two nodes inside is not drawn.)
    expect(flow.edges.some((edge) => edge.id === "audio:3>4")).toBe(false);
    expect(flow.edges.map((edge) => edge.id)).toEqual(["audio:10>11", `audio:11>${String(MASTER)}`, "send:7", `audio:4>${String(MASTER)}`]);
  });

  test("has the cables of modulation that cross it: into it by the port of what is driven inside, out of it by the port of the modulator inside", () => {
    const graph: Graph = {
      nodes: [lane(10, "Oscillator"), lane(2, "Chain", null), effect(3, 2, "Filter"), modulator(20, "Outer"), modulator(21, "Inner"), moduleNode(1, "Wobble", [2, 21]), master],
      edges: [drives(5, 20, 3, 1), drives(6, 21, 10, 3)],
    };
    const flow = toFlow(graph);
    expect(flow.edges.find((edge) => edge.id === "mod:5")).toMatchObject({ source: "20", target: "1", sourceHandle: "modOut", targetHandle: "param-1@3" });
    expect(flow.edges.find((edge) => edge.id === "mod:6")).toMatchObject({ source: "1", target: "10", sourceHandle: "modOut@21", targetHandle: "param-3" });
    // (The cable is the modulator's: it lists it, in the words of what it drives, whether that is inside a box or not.)
    expect(byId(flow.nodes, 20).data.cables.map((row) => row.label)).toEqual(["Filter: p0"]);
    expect(byId(flow.nodes, 1).data.box?.modIn.map((port) => port.label)).toEqual(["Filter: p0"]);
  });

  test("has a modulator that hears a lane inside above it, as it is above the lane it hears", () => {
    const graph: Graph = {
      nodes: [lane(2, "Oscillator"), modulator(20, "LFO", 2), moduleNode(1, "Voice", [2]), master],
      edges: [],
    };
    const flow = toFlow(graph);
    const lfo = byId(flow.nodes, 20);
    const box = byId(flow.nodes, 1);
    expect(lfo.position.y).toBeLessThan(box.position.y);
    expect(lfo.position.x).toBe(box.position.x);
  });

  test("has that modulator in the room above its row, where the modulators of a lane are, and not in the row of those that hear every note, which is above the first lane", () => {
    const graph: Graph = { nodes: [lane(10, "Oscillator"), lane(2, "Inner"), modulator(20, "LFO", 2), moduleNode(1, "Voice", [2]), master], edges: [] };
    const flow = toFlow(graph);
    const { blocks } = blocksOf(graph, flow);
    const lfo = blocks.find((block) => block.id === 20);
    expect(byId(flow.nodes, 1).position.y - byId(flow.nodes, 20).position.y).toBe((lfo?.height ?? 0) + CABLE_GAP);
    // (The whole strip is below the lane above it, with the gap of a row: the modulator is not over that lane.)
    const outside = blocks.find((block) => block.id === 10);
    expect(byId(flow.nodes, 20).position.y).toBe((outside?.y ?? 0) + (outside?.height ?? 0) + ROW_GAP);
    // (And the master, which is in the middle of everything, is level with the middle of the lanes and the box: the room for the modulator over the box is in what the layout adds up.)
    const box = blocks.find((block) => block.id === 1);
    const mix = blocks.find((block) => block.id === MASTER);
    expect((mix?.y ?? 0) + (mix?.height ?? 0) / 2).toBe(((box?.y ?? 0) + (box?.height ?? 0)) / 2);
  });

  test("takes as many columns as the modulators that hear it take, as a lane does, and what comes after it starts after them", () => {
    const wide: Graph = { nodes: [lane(2, "Inner"), modulator(20, "A", 2), modulator(21, "B", 2), modulator(22, "C", 2), moduleNode(1, "Voice", [2]), master], edges: [] };
    const flow = toFlow(wide);
    expect(byId(flow.nodes, MASTER).position.x).toBe(3 * COLUMN);
  });

  test("has the link by which a lane inside hears every source drawn to the port of notes, and none for a lane that has no instrument", () => {
    const graph: Graph = {
      nodes: [keys, lane(2, "Oscillator"), lane(3, "Chain", null), effect(4, 3, "Filter"), moduleNode(1, "Voice", [2, 3]), master],
      edges: [hears(99, 2), hears(99, 3), link(2, 4)],
    };
    const flow = toFlow(graph);
    expect(flow.edges.filter((edge) => edge.type === "hears").map((edge) => [edge.source, edge.target, edge.targetHandle])).toEqual([["99", "1", "eventsIn@2"]]);
  });

  test("is laid out whether the nodes are arranged or not, with no two nodes nearer than the margin", () => {
    const graph: Graph = {
      nodes: [lane(10, "Oscillator"), effect(11, 10, "Reverb"), lane(2, "Chain", null), effect(3, 2, "Filter"), effect(4, 2, "Delay"), modulator(20, "LFO", 10), moduleNode(1, "Room", [2]), master],
      edges: [link(10, 11), wayOut(11), link(3, 4), send(7, 11, 3), wayOut(4), drives(5, 20, 3, 1)],
    };
    for (const arrange of [false, true]) {
      const flow = toFlow(graph, undefined, undefined, { arrange });
      const { blocks, wires } = blocksOf(graph, flow);
      expect(blocks.map((block) => block.id).sort()).toEqual([1, 10, 11, 20, MASTER].sort());
      expect(measure(blocks, wires).crowded, `arrange: ${String(arrange)}`).toBe(0);
      // (Every cable that is drawn is between two nodes that are on the canvas, and so is every cable that the arrangement is asked to make short.)
      const ids = new Set(flow.nodes.map((node) => node.id));
      expect(flow.edges.every((edge) => ids.has(edge.source) && ids.has(edge.target))).toBe(true);
      const blockIds = new Set(blocks.map((block) => block.id));
      expect(wires.length).toBeGreaterThan(0);
      expect(wires.every((wire) => blockIds.has(wire.from) && blockIds.has(wire.to))).toBe(true);
    }
  });
});

describe("a module that is open", () => {
  test("is its nodes, as a session without the module has them, and has no box", () => {
    const flow = toFlow(room, undefined, undefined, { open: open(1) });
    expect(flow.nodes.map((node) => [node.id, node.type])).toEqual([
      ["10", "instrument"],
      ["11", "effect"],
      ["3", "effect"],
      ["4", "effect"],
      [String(MASTER), "master"],
    ]);
    expect(flow.nodes.every((node) => node.data.box === null)).toBe(true);
    expect(flow.edges.map((edge) => edge.id)).toEqual(["audio:10>11", `audio:11>${String(MASTER)}`, "audio:3>4"]);
  });

  test("has a tag over its lane that closes it: over the lane's card, or over the first effect of a lane that has no card", () => {
    const held = toFlow(room, undefined, undefined, { open: open(1) });
    expect(held.nodes.filter((node) => node.data.module !== null).map((node) => [node.id, node.data.module])).toEqual([["3", { node: 1, name: "Room" }]]);
    const carded = toFlow(voice, undefined, undefined, { open: open(1) });
    // (Not over the filter in the lane, and not over the modulator.)
    expect(carded.nodes.filter((node) => node.data.module !== null).map((node) => [node.id, node.data.module])).toEqual([["2", { node: 1, name: "Voice" }]]);
  });

  test("leaves the nodes outside it where they were, and a module that is closed again is where it was", () => {
    const closed = toFlow(room);
    const opened = toFlow(room, closed.places, undefined, { open: open(1) });
    for (const id of [10, 11, MASTER]) expect(byId(opened.nodes, id).position, String(id)).toEqual(byId(closed.nodes, id).position);
    const again = toFlow(room, opened.places);
    expect(again.nodes.map((node) => [node.id, node.position])).toEqual(closed.nodes.map((node) => [node.id, node.position]));
  });

  test("is open for the module that is in the set and no other: another stays a box", () => {
    const two: Graph = {
      nodes: [lane(2, "A", null), effect(3, 2, "First"), lane(4, "B", null), effect(5, 4, "Second"), moduleNode(1, "One", [2]), moduleNode(6, "Two", [4]), master],
      edges: [],
    };
    const flow = toFlow(two, undefined, undefined, { open: open(6) });
    expect(flow.nodes.map((node) => [node.id, node.type])).toEqual([
      ["5", "effect"],
      ["1", "module"],
      [String(MASTER), "master"],
    ]);
    // (A module that is not there is none to open.)
    expect(toFlow(two, undefined, undefined, { open: open(77) }).nodes.map((node) => node.type)).toEqual(["module", "module", "master"]);
  });
});

describe("the places of what a closed module hides (step 109)", () => {
  /** The session of `room` before its module was made: the lane, its filter and its delay are on the canvas as any are. */
  const flat: Graph = { nodes: room.nodes.filter((node) => node.kind !== "module"), edges: room.edges };
  const at = (nodes: readonly PatchNode[], ...ids: NodeId[]) => ids.map((id) => byId(nodes, id).position);

  /** The session drawn with the filter and the delay somewhere that the layout would not put them. */
  const drawn = (): ReturnType<typeof toFlow> => {
    const rows = toFlow(flat);
    const moved = new Map(rows.places);
    for (const id of [3, 4]) {
      const place = moved.get(id);
      if (!place) throw new Error(`node ${String(id)} is remembered`);
      moved.set(id, { ...place, x: place.x + 500, y: place.y + 120 });
    }
    return toFlow(flat, moved);
  };

  test("are remembered while the nodes are hidden, and are where they were when the module is opened", () => {
    const before = drawn();
    expect(at(before.nodes, 3)[0]).not.toEqual(at(toFlow(flat).nodes, 3)[0]);
    const closed = toFlow(room, before.places);
    expect(closed.nodes.map((node) => node.id)).not.toContain("3");
    expect(closed.places.get(3)).toEqual(before.places.get(3));
    expect(closed.places.get(4)).toEqual(before.places.get(4));
    const opened = toFlow(room, closed.places, undefined, { open: open(1) });
    expect(at(opened.nodes, 3, 4)).toEqual(at(before.nodes, 3, 4));
  });

  test("are where they were when the module is taken apart, and the nodes that stayed are where they were all the time", () => {
    const before = drawn();
    const closed = toFlow(room, before.places);
    const apart = toFlow(flat, closed.places);
    expect(at(apart.nodes, 3, 4)).toEqual(at(before.nodes, 3, 4));
    for (const id of [10, 11, MASTER]) expect(at(closed.nodes, id), String(id)).toEqual(at(before.nodes, id));
  });

  test("are none that was never drawn: a module that is closed from the first has no place for what it hides", () => {
    expect(toFlow(room).places.has(3)).toBe(false);
    expect(toFlow(room).places.has(4)).toBe(false);
    expect(toFlow(room, drawn().places).places.has(3)).toBe(true);
  });

  test("are not kept for a node that a person placed: the place is its own, in the session", () => {
    const before = drawn();
    const placed: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.id === 3 ? { ...node, ui: { x: 40, y: 40 } } : node)) };
    const closed = toFlow(placed, before.places);
    expect(closed.places.has(3)).toBe(false);
    expect(closed.places.has(4)).toBe(true);
  });

  test("are not kept for a node that is drawn: what is remembered of it, in a slot that it is not in now, is not where it is, and the next layout is told where it is", () => {
    const rows = toFlow(flat);
    const stale = new Map(rows.places);
    const was = stale.get(3);
    if (!was) throw new Error("node 3 is remembered");
    stale.set(3, { x: was.x + 700, y: was.y + 300, slot: "a slot that it is not in" });
    const again = toFlow(flat, stale);
    expect(at(again.nodes, 3)).toEqual(at(rows.nodes, 3));
    expect(again.places.get(3)).toEqual(rows.places.get(3));
  });

  test("are not kept for a lane that has no card either: it is not drawn, and a module did not hide it", () => {
    // (The lane of the filter and the delay has no instrument and nothing else of its own, so the canvas draws the two effects and not the lane.)
    const rows = toFlow(flat);
    expect(rows.nodes.map((node) => node.id)).not.toContain("2");
    expect(rows.places.has(2)).toBe(false);
    const remembered = new Map(rows.places);
    remembered.set(2, { x: 12, y: 34, slot: "instrument" });
    expect(toFlow(flat, remembered).places.has(2)).toBe(false);
    // (The same in the session that has the module: the lane is in it, and has no place to go back to when it is opened, since it is not drawn then either; the filter and the delay in it have.)
    const closed = toFlow(room, remembered);
    expect(closed.places.has(2)).toBe(false);
    expect(closed.places.has(3)).toBe(true);
  });
});

describe("the structure of a session that has a module", () => {
  test("is the same while the module has the same nodes, and not when it has others", () => {
    const more: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.kind === "module" ? { ...node, members: [2, 77] } : node)) };
    const same: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.kind === "module" ? { ...node, members: [2] } : node)) };
    const fewer: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.kind === "module" ? { ...node, members: [] } : node)) };
    const other: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.kind === "module" ? { ...node, members: [3] } : node)) };
    expect(sameStructure(room, other)).toBe(false);
    expect(sameStructure(room, same)).toBe(true);
    expect(sameStructure(room, more)).toBe(false);
    expect(sameStructure(room, fewer)).toBe(false);
  });

  test("a plugin with a few more parameters in a module is another structure, as it is outside one (the box's ports and what is on the canvas are read from it)", () => {
    const other: Graph = { ...room, nodes: room.nodes.map((node): Node => (node.kind === "effect" && node.id === 3 ? { ...node, plugin: plugin(5, "builtin:filter") } : node)) };
    expect(sameStructure(room, other)).toBe(false);
  });
});
