import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { cables, centre, flowBoxes, node, nodeBoxes, open, showAll, still } from "./canvas";
import { modulatorsIn, test as program } from "./own";

// The right-click menus of the canvas and the choosing of nodes with Shift held (docs/web-ui.md step 107), against the mock engine in the page: a box drawn with Shift held chooses at any time, nodes that are chosen are moved together,
// and a right click opens a menu where the pointer is, for what is under it.

/** The status line of the canvas's bar that says what is chosen. */
const status = (page: Page): Locator => page.getByRole("status").filter({ hasText: /chosen|Drag a box/ });

/** A place on the canvas that nothing covers: the first of a grid of places over it whose topmost element is the canvas itself. */
async function emptySpot(page: Page): Promise<{ x: number; y: number }> {
  const spot = await page.evaluate(() => {
    const pane = document.querySelector(".react-flow__pane");
    const box = pane?.getBoundingClientRect();
    if (!pane || !box) return null;
    for (let y = box.top + 30; y < box.bottom - 30; y += 24) {
      for (let x = box.left + 30; x < box.right - 30; x += 24) {
        if (document.elementFromPoint(x, y) === pane) return { x, y };
      }
    }
    return null;
  });
  if (!spot) throw new Error("nothing of the canvas is empty");
  return spot;
}

/** The edges of the box round the nodes called `names`, a little outside them. */
async function aroundNodes(page: Page, names: string[]): Promise<{ left: number; top: number; right: number; bottom: number }> {
  const boxes = (await nodeBoxes(page)).filter((box) => names.includes(box.name));
  expect(boxes).toHaveLength(names.length);
  return {
    left: Math.min(...boxes.map((box) => box.left)) - 12,
    top: Math.min(...boxes.map((box) => box.top)) - 12,
    right: Math.max(...boxes.map((box) => box.left + box.width)) + 12,
    bottom: Math.max(...boxes.map((box) => box.top + box.height)) + 12,
  };
}

/** Draw a box round the nodes called `names` with Shift held, from a corner of it to the other, which has no other node in it; `letGoOfShiftFirst` lets go of the key before the button, as a hand does. */
async function shiftBox(page: Page, names: string[], letGoOfShiftFirst = false): Promise<void> {
  const { left, top, right, bottom } = await aroundNodes(page, names);
  await page.keyboard.down("Shift");
  await page.mouse.move(left, top);
  await page.mouse.down();
  await page.mouse.move((left + right) / 2, (top + bottom) / 2, { steps: 6 });
  await page.mouse.move(right, bottom, { steps: 6 });
  if (letGoOfShiftFirst) await page.keyboard.up("Shift");
  await page.mouse.up();
  if (!letGoOfShiftFirst) await page.keyboard.up("Shift");
}

mock("a box drawn with Shift held chooses the nodes in it, with choosing off, and says so; a click on the canvas gives them up", async ({ page }) => {
  await open(page);
  await showAll(page);
  await expect(page.getByRole("button", { name: "Choose nodes to save as a module" })).toHaveAttribute("aria-pressed", "false");
  await expect(status(page)).toHaveCount(0);
  await shiftBox(page, ["Oscillator", "Filter"]);
  await expect(status(page).filter({ hasText: /^2 chosen$/ })).toBeVisible();
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(2);
  await expect(node(page, "Oscillator")).toHaveClass(/selected/);
  await expect(node(page, "Filter")).toHaveClass(/selected/);
  await expect(page.getByRole("button", { name: "Save the chosen nodes as a module" })).toBeEnabled();
  // (Choosing is not on: the button says so.)
  await expect(page.getByRole("button", { name: "Choose nodes to save as a module" })).toHaveAttribute("aria-pressed", "false");
  const spot = await emptySpot(page);
  await page.mouse.click(spot.x, spot.y);
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);
  await expect(status(page)).toHaveCount(0);
});

mock("a box that Shift is let go of before the button is is a box all the same, and a drag without Shift pans the canvas and leaves what is chosen as it was", async ({ page }) => {
  await open(page);
  await showAll(page);
  // (The box round the LFO and the oscillator has the filter in it too.)
  await shiftBox(page, ["LFO", "Oscillator", "Filter"], true);
  await expect(status(page).filter({ hasText: /^3 chosen$/ })).toBeVisible();
  // A click with no key held chooses nothing (and does not give up what is chosen: it is on a node, not on the canvas).
  await node(page, "Master").locator(".tang-node__header").click();
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(3);
  // A drag of the canvas with no key held is a pan, which chooses no node and gives none up: it is no click.
  const spot = await emptySpot(page);
  const before = await nodeBoxes(page);
  await page.mouse.move(spot.x, spot.y);
  await page.mouse.down();
  await page.mouse.move(spot.x + 30, spot.y + 20, { steps: 5 });
  await page.mouse.up();
  const after = await nodeBoxes(page);
  expect(after[0]?.left).not.toBe(before[0]?.left);
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(3);
  // (The canvas has not stopped panning, as it did when the box ended with Shift let go of: the nodes stand still.)
  await still(node(page, "Master"));
});

mock("a click with Shift held adds a node to what is chosen and takes it out of it, and a click on a knob chooses nothing", async ({ page }) => {
  await open(page);
  await showAll(page);
  await node(page, "Oscillator").locator(".tang-node__header").click({ modifiers: ["Shift"] });
  await expect(status(page).filter({ hasText: /^1 chosen$/ })).toBeVisible();
  await node(page, "Filter").locator(".tang-node__header").click({ modifiers: ["Shift"] });
  await expect(status(page).filter({ hasText: /^2 chosen$/ })).toBeVisible();
  await node(page, "Filter").locator(".tang-node__header").click({ modifiers: ["Shift"] });
  await expect(status(page).filter({ hasText: /^1 chosen$/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);
  // A click on a knob of a node, with no key held, is a click on the knob.
  await node(page, "Filter").getByRole("slider", { name: /cutoff/ }).first().click();
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);
  await expect(status(page)).toHaveCount(0);
});

mock("nodes that are chosen are moved together by taking one of them, each is kept where it is put, and the controls of a chosen node are still turned", async ({ page }) => {
  await open(page);
  await showAll(page);
  await shiftBox(page, ["Oscillator", "Filter"]);
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(2);
  const before = await nodeBoxes(page);
  // (By the name in its header: the middle of the header is a button.)
  const grip = await centre(node(page, "Oscillator").locator(".tang-node__name"));
  await page.mouse.move(grip.x, grip.y);
  await page.mouse.down();
  await page.mouse.move(grip.x + 30, grip.y + 20, { steps: 6 });
  await page.mouse.move(grip.x + 60, grip.y + 40, { steps: 6 });
  await page.mouse.up();
  await still(node(page, "Filter"));
  const after = await nodeBoxes(page);
  const moved = (name: string): { dx: number; dy: number } => {
    const was = before.find((box) => box.name === name);
    const now = after.find((box) => box.name === name);
    if (!was || !now) throw new Error(`no ${name}`);
    return { dx: Math.round(now.left - was.left), dy: Math.round(now.top - was.top) };
  };
  // (The drag begins a few pixels into the move, so the nodes are a little short of the 60 and 40 that the pointer went: what counts is that both are moved, and by the same.)
  expect(moved("Oscillator").dx).toBeGreaterThan(45);
  expect(moved("Oscillator").dy).toBeGreaterThan(30);
  expect(moved("Filter")).toEqual(moved("Oscillator"));
  // A node that was not chosen, and that the others did not come near, is where it was.
  expect(moved("LFO")).toEqual({ dx: 0, dy: 0 });
  // (Still chosen, and the nodes are the session's: every one that was moved was asked for.)
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(2);
  // The frame round what is chosen is a picture: a control of a node in it is still turned.
  const cutoff = node(page, "Filter").getByRole("slider", { name: /cutoff/ }).first();
  const value = await cutoff.getAttribute("aria-valuenow");
  await cutoff.focus();
  await page.keyboard.press("ArrowUp");
  await expect(cutoff).not.toHaveAttribute("aria-valuenow", value ?? "");
  // (The key was the knob's: the chosen nodes did not move by it.)
  const keyed = await nodeBoxes(page);
  expect(keyed.find((box) => box.name === "Filter")?.left).toBe(after.find((box) => box.name === "Filter")?.left);
});

mock("with choosing on, the middle button pans the canvas, a drag with the main button from a node moves the node and does not pan, and a right click is the menu's, as with choosing off", async ({ page }) => {
  await open(page);
  await showAll(page);
  await page.getByRole("button", { name: "Choose nodes to save as a module" }).click();
  const viewport = page.locator(".react-flow__viewport");
  const view = (): Promise<string> => viewport.evaluate((element) => (element as HTMLElement).style.transform);
  const before = await view();
  const spot = await emptySpot(page);
  // The middle button pans.
  await page.mouse.move(spot.x, spot.y);
  await page.mouse.down({ button: "middle" });
  await page.mouse.move(spot.x + 60, spot.y + 40, { steps: 5 });
  await page.mouse.up({ button: "middle" });
  await expect.poll(view).not.toBe(before);
  // A drag with the main button from a node moves the node (a card is draggable by its own word), and does not pan.
  const moved = await view();
  const place = await placeOf(page, "Oscillator");
  const grip = await node(page, "Oscillator").locator(".tang-node__name").boundingBox();
  if (!grip) throw new Error("the oscillator has no name to take it by");
  await page.mouse.move(grip.x + 6, grip.y + grip.height / 2);
  await page.mouse.down();
  await page.mouse.move(grip.x + 6 + 70, grip.y + grip.height / 2 + 50, { steps: 6 });
  await page.mouse.up();
  await expect.poll(async () => (await placeOf(page, "Oscillator")).y).toBeGreaterThan(place.y + 30);
  expect((await placeOf(page, "Oscillator")).x).toBeGreaterThan(place.x + 30);
  expect(await view()).toBe(moved);
  // The right button is the menu's: nothing pans, and the menu opens.
  const again = await emptySpot(page);
  await page.mouse.click(again.x, again.y, { button: "right" });
  await expect(page.getByRole("menu", { name: "the canvas" })).toBeVisible();
  expect(await view()).toBe(moved);
});

mock("a right click on the canvas opens its menu where the pointer is, and what is added from it is put there", async ({ page }) => {
  await open(page);
  await showAll(page);
  const spot = await emptySpot(page);
  await page.mouse.click(spot.x, spot.y, { button: "right" });
  const menu = page.getByRole("menu", { name: "the canvas" });
  await expect(menu).toBeVisible();
  const box = await menu.boundingBox();
  expect(Math.abs((box?.x ?? 0) - spot.x)).toBeLessThan(3);
  expect(Math.abs((box?.y ?? 0) - spot.y)).toBeLessThan(3);
  await menu.getByRole("menuitem", { name: "Add a modulator" }).click();
  await page.getByRole("menu", { name: "Add a modulator" }).getByRole("menuitem", { name: "LFO" }).click();
  await expect(page.getByRole("menu")).toHaveCount(0);
  // The new LFO is called LFO 2 (the tracer has an LFO), and its top left corner is where the pointer was.
  const added = node(page, "LFO 2");
  await expect(added).toBeVisible();
  await still(added);
  const placed = await added.boundingBox();
  expect(Math.abs((placed?.x ?? 0) - spot.x)).toBeLessThan(2);
  expect(Math.abs((placed?.y ?? 0) - spot.y)).toBeLessThan(2);
  // (Two steps: the node, and its place. Undo puts the node where the layout had it, and then takes it out.)
  await page.getByRole("button", { name: "Undo move a node" }).click();
  await expect(page.getByRole("button", { name: /^Undo add an LFO/ })).toBeVisible();
});

mock("the menu of the canvas is used with the keyboard, and Escape closes it without leaving what is chosen", async ({ page }) => {
  await open(page);
  await showAll(page);
  await shiftBox(page, ["Oscillator"]);
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(1);
  const spot = await emptySpot(page);
  await page.mouse.click(spot.x, spot.y, { button: "right" });
  const menu = page.getByRole("menu", { name: "the canvas" });
  await expect(menu).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(menu.getByRole("menuitem", { name: "Add an instrument" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("ArrowRight");
  const kinds = page.getByRole("menu", { name: "Add a modulator" });
  await expect(kinds.getByRole("menuitem", { name: "LFO" })).toBeFocused();
  await page.keyboard.press("ArrowLeft");
  await expect(menu.getByRole("menuitem", { name: "Add a modulator" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  // Escape was the menu's: what was chosen is chosen still.
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);
});

mock("a right click on a node opens its menu: mute it from there, and the node is dimmed; and a right click on a cable takes it out", async ({ page }) => {
  await open(page);
  await showAll(page);
  const oscillator = node(page, "Oscillator");
  const grip = await centre(oscillator.locator(".tang-node__name"));
  await page.mouse.click(grip.x, grip.y, { button: "right" });
  const menu = page.getByRole("menu", { name: "Oscillator" });
  await expect(menu).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Send audio to" })).toBeVisible();
  await menu.getByRole("menuitemcheckbox", { name: "Mute" }).click();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(oscillator.locator(".tang-node")).toHaveClass(/tang-node--unheard/);
  await expect(page.getByRole("button", { name: "Mute Oscillator" })).toHaveAttribute("aria-pressed", "true");
  // (The menu is made when it is opened: the switch says it is on.)
  await page.mouse.click(grip.x, grip.y, { button: "right" });
  await expect(page.getByRole("menuitemcheckbox", { name: "Mute" })).toHaveAttribute("aria-checked", "true");
  await page.keyboard.press("Escape");

  // A cable of modulation: its menu takes it out.
  const wired = await cables(page).count();
  expect(wired).toBeGreaterThan(0);
  const cable = cables(page).first();
  const point = await cable.evaluate((path) => {
    const svg = path as unknown as SVGPathElement;
    const length = svg.getTotalLength();
    const at = svg.getPointAtLength(length / 2);
    const matrix = svg.getScreenCTM();
    return matrix ? { x: at.x * matrix.a + at.y * matrix.c + matrix.e, y: at.x * matrix.b + at.y * matrix.d + matrix.f } : null;
  });
  if (!point) throw new Error("the cable has a place");
  await page.mouse.click(point.x, point.y, { button: "right" });
  const remove = page.getByRole("menuitem", { name: /^Remove cable / });
  await expect(remove).toBeVisible();
  await remove.click();
  await expect(cables(page)).toHaveCount(wired - 1);
});

mock("what is chosen is saved as a module from the menu of the canvas, in a field that opens where the pointer was", async ({ page }) => {
  await open(page);
  await showAll(page);
  await shiftBox(page, ["Oscillator", "Filter"]);
  await expect(status(page).filter({ hasText: /^2 chosen$/ })).toBeVisible();
  const spot = await emptySpot(page);
  await page.mouse.click(spot.x, spot.y, { button: "right" });
  await page.getByRole("menuitem", { name: "Save the 2 chosen as a module…" }).click();
  const dialog = page.getByRole("dialog", { name: "Save the chosen nodes as a module" });
  const field = dialog.getByRole("textbox", { name: "Module name" });
  await expect(field).toBeFocused();
  const where = await dialog.boundingBox();
  expect(Math.abs((where?.x ?? 0) - spot.x)).toBeLessThan(3);
  await field.fill("Pair");
  await field.press("Enter");
  await expect(page.getByText('Saved the module "Pair": + Module adds it')).toBeVisible();
  await expect(dialog).toHaveCount(0);
  await page.getByRole("button", { name: "Add a module", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: "Pair", exact: true })).toBeVisible();
});

/** Where the canvas draws the node called `name`, in its own pixels (the view pans and zooms, so the screen's are not the node's). */
async function placeOf(page: Page, name: string): Promise<{ x: number; y: number }> {
  const found = (await flowBoxes(page)).find((box) => box.name === name);
  if (!found) throw new Error(`the canvas has no node called ${name}`);
  return { x: found.x, y: found.y };
}

/** The places that a saved file has for its nodes, in the order of the file (`ui = { x = 1, y = 2 }`). */
const placesIn = (saved: string): { x: number; y: number }[] => [...saved.matchAll(/^ui = \{ x = (-?[\d.]+), y = (-?[\d.]+) \}$/gm)].map((found) => ({ x: Number(found[1]), y: Number(found[2]) }));

program("what is added from the menu of the canvas is put where the pointer was in the real program, which keeps the place in its file and gives it to a page that is opened again", async ({ page, own }) => {
  program.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await showAll(page);
  const spot = await emptySpot(page);
  await page.mouse.click(spot.x, spot.y, { button: "right" });
  await page.getByRole("menu", { name: "the canvas" }).getByRole("menuitem", { name: "Add a modulator" }).click();
  await page.getByRole("menu", { name: "Add a modulator" }).getByRole("menuitem", { name: "LFO" }).click();
  const added = node(page, "LFO 2");
  await expect(added).toBeVisible();
  await still(added);
  const box = await added.boundingBox();
  expect(Math.abs((box?.x ?? 0) - spot.x)).toBeLessThan(2);
  expect(Math.abs((box?.y ?? 0) - spot.y)).toBeLessThan(2);
  // The node is added, and then put: the second step is the program's, and is the one that Undo takes back first.
  await expect(page.getByRole("button", { name: "Undo move a node" })).toBeVisible();
  const placed = await placeOf(page, "LFO 2");

  // The file has the place on the new modulator, the second of the file (the tracer's own comes first and has none).
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  const tables = modulatorsIn(saved);
  expect(tables).toHaveLength(2);
  expect(tables[0]).not.toMatch(/^ui = /m);
  const [place] = placesIn(tables[1] ?? "");
  expect(place, saved).toBeDefined();
  expect(Math.abs((place?.x ?? 0) - placed.x)).toBeLessThan(0.06);
  expect(Math.abs((place?.y ?? 0) - placed.y)).toBeLessThan(0.06);

  // A page that is opened again draws it where it was put, and not where the layout of the rows would have it.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect.poll(async () => placeOf(page, "LFO 2")).toEqual(placed);

  // Undo takes the move back (the node is where the layout has it, and is still there), and then the add.
  await page.getByRole("button", { name: "Undo move a node" }).click();
  await expect(page.getByRole("button", { name: /^Undo add an LFO/ })).toBeVisible();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect.poll(async () => placeOf(page, "LFO 2")).not.toEqual(placed);
  await page.getByRole("button", { name: /^Undo add an LFO/ }).click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
});

program("nodes chosen with a box drawn with Shift held are moved together by a drag of one of them, and the real program keeps the place of each, in its file and for a page that is opened again", async ({ page, own }) => {
  program.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await showAll(page);
  await shiftBox(page, ["Oscillator", "Filter"]);
  await expect(status(page).filter({ hasText: /^2 chosen$/ })).toBeVisible();
  const before = { oscillator: await placeOf(page, "Oscillator"), filter: await placeOf(page, "Filter") };

  // One of them is taken by its name and moved down and to the right; the other goes with it.
  const grip = await node(page, "Oscillator").locator(".tang-node__name").boundingBox();
  if (!grip) throw new Error("the oscillator has no name to take it by");
  await page.mouse.move(grip.x + 6, grip.y + grip.height / 2);
  await page.mouse.down();
  await page.mouse.move(grip.x + 6 + 60, grip.y + grip.height / 2 + 70, { steps: 8 });
  await page.mouse.up();
  await expect.poll(async () => (await placeOf(page, "Oscillator")).y, { message: "the oscillator is where it was put down" }).toBeGreaterThan(before.oscillator.y + 20);
  await expect(page.getByRole("button", { name: "Undo move a node" })).toBeVisible();
  const after = { oscillator: await placeOf(page, "Oscillator"), filter: await placeOf(page, "Filter") };
  const by = { x: after.oscillator.x - before.oscillator.x, y: after.oscillator.y - before.oscillator.y };
  expect(by.x).toBeGreaterThan(20);
  expect(Math.abs(after.filter.x - before.filter.x - by.x)).toBeLessThan(0.6);
  expect(Math.abs(after.filter.y - before.filter.y - by.y)).toBeLessThan(0.6);
  // They are chosen still.
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(2);

  // The file has the two places and no other, and a page that is opened again draws both where they were put.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const places = placesIn(readFileSync(own.file, "utf8"));
  expect(places).toHaveLength(2);
  const keptAt = (wanted: { x: number; y: number }): boolean => places.some((place) => Math.abs(place.x - wanted.x) < 0.06 && Math.abs(place.y - wanted.y) < 0.06);
  expect(keptAt(after.oscillator)).toBe(true);
  expect(keptAt(after.filter)).toBe(true);
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect.poll(async () => placeOf(page, "Oscillator")).toEqual(after.oscillator);
  await expect.poll(async () => placeOf(page, "Filter")).toEqual(after.filter);
});
