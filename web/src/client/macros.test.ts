import { describe, expect, test } from "vitest";

import type { Intent, Node, TelemetryLayout } from "../protocol";
import { macrosFixture } from "./fixtures";
import { macroExposed, macroName } from "./macros";
import type { MacroSource } from "./macros";
import { MockServer } from "./mock";
import { macroExposedOf, nodeById } from "./selectors";
import type { ClientState } from "./connection";

// A macro that has a name and shows its knob on the box of a module (`RenameMacro`, `ExposeMacro`: step 110), against the mock engine: what the controller does (`rename_macro_intent`, `expose_macro_intent`).
// `fixtures/macros.json` is what the controller made of a script on a session of its own, step by step; the mock is held to it here.

/** The layout of the telemetry with the same fingerprint, which the controller's comes from a hasher and the mock's from a counter. */
const layoutOf = (layout: TelemetryLayout): TelemetryLayout => ({ ...layout, rev: 0 });

const refusedWith = (server: MockServer, intent: Intent): string => {
  try {
    server.apply(intent);
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  }
  return "done";
};

const fixture = macrosFixture;
const macrosOf = (server: MockServer): Extract<Node, { kind: "modulator" }>[] =>
  server.snapshot.graph.nodes.filter((node): node is Extract<Node, { kind: "modulator" }> => node.kind === "modulator" && node.source.type === "macro");

describe("the mock engine's macros that have a name and a knob that a box shows", () => {
  test("are what the controller makes of them, step by step: the events, the nodes and the sources they have (a name and a flag that are what a macro starts as are left out), the cables, the layout of the telemetry, and what Undo and Redo would do", () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
    expect(fixture.steps.length).toBeGreaterThanOrEqual(17);
    for (const [i, step] of fixture.steps.entries()) {
      const told = server.apply(step.intent);
      const what = `step ${String(i + 1)}: ${JSON.stringify(step.intent)}`;
      expect(told.events, what).toEqual(step.events);
      expect(told.restructured, what).toBe(step.restructured);
      expect(server.history, what).toEqual(step.history);
      expect(server.snapshot.graph, what).toEqual(step.graph);
      expect(layoutOf(server.snapshot.telemetry), what).toEqual(layoutOf(step.telemetry));
    }
  });

  test("refuse what the controller refuses, in its words, with nothing changed: a node that is no macro, one that is not there, the master, a name that is too long and one with a control character", () => {
    expect(fixture.refusals.length).toBeGreaterThanOrEqual(14);
    for (const [i, refusal] of fixture.refusals.entries()) {
      const server = new MockServer({ snapshot: fixture.snapshot });
      for (const step of fixture.steps.slice(0, refusal.after)) server.apply(step.intent);
      const [before, history] = [server.snapshot, server.history];
      const what = `refusal ${String(i + 1)}: ${JSON.stringify(refusal.intent)}`;
      expect(refusedWith(server, refusal.intent), what).toBe(refusal.error);
      expect(server.snapshot, what).toBe(before);
      expect(server.history, what).toEqual(history);
    }
  });

  test("call the node by the name, say the name and the flag in the source only while they are not what a macro starts as, and keep the value and the other macro as they were", () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
    const [plain, hidden] = macrosOf(server);
    expect(plain?.name).toBe("Macro");
    expect(plain?.source).toEqual({ type: "macro", value: 0.5 });
    expect(hidden?.source).toEqual({ type: "macro", value: 0, name: "Wet", expose: false });
    server.apply({ type: "renameMacro", node: plain?.id ?? -1, name: "  Brightness  " });
    server.apply({ type: "exposeMacro", node: plain?.id ?? -1, expose: false });
    const [renamed, other] = macrosOf(server);
    expect(renamed?.name).toBe("Brightness");
    expect(renamed?.source).toEqual({ type: "macro", value: 0.5, name: "Brightness", expose: false });
    expect(other).toBe(hidden);
    // A name of nothing, and of spaces, is no name; and a flag that is on is left out.
    for (const blank of ["", " \t ", " "]) {
      server.apply({ type: "renameMacro", node: plain?.id ?? -1, name: blank });
      expect(macrosOf(server)[0]?.name, JSON.stringify(blank)).toBe("Macro");
      expect(macrosOf(server)[0]?.source, JSON.stringify(blank)).toEqual({ type: "macro", value: 0.5, expose: false });
    }
    server.apply({ type: "exposeMacro", node: plain?.id ?? -1, expose: true });
    expect(macrosOf(server)[0]?.source).toEqual({ type: "macro", value: 0.5 });
  });

  test("keep a name of the longest length that a person can see, counted in characters, and refuse one character more", () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
    const node = macrosOf(server)[0]?.id ?? -1;
    expect(refusedWith(server, { type: "renameMacro", node, name: "å".repeat(65) })).toBe("a name is at most 64 characters");
    expect(refusedWith(server, { type: "renameMacro", node, name: "😀".repeat(65) })).toBe("a name is at most 64 characters");
    expect(refusedWith(server, { type: "renameMacro", node, name: "😀".repeat(64) })).toBe("done");
    expect(macrosOf(server)[0]?.name).toBe("😀".repeat(64));
  });

  test("make a step of the history only for a change: a name or a flag that the macro has is none, and a name that is refused is none either", () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
    const node = macrosOf(server)[0]?.id ?? -1;
    expect(server.history).toEqual({ undo: null, redo: null });
    server.apply({ type: "renameMacro", node, name: "" });
    server.apply({ type: "renameMacro", node, name: "   " });
    server.apply({ type: "exposeMacro", node, expose: true });
    expect(server.history).toEqual({ undo: null, redo: null });
    server.apply({ type: "renameMacro", node, name: "Wet" });
    expect(server.history.undo).toBe("rename a macro");
    server.apply({ type: "renameMacro", node, name: " Wet " });
    expect(refusedWith(server, { type: "renameMacro", node, name: "x".repeat(65) })).toBe("a name is at most 64 characters");
    server.apply({ type: "undo" });
    expect(server.history).toEqual({ undo: null, redo: "rename a macro" });
    expect(macrosOf(server)[0]?.name).toBe("Macro");
    // Two in a row are two steps: nothing of a name is merged.
    server.apply({ type: "renameMacro", node, name: "A" });
    server.apply({ type: "renameMacro", node, name: "B" });
    server.apply({ type: "undo" });
    expect(macrosOf(server)[0]?.name).toBe("A");
    server.apply({ type: "undo" });
    expect(macrosOf(server)[0]?.name).toBe("Macro");
    server.apply({ type: "exposeMacro", node, expose: false });
    expect(server.history.undo).toBe("hide a macro from its box");
    server.apply({ type: "exposeMacro", node, expose: true });
    expect(server.history.undo).toBe("show a macro on its box");
  });
});

describe("what the page reads of a macro", () => {
  const source = (extra: Partial<MacroSource>): MacroSource => ({ type: "macro", value: 0.25, ...extra });

  test("is a name that is none while the wire has none and a flag that is on while the wire has none", () => {
    expect(macroName(source({}))).toBe("");
    expect(macroName(source({ name: "Brightness" }))).toBe("Brightness");
    expect(macroExposed(source({}))).toBe(true);
    expect(macroExposed(source({ expose: true }))).toBe(true);
    expect(macroExposed(source({ expose: false }))).toBe(false);
  });

  test("is told by the client's state for a node that is a macro, and for none that is not", () => {
    const server = new MockServer({ snapshot: fixture.snapshot });
    const [plain, hidden] = macrosOf(server);
    const lfo = server.snapshot.graph.nodes.find((node) => node.kind === "modulator" && node.source.type === "lfo");
    const state = { snapshot: server.snapshot } as unknown as ClientState;
    expect(macroExposedOf(state, plain?.id ?? -1)).toBe(true);
    expect(macroExposedOf(state, hidden?.id ?? -1)).toBe(false);
    expect(macroExposedOf(state, lfo?.id ?? -1)).toBeUndefined();
    expect(macroExposedOf(state, 9_999)).toBeUndefined();
    expect(macroExposedOf({ snapshot: null } as unknown as ClientState, plain?.id ?? -1)).toBeUndefined();
    expect(nodeById(state, hidden?.id ?? -1)?.name).toBe("Wet");
  });
});
