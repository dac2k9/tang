import { describe, expect, test } from "vitest";

import { keepHeld } from "./moving";
import type { PatchNode } from "./toFlow";

const node = (id: string, x: number, y: number, dragging?: boolean, selected?: boolean): PatchNode => ({
  id,
  type: "effect",
  position: { x, y },
  data: { node: Number(id), name: `Node ${id}`, format: null, knobs: [], modulated: [], targets: [], drivenSettings: [], cables: [], notes: false, groupCell: false, scope: false, source: null, heldIn: null, side: null, box: null, module: null },
  ...(dragging === undefined ? {} : { dragging }),
  ...(selected === undefined ? {} : { selected }),
});

const held = (...ids: string[]): ReadonlySet<string> => new Set(ids);

describe("keeping the nodes a person has in hand", () => {
  test("with none in hand the canvas is what the layout says, the very nodes of it", () => {
    const incoming = [node("1", 0, 0), node("2", 400, 0)];
    const current = [node("1", 50, 50), node("2", 450, 50)];
    const kept = keepHeld(incoming, current, held());
    expect(kept).toEqual(incoming);
    expect(kept[0]).toBe(incoming[0]);
    expect(kept).not.toBe(incoming);
  });

  test("a node in hand stays where it is on the canvas, whatever the layout says, and keeps being dragged; the others go where the layout says", () => {
    const incoming = [node("1", 0, 0), node("2", 400, 0), node("3", 800, 0)];
    const current = [node("1", 50, 60, true), node("2", 450, 50), node("3", 850, 50)];
    const kept = keepHeld(incoming, current, held("1"));
    expect(kept.map((n) => [n.id, n.position, n.dragging])).toEqual([
      ["1", { x: 50, y: 60 }, true],
      ["2", { x: 400, y: 0 }, undefined],
      ["3", { x: 800, y: 0 }, undefined],
    ]);
    expect(kept[1]).toBe(incoming[1]);
  });

  test("a node put down and not answered for yet stays too, and is not being dragged", () => {
    const kept = keepHeld([node("1", 0, 0)], [node("1", 70, 80, false)], held("1"));
    expect(kept.map((n) => [n.position, n.dragging])).toEqual([[{ x: 70, y: 80 }, false]]);
  });

  test("the rest of a node in hand is the layout's: its data comes from the new layout", () => {
    const incoming = [{ ...node("1", 0, 0), data: { ...node("1", 0, 0).data, name: "Renamed" } }];
    const kept = keepHeld(incoming, [node("1", 9, 9, true)], held("1"));
    expect(kept[0]?.data.name).toBe("Renamed");
    expect(kept[0]?.position).toEqual({ x: 9, y: 9 });
  });

  test("a node the canvas does not have yet, or the layout no longer has, is as the layout says", () => {
    // In hand and new to the canvas: nothing to keep. In hand and gone from the layout: not brought back.
    const kept = keepHeld([node("1", 0, 0), node("2", 400, 0)], [node("1", 5, 5), node("9", 1, 1, true)], held("2", "9"));
    expect(kept.map((n) => [n.id, n.position])).toEqual([
      ["1", { x: 0, y: 0 }],
      ["2", { x: 400, y: 0 }],
    ]);
  });
});

describe("keeping the nodes that are chosen (step 107)", () => {
  test("a node that is chosen stays chosen when the layout is made again, wherever it is put by it: the layout does not know what a person chose", () => {
    const incoming = [node("1", 0, 0), node("2", 400, 0)];
    const current = [node("1", 0, 0, undefined, true), node("2", 400, 0)];
    const kept = keepHeld(incoming, current, held());
    expect(kept.map((n) => [n.id, n.selected])).toEqual([
      ["1", true],
      ["2", undefined],
    ]);
    // (The node that was not chosen is the layout's own object.)
    expect(kept[1]).toBe(incoming[1]);
    expect(kept[0]?.position).toEqual({ x: 0, y: 0 });
  });

  test("a node in hand that is chosen is both: where it is on the canvas, and chosen", () => {
    const kept = keepHeld([node("1", 0, 0)], [node("1", 30, 40, false, true)], held("1"));
    expect(kept.map((n) => [n.position, n.dragging, n.selected])).toEqual([[{ x: 30, y: 40 }, false, true]]);
  });

  test("a node that was not chosen is not made so by one that has its id, and one that is gone is not brought back", () => {
    const kept = keepHeld([node("1", 0, 0), node("2", 400, 0)], [node("1", 0, 0, undefined, false), node("9", 1, 1, undefined, true)], held());
    expect(kept.map((n) => [n.id, n.selected])).toEqual([
      ["1", undefined],
      ["2", undefined],
    ]);
  });
});
