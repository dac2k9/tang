// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { useEffect } from "react";
import { afterEach, describe, expect, test } from "vitest";

import { tracerSnapshot } from "../../client/fixtures";
import type { Module } from "../../client/modules";
import type { Intent, Node } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import type { Reveals } from "../../client/reveals";
import { useNotices, useReveals } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { ADD_FREE_EFFECT_ID } from "./nodes";

afterEach(cleanup);

// A module that is added is one box on the canvas (step 108): what the box says, how it opens into the nodes that are in it and closes again, and what a right click on it and on them offers. What the page shows,
// what it asks and when. (The cables that cross the box's edge are in `toFlow.boxes.test.ts` and `connection.boxes.test.ts`: the canvas of a test has no size, so that no cable is drawn here.)

/** Two effects in a lane that has no instrument, and no cable. */
const space: Module = {
  lanes: [
    {
      instrument: null,
      effects: [
        { plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true },
        { plugin: "builtin:reverb", preset: null, params: {}, mix: 0.4, pinned: [], fed: true },
      ],
    },
  ],
  cables: [],
  modulators: [],
};

/** A lane that has an oscillator and a filter, with an LFO that hears the lane and drives the filter's cutoff. */
const voice: Module = {
  lanes: [{ instrument: { plugin: "builtin:osc", preset: null, params: {}, volume: 1, pinned: [] }, effects: [{ plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true }] }],
  cables: [],
  modulators: [{ source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 }, hears: 0, targets: [{ drives: { type: "param", plugin: { lane: 0, effect: 0 }, name: "cutoff" }, depth: 0.5 }] }],
};

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** The bar's "+ Effect" (the page's bar is not in these tests): where the focus goes when a module is taken out. */
function Bar() {
  return (
    <button type="button" id={ADD_FREE_EFFECT_ID}>
      Add an effect
    </button>
  );
}

/** Let the page have its frames and its promises: what a click asks of the server goes out a frame later. */
const settled = () =>
  act(async () => {
    await new Promise<void>((resolve) => setTimeout(resolve, 60));
  });

async function rigged() {
  const made = await rig(
    <>
      <PatchView />
      <Board />
      <Bar />
    </>,
    { snapshot: tracerSnapshot, modules: { Space: space, Voice: voice } },
  );
  return made;
}

type Rigged = Awaited<ReturnType<typeof rigged>>;

const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);
const names = (): (string | null)[] => Array.from(document.querySelectorAll(".tang-node__name")).map((name) => name.textContent);
const moduleNodes = (r: Rigged): Node[] => (r.client.state.snapshot?.graph.nodes ?? []).filter((node) => node.kind === "module");

/** Add the module `name` of the library and wait for its box. */
async function addModule(r: Rigged, name: string): Promise<HTMLElement> {
  r.server.apply({ type: "addFreeEffect", plugin: `module:${name}` });
  const open = await screen.findByRole("button", { name: `Open ${name}` });
  const box = open.closest<HTMLElement>(".react-flow__node");
  if (!box) throw new Error("the box is a node of the canvas");
  return box;
}

describe("a module that is added", () => {
  test("is one box that says what is in it, with a row for each port of the module, and what is in it is not on the canvas", async () => {
    const r = await rigged();
    const box = await addModule(r, "Voice");
    // (The tracer's own nodes, and the box: the oscillator, the filter and the LFO of the module are in it.)
    expect(names()).toEqual(["Oscillator 1", "Filter 1", "LFO 1", "Voice", "Master"]);
    expect(within(box).getByText("2 plugins · 1 modulator")).toBeTruthy();
    expect(within(box).getByText("Module")).toBeTruthy();
    // The input of notes of its instrument, and the output of its filter: the ports of the module, called what the cables at them are (the session numbers the names that are the same, the nodes in the box too).
    const ports = within(box).getByRole("list", { name: "Ports of Voice" });
    expect(within(ports).getAllByRole("listitem").map((row) => row.textContent)).toEqual(["♪ Oscillator 2Filter 2"]);
    const [module] = moduleNodes(r);
    if (module?.kind !== "module") throw new Error("the session has the module");
    const lane = module.members[0];
    expect(box.querySelector(`[data-handleid="eventsIn@${String(lane)}"]`)).not.toBeNull();
    expect(box.querySelectorAll(".tang-port--audio.tang-port--out")).toHaveLength(1);
    expect(box.querySelector(".tang-port--notes.tang-port--in")).not.toBeNull();
  });

  test("is a box of the same kind whether it is of effects only (one input and one output) or has an instrument", async () => {
    const r = await rigged();
    const box = await addModule(r, "Space");
    expect(within(box).getByText("2 plugins")).toBeTruthy();
    const ports = within(box).getByRole("list", { name: "Ports of Space" });
    // (The filter takes audio that nothing inside feeds, and is the second of that name; the reverb puts out what nothing inside takes.)
    expect(within(ports).getAllByRole("listitem").map((row) => row.textContent)).toEqual(["Filter 2Reverb"]);
    expect(box.querySelector(".tang-port--audio.tang-port--in")).not.toBeNull();
    expect(box.querySelector(".tang-port--notes")).toBeNull();
  });

  test("opens into the nodes that are in it, with a tag over its lane that closes it, and the box goes while it is open", async () => {
    const r = await rigged();
    await addModule(r, "Voice");
    fireEvent.click(screen.getByRole("button", { name: "Open Voice" }));
    // (The module's oscillator, filter and LFO are on the canvas now, with the tracer's: the names are numbered, and the box is not.)
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Open Voice" })).toBeNull();
    });
    expect(names().filter((name) => name?.startsWith("Oscillator"))).toHaveLength(2);
    expect(names().filter((name) => name?.startsWith("Filter"))).toHaveLength(2);
    expect(names()).not.toContain("Voice");
    // The tag is over the lane of the module and nowhere else: one lane, one tag.
    const tag = screen.getByRole("button", { name: "Close Voice" });
    expect(tag.textContent).toBe("▾ Voice");
    expect(tag.getAttribute("aria-expanded")).toBe("true");
    expect(document.querySelectorAll(".tang-node__module-tag")).toHaveLength(1);
    expect(tag.closest(".react-flow__node")?.querySelector(".tang-node__name")?.textContent).toMatch(/^Oscillator/);
  });

  test("closes again at the tag, and is the box it was: what the person had open stays open while the session changes", async () => {
    const r = await rigged();
    await addModule(r, "Voice");
    fireEvent.click(screen.getByRole("button", { name: "Open Voice" }));
    fireEvent.click(await screen.findByRole("button", { name: "Close Voice" }));
    expect(await screen.findByRole("button", { name: "Open Voice" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Close Voice" })).toBeNull();
    expect(names()).toEqual(["Oscillator 1", "Filter 1", "LFO 1", "Voice", "Master"]);
    // Open, and another module is added: the first stays open, and the second is a box.
    fireEvent.click(screen.getByRole("button", { name: "Open Voice" }));
    await screen.findByRole("button", { name: "Close Voice" });
    r.server.apply({ type: "addFreeEffect", plugin: "module:Space" });
    await screen.findByRole("button", { name: "Open Space" });
    expect(screen.getByRole("button", { name: "Close Voice" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Open Voice" })).toBeNull();
  });

  test("opens, for a lane that has no card, with the tag over the first of its effects", async () => {
    const r = await rigged();
    await addModule(r, "Space");
    fireEvent.click(screen.getByRole("button", { name: "Open Space" }));
    const tag = await screen.findByRole("button", { name: "Close Space" });
    expect(tag.closest(".react-flow__node")?.querySelector(".tang-node__name")?.textContent).toMatch(/^Filter/);
    expect(document.querySelectorAll(".tang-node__module-tag")).toHaveLength(1);
  });

  test("is taken out with all that is in it by the menu of its ×, which asks first, in one step that undo takes back: the box is back, and with it what was in it", async () => {
    const r = await rigged();
    await addModule(r, "Voice");
    const before = r.client.state.snapshot?.graph.nodes.length ?? 0;
    const [module] = moduleNodes(r);
    if (!module) throw new Error("the session has the module");
    fireEvent.click(screen.getByRole("button", { name: "Remove Voice" }));
    const menu = await screen.findByRole("menu", { name: "Remove Voice" });
    const choices = within(menu).getAllByRole("menuitem").map((entry) => entry.textContent);
    expect(choices).toEqual(["Keep Voice", "Ungroup Voice: keep the 2 plugins · 1 modulator, take the box away", "Remove Voice for good: the 2 plugins · 1 modulator in it go with it"]);
    // (Keeping it asks nothing, and the box is there still: what a click asks of the server goes out a frame later, so the page is given its frames first.)
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Keep Voice" }));
    await settled();
    expect(asked(r.requests).filter((intent) => intent.type === "removeNode")).toEqual([]);
    expect(screen.getByRole("button", { name: "Open Voice" })).toBeTruthy();
    expect(document.activeElement?.id).not.toBe(ADD_FREE_EFFECT_ID);
    fireEvent.click(screen.getByRole("button", { name: "Remove Voice" }));
    fireEvent.click(within(await screen.findByRole("menu", { name: "Remove Voice" })).getByRole("menuitem", { name: /^Remove Voice for good/ }));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Open Voice" })).toBeNull();
    });
    expect(asked(r.requests).filter((intent) => intent.type === "removeNode")).toEqual([{ type: "removeNode", node: module.id }]);
    // (The focus goes to the bar's "+ Effect", which is where a person who took a module out goes on from.)
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(ADD_FREE_EFFECT_ID);
    });
    expect(names()).toEqual(["Oscillator", "Filter", "LFO", "Master"]);
    expect(r.server.history.undo).toBe("remove Voice");
    // One step puts the box back, and what was in it.
    r.server.apply({ type: "undo" });
    await screen.findByRole("button", { name: "Open Voice" });
    expect(r.client.state.snapshot?.graph.nodes.length).toBe(before);
    expect(names()).toEqual(["Oscillator 1", "Filter 1", "LFO 1", "Voice", "Master"]);
  });
});

describe("a module that is ungrouped (step 109)", () => {
  test("by the menu of its ×: the box goes and what was in it is on the canvas, in one step that undo takes back, and the focus goes to the bar", async () => {
    const r = await rigged();
    await addModule(r, "Voice");
    const [module] = moduleNodes(r);
    if (!module) throw new Error("the session has the module");
    const nodes = r.client.state.snapshot?.graph.nodes.length ?? 0;
    fireEvent.click(screen.getByRole("button", { name: "Remove Voice" }));
    const menu = await screen.findByRole("menu", { name: "Remove Voice" });
    fireEvent.click(within(menu).getByRole("menuitem", { name: /^Ungroup Voice/ }));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Open Voice" })).toBeNull();
    });
    expect(asked(r.requests).filter((intent) => intent.type !== "undo")).toContainEqual({ type: "ungroupModule", node: module.id });
    expect(asked(r.requests).filter((intent) => intent.type === "removeNode")).toEqual([]);
    // (Nothing was taken out: the module's oscillator, filter and LFO are on the canvas as any are, beside the tracer's, and the session has all its nodes but the record.)
    expect(moduleNodes(r)).toEqual([]);
    expect(r.client.state.snapshot?.graph.nodes.length).toBe(nodes - 1);
    expect(names()).toEqual(expect.arrayContaining(["Oscillator 2", "Filter 2", "LFO 2"]));
    expect(names()).not.toContain("Voice");
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(ADD_FREE_EFFECT_ID);
    });
    expect(r.server.history.undo).toBe("ungroup Voice");
    // One step puts the box back, and what is in it is hidden again.
    r.server.apply({ type: "undo" });
    await screen.findByRole("button", { name: "Open Voice" });
    expect(names()).toEqual(["Oscillator 1", "Filter 1", "LFO 1", "Voice", "Master"]);
  });

  test("by the right click on the box and on a lane of the module when it is open", async () => {
    const r = await rigged();
    const box = await addModule(r, "Voice");
    const [module] = moduleNodes(r);
    if (!module) throw new Error("the session has the module");
    fireEvent.contextMenu(box);
    fireEvent.click(within(screen.getByRole("menu", { name: "Voice" })).getByRole("menuitem", { name: /^Ungroup Voice/ }));
    await waitFor(() => {
      expect(moduleNodes(r)).toEqual([]);
    });
    r.server.apply({ type: "undo" });
    fireEvent.click(await screen.findByRole("button", { name: "Open Voice" }));
    const tag = await screen.findByRole("button", { name: "Close Voice" });
    const lane = tag.closest<HTMLElement>(".react-flow__node");
    if (!lane) throw new Error("the tag is on a node");
    fireEvent.contextMenu(lane);
    fireEvent.click(within(screen.getByRole("menu", { name: /^Oscillator/ })).getByRole("menuitem", { name: "Ungroup Voice" }));
    await waitFor(() => {
      expect(moduleNodes(r)).toEqual([]);
    });
    expect(screen.queryByRole("button", { name: "Close Voice" })).toBeNull();
    expect(asked(r.requests).filter((intent) => intent.type === "ungroupModule")).toEqual([
      { type: "ungroupModule", node: module.id },
      { type: "ungroupModule", node: module.id },
    ]);
  });
});

/** A canvas that is asked to show a node by `Reveals`: the nodes it was asked to show, in order. */
async function watched() {
  let reveals: Reveals | undefined;
  function Probe() {
    const found = useReveals();
    // (In an effect, which is where a value that is read after the render is kept.)
    useEffect(() => {
      reveals = found;
    }, [found]);
    return null;
  }
  const made = await rig(
    <>
      <PatchView />
      <Probe />
    </>,
    { snapshot: tracerSnapshot, modules: { Space: space, Voice: voice } },
  );
  if (!reveals) throw new Error("no reveals");
  const show = reveals;
  const shown: number[] = [];
  show.subscribe((node) => shown.push(node));
  return { ...made, shown };
}

describe("the view of a module that is opened or closed", () => {
  test("is moved to the first lane that is on the canvas when the module is opened, and to the box when it is closed again", async () => {
    const r = await watched();
    r.server.apply({ type: "addFreeEffect", plugin: "module:Voice" });
    const open = await screen.findByRole("button", { name: "Open Voice" });
    const module = r.client.state.snapshot?.graph.nodes.find((node) => node.kind === "module");
    if (module?.kind !== "module") throw new Error("the session has the module");
    expect(r.shown).toEqual([]);
    fireEvent.click(open);
    // (The lane of the module has a card, and is what the tag is over.)
    await waitFor(() => {
      expect(r.shown).toEqual([module.members[0]]);
    });
    fireEvent.click(await screen.findByRole("button", { name: "Close Voice" }));
    await waitFor(() => {
      expect(r.shown).toEqual([module.members[0], module.id]);
    });
  });

  test("is moved to the first effect that is on the canvas when the module has a lane that has no card: the lane is not on the canvas", async () => {
    const r = await watched();
    r.server.apply({ type: "addFreeEffect", plugin: "module:Space" });
    fireEvent.click(await screen.findByRole("button", { name: "Open Space" }));
    const tag = await screen.findByRole("button", { name: "Close Space" });
    const name = tag.closest(".react-flow__node")?.querySelector(".tang-node__name")?.textContent;
    expect(name).toMatch(/^Filter/);
    await waitFor(() => {
      expect(r.shown).toHaveLength(1);
    });
    const effect = r.client.state.snapshot?.graph.nodes.find((node) => node.kind === "effect" && node.name === "Filter" && node.parent !== null && node.id !== 1 && r.shown.includes(node.id));
    expect(effect).toBeDefined();
  });
});

describe("the menus of a module", () => {
  test("a right click on the box opens its menu: open it, and remove it with all that is in it; the first opens it", async () => {
    const r = await rigged();
    const box = await addModule(r, "Voice");
    fireEvent.contextMenu(box);
    const menu = screen.getByRole("menu", { name: "Voice" });
    expect(within(menu).getAllByRole("menuitem").map((entry) => entry.textContent)).toEqual([
      "Open Voice",
      "Ungroup Voice: keep the 2 plugins · 1 modulator, take the box away",
      "Remove Voice for good: the 2 plugins · 1 modulator in it go with it",
    ]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Open Voice" }));
    await screen.findByRole("button", { name: "Close Voice" });
    expect(screen.queryByRole("button", { name: "Open Voice" })).toBeNull();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("a right click on a lane of a module that is open has the item that closes it first, over the lane's own", async () => {
    const r = await rigged();
    await addModule(r, "Voice");
    fireEvent.click(screen.getByRole("button", { name: "Open Voice" }));
    const tag = await screen.findByRole("button", { name: "Close Voice" });
    const lane = tag.closest<HTMLElement>(".react-flow__node");
    if (!lane) throw new Error("the tag is on a node");
    fireEvent.contextMenu(lane);
    const menu = screen.getByRole("menu", { name: /^Oscillator/ });
    const items = within(menu).getAllByRole("menuitem").map((entry) => entry.textContent);
    expect(items.slice(0, 2)).toEqual(["Close Voice", "Ungroup Voice"]);
    expect(items.length).toBeGreaterThan(3);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Close Voice" }));
    await screen.findByRole("button", { name: "Open Voice" });
    expect(screen.queryByRole("button", { name: "Close Voice" })).toBeNull();
  });

  test("a node that is not in a module has no such item, and a module that was never opened is a box where it was added", async () => {
    const r = await rigged();
    await addModule(r, "Space");
    const lane = (await screen.findAllByText("Oscillator", { selector: ".tang-node__name" }))[0]?.closest<HTMLElement>(".react-flow__node");
    if (!lane) throw new Error("the tracer has its oscillator");
    fireEvent.contextMenu(lane);
    const menu = screen.getByRole("menu", { name: "Oscillator" });
    expect(within(menu).queryByRole("menuitem", { name: /^Close / })).toBeNull();
  });
});
