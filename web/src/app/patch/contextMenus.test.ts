import { describe, expect, test } from "vitest";

import { MODULE_FORMAT } from "../../client/catalog";
import type { ClientState } from "../../client/connection";
import { builtinCatalog, moduleGraphsFixture, nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { CatalogEntry, Graph, Node, NodeId, Snapshot } from "../../protocol";
import type { ContextItem } from "../../ui/ContextMenu";
import { effect, lane, link, master as theMaster, moduleNode } from "./boxesKit";
import { COLLAPSE_CHOSEN, edgeMenu, edgeSpec, nodeMenu, nodeSpec, paneMenu, SAVE_CHOSEN, selectionMenu } from "./contextMenus";
import type { CanvasActions, MenuDeps, MenuEditor } from "./contextMenus";
import { COLLAPSE_NAME } from "./SaveModule";
import type { NameOptions } from "./SaveModule";
import { collapsibleOf, modulePluginsOf } from "./selection";
import type { PatchEdge, PatchNode, PatchNodeData } from "./toFlow";

/** What the editor was asked, in order: the name of the method and what it was given. */
type Ask = [method: string, ...args: unknown[]];

/** An editor that remembers what it is asked, and says that it was done (the id of a node it made is none: what a menu does with it is nothing). */
function recorder(): { editor: MenuEditor; asked: Ask[] } {
  const asked: Ask[] = [];
  const done =
    (method: string) =>
    (...args: unknown[]): Promise<boolean> => {
      asked.push([method, ...args]);
      return Promise.resolve(true);
    };
  const added =
    (method: string) =>
    (...args: unknown[]): Promise<NodeId | null> => {
      asked.push([method, ...args]);
      return Promise.resolve(null);
    };
  const editor: MenuEditor = {
    addInstrument: added("addInstrument"),
    addFreeEffect: added("addFreeEffect"),
    addModule: added("addModule"),
    addFreeModulator: added("addFreeModulator"),
    addSource: added("addSource"),
    addEffect: added("addEffect"),
    replaceInstrument: done("replaceInstrument"),
    clearInstrument: done("clearInstrument"),
    loadPreset: done("loadPreset"),
    openEditor: done("openEditor"),
    addAudioCable: done("addAudioCable"),
    addNotesCable: done("addNotesCable"),
    setMute: done("setMute"),
    setSolo: done("setSolo"),
    setModulatorEar: done("setModulatorEar"),
    setLimiter: done("setLimiter"),
    moveEffect: done("moveEffect"),
    removeNode: done("removeNode"),
    collapseIntoModule: added("collapseIntoModule"),
    ungroupModule: done("ungroupModule"),
    removeCable: done("removeCable"),
    setRoute: done("setRoute"),
    setFeed: done("setFeed"),
    saveModule: done("saveModule"),
    saveSelectionAsModule: done("saveSelectionAsModule"),
    undo: done("undo"),
    redo: done("redo"),
  };
  return { editor, asked };
}

const stateOf = (snapshot: Snapshot | null, extra: Partial<ClientState> = {}): ClientState => ({
  status: "open",
  problem: null,
  hello: null,
  snapshot,
  catalog: { scanning: false, entries: builtinCatalog },
  inputs: [],
  history: { undo: null, redo: null },
  reconnects: 0,
  probed: [],
  ...extra,
});

interface Rig {
  deps: MenuDeps;
  asked: Ask[];
  /** What the canvas was asked, in order. */
  canvasAsked: string[];
  /** The names that were asked for a module, and what asking the server for it would be. */
  naming: { label: string; save: (name: string) => Promise<boolean>; options: NameOptions | undefined }[];
}

/** The menus' dependencies over a session, with an editor and a canvas that remember what they are asked. */
function rig(snapshot: Snapshot | null, options: { state?: Partial<ClientState>; canvas?: Partial<CanvasActions>; at?: { x: number; y: number } } = {}): Rig {
  const { editor, asked } = recorder();
  const canvasAsked: string[] = [];
  const naming: Rig["naming"] = [];
  const say = (name: string) => () => {
    canvasAsked.push(name);
  };
  const canvas: CanvasActions = {
    inspecting: () => false,
    inspect: (node) => canvasAsked.push(`inspect ${String(node)}`),
    scoped: () => false,
    scope: (node) => canvasAsked.push(`scope ${String(node)}`),
    choosing: false,
    toggleChoosing: say("toggleChoosing"),
    chosen: [],
    held: { nodes: [], plugins: 0, left: 0 },
    collapsible: { nodes: [], lanes: 0, modulators: 0, brought: 0, left: 0, hides: new Set() },
    chooseAll: say("chooseAll"),
    chooseNone: say("chooseNone"),
    nameModule: (label, save, options) => naming.push({ label, save, options }),
    arrange: say("arrange"),
    canArrange: true,
    fit: say("fit"),
    toggleModule: (module) => canvasAsked.push(`toggleModule ${String(module)}`),
    ...options.canvas,
  };
  const state = stateOf(snapshot, options.state);
  return { deps: { state, editor, canvas, ...(options.at ? { at: options.at } : {}) }, asked, canvasAsked, naming };
}

const idsOf = (items: readonly ContextItem[]): string[] => items.map((item) => item.id);
const labelsOf = (items: readonly ContextItem[]): string[] => items.map((item) => item.label);

/** The item `id` of a list of items, or a failure that says what there is. */
function find(items: readonly ContextItem[], id: string): ContextItem {
  const found = items.find((item) => item.id === id);
  if (!found) throw new Error(`no item ${id} among ${idsOf(items).join(", ")}`);
  return found;
}

/** The items of the menu that the item `id` opens. */
function opened(items: readonly ContextItem[], id: string): readonly ContextItem[] {
  const menu = find(items, id).items;
  if (!menu) throw new Error(`${id} opens no menu`);
  return menu;
}

/** Chooses the item `id`, which must be there and run something. */
function choose(items: readonly ContextItem[], id: string): void {
  const item = find(items, id);
  if (!item.run) throw new Error(`${id} does nothing`);
  item.run();
}

const session = moduleGraphsFixture.snapshot;
const node = (name: string, kind?: Node["kind"]): Node => {
  const found = session.graph.nodes.find((candidate) => candidate.name === name && (kind === undefined || candidate.kind === kind));
  if (!found) throw new Error(`no node called ${name}`);
  return found;
};
const oscillator = node("Oscillator");
const filterOnBus = node("Filter", "effect");
const keys = node("Keys");

/** What the canvas knows of a node of the session, as much as a menu reads. */
function dataOf(found: Node, overrides: Partial<PatchNodeData> = {}): PatchNodeData {
  return {
    node: found.id,
    name: found.name,
    format: found.kind === "instrument" || found.kind === "effect" ? (found.plugin?.format ?? null) : null,
    knobs: [],
    modulated: [],
    targets: [],
    drivenSettings: [],
    cables: [],
    notes: found.kind === "instrument",
    groupCell: false,
    scope: false,
    source: found.kind === "source" ? found.source : null,
    heldIn: null,
    side: null,
    box: null,
    module: null,
    ...overrides,
  };
}

describe("the menu of the canvas", () => {
  test("adds an instrument, an effect, a modulator, a source of notes and a module, goes back and forward, chooses nodes, arranges them and shows them all, in that order", () => {
    const { deps } = rig(tracerSnapshot);
    const items = paneMenu(deps);
    expect(idsOf(items)).toEqual(["add-instrument", "add-effect", "add-modulator", "add-source", "add-module", "undo", "redo", "choose-all", "choosing", "arrange", "fit"]);
    expect(labelsOf(items)).toEqual(["Add an instrument", "Add an effect", "Add a modulator", "Add a source of notes", "Add a module", "Undo", "Redo", "Choose every node", "Choose nodes with a box", "Arrange the nodes", "Show every node"]);
    // (The rules that set the sections apart: adding, history, choosing, the view.)
    expect(items.filter((item) => item.rule === true).map((item) => item.id)).toEqual(["undo", "choose-all", "arrange"]);
  });

  test("adds an instrument of the catalog by choosing it, under the format of each, where the pointer was", () => {
    const { deps, asked } = rig(tracerSnapshot, { at: { x: 120, y: -40 } });
    const plugins = opened(paneMenu(deps), "add-instrument");
    const instruments = builtinCatalog.filter((entry) => entry.isInstrument).map((entry) => entry.id);
    expect(plugins.map((item) => item.id)).toEqual(instruments);
    expect(plugins.every((item) => item.group === "Built-in")).toBe(true);
    const first = plugins[0];
    if (!first?.run) throw new Error("an instrument is there to choose");
    first.run();
    expect(asked).toEqual([["addInstrument", first.id, { x: 120, y: -40 }]]);
  });

  test("adds an effect where the pointer was, and the effects are the plugins that can be one: a module that was saved is not among them (step 111)", () => {
    const module: CatalogEntry = { id: "module:Dub chain", name: "Dub chain", format: MODULE_FORMAT, isInstrument: false, params: 2, presets: 0 };
    const { deps, asked } = rig(tracerSnapshot, { at: { x: 10, y: 20 }, state: { catalog: { scanning: false, entries: [...builtinCatalog, module] } } });
    const effects = opened(paneMenu(deps), "add-effect");
    expect(effects.map((item) => item.id)).toEqual(builtinCatalog.filter((entry) => !entry.isInstrument).map((entry) => entry.id));
    expect(effects.every((item) => item.group === "Built-in")).toBe(true);
    const plugin = effects[0];
    if (!plugin?.run) throw new Error("a built-in effect is there to choose");
    plugin.run();
    expect(asked).toEqual([["addFreeEffect", plugin.id, { x: 10, y: 20 }]]);
  });

  test("adds a module where the pointer was, which is one box on the canvas, from a list of the modules that were saved, in the order that the server has them and under no heading", () => {
    const saved = (name: string): CatalogEntry => ({ id: `module:${name}`, name, format: MODULE_FORMAT, isInstrument: false, params: 2, presets: 0 });
    const { deps, asked } = rig(tracerSnapshot, { at: { x: 10, y: 20 }, state: { catalog: { scanning: false, entries: [...builtinCatalog, saved("Dub chain"), saved("Hall")] } } });
    const modules = opened(paneMenu(deps), "add-module");
    expect(modules.map((item) => [item.id, item.label, item.group])).toEqual([
      ["module:Dub chain", "Dub chain", undefined],
      ["module:Hall", "Hall", undefined],
    ]);
    choose(modules, "module:Hall");
    expect(asked).toEqual([["addModule", "module:Hall", { x: 10, y: 20 }]]);
  });

  test("has no module to add until one is saved, and says so in the item that is shut", () => {
    const { deps } = rig(tracerSnapshot);
    const item = find(paneMenu(deps), "add-module");
    expect(item.disabled).toBe(true);
    expect(item.items).toEqual([]);
    expect(item.title).toBe("No module is saved: choose nodes and save them as a module");
  });

  test("adds a modulator of every kind, hearing every note, where the pointer was", () => {
    const { deps, asked } = rig(tracerSnapshot, { at: { x: 1, y: 2 } });
    const kinds = opened(paneMenu(deps), "add-modulator");
    expect(labelsOf(kinds)).toEqual(["LFO", "Envelope", "Macro", "MIDI control", "Velocity", "Key", "Step sequencer"]);
    choose(kinds, "steps");
    expect(asked).toEqual([["addFreeModulator", "steps", null, { x: 1, y: 2 }]]);
  });

  test("a node is left to the layout when the pointer is not on the canvas", () => {
    const { deps, asked } = rig(tracerSnapshot);
    choose(opened(paneMenu(deps), "add-modulator"), "lfo");
    expect(asked).toEqual([["addFreeModulator", "lfo", null, undefined]]);
  });

  test("adds the keys, and the MIDI inputs that are open and that no device stands for, and says why it cannot when there is none", () => {
    const { deps, asked } = rig(tracerSnapshot, { at: { x: 5, y: 6 }, state: { inputs: ["Launchkey Mini"] } });
    const sources = opened(paneMenu(deps), "add-source");
    expect(labelsOf(sources)).toEqual(["Keys", "Launchkey Mini"]);
    choose(sources, "device:Launchkey Mini");
    choose(sources, "keys");
    expect(asked).toEqual([
      ["addSource", { type: "device", name: "Launchkey Mini" }, { x: 5, y: 6 }],
      ["addSource", { type: "keys" }, { x: 5, y: 6 }],
    ]);
    // The session of the fixture has the keys already, and no input is open.
    const none = find(paneMenu(rig(session).deps), "add-source");
    expect(none.disabled).toBe(true);
    expect(none.title).toBe("The keys and every input that is open are sources already");
  });

  test("says that nothing is installed, where nothing is: the catalog is not there before the scan has told it", () => {
    const { deps } = rig(tracerSnapshot, { state: { catalog: null } });
    const items = paneMenu(deps);
    for (const [id, title] of [
      ["add-instrument", "No instrument is installed"],
      ["add-effect", "No effect is installed"],
    ] as const) {
      expect(find(items, id).disabled).toBe(true);
      expect(find(items, id).title).toBe(title);
    }
    // (The modulators are the page's own list.)
    expect(find(items, "add-modulator").disabled).toBeUndefined();
  });

  test("goes back and forward in the words that the history has, and cannot where it has none", () => {
    const { deps, asked } = rig(tracerSnapshot, { state: { history: { undo: "add Reverb", redo: "set cutoff" } } });
    const items = paneMenu(deps);
    expect(find(items, "undo").label).toBe("Undo add Reverb");
    expect(find(items, "redo").label).toBe("Redo set cutoff");
    expect(find(items, "undo").disabled).toBe(false);
    choose(items, "undo");
    choose(items, "redo");
    expect(asked).toEqual([["undo"], ["redo"]]);
    const bare = paneMenu(rig(tracerSnapshot).deps);
    expect(find(bare, "undo")).toMatchObject({ label: "Undo", disabled: true, title: "Nothing to undo" });
    expect(find(bare, "redo")).toMatchObject({ label: "Redo", disabled: true, title: "Nothing to redo" });
  });

  test("chooses every node, turns choosing on or off with a check that says which, arranges the nodes and shows them all, unless they are arranged", () => {
    const { deps, canvasAsked } = rig(tracerSnapshot, { canvas: { choosing: true, canArrange: false } });
    const items = paneMenu(deps);
    expect(find(items, "choosing").checked).toBe(true);
    expect(find(items, "arrange")).toMatchObject({ disabled: true, title: "They are arranged" });
    for (const id of ["choose-all", "choosing", "arrange", "fit"]) find(items, id).run?.();
    expect(canvasAsked).toEqual(["chooseAll", "toggleChoosing", "arrange", "fit"]);
    const loose = paneMenu(rig(tracerSnapshot).deps);
    expect(find(loose, "choosing").checked).toBe(false);
    expect(find(loose, "arrange").disabled).toBe(false);
    expect(find(loose, "arrange").title).toBeUndefined();
  });

  test("offers to save what is chosen as a module and to choose none, above the rest, once something is chosen", () => {
    const held = modulePluginsOf(session.graph, [oscillator.id, node("Delay").id]);
    const { deps, naming, asked, canvasAsked } = rig(session, { canvas: { chosen: [oscillator.id, node("Delay").id], held } });
    const items = paneMenu(deps);
    expect(idsOf(items).slice(5, 11)).toEqual(["undo", "redo", "save-chosen", "collapse-chosen", "choose-none", "choose-all"]);
    expect(find(items, "save-chosen")).toMatchObject({ label: "Save the 2 chosen as a module…", disabled: false, rule: true });
    choose(items, "save-chosen");
    expect(naming.map((name) => name.label)).toEqual([SAVE_CHOSEN]);
    void naming[0]?.save("Voice");
    expect(asked).toEqual([["saveSelectionAsModule", held.nodes, "Voice"]]);
    choose(items, "choose-none");
    expect(canvasAsked).toEqual(["chooseNone"]);
  });
});

describe("the menu of what is chosen", () => {
  test("saves what a module can hold of it under the name that is typed, and none of it is chosen on request", () => {
    const chosen = [oscillator.id, keys.id];
    const held = modulePluginsOf(session.graph, chosen);
    expect(held).toEqual({ nodes: [oscillator.id], plugins: 1, left: 1 });
    const { deps, naming, asked, canvasAsked } = rig(session, { canvas: { chosen, held } });
    const items = selectionMenu(deps);
    expect(idsOf(items)).toEqual(["save-chosen", "collapse-chosen", "choose-none"]);
    expect(labelsOf(items)).toEqual(["Save the 1 chosen as a module…", "Collapse the chosen nodes into a module…", "Choose none"]);
    choose(items, "save-chosen");
    expect(naming).toHaveLength(1);
    void naming[0]?.save("Voice");
    expect(asked).toEqual([["saveSelectionAsModule", [oscillator.id], "Voice"]]);
    choose(items, "choose-none");
    expect(canvasAsked).toEqual(["chooseNone"]);
  });

  test("saves what is chosen when it has one plugin and a modulator, and counts the nodes that a module holds and not the plugins", () => {
    const chosen = [node("Delay").id, node("LFO").id];
    const held = modulePluginsOf(session.graph, chosen);
    expect(held).toMatchObject({ plugins: 1 });
    expect(held.nodes).toHaveLength(2);
    const { deps } = rig(session, { canvas: { chosen, held } });
    const save = find(selectionMenu(deps), "save-chosen");
    expect(save.label).toBe("Save the 2 chosen as a module…");
    expect(save.disabled).toBe(false);
    expect(save.title).toBeUndefined();
  });

  test("cannot save what has no effect or instrument in it, and says why: a modulator goes with the plugins that it drives", () => {
    const modulator = node("LFO");
    const chosen = [modulator.id];
    const { deps } = rig(session, { canvas: { chosen, held: modulePluginsOf(session.graph, chosen) } });
    const save = find(selectionMenu(deps), "save-chosen");
    expect(save.disabled).toBe(true);
    expect(save.title).toBe("A module needs an effect or an instrument");
    const held = modulePluginsOf(session.graph, [oscillator.id]);
    expect(find(selectionMenu(rig(session, { canvas: { chosen: [oscillator.id], held } }).deps), "save-chosen").title).toBeUndefined();
  });
});

describe("the menu of what is chosen, for collapsing it into one module where it is (step 109)", () => {
  const chosen = [oscillator.id, node("LFO").id];
  const collapsible = collapsibleOf(session.graph, chosen);

  test("says what it takes, and asks for the name of the box, which starts as Module, and then the server, for the nodes that a module can take and the place that the box has", () => {
    expect(collapsible).toMatchObject({ lanes: 1, modulators: 1 });
    const at = { x: 24, y: 48.5 };
    const { deps, naming, asked } = rig(session, { canvas: { chosen, collapsible, collapseAt: at } });
    const item = find(selectionMenu(deps), "collapse-chosen");
    expect(item).toMatchObject({ label: "Collapse the 1 lane and 1 modulator into a module…", disabled: false });
    expect(item.title).toBeUndefined();
    choose(selectionMenu(deps), "collapse-chosen");
    expect(naming.map((name) => name.label)).toEqual([COLLAPSE_CHOSEN]);
    expect(naming[0]?.options).toEqual(COLLAPSE_NAME);
    expect(asked).toEqual([]);
    void naming[0]?.save("Voice");
    expect(asked).toEqual([["collapseIntoModule", collapsible.nodes, "Voice", at]]);
  });

  test("resolves with whether the box was made: the field is done with it when it was, and stays when it was refused", async () => {
    const { deps, naming } = rig(session, { canvas: { chosen, collapsible } });
    choose(selectionMenu(deps), "collapse-chosen");
    // (The recorder's editor answers with no node, as a refusal does.)
    await expect(naming[0]?.save("Voice")).resolves.toBe(false);
  });

  test("cannot collapse what a module cannot take, or nothing, and says why", () => {
    const sources = [keys.id];
    const none = rig(session, { canvas: { chosen: sources, collapsible: collapsibleOf(session.graph, sources) } });
    expect(find(selectionMenu(none.deps), "collapse-chosen")).toMatchObject({
      label: "Collapse the chosen nodes into a module…",
      disabled: true,
      title: "None of what is chosen can be collapsed: a module holds lanes and modulators that are in no module",
    });
    expect(find(selectionMenu(rig(session).deps), "collapse-chosen").title).toBe("Choose the nodes to collapse into a module");
  });
});

describe("the menu of a lane", () => {
  const lane = (overrides: Partial<PatchNodeData> = {}) => dataOf(oscillator, overrides);

  test("changes its instrument, adds an effect and a modulator to it, sends its audio and takes notes into it, and has its mute and solo, a module of its chain and its removal", () => {
    const { deps } = rig(session);
    const items = nodeMenu(deps, oscillator.id, lane());
    expect(idsOf(items)).toEqual(["instrument", "parameters", "scope", "effect", "modulator", "send", "notes", "mute", "solo", "save-chain", "remove-instrument", "remove"]);
    expect(labelsOf(items)).toEqual([
      "Change the instrument",
      "Parameters…",
      "Scope",
      "Add an effect",
      "Add a modulator",
      "Send audio to",
      "Take notes from",
      "Mute",
      "Solo",
      "Save the chain as a module…",
      "Remove Oscillator only: its 3 effects stay in the lane",
      "Remove the lane of Oscillator for good: its 3 effects go with it, and 1 modulator that hears it stays, hearing every note",
    ]);
  });

  test("puts another instrument in the lane, an effect at the end of its chain, and a modulator that hears it", () => {
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, oscillator.id, lane());
    choose(opened(items, "instrument"), "builtin:osc");
    const effect = opened(items, "effect").find((item) => item.group === "Built-in");
    effect?.run?.();
    choose(opened(items, "modulator"), "envelope");
    expect(asked).toEqual([
      ["replaceInstrument", oscillator.id, "builtin:osc"],
      ["addEffect", oscillator.id, effect?.id],
      ["addFreeModulator", "envelope", oscillator.id],
    ]);
  });

  test("sends the audio of the lane to a place that takes it, and takes notes into it from a source", () => {
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, oscillator.id, lane());
    const places = opened(items, "send");
    expect(places.length).toBeGreaterThan(0);
    const place = places[0];
    place?.run?.();
    choose(opened(items, "notes"), String(keys.id));
    expect(asked).toEqual([
      ["addAudioCable", oscillator.id, Number(place?.id.replace(":side", "")), null, place?.id.endsWith(":side") === true],
      ["addNotesCable", keys.id, oscillator.id],
    ]);
  });

  test("turns its mute and its solo over, as the cell of its card does", () => {
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, oscillator.id, lane());
    expect(find(items, "mute").checked).toBe(false);
    choose(items, "mute");
    choose(items, "solo");
    expect(asked).toEqual([
      ["setMute", oscillator.id, true],
      ["setSolo", oscillator.id, true],
    ]);
    // (Each says what is its own: a lane that is muted is not soloed, and one that is soloed is not muted.)
    const flagged = (flags: { muted: boolean; solo: boolean }) => {
      const copy = structuredClone(session);
      for (const each of copy.graph.nodes) if (each.id === oscillator.id && each.kind === "instrument") Object.assign(each, flags);
      return rig(copy);
    };
    const muted = flagged({ muted: true, solo: false });
    const mutedItems = nodeMenu(muted.deps, oscillator.id, lane());
    expect([find(mutedItems, "mute").checked, find(mutedItems, "solo").checked]).toEqual([true, false]);
    choose(mutedItems, "mute");
    choose(mutedItems, "solo");
    expect(muted.asked).toEqual([
      ["setMute", oscillator.id, false],
      ["setSolo", oscillator.id, true],
    ]);
    const soloed = flagged({ muted: false, solo: true });
    const soloItems = nodeMenu(soloed.deps, oscillator.id, lane());
    expect([find(soloItems, "mute").checked, find(soloItems, "solo").checked]).toEqual([false, true]);
    choose(soloItems, "solo");
    expect(soloed.asked).toEqual([["setSolo", oscillator.id, false]]);
  });

  test("sets its items apart by a rule above the first of each kind: what is added to it, its mute and solo, and what takes something away", () => {
    const { deps } = rig(session);
    const ruled = (items: readonly ContextItem[]): string[] => items.filter((item) => item.rule === true).map((item) => item.id);
    expect(ruled(nodeMenu(deps, oscillator.id, lane()))).toEqual(["effect", "mute", "remove-instrument"]);
  });

  test("saves the chain of the lane as a module, in a field that says whose chain it is, and asks the server for the name that is typed", () => {
    const { deps, naming, asked } = rig(session);
    choose(nodeMenu(deps, oscillator.id, lane()), "save-chain");
    expect(naming.map((name) => name.label)).toEqual(["Save the chain of Oscillator as a module"]);
    void naming[0]?.save("Room");
    expect(asked).toEqual([["saveModule", oscillator.id, "Room"]]);
  });

  test("takes the instrument out alone, or the lane for good, which are two items that are told apart by the words of what goes and what stays", () => {
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, oscillator.id, lane());
    expect(find(items, "remove-instrument").danger).toBe(true);
    expect(find(items, "remove").danger).toBe(true);
    // (One rule, above the first of the two.)
    expect([find(items, "remove-instrument").rule, find(items, "remove").rule]).toEqual([true, false]);
    choose(items, "remove-instrument");
    choose(items, "remove");
    expect(asked).toEqual([
      ["clearInstrument", oscillator.id],
      ["removeNode", oscillator.id],
    ]);
  });

  test("opens the panel of its parameters and shows its scope, as switches that say which is on", () => {
    const { deps, canvasAsked } = rig(session, { canvas: { inspecting: (id) => id === oscillator.id, scoped: () => false } });
    const items = nodeMenu(deps, oscillator.id, lane());
    expect(idsOf(items).slice(0, 3)).toEqual(["instrument", "parameters", "scope"]);
    expect([find(items, "parameters").checked, find(items, "scope").checked]).toEqual([true, false]);
    choose(items, "parameters");
    choose(items, "scope");
    expect(canvasAsked).toEqual([`inspect ${String(oscillator.id)}`, `scope ${String(oscillator.id)}`]);
  });

  test("has no audio to send, no scope and no module of a chain where it is empty, and is asked for an instrument", () => {
    // (The session has two lanes that are empty: one holds effects, which are a chain to save, and the other holds nothing.)
    const empty = session.graph.nodes.find((each) => each.kind === "instrument" && each.plugin === null && !session.graph.nodes.some((child) => child.parent === each.id));
    if (!empty) throw new Error("the session has a lane that holds nothing");
    const { deps } = rig(session);
    const items = nodeMenu(deps, empty.id, dataOf(empty));
    expect(find(items, "instrument").label).toBe("Choose an instrument");
    expect(idsOf(items)).not.toContain("send");
    expect(idsOf(items)).not.toContain("scope");
    expect(idsOf(items)).not.toContain("save-chain");
    expect(idsOf(items)).not.toContain("remove-instrument");
  });

  test("offers no notes to a lane that takes none, and no way to take the last lane out", () => {
    const { deps } = rig(session);
    expect(idsOf(nodeMenu(deps, oscillator.id, lane({ notes: false })))).not.toContain("notes");
    const alone = rig(tracerSnapshot);
    const only = nodeNamed(tracerSnapshot, "Oscillator");
    const items = nodeMenu(alone.deps, only.id, dataOf(only));
    expect(idsOf(items)).toContain("remove-instrument");
    expect(idsOf(items)).not.toContain("remove");
  });

  test("shuts a menu that has nothing to choose from and says why: a lane has a cable of notes from every source", () => {
    // Every lane has a cable from the keys, which is the one source: nothing is left to take notes from.
    const withCable = structuredClone(session);
    for (const lane of withCable.graph.nodes.filter((each) => each.kind === "instrument")) {
      withCable.graph.edges.push({ kind: "events", id: 9000 + lane.id, from: { node: keys.id, port: { type: "eventsOut" } }, to: { node: lane.id, port: { type: "eventsIn" } } });
    }
    const items = nodeMenu(rig(withCable).deps, oscillator.id, lane());
    expect(find(items, "notes")).toMatchObject({ disabled: true, title: "No source of notes is left to take notes from" });
  });
});

describe("the menu of an effect", () => {
  const delay = node("Delay");
  const reverb = node("Reverb");

  test("moves it along its chain, loads one of its presets, opens its panel and its scope, sends its audio and takes it out", () => {
    const { deps } = rig(session);
    const items = nodeMenu(deps, delay.id, dataOf(delay));
    expect(idsOf(items)).toEqual(["earlier", "later", "presets", "parameters", "scope", "send", "remove"]);
    expect(labelsOf(items)).toEqual(["Move earlier in the chain", "Move later in the chain", "Load a preset", "Parameters…", "Scope", "Send audio to", "Remove Delay"]);
  });

  test("cannot move the first effect of a chain earlier or the last later, and moves the others one place", () => {
    const { deps, asked } = rig(session);
    // (The filter of the oscillator's lane is the first of its chain; the other filter is on the bus of the group.)
    const inChain = session.graph.nodes.find((each) => each.kind === "effect" && each.name === "Filter" && each.parent === oscillator.id);
    if (!inChain) throw new Error("the lane has a filter");
    const filterItems = nodeMenu(deps, inChain.id, dataOf(inChain));
    expect([find(filterItems, "earlier").disabled, find(filterItems, "later").disabled]).toEqual([true, false]);
    const reverbItems = nodeMenu(deps, reverb.id, dataOf(reverb));
    expect([find(reverbItems, "earlier").disabled, find(reverbItems, "later").disabled]).toEqual([false, true]);
    choose(nodeMenu(deps, delay.id, dataOf(delay)), "earlier");
    choose(nodeMenu(deps, delay.id, dataOf(delay)), "later");
    expect(asked).toEqual([
      ["moveEffect", delay.id, 0],
      ["moveEffect", delay.id, 2],
    ]);
  });

  test("loads a preset of its plugin, the one that was loaded last marked", () => {
    const withPreset = structuredClone(session);
    for (const each of withPreset.graph.nodes) if (each.id === delay.id && each.kind === "effect") each.plugin.currentPreset = each.plugin.presets[1]?.name ?? null;
    const { deps, asked } = rig(withPreset);
    const items = nodeMenu(deps, delay.id, dataOf(delay));
    const presets = opened(items, "presets");
    expect(presets).toHaveLength(7);
    expect(presets.map((preset) => preset.current === true)).toEqual([false, true, false, false, false, false, false]);
    presets[2]?.run?.();
    expect(asked).toEqual([["loadPreset", delay.id, presets[2]?.id]]);
  });

  test("has no presets to load in an effect that has none, and none for an effect on the bus of a group, whatever its plugin has: the audio thread loads a preset by a lane's slot", () => {
    const { deps } = rig(session);
    expect(idsOf(nodeMenu(deps, filterOnBus.id, dataOf(filterOnBus)))).not.toContain("presets");
    const onBus = structuredClone(session);
    for (const each of onBus.graph.nodes) if (each.id === delay.id) each.parent = node("Group 1").id;
    expect(idsOf(nodeMenu(rig(onBus).deps, delay.id, dataOf(delay)))).not.toContain("presets");
    expect(idsOf(nodeMenu(deps, delay.id, dataOf(delay)))).toContain("presets");
  });

  test("takes the lane with it when it is on its own in a lane that only holds it, as the button of its node does, and has the lane's own items when it is the first", () => {
    const { deps, asked } = rig(session);
    const math = node("Math");
    const holder = math.parent;
    if (holder === null) throw new Error("the math effect is in a lane");
    const alone = nodeMenu(deps, math.id, dataOf(math, { heldIn: { lane: holder, first: true, alone: true } }));
    expect(idsOf(alone)).toEqual(expect.arrayContaining(["modulator", "mute", "solo", "remove"]));
    choose(alone, "remove");
    choose(nodeMenu(deps, math.id, dataOf(math, { heldIn: { lane: holder, first: false, alone: false } })), "remove");
    expect(asked).toEqual([
      ["removeNode", holder],
      ["removeNode", math.id],
    ]);
    // The mute is the lane's.
    choose(alone, "mute");
    expect(asked.at(-1)).toEqual(["setMute", holder, true]);
    // Not the first of its lane's effects: it has none of the lane's own.
    const second = nodeMenu(deps, math.id, dataOf(math, { heldIn: { lane: holder, first: false, alone: false } }));
    expect(idsOf(second)).not.toContain("mute");
    expect(idsOf(second)).not.toContain("modulator");
  });

  test("sets its items apart by a rule above what it sends and above what it takes away, and above the lane's own items where it stands for the lane", () => {
    const { deps } = rig(session);
    const ruled = (items: readonly ContextItem[]): string[] => items.filter((item) => item.rule === true).map((item) => item.id);
    expect(ruled(nodeMenu(deps, delay.id, dataOf(delay)))).toEqual(["send", "remove"]);
    const math = node("Math");
    const holder = math.parent;
    if (holder === null) throw new Error("the math effect is in a lane");
    expect(ruled(nodeMenu(deps, math.id, dataOf(math, { heldIn: { lane: holder, first: true, alone: true } })))).toEqual(["send", "modulator", "remove"]);
  });

  test("says what an effect in a chain is called when it is taken out: its name", () => {
    const { deps } = rig(session);
    expect(find(nodeMenu(deps, reverb.id, dataOf(reverb, { name: "Reverb 2" })), "remove").label).toBe("Remove Reverb 2");
  });
});

describe("the menu of a modulator, a source, a group and the master", () => {
  test("a modulator chooses whose notes it hears, the choice now marked, and is taken out", () => {
    const lfo = node("LFO");
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, lfo.id, dataOf(lfo));
    expect(idsOf(items)).toEqual(["hears", "remove"]);
    const ears = opened(items, "hears");
    expect(ears[0]).toMatchObject({ id: "every-note", label: "Every note" });
    choose(ears, "every-note");
    choose(ears, String(oscillator.id));
    choose(items, "remove");
    expect(asked).toEqual([
      ["setModulatorEar", lfo.id, null],
      ["setModulatorEar", lfo.id, oscillator.id],
      ["removeNode", lfo.id],
    ]);
  });

  test("a source sends its notes to a lane that has no cable from it, and is taken out", () => {
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, keys.id, dataOf(keys));
    expect(idsOf(items)).toEqual(["notes", "remove"]);
    const lanes = opened(items, "notes");
    expect(lanes.map((lane) => lane.label)).toEqual(expect.arrayContaining(["Oscillator"]));
    choose(lanes, String(oscillator.id));
    choose(items, "remove");
    expect(asked).toEqual([
      ["addNotesCable", keys.id, oscillator.id],
      ["removeNode", keys.id],
    ]);
  });

  test("a group has the items of its header: a scope, an effect and a modulator that it hears, its audio, its mute and solo, and its removal", () => {
    const group = node("Group 1");
    const { deps, asked } = rig(session);
    const items = nodeMenu(deps, group.id, dataOf(group, { notes: false }));
    expect(idsOf(items)).toEqual(["scope", "effect", "modulator", "send", "mute", "solo", "remove"]);
    choose(opened(items, "modulator"), "lfo");
    choose(items, "remove");
    expect(asked).toEqual([
      ["addFreeModulator", "lfo", group.id],
      ["removeNode", group.id],
    ]);
    expect(find(items, "remove").label).toContain("Remove Group 1 for good");
  });

  test("a group sets its items apart by a rule above what is added to it, its mute and solo, and what takes it away", () => {
    const group = node("Group 1");
    const items = nodeMenu(rig(session).deps, group.id, dataOf(group, { notes: false }));
    expect(items.filter((item) => item.rule === true).map((item) => item.id)).toEqual(["effect", "mute", "remove"]);
  });

  test("the master puts its limiter in the sound or takes it out, a switch that says which", () => {
    const master = nodeNamed(tracerSnapshot, "Master");
    const { deps, asked } = rig(tracerSnapshot);
    const items = nodeMenu(deps, master.id, dataOf(master));
    expect(idsOf(items)).toEqual(["limiter"]);
    expect(find(items, "limiter").checked).toBe(true);
    choose(items, "limiter");
    expect(asked).toEqual([["setLimiter", false]]);
  });

  test("nothing for a node that is not there", () => {
    const { deps } = rig(session);
    expect(nodeMenu(deps, 987654, dataOf(oscillator))).toEqual([]);
    expect(nodeMenu(rig(null).deps, oscillator.id, dataOf(oscillator))).toEqual([]);
  });
});

describe("the menu of a cable", () => {
  const edge = (data: PatchEdge["data"]): PatchEdge => ({ id: "e", source: "1", target: "2", ...(data ? { data } : {}) });

  test("takes a cable out, in the words of the button at its middle", () => {
    const { deps, asked } = rig(session);
    const items = edgeMenu(deps, edge({ cable: 77, label: "LFO → Filter: cutoff" }));
    expect(labelsOf(items)).toEqual(["Remove cable LFO → Filter: cutoff"]);
    expect(items[0]?.danger).toBe(true);
    choose(items, "remove");
    expect(asked).toEqual([["removeCable", 77]]);
  });

  test("cuts a link of the audio path, as the button on it does: the way out of a lane, or the input of an effect", () => {
    const { deps, asked } = rig(session);
    choose(edgeMenu(deps, edge({ wayOut: 5, label: "Oscillator → Master" })), "remove");
    choose(edgeMenu(deps, edge({ feedOf: 6, label: "Oscillator → Filter" })), "remove");
    expect(asked).toEqual([
      ["setRoute", 5, "none"],
      ["setFeed", 6, "none"],
    ]);
  });

  test("says a cable when it has no words, and has nothing for a link that nothing can be done to", () => {
    const { deps } = rig(session);
    expect(labelsOf(edgeMenu(deps, edge({ cable: 3 })))).toEqual(["Remove cable a cable"]);
    expect(edgeMenu(deps, edge({ label: "Keys → Oscillator" }))).toEqual([]);
    expect(edgeMenu(deps, edge(undefined))).toEqual([]);
  });
});

describe("the menu that a right click on a cable opens", () => {
  const edge = (data: PatchEdge["data"]): PatchEdge => ({ id: "e", source: "1", target: "2", ...(data ? { data } : {}) });

  test("is the cable's own, named for the cable by the words of its button, and the canvas's where there is nothing to ask of it", () => {
    const { deps } = rig(session);
    const own = edgeSpec(deps, edge({ cable: 77, label: "LFO → Filter: cutoff" }));
    expect(own.label).toBe("LFO → Filter: cutoff");
    expect(labelsOf(own.items)).toEqual(["Remove cable LFO → Filter: cutoff"]);
    expect(edgeSpec(deps, edge({ cable: 3 })).label).toBe("a cable");
    // (The link by which a lane hears every source has no button: what is under it is the canvas.)
    const hears = edgeSpec(deps, edge({ label: "Keys → Oscillator" }));
    expect(hears.label).toBe("the canvas");
    expect(idsOf(hears.items)).toEqual(idsOf(paneMenu(deps)));
    expect(edgeSpec(deps, edge(undefined)).label).toBe("the canvas");
  });
});

describe("the menu that a right click on a node opens", () => {
  const onCanvas = (found: Node, selected: boolean): PatchNode => ({ id: String(found.id), type: found.kind, position: { x: 0, y: 0 }, data: dataOf(found), ...(selected ? { selected: true } : {}) });
  const chooseNodes = (...nodes: Node[]) => {
    const chosen = nodes.map((each) => each.id);
    return rig(session, { canvas: { chosen, held: modulePluginsOf(session.graph, chosen) } });
  };

  test("is the node's own, named for it, when nothing is chosen, and when it is the only node that is, which is not a choice of several", () => {
    const own = nodeSpec(rig(session).deps, onCanvas(oscillator, false));
    expect(own.label).toBe("Oscillator");
    expect(idsOf(own.items)).toContain("mute");
    const alone = chooseNodes(oscillator);
    const spec = nodeSpec(alone.deps, onCanvas(oscillator, true));
    expect(spec.label).toBe("Oscillator");
    expect(idsOf(spec.items)).toContain("mute");
  });

  test("is the menu of what is chosen, named for how many are, when the node is one of several that are", () => {
    // Three are chosen and a module can hold two of them (the keys are no plugin): the name counts what is chosen.
    const several = chooseNodes(oscillator, filterOnBus, keys);
    const spec = nodeSpec(several.deps, onCanvas(oscillator, true));
    expect(spec.label).toBe("the 3 chosen nodes");
    expect(idsOf(spec.items)).toEqual(["save-chosen", "collapse-chosen", "choose-none"]);
  });

  test("is the node's own for a node that is not one of those that are chosen, whatever is chosen", () => {
    const several = chooseNodes(oscillator, filterOnBus);
    const spec = nodeSpec(several.deps, onCanvas(keys, false));
    expect(spec.label).toBe("Keys");
    expect(idsOf(spec.items)).toEqual(["notes", "remove"]);
  });
});

describe("the menus of a module that is a box, and of the lanes of one that is open (step 108)", () => {
  const graph: Graph = { nodes: [lane(2, "Oscillator"), effect(3, 2, "Filter"), moduleNode(1, "Voice", [2]), theMaster], edges: [link(2, 3)] };
  const snapshot: Snapshot = { ...tracerSnapshot, graph };
  const at = (id: NodeId): Node => {
    const found = graph.nodes.find((candidate) => candidate.id === id);
    if (!found) throw new Error(`no node ${String(id)}`);
    return found;
  };
  const box = { node: 1, plugins: 2, modulators: 1, inputs: [], outputs: [], modIn: [], modOut: [], knobs: [] };

  test("a box opens, or is taken out with all that is in it, in the words that its × says", () => {
    const { deps, asked, canvasAsked } = rig(snapshot);
    const items = nodeMenu(deps, 1, dataOf(at(1), { box }));
    expect(idsOf(items)).toEqual(["open-module", "ungroup-module", "remove"]);
    expect(labelsOf(items)).toEqual(["Open Voice", "Ungroup Voice: keep the 2 plugins · 1 modulator, take the box away", "Remove Voice for good: the 2 plugins · 1 modulator in it go with it"]);
    expect(items[1]?.danger).toBeUndefined();
    expect(items[2]?.danger).toBe(true);
    choose(items, "open-module");
    expect(canvasAsked).toEqual(["toggleModule 1"]);
    expect(asked).toEqual([]);
    choose(items, "ungroup-module");
    expect(asked).toEqual([["ungroupModule", 1]]);
    choose(items, "remove");
    expect(asked).toEqual([["ungroupModule", 1], ["removeNode", 1]]);
  });

  test("a box that does not say what is in it is taken out, and ungrouped, in words that do not either", () => {
    const { deps } = rig(snapshot);
    expect(labelsOf(nodeMenu(deps, 1, dataOf(at(1))))).toEqual(["Open Voice", "Ungroup Voice", "Remove Voice for good"]);
  });

  test("a lane of a module that is open has the item that closes it first, with a rule under it, and a node that is in no open module has none", () => {
    const { deps, canvasAsked, asked } = rig(snapshot);
    const tagged = nodeMenu(deps, 2, dataOf(at(2), { module: { node: 1, name: "Voice" } }));
    expect(tagged[0]).toMatchObject({ id: "close-module", label: "Close Voice" });
    // (And the item that takes the module apart, which keeps its nodes: step 109.)
    expect(tagged[1]).toMatchObject({ id: "ungroup-module", label: "Ungroup Voice" });
    expect(tagged[2]?.rule).toBe(true);
    choose(tagged, "close-module");
    expect(canvasAsked).toEqual(["toggleModule 1"]);
    expect(asked).toEqual([]);
    choose(tagged, "ungroup-module");
    expect(asked).toEqual([["ungroupModule", 1]]);
    // (The rest of the menu is the lane's own, as it is for a lane that is in none.)
    const plain = nodeMenu(deps, 2, dataOf(at(2)));
    expect(idsOf(plain)).not.toContain("close-module");
    expect(idsOf(plain)).not.toContain("ungroup-module");
    expect(idsOf(tagged).slice(2)).toEqual(idsOf(plain));
  });

  test("the effect that stands for a lane that has no card closes the module as well", () => {
    const { deps } = rig(snapshot);
    const tagged = nodeMenu(deps, 3, dataOf(at(3), { module: { node: 1, name: "Voice" } }));
    expect(idsOf(tagged)[0]).toBe("close-module");
  });
});

describe("the menu of a plugin that has a window of its own (step 112)", () => {
  const shows = { protocol: 1, rev: 0, features: ["notes", "telemetry", "windows"], needsToken: false, libraries: [] };
  /** The session with the oscillator's plugin saying that it has a window of its own (the delay's does not). */
  const windowed = (): Snapshot => {
    const copy = structuredClone(session);
    for (const candidate of copy.graph.nodes) {
      if (candidate.name === "Oscillator" && candidate.kind === "instrument" && candidate.plugin) candidate.plugin.hasEditor = true;
    }
    return copy;
  };

  test("has it first, on a machine that can show a window, and asks the editor for the plugin's window", () => {
    const snapshot = windowed();
    const { deps, asked } = rig(snapshot, { state: { hello: shows } });
    const items = nodeMenu(deps, oscillator.id, dataOf(oscillator));
    expect(idsOf(items).slice(0, 3)).toEqual(["instrument", "window", "parameters"]);
    expect(find(items, "window")).toMatchObject({ label: "Open its own window", title: "On the machine that runs Tang" });
    choose(items, "window");
    expect(asked).toEqual([["openEditor", oscillator.id]]);
  });

  test("has none for a machine that cannot show one, and none for a plugin that says it has no window", () => {
    const noScreen = rig(windowed(), { state: { hello: { ...shows, features: ["notes", "telemetry"] } } });
    expect(idsOf(nodeMenu(noScreen.deps, oscillator.id, dataOf(oscillator)))).not.toContain("window");
    const noHello = rig(windowed());
    expect(idsOf(nodeMenu(noHello.deps, oscillator.id, dataOf(oscillator)))).not.toContain("window");
    const none = rig(session, { state: { hello: shows } });
    expect(idsOf(nodeMenu(none.deps, oscillator.id, dataOf(oscillator)))).not.toContain("window");
  });

  test("is in the menu of an effect too, before its panel", () => {
    const snapshot = structuredClone(session);
    const delay = snapshot.graph.nodes.find((candidate) => candidate.name === "Delay" && candidate.kind === "effect");
    if (delay?.kind !== "effect") throw new Error("no delay");
    delay.plugin.hasEditor = true;
    const { deps } = rig(snapshot, { state: { hello: shows } });
    const items = nodeMenu(deps, delay.id, dataOf(delay));
    const ids = idsOf(items);
    expect(ids).toContain("window");
    expect(ids.indexOf("window")).toBeLessThan(ids.indexOf("parameters"));
  });
});
