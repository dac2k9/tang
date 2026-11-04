// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Intent } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The right-click menus of the canvas and the choosing of nodes with the keys (step 107): a menu opens where the pointer is, for what is under it, and its items ask the server; a node is chosen by a click with Shift held, and by
// nothing less (a click on a knob is a click on its node); what is chosen is what the menus of the canvas and of a node say they save as a module.

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

async function rigged() {
  return rig(
    <>
      <PatchView />
      <Board />
    </>,
    { snapshot: tracerSnapshot },
  );
}

const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);

/** The node on the canvas that has this name in its header, as the element React Flow puts it in. */
async function nodeOnCanvas(name: string): Promise<HTMLElement> {
  const names = await screen.findAllByText(name, { selector: ".tang-node__name" });
  const found = names[0]?.closest<HTMLElement>(".react-flow__node");
  if (!found) throw new Error(`no node called ${name}`);
  return found;
}

/** The canvas itself, the pane that nodes and cables are over. */
async function pane(): Promise<HTMLElement> {
  await nodeOnCanvas("Oscillator");
  const found = document.querySelector<HTMLElement>(".react-flow__pane");
  if (!found) throw new Error("the canvas has no pane");
  return found;
}

/** A click on `target` while Shift is held, which React Flow reads from the keys and not from the click. */
function shiftClick(target: HTMLElement): void {
  fireEvent.keyDown(document.body, { key: "Shift", code: "ShiftLeft" });
  fireEvent.click(target, { shiftKey: true });
  fireEvent.keyUp(document.body, { key: "Shift", code: "ShiftLeft" });
}

const chosenNodes = (): number => document.querySelectorAll(".react-flow__node.selected").length;

const menu = (name: string): HTMLElement => screen.getByRole("menu", { name });
const item = (name: string): HTMLElement => screen.getByRole("menuitem", { name });
/** The item that is a switch. */
const toggle = (name: string): HTMLElement => screen.getByRole("menuitemcheckbox", { name });

describe("the menu of the canvas", () => {
  test("opens at a right click on the canvas, in the page, named for what it is of, and the browser's own menu is kept away", async () => {
    await rigged();
    const canvas = await pane();
    expect(screen.queryByRole("menu")).toBeNull();
    // (`fireEvent` says whether the event was left alone.)
    expect(fireEvent.contextMenu(canvas, { clientX: 300, clientY: 200 })).toBe(false);
    const opened = menu("the canvas");
    expect(opened.parentElement).toBe(document.body);
    expect(within(opened).getAllByRole("menuitem").slice(0, 4).map((entry) => entry.textContent)).toEqual(["Add an instrument", "Add an effect", "Add a modulator", "Add a source of notes"]);
  });

  test("is the menu of the canvas for a cable that has no menu of its own", async () => {
    await rigged();
    await pane();
    const links = Array.from(document.querySelectorAll<HTMLElement>(".react-flow__edge"));
    const [link] = links;
    if (!link) throw new Error("the tracer has links");
    // (The tracer's audio links have a button, and so a menu; what has no menu is the link by which a lane hears every source, and the tracer has no source.)
    fireEvent.contextMenu(link);
    expect(screen.getAllByRole("menu")).toHaveLength(1);
  });

  test("adds a modulator where the pointer was: it asks for the modulator, and then for it to be put there, and the page does not move to show it", async () => {
    const { requests, client } = await rigged();
    fireEvent.contextMenu(await pane(), { clientX: 420, clientY: 310 });
    fireEvent.click(item("Add a modulator"));
    fireEvent.click(screen.getByRole("menuitem", { name: "LFO" }));
    await waitFor(() => {
      expect(asked(requests).map((intent) => intent.type)).toEqual(["addFreeModulator", "setNodeUi"]);
    });
    const [added, placed] = asked(requests);
    expect(added).toEqual({ type: "addFreeModulator", kind: "lfo", hears: null });
    if (placed?.type !== "setNodeUi" || placed.ui === null) throw new Error("the node is put somewhere");
    expect(Number.isFinite(placed.ui.x) && Number.isFinite(placed.ui.y)).toBe(true);
    // The server keeps the place to a tenth of a pixel, which is where the node is drawn now.
    const node = client.state.snapshot?.graph.nodes.find((each) => each.id === placed.node);
    expect(node?.kind).toBe("modulator");
    expect(node?.ui?.x).toBeCloseTo(placed.ui.x, 1);
    expect(node?.ui?.y).toBeCloseTo(placed.ui.y, 1);
    // The menu is gone.
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("puts an instrument and an effect where the pointer was, the same way, and a menu that is shut says why", async () => {
    const { requests } = await rigged();
    fireEvent.contextMenu(await pane(), { clientX: 100, clientY: 100 });
    fireEvent.click(item("Add an effect"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
    await waitFor(() => {
      expect(asked(requests).map((intent) => intent.type)).toEqual(["addFreeEffect", "setNodeUi"]);
    });
    fireEvent.contextMenu(await pane(), { clientX: 200, clientY: 100 });
    // (The keys are a source of notes that the tracer has not got: there is something to add.)
    expect(screen.getByRole("menuitem", { name: "Add a source of notes" }).getAttribute("aria-disabled")).toBeNull();
  });

  test("puts what is added where the pointer was, across as it was across and down as it was down", async () => {
    const { requests } = await rigged();
    for (const [x, y] of [
      [300, 200],
      [500, 200],
      [300, 350],
    ] as const) {
      fireEvent.contextMenu(await pane(), { clientX: x, clientY: y });
      fireEvent.click(item("Add a modulator"));
      fireEvent.click(screen.getByRole("menuitem", { name: "LFO" }));
      await waitFor(() => {
        expect(asked(requests).filter((intent) => intent.type === "setNodeUi").length).toBe(requests().filter((request) => (request.intent as Intent).type === "addFreeModulator").length);
      });
    }
    const places = asked(requests).flatMap((intent) => (intent.type === "setNodeUi" && intent.ui !== null ? [intent.ui] : []));
    expect(places).toHaveLength(3);
    const [first, across, down] = places;
    if (!first || !across || !down) throw new Error("three nodes were put");
    expect(across.x - first.x).toBeGreaterThan(0);
    expect(across.y).toBeCloseTo(first.y, 5);
    expect(down.y - first.y).toBeGreaterThan(0);
    expect(down.x).toBeCloseTo(first.x, 5);
  });

  test("goes back and forward in the words of the history, and the menu of a session that has nothing to undo says so", async () => {
    const { requests } = await rigged();
    fireEvent.contextMenu(await pane());
    expect(item("Undo").getAttribute("aria-disabled")).toBe("true");
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    // Something to undo: a modulator is added from the menu, which is a step.
    fireEvent.contextMenu(await pane());
    fireEvent.click(item("Add a modulator"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Envelope" }));
    await waitFor(() => {
      expect(asked(requests).map((intent) => intent.type)).toContain("setNodeUi");
    });
    fireEvent.contextMenu(await pane());
    await waitFor(() => {
      expect(screen.getByRole("menuitem", { name: /^Undo / }).getAttribute("aria-disabled")).toBeNull();
    });
    fireEvent.click(screen.getByRole("menuitem", { name: /^Undo / }));
    await waitFor(() => {
      expect(asked(requests).at(-1)?.type).toBe("undo");
    });
  });

  test("closes with Escape, which leaves choosing as it was, and with a press anywhere else", async () => {
    await rigged();
    fireEvent.click(await screen.findByRole("button", { name: "Choose nodes to save as a module" }));
    fireEvent.contextMenu(await pane());
    expect(toggle("Choose nodes with a box").getAttribute("aria-checked")).toBe("true");
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(screen.getByRole("button", { name: "Choose nodes to save as a module" }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.contextMenu(await pane());
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("turns choosing on from the menu, and chooses every node", async () => {
    await rigged();
    fireEvent.contextMenu(await pane());
    fireEvent.click(toggle("Choose nodes with a box"));
    expect(screen.getByRole("button", { name: "Choose nodes to save as a module" }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.contextMenu(await pane());
    fireEvent.click(item("Choose every node"));
    await waitFor(() => {
      expect(chosenNodes()).toBe(tracerSnapshot.graph.nodes.length);
    });
  });

  test("arranges the nodes and shows them all, which the canvas's own button does too, and cannot arrange them again while they are arranged", async () => {
    await rigged();
    fireEvent.contextMenu(await pane());
    expect(item("Arrange the nodes").getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(item("Arrange the nodes"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Arrange the nodes again" })).toHaveProperty("disabled", true);
    });
    fireEvent.contextMenu(await pane());
    expect(item("Arrange the nodes").getAttribute("aria-disabled")).toBe("true");
    expect(item("Arrange the nodes").getAttribute("title")).toBe("They are arranged");
    expect(() => {
      fireEvent.click(item("Show every node"));
    }).not.toThrow();
    expect(screen.queryByRole("menu")).toBeNull();
  });
});

describe("the menu of a node", () => {
  test("opens at a right click on a node, named for it, with what its header's buttons are and its mute and solo, and asks the server for what is chosen from it", async () => {
    const { requests } = await rigged();
    expect(fireEvent.contextMenu(await nodeOnCanvas("Oscillator"), { clientX: 50, clientY: 60 })).toBe(false);
    const opened = menu("Oscillator");
    const labels = Array.from(opened.querySelectorAll('[role^="menuitem"]'), (entry) => entry.textContent);
    expect(labels).toEqual(expect.arrayContaining(["Change the instrument", "Add an effect", "Add a modulator", "Send audio to", "Mute", "Solo"]));
    fireEvent.click(within(opened).getByRole("menuitemcheckbox", { name: "Mute" }));
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "setMute", node: oscillator.id, muted: true }]);
    });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("is for the node that is under the pointer, whatever is chosen: a node that is not one of those that are chosen has its own", async () => {
    await rigged();
    shiftClick(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(chosenNodes()).toBe(1);
    });
    shiftClick(await nodeOnCanvas("Filter"));
    await waitFor(() => {
      expect(chosenNodes()).toBe(2);
    });
    // A node that is chosen, with others: the menu is of what is chosen.
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    expect(screen.getByRole("menu", { name: "the 2 chosen nodes" })).toBeTruthy();
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    // A node that is not: its own.
    fireEvent.contextMenu(await nodeOnCanvas("Master"));
    expect(screen.getByRole("menu", { name: "Master" })).toBeTruthy();
  });

  test("is not opened over a field that takes text, which has a menu of its own that cuts and pastes", async () => {
    await rigged();
    const node = await nodeOnCanvas("Oscillator");
    const field = document.createElement("input");
    field.type = "text";
    node.append(field);
    // (Not taken: the event is left alone and no menu opens.)
    expect(fireEvent.contextMenu(field)).toBe(true);
    expect(screen.queryByRole("menu")).toBeNull();
    field.remove();
  });

  test("is made from the session as it is when it opens: a menu opened again has what has changed", async () => {
    const { requests } = await rigged();
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    fireEvent.click(within(menu("Oscillator")).getByRole("menuitemcheckbox", { name: "Mute" }));
    await waitFor(() => {
      expect(asked(requests)).toHaveLength(1);
    });
    await waitFor(() => {
      expect(document.querySelector(".tang-node--unheard")).not.toBeNull();
    });
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    expect(within(menu("Oscillator")).getByRole("menuitemcheckbox", { name: "Mute" }).getAttribute("aria-checked")).toBe("true");
  });
});

describe("the menu of a node, for what is the canvas's own", () => {
  test("opens the panel of the parameters and shows the scope, as switches that say which is on when the menu is opened again, and shuts them the same way", async () => {
    await rigged();
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    expect(toggle("Parameters…").getAttribute("aria-checked")).toBe("false");
    expect(toggle("Scope").getAttribute("aria-checked")).toBe("false");
    fireEvent.click(toggle("Parameters…"));
    expect(await screen.findByRole("complementary", { name: "Parameters of Oscillator" })).toBeTruthy();
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    expect(toggle("Parameters…").getAttribute("aria-checked")).toBe("true");
    expect(toggle("Scope").getAttribute("aria-checked")).toBe("false");
    fireEvent.click(toggle("Scope"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Scope of Oscillator" }).getAttribute("aria-expanded")).toBe("true");
    });
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    expect(toggle("Scope").getAttribute("aria-checked")).toBe("true");
    expect(toggle("Parameters…").getAttribute("aria-checked")).toBe("true");
    fireEvent.click(toggle("Parameters…"));
    await waitFor(() => {
      expect(screen.queryByRole("complementary", { name: "Parameters of Oscillator" })).toBeNull();
    });
    fireEvent.contextMenu(await nodeOnCanvas("Oscillator"));
    fireEvent.click(toggle("Scope"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Scope of Oscillator" }).getAttribute("aria-expanded")).toBe("false");
    });
  });
});

describe("the menu of a cable", () => {
  test("takes it out, as the button at its middle does", async () => {
    const { requests } = await rigged();
    await pane();
    const cable = document.querySelector(".tang-cable--modulation")?.closest<HTMLElement>(".react-flow__edge");
    if (!cable) throw new Error("the tracer has a cable of modulation");
    fireEvent.contextMenu(cable);
    const remove = screen.getByRole("menuitem", { name: /^Remove cable / });
    fireEvent.click(remove);
    await waitFor(() => {
      expect(asked(requests).map((intent) => intent.type)).toEqual(["removeEdge"]);
    });
  });
});

describe("choosing nodes with the keys", () => {
  test("chooses a node by a click with Shift held, with choosing off, and says so; a plain click chooses nothing", async () => {
    await rigged();
    expect(screen.queryByRole("status")).toBeNull();
    fireEvent.click(await nodeOnCanvas("Oscillator"));
    expect(chosenNodes()).toBe(0);
    shiftClick(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("1 chosen");
    });
    expect(chosenNodes()).toBe(1);
    expect(screen.getByRole("button", { name: "Save the chosen nodes as a module" })).toHaveProperty("disabled", false);
    // (Choosing is not on: the button says so, and a click without a key chooses no more.)
    expect(screen.getByRole("button", { name: "Choose nodes to save as a module" }).getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(await nodeOnCanvas("Filter"));
    expect(chosenNodes()).toBe(1);
  });

  test("adds the nodes that are clicked with Shift held, one after the other, and a click on the canvas gives them all up", async () => {
    await rigged();
    shiftClick(await nodeOnCanvas("Oscillator"));
    fireEvent.keyDown(document.body, { key: "Shift", code: "ShiftLeft" });
    fireEvent.click(await nodeOnCanvas("Filter"), { shiftKey: true });
    fireEvent.keyUp(document.body, { key: "Shift", code: "ShiftLeft" });
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("2 chosen");
    });
    fireEvent.click(await pane());
    await waitFor(() => {
      expect(chosenNodes()).toBe(0);
    });
    expect(screen.queryByRole("status")).toBeNull();
  });

  test("leaves choosing at Escape when nothing is chosen", async () => {
    await rigged();
    const button = await screen.findByRole("button", { name: "Choose nodes to save as a module" });
    fireEvent.click(button);
    expect(button.getAttribute("aria-pressed")).toBe("true");
    fireEvent.keyDown(document.body, { key: "Escape" });
    await waitFor(() => {
      expect(button.getAttribute("aria-pressed")).toBe("false");
    });
  });

  test("gives up what is chosen at Escape, with choosing off, unless something that is open took the key", async () => {
    await rigged();
    shiftClick(await nodeOnCanvas("Oscillator"));
    await waitFor(() => {
      expect(chosenNodes()).toBe(1);
    });
    const take = (event: KeyboardEvent): void => {
      if (event.key === "Escape") event.preventDefault();
    };
    window.addEventListener("keydown", take, true);
    fireEvent.keyDown(document.body, { key: "Escape" });
    window.removeEventListener("keydown", take, true);
    expect(chosenNodes()).toBe(1);
    fireEvent.keyDown(document.body, { key: "Escape" });
    await waitFor(() => {
      expect(chosenNodes()).toBe(0);
    });
  });

  test("saves what is chosen as a module from the menu of the canvas, in the field that opens where the pointer was, under the name that is typed", async () => {
    const { requests } = await rigged();
    shiftClick(await nodeOnCanvas("Oscillator"));
    fireEvent.keyDown(document.body, { key: "Shift", code: "ShiftLeft" });
    fireEvent.click(await nodeOnCanvas("Filter"), { shiftKey: true });
    fireEvent.keyUp(document.body, { key: "Shift", code: "ShiftLeft" });
    await waitFor(() => {
      expect(chosenNodes()).toBe(2);
    });
    fireEvent.contextMenu(await pane(), { clientX: 250, clientY: 150 });
    fireEvent.click(item("Save the 2 chosen as a module…"));
    const dialog = await screen.findByRole("dialog", { name: "Save the chosen nodes as a module" });
    expect(dialog.style.left).toBe("250px");
    expect(dialog.style.top).toBe("150px");
    const field = within(dialog).getByRole("textbox", { name: "Module name" });
    expect(document.activeElement).toBe(field);
    fireEvent.change(field, { target: { value: "Voice" } });
    fireEvent.submit(dialog);
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "saveSelectionAsModule", nodes: [oscillator.id, filter.id], name: "Voice" }]);
    });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    // (What is chosen stays so: another name is a menu away.)
    expect(chosenNodes()).toBe(2);
  });

  test("collapses what is chosen from the menu of the canvas, in the field that opens where the pointer was, with the button and the name that the bar's field has (step 109)", async () => {
    const { requests } = await rigged();
    shiftClick(await nodeOnCanvas("Oscillator"));
    fireEvent.keyDown(document.body, { key: "Shift", code: "ShiftLeft" });
    fireEvent.click(await nodeOnCanvas("Filter"), { shiftKey: true });
    fireEvent.keyUp(document.body, { key: "Shift", code: "ShiftLeft" });
    await waitFor(() => {
      expect(chosenNodes()).toBe(2);
    });
    fireEvent.contextMenu(await pane(), { clientX: 250, clientY: 150 });
    fireEvent.click(item("Collapse the 1 lane into a module…"));
    const dialog = await screen.findByRole("dialog", { name: "Collapse the chosen nodes into a module" });
    expect(dialog.style.left).toBe("250px");
    expect(dialog.style.top).toBe("150px");
    const field = within(dialog).getByRole<HTMLInputElement>("textbox", { name: "Module name" });
    expect(document.activeElement).toBe(field);
    // (The field of the save is the same field: what is different is what the button says, what the field starts as and what it says it is for.)
    expect(field.value).toBe("Module");
    expect(field.title).toBe("What the box is called: it is the module's own name, and not a file");
    expect(within(dialog).getByRole("button", { name: "Collapse" })).toBeTruthy();
    expect(within(dialog).queryByRole("button", { name: "Save" })).toBeNull();
    fireEvent.change(field, { target: { value: "Voice" } });
    fireEvent.submit(dialog);
    await waitFor(() => {
      expect(asked(requests)).toMatchObject([{ type: "collapseIntoModule", nodes: [oscillator.id, filter.id], name: "Voice", ui: { x: expect.any(Number) as number, y: expect.any(Number) as number } }]);
    });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
  });

  test("saves what is chosen from the menu of a node that is one of them", async () => {
    const { requests } = await rigged();
    shiftClick(await nodeOnCanvas("Oscillator"));
    fireEvent.keyDown(document.body, { key: "Shift", code: "ShiftLeft" });
    fireEvent.click(await nodeOnCanvas("Filter"), { shiftKey: true });
    fireEvent.keyUp(document.body, { key: "Shift", code: "ShiftLeft" });
    await waitFor(() => {
      expect(chosenNodes()).toBe(2);
    });
    fireEvent.contextMenu(await nodeOnCanvas("Filter"));
    fireEvent.click(item("Save the 2 chosen as a module…"));
    const field = await screen.findByRole("textbox", { name: "Module name" });
    fireEvent.change(field, { target: { value: "Pair" } });
    const form = field.closest("form");
    if (!form) throw new Error("the field is in a form");
    fireEvent.submit(form);
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "saveSelectionAsModule", nodes: [oscillator.id, filter.id], name: "Pair" }]);
    });
  });

  test("chooses none from the menu, and says that a module needs an effect or an instrument where what is chosen has none", async () => {
    await rigged();
    const modulator = tracerSnapshot.graph.nodes.find((node) => node.kind === "modulator");
    if (!modulator) throw new Error("the tracer has a modulator");
    shiftClick(await nodeOnCanvas(modulator.name));
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("1 chosen, and a module needs an effect or an instrument");
    });
    fireEvent.contextMenu(await pane());
    const save = item("Save the 1 chosen as a module…");
    expect(save.getAttribute("aria-disabled")).toBe("true");
    expect(save.getAttribute("title")).toBe("A module needs an effect or an instrument");
    fireEvent.click(item("Choose none"));
    await waitFor(() => {
      expect(chosenNodes()).toBe(0);
    });
  });
});
