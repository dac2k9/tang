// @vitest-environment jsdom
// What the layout of the canvas says of itself: whether the nodes are where the arrangement that makes the cables short put them, which is what the button that arranges them looks at (docs/web-ui.md step 98).
import { act, renderHook } from "@testing-library/react";
import { describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Graph, NodeId } from "../../protocol";
import { effect, lane as laneNode, link, master, moduleNode } from "./boxesKit";
import { toFlow } from "./toFlow";
import type { Flow } from "./toFlow";
import { useLayout } from "./useLayout";

const graph = tracerSnapshot.graph;
const scopes: ReadonlySet<NodeId> = new Set();
const lane = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

/** `graph` with the nodes in `placed` placed by a person. */
const withPlaced = (...placed: NodeId[]): Graph => ({ ...graph, nodes: graph.nodes.map((node) => (placed.includes(node.id) ? { ...node, ui: { x: 10 * node.id, y: 20 } } : node)) });

/** `graph` after a change of its structure: the first node has another name. */
const renamed: Graph = { ...graph, nodes: graph.nodes.map((node, i) => (i === 0 ? { ...node, name: "Renamed" } : node)) };

/** Where the layout has each node. */
const placesOf = (flow: Flow) => new Map(flow.nodes.map((node) => [node.id, node.position]));

/** A layout that is given a session when the test says (none to begin with, as the page's is before the session has come). */
const layout = (session: Graph | null) => renderHook(({ shown }: { shown: Graph | null }) => useLayout(shown, scopes), { initialProps: { shown: session } });

describe("the layout of the canvas", () => {
  test("has nothing to arrange without a session, before one comes and after it has gone", () => {
    const { result, rerender } = layout(null);
    expect(result.current.optimized).toBe(true);
    rerender({ shown: graph });
    expect(result.current.optimized).toBe(false);
    rerender({ shown: null });
    expect(result.current.optimized).toBe(true);
  });

  test("has something to arrange when a session is opened, which is laid out in rows, nothing once it is arranged, and something again when the session changes", () => {
    const { result, rerender } = layout(null);
    rerender({ shown: graph });
    expect(result.current.optimized).toBe(false);
    expect(placesOf(result.current.flow)).toEqual(placesOf(toFlow(graph)));

    act(() => {
      result.current.arrangeAgain();
    });
    expect(result.current.optimized).toBe(true);
    expect(placesOf(result.current.flow)).toEqual(placesOf(toFlow(graph, undefined, undefined, { arrange: true })));
    expect(placesOf(result.current.flow)).not.toEqual(placesOf(toFlow(graph)));

    rerender({ shown: renamed });
    expect(result.current.optimized).toBe(false);
    // (The nodes stay where the arrangement put them: it is the button that is bright, and not the canvas that moves.)
    expect(placesOf(result.current.flow)).toEqual(placesOf(toFlow(graph, undefined, undefined, { arrange: true })));
  });

  test("lays the nodes out in rows while the places that a person gave are being given back one at a time, and arranges them when the last has been", () => {
    const { result, rerender } = layout(withPlaced(lane.id, filter.id));
    act(() => {
      result.current.arrangeAgain();
    });
    expect(result.current).toMatchObject({ arranging: true, optimized: false });

    // (The server answers the first request: one node is still placed.)
    rerender({ shown: withPlaced(filter.id) });
    expect(result.current).toMatchObject({ arranging: true, optimized: false });
    expect(placesOf(result.current.flow)).toEqual(placesOf(toFlow(withPlaced(filter.id))));

    // (And the second: none is.)
    rerender({ shown: graph });
    expect(result.current).toMatchObject({ arranging: false, optimized: true });
    expect(placesOf(result.current.flow)).toEqual(placesOf(toFlow(graph, undefined, undefined, { arrange: true })));
  });

  test("has something to arrange when a person has placed a node, whether or not it is where the arrangement would put it", () => {
    const { result } = layout(withPlaced(lane.id));
    expect(result.current.optimized).toBe(false);
    act(() => {
      result.current.arrangeAgain();
    });
    expect(result.current.optimized).toBe(false);
  });
});

describe("the layout of a session that has a module (step 108)", () => {
  const withModule: Graph = { nodes: [laneNode(10, "Oscillator"), laneNode(2, "Chain", null), effect(3, 2, "Filter"), moduleNode(1, "Room", [2]), master], edges: [link(2, 3)] };
  const sets = (...ids: NodeId[]): ReadonlySet<NodeId> => new Set(ids);
  const layoutOf = (open: ReadonlySet<NodeId>) => renderHook(({ opened }: { opened: ReadonlySet<NodeId> }) => useLayout(withModule, scopes, opened), { initialProps: { opened: open } });
  const kinds = (flow: Flow): string[] => flow.nodes.map((node) => `${node.id}:${node.type}`);

  test("draws a module that is closed as its box, and as its nodes when it is open, from the render that says so", () => {
    const { result, rerender } = layoutOf(sets());
    expect(kinds(result.current.flow)).toEqual(["10:instrument", "1:module", `${String(master.id)}:master`]);
    rerender({ opened: sets(1) });
    expect(kinds(result.current.flow)).toEqual(["10:instrument", "3:effect", `${String(master.id)}:master`]);
    rerender({ opened: sets() });
    expect(kinds(result.current.flow)).toEqual(["10:instrument", "1:module", `${String(master.id)}:master`]);
  });

  test("keeps a module open when the nodes are arranged again, and arranges the nodes that are on the canvas", () => {
    const { result } = layoutOf(sets(1));
    act(() => {
      result.current.arrangeAgain();
    });
    expect(kinds(result.current.flow)).toEqual(["10:instrument", "3:effect", `${String(master.id)}:master`]);
    expect(placesOf(result.current.flow)).toEqual(placesOf(toFlow(withModule, undefined, undefined, { arrange: true, open: sets(1) })));
  });
});
