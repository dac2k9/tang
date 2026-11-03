import { describe, expect, test } from "vitest";

import { moduleGraphsFixture } from "../../client/fixtures";
import type { Graph, Node } from "../../protocol";
import { effect, lane, master as theMaster, modulator as lfo, moduleNode } from "./boxesKit";
import { modulePluginsOf } from "./selection";

// What a module can hold of the nodes that are chosen on the canvas (steps 105 and 106): the plugins (the instrument of a lane that has one, an effect that is in a lane) and the modulators that go with them, and nothing else,
// which is left out and counted.

const { graph } = moduleGraphsFixture.snapshot;

const find = (predicate: (node: Node) => boolean): Node => {
  const found = graph.nodes.find(predicate);
  if (!found) throw new Error("the fixture has no such node");
  return found;
};
const oscillator = find((node) => node.kind === "instrument" && node.plugin !== null);
const filter = find((node) => node.kind === "effect" && node.parent === oscillator.id);
const holder = find((node) => node.kind === "instrument" && node.plugin === null && graph.nodes.some((n) => n.parent === node.id));
const math = find((node) => node.kind === "effect" && node.parent === holder.id);
const empty = find((node) => node.kind === "instrument" && node.plugin === null && !graph.nodes.some((n) => n.parent === node.id));
const group = find((node) => node.kind === "group");
const bus = find((node) => node.kind === "effect" && node.parent === group.id);
const modulator = find((node) => node.kind === "modulator");
const source = find((node) => node.kind === "source");
const master = find((node) => node.kind === "master");

describe("what a module holds of what is chosen", () => {
  test("is the instrument of a lane that has one, the effects that are in a lane and the modulators, in the order they were chosen in, and says how many of them are plugins", () => {
    expect(modulePluginsOf(graph, [math.id, oscillator.id, filter.id])).toEqual({ nodes: [math.id, oscillator.id, filter.id], plugins: 3, left: 0 });
    expect(modulePluginsOf(graph, [filter.id])).toEqual({ nodes: [filter.id], plugins: 1, left: 0 });
    expect(modulePluginsOf(graph, [])).toEqual({ nodes: [], plugins: 0, left: 0 });
    // A modulator is held, whatever it hears and drives: it is a plugin's that it goes with, and is not one itself (a module of nothing else is not one).
    expect(modulePluginsOf(graph, [modulator.id, filter.id, math.id])).toEqual({ nodes: [modulator.id, filter.id, math.id], plugins: 2, left: 0 });
    expect(modulePluginsOf(graph, [modulator.id])).toEqual({ nodes: [modulator.id], plugins: 0, left: 0 });
  });

  test("leaves out what a module has none of, and counts it: a group and the bus effect in it, a source of notes, an empty lane, the lane of effects alone, and the master", () => {
    for (const other of [group, bus, source, empty, holder, master]) {
      expect(modulePluginsOf(graph, [other.id]), `${other.kind} ${other.name}`).toEqual({ nodes: [], plugins: 0, left: 1 });
    }
    expect(modulePluginsOf(graph, [filter.id, modulator.id, bus.id, math.id, master.id])).toEqual({ nodes: [filter.id, modulator.id, math.id], plugins: 2, left: 2 });
  });

  test("leaves out a node that the session does not have, and everything for a session that is not there yet", () => {
    expect(modulePluginsOf(graph, [filter.id, 99_999])).toEqual({ nodes: [filter.id], plugins: 1, left: 1 });
    expect(modulePluginsOf(null, [filter.id, math.id])).toEqual({ nodes: [], plugins: 0, left: 2 });
  });

  test("takes an effect only if the node that holds it is a lane: one whose parent is a group is a bus effect, and one with no parent is in no lane", () => {
    const orphan = { ...filter, id: 88_888, parent: null } as Node;
    const moved = { ...graph, nodes: [...graph.nodes, orphan] };
    expect(modulePluginsOf(moved, [orphan.id])).toEqual({ nodes: [], plugins: 0, left: 1 });
    expect(modulePluginsOf(moved, [bus.id])).toEqual({ nodes: [], plugins: 0, left: 1 });
    expect(modulePluginsOf(moved, [filter.id])).toEqual({ nodes: [filter.id], plugins: 1, left: 0 });
  });
});

describe("what a module holds of a box that is chosen (step 108)", () => {
  const voice: Graph = {
    nodes: [lane(2, "Oscillator"), effect(3, 2, "Filter"), lane(4, "Chain", null), effect(5, 4, "Delay"), lfo(20, "LFO", 2), lane(6, "Outside"), moduleNode(1, "Voice", [2, 4, 20]), theMaster],
    edges: [],
  };

  test("is what is in it: the instruments of its lanes, their effects and its modulators, in the order of the graph, and a lane that has no instrument is no part of it but for its effects", () => {
    expect(modulePluginsOf(voice, [1])).toEqual({ nodes: [2, 3, 5, 20], plugins: 3, left: 0 });
  });

  test("is each node once, whatever else is chosen with it, and what is chosen with it that a module cannot hold is left out as it is without it", () => {
    expect(modulePluginsOf(voice, [3, 1])).toEqual({ nodes: [3, 2, 5, 20], plugins: 3, left: 0 });
    expect(modulePluginsOf(voice, [6, 1, 99_999])).toEqual({ nodes: [6, 2, 3, 5, 20], plugins: 4, left: 1 });
  });

  test("is nothing for a box that has nothing in it that a module holds, and the box is not counted as left out", () => {
    const hollow: Graph = { nodes: [lane(2, "Chain", null), moduleNode(1, "Empty", [2]), theMaster], edges: [] };
    expect(modulePluginsOf(hollow, [1])).toEqual({ nodes: [], plugins: 0, left: 0 });
  });
});
