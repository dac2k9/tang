import { describe, expect, test } from "vitest";

import type { Graph } from "../../protocol";
import { audioCableOf, canConnect, cableOf, drawableFrom, drawCable, keyOf, kindOfDrag, notesCableOf, takersOf } from "./connection";
import { drives, effect, keys, lane, link, macro, MASTER, master, modulator, moduleNode, plugin } from "./boxesKit";
import { toFlow } from "./toFlow";

// A cable that is dragged to or from a box, which is a module that is closed (step 108), is a cable to or from the node inside that the handle of the box stands for.

/** Outside: an oscillator (10) and its reverb (11), a modulator (20) and the keys (99). The module "Room" (1): a lane (2) of a filter (3) and a delay (4), and a lane (5) that has an instrument. */
const graph: Graph = {
  nodes: [lane(10, "Oscillator"), effect(11, 10, "Reverb"), lane(2, "Chain", null), effect(3, 2, "Filter"), effect(4, 2, "Delay"), lane(5, "Inner", plugin(3, "builtin:osc")), modulator(20, "LFO"), keys, moduleNode(1, "Room", [2, 5]), master],
  edges: [link(10, 11), link(2, 3), link(3, 4)],
};

const connection = (source: string, sourceHandle: string | null, target: string, targetHandle: string | null) => ({ source, sourceHandle, target, targetHandle });

describe("a cable drawn to or from a box", () => {
  test("from the output of audio of a node outside to an input at the box is a cable to the node inside that the input is of", () => {
    expect(audioCableOf(connection("11", "audioOut", "1", "audioIn@3"))).toEqual({ from: 11, to: 3, side: false });
    expect(audioCableOf(connection("11", "audioOut", "1", "audioInSide@4"))).toEqual({ from: 11, to: 4, side: true });
  });

  test("from an output at the box to an input outside is a cable from the node inside that the output is of", () => {
    expect(audioCableOf(connection("1", "audioOut@4", "11", "audioIn"))).toEqual({ from: 4, to: 11, side: false });
    expect(audioCableOf(connection("1", "audioOut@4", String(MASTER), "audioIn"))).toEqual({ from: 4, to: MASTER, side: false });
  });

  test("from a box to a box is a cable between the nodes inside", () => {
    expect(audioCableOf(connection("1", "audioOut@4", "7", "audioIn@40"))).toEqual({ from: 4, to: 40, side: false });
  });

  test("a cable of notes ends at the lane inside whose input of events the box's port is, and a cable of modulation at the port of the node inside", () => {
    expect(notesCableOf(connection("99", "eventsOut", "1", "eventsIn@5"))).toEqual({ source: 99, lane: 5 });
    expect(cableOf(connection("20", "modOut", "1", "param-1@3"))).toEqual({ modulator: 20, node: 3, port: { type: "param", index: 1 } });
    expect(cableOf(connection("20", "modOut", "1", "volume@5"))).toEqual({ modulator: 20, node: 5, port: { type: "volume" } });
    expect(cableOf(connection("20", "modOut", "1", "depth-7@20"))).toEqual({ modulator: 20, node: 20, port: { type: "depth", edge: 7 } });
  });

  test("is no cable when the handle of the box is the wrong kind of port for it, as it is for a node's own", () => {
    expect(audioCableOf(connection("11", "audioOut", "1", "eventsIn@5"))).toBeNull();
    expect(audioCableOf(connection("11", "audioOut", "1", "param-1@3"))).toBeNull();
    expect(notesCableOf(connection("99", "eventsOut", "1", "audioIn@3"))).toBeNull();
    expect(cableOf(connection("20", "modOut", "1", "audioIn@3"))).toBeNull();
    // (A handle that is no box's keeps the node that React Flow says.)
    expect(audioCableOf(connection("11", "audioOut", "3", "audioIn"))).toEqual({ from: 11, to: 3, side: false });
  });

  test("the kind of cable that a drag from a box's port draws is the port's", () => {
    expect(kindOfDrag("audioOut@4")).toBe("audio");
    expect(kindOfDrag("audioOut")).toBe("audio");
    expect(kindOfDrag("eventsOut")).toBe("notes");
    expect(kindOfDrag("modOut@22")).toBe("modulation");
    expect(kindOfDrag(null)).toBe("modulation");
  });

  test("the inputs that take a cable from the output of a box are those that take it from the node inside, and a port that is asked for is the node inside's", () => {
    const { nodes } = toFlow(graph);
    const inside = takersOf(graph, nodes, { nodeId: "4", handleType: "source", handleId: "audioOut" });
    const at = takersOf(graph, nodes, { nodeId: "1", handleType: "source", handleId: "audioOut@4" });
    expect(inside).not.toBeNull();
    expect(at).toEqual(inside);
    // (The reverb outside takes it, and so does the master; the node it starts at does not take its own cable.)
    expect(inside?.has(keyOf(11, "audioIn"))).toBe(true);
    expect(inside?.has(keyOf(MASTER, "audioIn"))).toBe(true);
    expect(inside?.has(keyOf(4, "audioIn"))).toBe(false);
    // From outside: the filter inside takes a cable from the reverb, and the box's port for it is asked by the filter's own key.
    const fromReverb = takersOf(graph, nodes, { nodeId: "11", handleType: "source", handleId: "audioOut" });
    expect(fromReverb?.has(keyOf(3, "audioIn"))).toBe(true);
  });

  test("a drag that starts at a port that takes a cable and starts none (an input) takes none, in a box as anywhere", () => {
    const { nodes } = toFlow(graph);
    expect(takersOf(graph, nodes, { nodeId: "1", handleType: "target", handleId: "audioIn@3" })).toBeNull();
    expect(takersOf(graph, nodes, { nodeId: "1", handleType: "source", handleId: "audioOut@x" })).toBeNull();
  });

  test("can be drawn when the controller would take the cable between the nodes inside and out, and not when it would not", () => {
    const { nodes } = toFlow(graph);
    expect(canConnect(graph, nodes, connection("11", "audioOut", "1", "audioIn@3"))).toBe(true);
    expect(canConnect(graph, nodes, connection("1", "audioOut@4", String(MASTER), "audioIn"))).toBe(true);
    // (The delay is fed by the filter by the chain: that cable is not a second one, but its end at the delay is.)
    expect(canConnect(graph, nodes, connection("1", "audioOut@4", "1", "audioIn@3"))).toBe(false);
    expect(canConnect(graph, nodes, connection("1", "audioOut@4", "1", "audioIn@4"))).toBe(false);
    expect(canConnect(graph, nodes, connection("99", "eventsOut", "1", "eventsIn@5"))).toBe(true);
    expect(canConnect(graph, nodes, connection("20", "modOut", "1", "param-1@3"))).toBe(true);
  });

  test("is asked of the editor as the cable between the nodes inside and out, whichever kind it is", () => {
    const asked: unknown[][] = [];
    const editor = {
      addAudioCable: (...args: unknown[]) => asked.push(["audio", ...args]),
      addNotesCable: (...args: unknown[]) => asked.push(["notes", ...args]),
      addCable: (...args: unknown[]) => asked.push(["modulation", ...args]),
    };
    const draw = (c: ReturnType<typeof connection>): void => {
      drawCable(editor as unknown as Parameters<typeof drawCable>[0], c);
    };
    draw(connection("11", "audioOut", "1", "audioInSide@4"));
    draw(connection("99", "eventsOut", "1", "eventsIn@5"));
    draw(connection("20", "modOut", "1", "param-3@3"));
    draw(connection("1", "audioOut@4", String(MASTER), "audioIn"));
    expect(asked).toEqual([
      ["audio", 11, 4, null, true],
      ["notes", 99, 5],
      ["modulation", 20, 3, { type: "param", index: 3 }],
      ["audio", 4, MASTER, null, false],
    ]);
  });
});

// A cable that is drawn from the knob of a macro on a box (step 111; the user's "attach cables to the macro on the box"): the macro is a node inside a module that is closed, so that it is no node of the canvas, and what it can be drawn to is the knob's.
describe("a cable drawn from the knob of a macro on a box", () => {
  /** Outside: an oscillator (10) with a reverb (11) and a modulator (30). The module "Room" (1): a lane (2) that holds a filter (3), and a macro (20) that shows its knob and one (21) that does not. */
  const withMacros: Graph = {
    nodes: [lane(10, "Oscillator"), effect(11, 10, "Reverb"), modulator(30, "LFO"), lane(2, "Chain", null), effect(3, 2, "Filter"), macro(20, "Brightness", { name: "Brightness" }), macro(21, "Hidden", { expose: false }), moduleNode(1, "Room", [2, 20, 21]), master],
    edges: [link(10, 11), link(2, 3), drives(5, 20, 11, 1)],
  };

  test("is looked up in what the knob says the macro can be drawn to: the box has the macro's port, and the ports that take the cable are the ones that the macro can still drive", () => {
    const { nodes } = toFlow(withMacros);
    const takers = takersOf(withMacros, nodes, { nodeId: "1", handleType: "source", handleId: "modOut@20" });
    expect(takers).not.toBeNull();
    // (The reverb's second and third parameters take it: the first has a cable from the macro already. The filter in the module is a target that no canvas draws, and the LFO's rate is one.)
    expect(takers?.has(keyOf(11, "param-3"))).toBe(true);
    expect(takers?.has(keyOf(11, "param-5"))).toBe(true);
    expect(takers?.has(keyOf(11, "param-1"))).toBe(false);
    expect(takers?.has(keyOf(3, "param-1"))).toBe(true);
    expect(takers?.has(keyOf(30, "rate"))).toBe(true);
    // (A macro does not drive itself, or another macro: a macro has no setting that a cable ends at.)
    expect([...(takers ?? [])].some((key) => key.startsWith("20:") || key.startsWith("21:"))).toBe(false);
  });

  test("is the same list as the macro's own would have been on the canvas, whether the node is looked for by the knob or by the macro", () => {
    const { nodes } = toFlow(withMacros);
    expect(drawableFrom(nodes, 20)).toEqual(takersOf(withMacros, nodes, { nodeId: "1", handleType: "source", handleId: "modOut@20" }));
    const open = toFlow(withMacros, undefined, undefined, { open: new Set([1]) }).nodes;
    // (The module is open: the macro is a node of the canvas, which has the list, and there is no box.)
    expect(open.some((node) => node.data.box !== null)).toBe(false);
    expect(drawableFrom(open, 20)).toEqual(drawableFrom(nodes, 20));
  });

  test("has nothing to be drawn to for a macro that is not on the canvas and has no knob, and for a node that is no modulator: the macro that is told not to show its knob has no port to start at", () => {
    const { nodes } = toFlow(withMacros);
    expect(drawableFrom(nodes, 21).size).toBe(0);
    expect(drawableFrom(nodes, 3).size).toBe(0);
    expect(drawableFrom(nodes, 999).size).toBe(0);
    expect(takersOf(withMacros, nodes, { nodeId: "1", handleType: "source", handleId: "modOut@21" })?.size).toBe(0);
  });

  test("can be drawn to a parameter that is on the canvas, and to none that the macro drives already, and not from the knob of a macro that is not in the box", () => {
    const { nodes } = toFlow(withMacros);
    expect(canConnect(withMacros, nodes, connection("1", "modOut@20", "11", "param-3"))).toBe(true);
    expect(canConnect(withMacros, nodes, connection("1", "modOut@20", "11", "param-1"))).toBe(false);
    expect(canConnect(withMacros, nodes, connection("1", "modOut@21", "11", "param-3"))).toBe(false);
    expect(canConnect(withMacros, nodes, connection("1", "modOut@20", "30", "rate"))).toBe(true);
    // (To the reverb's volume-like gains: the mix of an effect is a place for a cable.)
    expect(canConnect(withMacros, nodes, connection("1", "modOut@20", "11", "mix"))).toBe(true);
  });

  test("is asked of the editor as the cable from the macro inside, to the port that it ends at", () => {
    const asked: unknown[][] = [];
    const editor = {
      addAudioCable: (...args: unknown[]) => asked.push(["audio", ...args]),
      addNotesCable: (...args: unknown[]) => asked.push(["notes", ...args]),
      addCable: (...args: unknown[]) => asked.push(["modulation", ...args]),
    };
    drawCable(editor as unknown as Parameters<typeof drawCable>[0], connection("1", "modOut@20", "11", "param-3"));
    drawCable(editor as unknown as Parameters<typeof drawCable>[0], connection("1", "modOut@20", "30", "rate"));
    expect(asked).toEqual([
      ["modulation", 20, 11, { type: "param", index: 3 }],
      ["modulation", 20, 30, { type: "rate" }],
    ]);
  });
});
