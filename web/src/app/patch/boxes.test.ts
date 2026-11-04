import { describe, expect, test } from "vitest";

import { displayNames } from "../../client/names";
import type { Graph, Node, NodeId } from "../../protocol";
import { boxDetail, boxRows, collapse } from "./boxes";
import { drives, effect, hears, keys, lane, link, macro, MASTER, master, modulator, moduleNode as module, plugin, send, wayOut } from "./boxesKit";

const run = (graph: Graph, open: ReadonlySet<NodeId> = new Set()) => collapse(graph, displayNames(graph), open);

/** An outside lane (10) with a reverb (11), and a module (1) of a lane (2) that holds a filter (3) and a delay (4) in a chain. */
const room: Graph = {
  nodes: [lane(10, "Oscillator"), effect(11, 10, "Reverb"), lane(2, "Chain", null), effect(3, 2, "Filter"), effect(4, 2, "Delay"), module(1, "Room", [2]), master],
  // (The session lists a link from a lane that has no instrument to the first effect of its chain: it carries nothing.)
  edges: [link(10, 11), wayOut(11), link(2, 3), link(3, 4)],
};

describe("a module that is closed is one box", () => {
  test("a session that has no module is the graph it was, and its cables are drawn as they are", () => {
    const graph: Graph = { nodes: [lane(10, "Oscillator"), effect(11, 10, "Reverb"), master], edges: [link(10, 11), wayOut(11), drives(5, 11, 10, 1)] };
    const collapsed = run(graph);
    expect(collapsed.graph).toBe(graph);
    expect(collapsed.boxes.size).toBe(0);
    expect(collapsed.moduleOf.size).toBe(0);
    expect(collapsed.edges.map((placed) => [placed.edge === placed.real, placed.sourceHandle, placed.targetHandle])).toEqual([
      [true, "audioOut", "audioIn"],
      [true, "audioOut", "audioIn"],
      [true, "modOut", "param-1"],
    ]);
  });

  test("its lanes and their effects are not on the canvas and the module is, and a link between two of them is not drawn", () => {
    const collapsed = run(room);
    expect(collapsed.graph.nodes.map((node) => node.id)).toEqual([10, 11, 1, MASTER]);
    expect(collapsed.graph.edges.map((edge) => [edge.from.node, edge.to.node])).toEqual([
      [10, 11],
      [11, MASTER],
    ]);
    expect([...collapsed.moduleOf]).toEqual([
      [2, 1],
      [3, 1],
      [4, 1],
    ]);
  });

  test("its ports are the input that nothing inside feeds and the output that nothing inside takes", () => {
    const box = run(room).boxes.get(1);
    expect(box).toEqual({
      node: 1,
      plugins: 2,
      modulators: 0,
      inputs: [{ member: 3, handle: "audioIn", label: "Filter", kind: "audio" }],
      outputs: [{ member: 4, handle: "audioOut", label: "Delay", kind: "audio" }],
      modIn: [],
      modOut: [],
      knobs: [],
    });
    expect(boxRows(box ?? { inputs: [], outputs: [] })).toBe(1);
    // (A box that has no port is as tall as one that has one, and one that has more of one side than of the other has as many rows as that side.)
    const port = { member: 3, handle: "audioIn", label: "Filter", kind: "audio" } as const;
    expect(boxRows({ inputs: [], outputs: [] })).toBe(1);
    expect(boxRows({ inputs: [port, port, port], outputs: [port] })).toBe(3);
    expect(boxRows({ inputs: [port], outputs: [port, port] })).toBe(2);
  });

  test("a cable to the input of an effect inside ends at the box, at the port of that effect", () => {
    const graph: Graph = { ...room, edges: [...room.edges, send(7, 11, 3)] };
    const collapsed = run(graph);
    const placed = collapsed.edges.find((candidate) => candidate.real.kind === "cable");
    expect(placed?.edge.to).toEqual({ node: 1, port: { type: "audioIn" } });
    expect(placed?.edge.from).toEqual({ node: 11, port: { type: "audioOut" } });
    expect([placed?.sourceHandle, placed?.targetHandle]).toEqual(["audioOut", "audioIn@3"]);
    // (The cable as the session has it is still there, for its words and its buttons.)
    expect(placed?.real.to.node).toBe(3);
    expect(collapsed.boxes.get(1)?.inputs).toHaveLength(1);
  });

  test("a cable from the output of an effect inside, to the mix, starts at the box, and the output is one port however it is told", () => {
    const graph: Graph = { ...room, edges: [...room.edges, wayOut(4)] };
    const collapsed = run(graph);
    const placed = collapsed.edges.find((candidate) => candidate.real.from.node === 4);
    expect([placed?.edge.from.node, placed?.sourceHandle, placed?.targetHandle]).toEqual([1, "audioOut@4", "audioIn"]);
    expect(collapsed.boxes.get(1)?.outputs.map((port) => port.member)).toEqual([4]);
    // (A cable that leaves the module is no input of it.)
    expect(collapsed.boxes.get(1)?.inputs.map((port) => port.member)).toEqual([3]);
  });

  test("a port that a cable crosses at is the box's though something inside feeds or takes it", () => {
    // (The filter feeds the delay, and a cable from the filter goes out to the reverb as well; another comes into the delay from outside.)
    const graph: Graph = { ...room, edges: [...room.edges, send(7, 3, 11), send(8, 11, 4)] };
    const box = run(graph).boxes.get(1);
    expect(box?.outputs.map((port) => [port.member, port.label])).toEqual([
      [3, "Filter"],
      [4, "Delay"],
    ]);
    expect(box?.inputs.map((port) => [port.member, port.label])).toEqual([
      [3, "Filter"],
      [4, "Delay"],
    ]);
  });

  test("an instrument in a lane that takes notes has an input of notes, and an instrument that takes none has none", () => {
    const graph: Graph = {
      nodes: [lane(2, "Oscillator"), effect(3, 2, "Filter"), lane(5, "Audio Input", plugin(2, "builtin:input", false)), module(1, "Voice", [2, 5]), master],
      edges: [link(2, 3)],
    };
    const box = run(graph).boxes.get(1);
    expect(box?.inputs).toEqual([{ member: 2, handle: "eventsIn", label: "Oscillator", kind: "notes" }]);
    expect(box?.outputs.map((port) => [port.member, port.label])).toEqual([
      [3, "Filter"],
      [5, "Audio Input"],
    ]);
    expect(box?.plugins).toBe(3);
  });

  test("an effect that has a second input has two ports, called what its plugin calls them", () => {
    const graph: Graph = { nodes: [lane(2, "Chain", null), effect(3, 2, "Math", { main: "A", side: "B" }), module(1, "Ring", [2]), master], edges: [] };
    expect(run(graph).boxes.get(1)?.inputs.map((port) => [port.handle, port.label])).toEqual([
      ["audioIn", "Math: A"],
      ["audioInSide", "Math: B"],
    ]);
  });

  test("a modulator inside is counted, and one that drives something outside, or is driven from outside, is at the box by its port", () => {
    const graph: Graph = {
      nodes: [lane(10, "Oscillator"), lane(2, "Chain", null), effect(3, 2, "Filter"), modulator(20, "LFO"), modulator(21, "Inner LFO"), modulator(22, "Out LFO"), module(1, "Wobble", [2, 21, 22]), master],
      edges: [drives(5, 20, 3, 1), drives(6, 21, 3, 3), drives(7, 22, 10, 1)],
    };
    const collapsed = run(graph);
    const box = collapsed.boxes.get(1);
    expect(box?.modulators).toBe(2);
    expect(box?.modIn).toEqual([{ member: 3, handle: "param-1", label: "Filter: p0", kind: "modulation" }]);
    expect(box?.modOut).toEqual([{ member: 22, handle: "modOut", label: "Out LFO", kind: "modulation" }]);
    const byCable = new Map(collapsed.edges.map((placed) => [placed.real.kind === "modulation" ? placed.real.id : -1, placed]));
    expect([byCable.get(5)?.edge.from.node, byCable.get(5)?.edge.to.node, byCable.get(5)?.targetHandle]).toEqual([20, 1, "param-1@3"]);
    expect(byCable.has(6)).toBe(false);
    expect([byCable.get(7)?.edge.from.node, byCable.get(7)?.sourceHandle, byCable.get(7)?.edge.to.node]).toEqual([1, "modOut@22", 10]);
  });

  test("a modulator outside that hears a lane inside hears the box, and the session's node is not changed", () => {
    const heard = modulator(20, "LFO", 2);
    const graph: Graph = { nodes: [lane(2, "Oscillator"), heard, module(1, "Voice", [2]), master], edges: [] };
    const collapsed = run(graph);
    const seen = collapsed.graph.nodes.find((node) => node.id === 20);
    expect(seen?.kind === "modulator" ? seen.hears : undefined).toBe(1);
    expect(heard.kind === "modulator" ? heard.hears : undefined).toBe(2);
  });

  test("the link by which a lane hears every source goes to the box's input of notes, and none goes for a lane that has no instrument", () => {
    const graph: Graph = {
      nodes: [keys, lane(2, "Oscillator"), lane(3, "Chain", null), effect(4, 3, "Filter"), module(1, "Voice", [2, 3]), master],
      edges: [hears(99, 2), hears(99, 3), link(2, 4)],
    };
    const collapsed = run(graph);
    const hearsLinks = collapsed.edges.filter((placed) => placed.real.kind === "hears");
    expect(hearsLinks.map((placed) => [placed.edge.to.node, placed.targetHandle])).toEqual([[1, "eventsIn@2"]]);
    // (The port is a port of notes, which the lane has whether or not the link is drawn.)
    expect(collapsed.boxes.get(1)?.inputs.map((port) => [port.member, port.handle, port.kind])).toEqual([[2, "eventsIn", "notes"]]);
  });

  test("lists the inputs of audio before the inputs of notes, each in the order of the nodes, though the lane that takes notes comes first", () => {
    const graph: Graph = { nodes: [lane(2, "Oscillator"), lane(3, "Chain", null), effect(4, 3, "Filter"), module(1, "Mixed", [2, 3]), master], edges: [link(3, 4)] };
    expect(run(graph).boxes.get(1)?.inputs.map((port) => [port.member, port.kind])).toEqual([
      [4, "audio"],
      [2, "notes"],
    ]);
  });

  test("a node that is in two modules is in the first, and a member that the session has not is none of it", () => {
    const graph: Graph = {
      nodes: [lane(2, "A"), lane(3, "B"), module(1, "First", [2, 77]), module(4, "Second", [2, 3]), master],
      edges: [],
    };
    const collapsed = run(graph);
    expect([...collapsed.moduleOf]).toEqual([
      [2, 1],
      [3, 4],
    ]);
    expect(collapsed.graph.nodes.map((node) => node.id)).toEqual([1, 4, MASTER]);
  });
});

describe("the knobs that a box shows (step 110)", () => {
  /** A module (1) of a lane (2) that holds a filter (3), and `inside`, which are the modulators that came with it; a macro (22) of the session's own is outside it. */
  const room_with = (...inside: Node[]): Graph => ({
    nodes: [lane(2, "Chain", null), effect(3, 2, "Filter"), ...inside, macro(22, "Macro"), module(1, "Room", [2, ...inside.map((node) => node.id)]), master],
    edges: [link(2, 3)],
  });
  const knobs = (graph: Graph, open: ReadonlySet<NodeId> = new Set()) => run(graph, open).boxes.get(1)?.knobs.map((knob) => [knob.member, knob.label]);

  test("are one for each macro in the module, in the order of the graph, called by the macro's name as the canvas calls the node (a name that more than one node has is numbered)", () => {
    // (The macro that is outside the module is called `Macro` too: the two are `Macro 1` and `Macro 2`, which is how the canvas tells them apart.)
    expect(knobs(room_with(macro(20, "Macro"), macro(21, "Brightness", { name: "Brightness" })))).toEqual([
      [20, "Macro 1"],
      [21, "Brightness"],
    ]);
    // (A macro that is alone with its name keeps it, whatever the module is called.)
    expect(knobs(room_with(macro(21, "Brightness", { name: "Brightness" })))).toEqual([[21, "Brightness"]]);
  });

  test("are none for a module that has no macro in it, and a box says nothing more of itself for the macros: they are modulators in the count of what is in it", () => {
    const graph = room_with(modulator(20, "LFO"));
    expect(knobs(graph)).toEqual([]);
    expect(run(graph).boxes.get(1)?.modulators).toBe(1);
    const hidden = room_with(macro(20, "Macro", { expose: false }), macro(21, "Macro"));
    expect(run(hidden).boxes.get(1)?.modulators, "a macro that is hidden from the box is still in it").toBe(2);
    expect(boxDetail(run(hidden).boxes.get(1) ?? { plugins: 0, modulators: 0 })).toBe("1 plugin · 2 modulators");
  });

  test("leave out a macro that is told not to show its knob, and the one outside the module, and any modulator that is not a macro; a flag that the wire leaves out, or says is on, shows it", () => {
    const graph = room_with(macro(20, "Hidden", { expose: false }), macro(21, "Left out"), macro(23, "Said", { expose: true }), modulator(24, "LFO"));
    expect(knobs(graph)).toEqual([
      [21, "Left out"],
      [23, "Said"],
    ]);
  });

  test("are on a box and not on a module that is open, which has its nodes: each macro has its own knob there", () => {
    const graph = room_with(macro(20, "Macro"));
    expect(knobs(graph, new Set([1]))).toBeUndefined();
    expect(run(graph, new Set([1])).graph.nodes.map((node) => node.id)).toContain(20);
  });

  test("are those of the module that each macro is in, and a macro that has been taken out of a module (the record no longer names it) is not on any box", () => {
    const graph: Graph = {
      nodes: [lane(2, "A"), lane(3, "B"), macro(20, "First"), macro(21, "Second"), macro(22, "Alone"), module(1, "One", [2, 20]), module(4, "Two", [3, 21]), master],
      edges: [],
    };
    const collapsed = run(graph);
    expect(collapsed.boxes.get(1)?.knobs.map((knob) => [knob.member, knob.label])).toEqual([[20, "First"]]);
    expect(collapsed.boxes.get(4)?.knobs.map((knob) => [knob.member, knob.label])).toEqual([[21, "Second"]]);
    expect(collapsed.graph.nodes.map((node) => node.id)).toEqual([22, 1, 4, MASTER]);
  });
});

describe("the port of a knob that a box shows (step 111)", () => {
  /** A module (1) of a lane (2) that holds a filter (3) and the macro 20; outside it a lane (10) with a reverb (11), and a modulator (30). */
  const graph = (...edges: Graph["edges"]): Graph => ({
    nodes: [lane(2, "Chain", null), effect(3, 2, "Filter"), macro(20, "Brightness", { name: "Brightness" }), lane(10, "Oscillator"), effect(11, 10, "Reverb"), modulator(30, "LFO"), module(1, "Room", [2, 20]), master],
    edges: [link(2, 3), link(10, 11), ...edges],
  });

  test("has what the macro can still be drawn to, as a modulator's node has it: the parameters of every plugin of the session and the settings of the modulators, which the macro does not drive already", () => {
    const targets = run(graph(drives(5, 20, 11, 1))).boxes.get(1)?.knobs[0]?.targets ?? [];
    // (The filter in the module is a target as well, though it is on no canvas while the box is closed; the reverb's first parameter has a cable from the macro already, and the LFO's rate is a setting that a cable can end at.)
    expect(targets.map((group) => group.node)).toEqual([3, 10, 11, 30]);
    const reverb = targets.find((group) => group.node === 11);
    expect(reverb?.params.map((param) => param.index)).toEqual([3, 5]);
    expect(targets.find((group) => group.node === 30)?.fields.map((field) => field.port.type)).toEqual(["rate"]);
    // (What each is called is what the canvas calls it.)
    expect(targets.map((group) => group.name)).toEqual(["Filter", "Oscillator", "Reverb", "LFO"]);
  });

  test("is a macro's knob and nothing else's: another macro that shows its knob has its own list, which has the cable of its own and not the first one's", () => {
    const both: Graph = { ...graph(drives(5, 20, 11, 1)), nodes: [...graph().nodes.slice(0, 3), macro(21, "Cutoff"), ...graph().nodes.slice(3)] };
    const withBoth: Graph = { nodes: both.nodes.map((node) => (node.kind === "module" ? { ...node, members: [2, 20, 21] } : node)), edges: both.edges };
    const [first, second] = run(withBoth).boxes.get(1)?.knobs ?? [];
    expect(first?.member).toBe(20);
    expect(second?.member).toBe(21);
    expect(first?.targets.find((group) => group.node === 11)?.params.map((param) => param.index)).toEqual([3, 5]);
    expect(second?.targets.find((group) => group.node === 11)?.params.map((param) => param.index)).toEqual([1, 3, 5]);
  });

  test("is the box's port for the macro, so that the cables of a macro that shows its knob are drawn from the knob, and not from a port on the bottom edge as well: a handle of a box is named once", () => {
    const collapsed = run(graph(drives(5, 20, 11, 1)));
    expect(collapsed.boxes.get(1)?.modOut).toEqual([]);
    // (The cable is drawn from the box, by the handle that the knob has: `modOut` of the macro.)
    const cable = collapsed.edges.find((placed) => placed.real.kind === "modulation");
    expect([cable?.edge.from.node, cable?.sourceHandle, cable?.edge.to.node, cable?.targetHandle]).toEqual([1, "modOut@20", 11, "param-1"]);
  });

  test("is not there for a macro that is told not to show its knob, which has a port on the bottom edge for a cable that leaves the box, as any modulator in it has, and for any other modulator that drives something outside", () => {
    const hidden: Graph = {
      nodes: [lane(2, "Chain", null), effect(3, 2, "Filter"), macro(20, "Macro", { expose: false }), modulator(21, "LFO"), lane(10, "Oscillator"), effect(11, 10, "Reverb"), module(1, "Room", [2, 20, 21]), master],
      edges: [link(2, 3), link(10, 11), drives(5, 20, 11, 1), drives(6, 21, 11, 3)],
    };
    const box = run(hidden).boxes.get(1);
    expect(box?.knobs).toEqual([]);
    expect(box?.modOut.map((port) => [port.member, port.handle, port.kind])).toEqual([
      [20, "modOut", "modulation"],
      [21, "modOut", "modulation"],
    ]);
  });

  test("keeps the port of another modulator that drives something outside, beside a macro that shows its knob: only the macro's port is the knob's", () => {
    const mixed: Graph = {
      nodes: [lane(2, "Chain", null), effect(3, 2, "Filter"), macro(20, "Brightness", { name: "Brightness" }), modulator(21, "LFO"), lane(10, "Oscillator"), effect(11, 10, "Reverb"), module(1, "Room", [2, 20, 21]), master],
      edges: [link(2, 3), link(10, 11), drives(5, 20, 11, 1), drives(6, 21, 11, 3)],
    };
    const box = run(mixed).boxes.get(1);
    expect(box?.knobs.map((knob) => knob.member)).toEqual([20]);
    expect(box?.modOut.map((port) => [port.member, port.label])).toEqual([[21, "LFO"]]);
  });

  test("keeps a macro's port at the knob when the cable goes to something in the module, which no box draws: the knob has the port whether there is a cable or not", () => {
    const inside = run(graph(drives(5, 20, 3, 1))).boxes.get(1);
    expect(inside?.modOut).toEqual([]);
    expect(inside?.knobs.map((knob) => knob.member)).toEqual([20]);
    expect(run(graph()).boxes.get(1)?.knobs.map((knob) => knob.member)).toEqual([20]);
  });
});

describe("a module that is open is its nodes", () => {
  test("its nodes are on the canvas with the cables between them, and its node is not", () => {
    const open = new Set<NodeId>([1]);
    const collapsed = run(room, open);
    expect(collapsed.graph.nodes.map((node) => node.id)).toEqual([10, 11, 2, 3, 4, MASTER]);
    expect(collapsed.graph.edges).toHaveLength(4);
    expect(collapsed.boxes.size).toBe(0);
    // (It is still the module of what is in it: the tag on a lane closes it.)
    expect([...collapsed.moduleOf.keys()]).toEqual([2, 3, 4]);
    expect(collapsed.edges.every((placed) => placed.edge === placed.real)).toBe(true);
  });

  test("only the modules that are open: another is a box still", () => {
    const graph: Graph = { nodes: [lane(2, "A"), lane(3, "B"), module(1, "First", [2]), module(4, "Second", [3]), master], edges: [] };
    const collapsed = run(graph, new Set([4]));
    expect(collapsed.graph.nodes.map((node) => node.id)).toEqual([3, 1, MASTER]);
    expect([...collapsed.boxes.keys()]).toEqual([1]);
  });
});

describe("what a box says is in it", () => {
  test("is its plugins, and its modulators if it has any, each in the singular for one", () => {
    expect(boxDetail({ plugins: 1, modulators: 0 })).toBe("1 plugin");
    expect(boxDetail({ plugins: 3, modulators: 0 })).toBe("3 plugins");
    expect(boxDetail({ plugins: 0, modulators: 0 })).toBe("0 plugins");
    expect(boxDetail({ plugins: 2, modulators: 1 })).toBe("2 plugins · 1 modulator");
    expect(boxDetail({ plugins: 1, modulators: 4 })).toBe("1 plugin · 4 modulators");
  });
});
