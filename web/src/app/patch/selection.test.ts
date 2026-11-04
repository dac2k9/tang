import { describe, expect, test } from "vitest";

import { moduleGraphsFixture } from "../../client/fixtures";
import type { Graph, Node } from "../../protocol";
import { effect, lane, master as theMaster, modulator as lfo, moduleNode } from "./boxesKit";
import { collapsePlace, collapseTitle, collapseWhyNot, collapseWords, collapsibleOf, modulePluginsOf } from "./selection";

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

describe("what collapsing the nodes that are chosen into one module takes (step 109)", () => {
  const effectsOf = (lane: Node): Node[] => graph.nodes.filter((node) => node.kind === "effect" && node.parent === lane.id);
  const [, delay, reverb] = effectsOf(oscillator);
  const [, drive] = effectsOf(holder);
  const modulators = graph.nodes.filter((node) => node.kind === "modulator");
  const [first, second] = modulators;
  if (!delay || !reverb || !drive || !first || !second) throw new Error("the fixture has the nodes that these tests choose");

  test("an effect is its lane, whole: the instrument and every effect of the lane come with it, and the ones that were not chosen are counted as brought along", () => {
    const taken = collapsibleOf(graph, [filter.id]);
    expect(taken).toMatchObject({ nodes: [filter.id], lanes: 1, modulators: 0, brought: 3, left: 0 });
    expect(taken.hides).toEqual(new Set([oscillator.id, filter.id, delay.id, reverb.id]));
    // (Every plugin of it chosen: nothing is brought along.)
    expect(collapsibleOf(graph, [oscillator.id, filter.id, delay.id, reverb.id])).toMatchObject({ nodes: [oscillator.id, filter.id, delay.id, reverb.id], lanes: 1, brought: 0 });
    // (The instrument alone brings the three effects.)
    expect(collapsibleOf(graph, [oscillator.id])).toMatchObject({ lanes: 1, brought: 3 });
  });

  test("a lane that only holds effects is taken by its effects, and brings no instrument; an empty lane is a lane that hides itself alone", () => {
    expect(collapsibleOf(graph, [math.id])).toMatchObject({ nodes: [math.id], lanes: 1, brought: 1 });
    expect(collapsibleOf(graph, [math.id, drive.id])).toMatchObject({ lanes: 1, brought: 0 });
    expect(collapsibleOf(graph, [math.id]).hides).toEqual(new Set([holder.id, math.id, drive.id]));
    const taken = collapsibleOf(graph, [empty.id]);
    expect(taken).toMatchObject({ nodes: [empty.id], lanes: 1, modulators: 0, brought: 0, left: 0 });
    expect(taken.hides).toEqual(new Set([empty.id]));
  });

  test("a modulator is itself, whatever it hears; the lanes and the modulators are counted apart, each node once, in the order that they were chosen in", () => {
    const taken = collapsibleOf(graph, [second.id, filter.id, first.id, second.id, math.id, filter.id]);
    expect(taken).toMatchObject({ nodes: [second.id, filter.id, first.id, math.id], lanes: 2, modulators: 2, brought: 4, left: 0 });
    expect(taken.hides.has(first.id) && taken.hides.has(second.id)).toBe(true);
    expect(collapsibleOf(graph, [first.id])).toMatchObject({ lanes: 0, modulators: 1, brought: 0 });
  });

  test("leaves out and counts what a module cannot take: a group, the bus effect in it, a source of notes, the master, a box and a node that the session does not have", () => {
    const box = moduleNode(77_777, "Voice", [empty.id]);
    const withBox: Graph = { ...graph, nodes: [...graph.nodes, box] };
    for (const other of [group, bus, source, master, box]) {
      expect(collapsibleOf(withBox, [other.id]), `${other.kind} ${other.name}`).toMatchObject({ nodes: [], lanes: 0, modulators: 0, brought: 0, left: 1 });
    }
    expect(collapsibleOf(graph, [99_999])).toMatchObject({ nodes: [], left: 1 });
    expect(collapsibleOf(graph, [filter.id, bus.id, first.id, master.id, source.id])).toMatchObject({ nodes: [filter.id, first.id], lanes: 1, modulators: 1, left: 3 });
  });

  test("leaves out a node that is in a module already, whether it is the lane, an effect of its chain or a modulator, and takes the others", () => {
    const boxed: Graph = { ...graph, nodes: [...graph.nodes, moduleNode(77_777, "Voice", [oscillator.id, first.id])] };
    expect(collapsibleOf(boxed, [oscillator.id, filter.id, first.id])).toMatchObject({ nodes: [], lanes: 0, modulators: 0, left: 3 });
    expect(collapsibleOf(boxed, [filter.id, math.id, first.id, second.id])).toMatchObject({ nodes: [math.id, second.id], lanes: 1, modulators: 1, left: 2 });
    // (A lane whose own node is in no module is taken, though a module has another lane.)
    expect(collapsibleOf(boxed, [empty.id])).toMatchObject({ nodes: [empty.id], lanes: 1, left: 0 });
  });

  test("takes nothing of a session that is not there, and nothing chosen takes nothing", () => {
    expect(collapsibleOf(null, [filter.id])).toMatchObject({ nodes: [], lanes: 0, modulators: 0, brought: 0, left: 0 });
    expect(collapsibleOf(graph, [])).toMatchObject({ nodes: [], lanes: 0, modulators: 0, brought: 0, left: 0 });
  });
});

describe("what collapsing is said to take, and why it cannot (step 109)", () => {
  test("words: lanes and modulators, in the singular for one", () => {
    expect(collapseWords({ lanes: 1, modulators: 0 })).toBe("1 lane");
    expect(collapseWords({ lanes: 2, modulators: 0 })).toBe("2 lanes");
    expect(collapseWords({ lanes: 0, modulators: 1 })).toBe("1 modulator");
    expect(collapseWords({ lanes: 0, modulators: 3 })).toBe("3 modulators");
    expect(collapseWords({ lanes: 1, modulators: 1 })).toBe("1 lane and 1 modulator");
    expect(collapseWords({ lanes: 2, modulators: 3 })).toBe("2 lanes and 3 modulators");
  });

  test("it cannot with nothing to take: nothing chosen is told apart from what is chosen and cannot be taken", () => {
    expect(collapseWhyNot({ lanes: 1, modulators: 0 }, 1)).toBeNull();
    expect(collapseWhyNot({ lanes: 0, modulators: 2 }, 2)).toBeNull();
    expect(collapseWhyNot({ lanes: 0, modulators: 0 }, 0)).toBe("Choose the nodes to collapse into a module");
    expect(collapseWhyNot({ lanes: 0, modulators: 0 }, 2)).toBe("None of what is chosen can be collapsed: a module holds lanes and modulators that are in no module");
  });

  test("the title says what it takes, with the plugins that come along counted in the singular for one and only when some do, and else why it cannot", () => {
    expect(collapseTitle({ lanes: 1, modulators: 0, brought: 0 }, 2)).toBe("Collapse 1 lane into one box where they are: open it to see them again, or ungroup it");
    expect(collapseTitle({ lanes: 1, modulators: 0, brought: 1 }, 1)).toBe("Collapse 1 lane into one box where they are, with 1 more plugin from their lanes: open it to see them again, or ungroup it");
    expect(collapseTitle({ lanes: 2, modulators: 1, brought: 3 }, 2)).toBe("Collapse 2 lanes and 1 modulator into one box where they are, with 3 more plugins from their lanes: open it to see them again, or ungroup it");
    expect(collapseTitle({ lanes: 0, modulators: 2, brought: 0 }, 2)).toBe("Collapse 2 modulators into one box where they are: open it to see them again, or ungroup it");
    // (The reason it cannot is the title of a button that cannot be pressed, whatever number of plugins is said to come along.)
    expect(collapseTitle({ lanes: 0, modulators: 0, brought: 4 }, 0)).toBe("Choose the nodes to collapse into a module");
    expect(collapseTitle({ lanes: 0, modulators: 0, brought: 0 }, 3)).toBe("None of what is chosen can be collapsed: a module holds lanes and modulators that are in no module");
  });
});

describe("where the box of what is collapsed is drawn (step 109)", () => {
  const at = (entries: [number, number, number][]): Map<number, { x: number; y: number }> => new Map(entries.map(([node, x, y]) => [node, { x, y }]));

  test("is the top left of the nodes that it hides, whichever of them is nearest to each edge, and not of the nodes that stay", () => {
    const positions = at([
      [1, 10, 50],
      [2, 30, 20],
      [3, 5, 70],
      [4, 0, 0],
    ]);
    expect(collapsePlace(new Set([1, 2, 3]), positions)).toEqual({ x: 5, y: 20 });
    expect(collapsePlace(new Set([1]), positions)).toEqual({ x: 10, y: 50 });
    expect(collapsePlace(new Set([2, 3]), positions)).toEqual({ x: 5, y: 20 });
  });

  test("is left to the layout when none of what it hides is on the canvas, and works over places that are below nothing", () => {
    expect(collapsePlace(new Set([9]), at([[1, 10, 50]]))).toBeUndefined();
    expect(collapsePlace(new Set(), at([[1, 10, 50]]))).toBeUndefined();
    expect(collapsePlace(new Set([1, 2]), at([[1, -40, 8], [2, -10, -60]]))).toEqual({ x: -40, y: -60 });
  });

  test("is a place of its own, and not the one of the node that it is taken from", () => {
    const positions = at([[1, 10, 50]]);
    const place = collapsePlace(new Set([1]), positions);
    expect(place).toEqual({ x: 10, y: 50 });
    expect(place).not.toBe(positions.get(1));
  });
});
