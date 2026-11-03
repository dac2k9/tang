import { describe, expect, test } from "vitest";

import type { Graph } from "../../protocol";
import { audioCableOf, canConnect, cableOf, drawCable, keyOf, kindOfDrag, notesCableOf, takersOf } from "./connection";
import { effect, keys, lane, link, MASTER, master, modulator, moduleNode, plugin } from "./boxesKit";
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
