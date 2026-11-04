import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { cablePoints, cables, drag, node, open, port, showAll, still } from "./canvas";
import { addFree, boxRound, choose as chooseNodes, names } from "./modules";
import { modulatorsIn, test } from "./own";

// A macro that has a name and a choice of whether the box of its module shows its knob (docs/web-ui.md D110; the user's "expose a macro-knob as well, maybe thats the most common usecase", "let a Macro have a name and an
// "expose" option/flip, that will expose the macro-knob when used in a module", "the expose can be default on as well"). The first test is against the mock engine in the page, the last against the real program, whose file keeps the
// name and the flag.

/** The node `plugin` has a parameter called `name` on, a macro made from the parameter's menu (it is made with a cable to it, and is called `Macro`). */
async function macroOn(plugin: Locator, name: string): Promise<void> {
  const page = plugin.page();
  await plugin.locator(".tang-cell").filter({ has: page.locator(`[aria-label="${name}"]`) }).hover();
  await plugin.getByRole("button", { name: `Modulate ${name}`, exact: true }).click();
  await page.getByRole("menuitem", { name: "New macro", exact: true }).click();
  await expect(node(page, "Macro")).toBeVisible();
}

/** The macro `called` is given the name `name`, in the field that its node's ✎ turns the name into. */
async function rename(page: Page, called: string, name: string): Promise<void> {
  await node(page, called).getByRole("button", { name: `Rename ${called}` }).click();
  // (The field takes the place of the name in the header, so that the node is not found by its name while it is there.)
  const field = page.getByRole("textbox", { name: `Name of ${called}` });
  await expect(field).toBeFocused();
  await field.fill(name);
  await field.press("Enter");
  await expect(node(page, name)).toBeVisible();
}

/** A box is drawn round the nodes called `chosen` (every node that it touches is chosen, so it is all of the nodes there are but the master) and what is chosen is collapsed under `name`. */
async function collapse(page: Page, chosen: string[], name: string): Promise<void> {
  await chooseNodes(page).click();
  await boxRound(page, chosen);
  await expect(page.getByRole("status").filter({ hasText: new RegExp(`^${String(chosen.length)} chosen`) })).toBeVisible();
  await page.getByRole("button", { name: "Collapse the chosen nodes into a module" }).click();
  const field = page.getByRole("dialog", { name: "Collapse the chosen nodes into a module" }).getByRole("textbox", { name: "Module name" });
  await field.fill(name);
  await field.press("Enter");
  await expect(node(page, name)).toBeVisible();
  await chooseNodes(page).click();
}

/** The group of knobs that the box called `module` shows, and the knob called `name` in it. */
const knobs = (box: Locator, module: string): Locator => box.getByRole("group", { name: `Knobs of ${module}` });
const knob = (box: Locator, module: string, name: string): Locator => knobs(box, module).getByRole("slider", { name, exact: true });

/** The port of the knob called `name` on the box `module` (step 111): the handle in the knob's cell, which is the macro's own output, where a cable is started. */
const knobPort = (box: Locator, module: string, name: string): Locator =>
  knobs(box, module)
    .locator(".tang-cell")
    .filter({ has: box.page().locator(`[aria-label="${name}"]`) })
    .locator(".react-flow__handle");

/** How tall a box is on the page, in the pixels of the layout (the canvas may be zoomed: this is not), and how tall the layout says it is: a header, a row for each port, the knobs, the line that says what is in it, the room under it. */
async function heights(box: Locator, shownKnobs: number): Promise<{ drawn: number; said: number }> {
  const module = box.locator(".tang-node--module");
  const drawn = await module.evaluate((element) => (element as HTMLElement).offsetHeight);
  const rows = await module.locator(".tang-box__row").count();
  const grid = shownKnobs === 0 ? 0 : Math.ceil(shownKnobs / 4) * 108 + 12;
  return { drawn, said: 44 + rows * 28 + grid + 20 + 16 };
}

mock("a macro that is called something has its knob on the box of the module it is collapsed into, which turns the macro, shows or hides it as its choice says, and is as tall as the layout says", async ({ page }) => {
  await open(page);
  await macroOn(node(page, "Filter"), "resonance");
  await showAll(page);
  await rename(page, "Macro", "Brightness");
  // Its node has the choice of whether a box shows its knob, on to begin with.
  await expect(node(page, "Brightness").getByRole("combobox", { name: "Expose" })).toHaveValue("1");
  await expect(node(page, "Brightness").locator(".tang-node__badge")).toHaveText("Macro");
  await expect(page.getByRole("button", { name: "Undo give Macro the name" }).or(page.getByRole("button", { name: "Undo rename a macro" }))).toBeVisible();

  await collapse(page, ["LFO", "Brightness", "Oscillator", "Filter"], "Voice");
  const box = node(page, "Voice");
  await expect(node(page, "Brightness")).toHaveCount(0);
  // The knob is on the box, called by the macro's name, at the macro's value; the box is as tall as the layout says.
  await expect(knob(box, "Voice", "Brightness")).toBeVisible();
  await expect(knob(box, "Voice", "Brightness")).toHaveAttribute("aria-valuenow", "0");
  const withKnob = await heights(box, 1);
  expect(withKnob.drawn).toBe(withKnob.said);

  // Turned on the box, it is the macro's value: the macro's own node says the same when the box is opened.
  await knob(box, "Voice", "Brightness").focus();
  for (let i = 0; i < 25; i += 1) await page.keyboard.press("ArrowUp");
  // (Twenty-five steps of a hundredth add up to a quarter and a little: the text of the value is what a person reads.)
  await expect(knob(box, "Voice", "Brightness")).toHaveAttribute("aria-valuetext", "0.25");
  await page.getByRole("button", { name: "Open Voice" }).click();
  await expect(knobs(box, "Voice")).toHaveCount(0);
  await expect(node(page, "Brightness").getByRole("slider", { name: "Value", exact: true })).toHaveAttribute("aria-valuetext", "0.25");

  // Told not to show its knob, the macro is not on the box, which is shorter by the grid it had.
  await node(page, "Brightness").getByRole("combobox", { name: "Expose" }).selectOption("off");
  await expect(node(page, "Brightness").getByRole("combobox", { name: "Expose" })).toHaveValue("0");
  await expect(page.getByRole("button", { name: "Undo hide a macro from its box" })).toBeVisible();
  await page.getByRole("button", { name: "Close Voice" }).click();
  await expect(box).toBeVisible();
  await expect(knobs(box, "Voice")).toHaveCount(0);
  const without = await heights(box, 0);
  expect(without.drawn).toBe(without.said);
  expect(withKnob.drawn - without.drawn).toBe(120);

  // Undo shows it again, at the value that it has, and Redo hides it.
  await page.getByRole("button", { name: "Undo hide a macro from its box" }).click();
  await expect(knob(box, "Voice", "Brightness")).toHaveAttribute("aria-valuetext", "0.25");
  expect((await heights(box, 1)).drawn).toBe(withKnob.drawn);
  await page.getByRole("button", { name: "Redo hide a macro from its box" }).click();
  await expect(knobs(box, "Voice")).toHaveCount(0);
});

mock("a macro that is renamed after it is in a box is the name of the knob on the box, and two macros in a box have a knob each, in a row of four", async ({ page }) => {
  await open(page);
  await macroOn(node(page, "Filter"), "resonance");
  await showAll(page);
  await rename(page, "Macro", "Brightness");
  await macroOn(node(page, "Filter"), "cutoff");
  await collapse(page, ["LFO", "Brightness", "Macro", "Oscillator", "Filter"], "Voice");
  const box = node(page, "Voice");
  await expect(knobs(box, "Voice").getByRole("slider")).toHaveCount(2);
  await expect(knobs(box, "Voice").getByRole("slider").first()).toHaveAttribute("aria-label", "Brightness");
  await expect(knobs(box, "Voice").getByRole("slider").last()).toHaveAttribute("aria-label", "Macro");
  // The grid is one row of four cells, as a plugin's controls are: both knobs are in one row.
  const [first, second] = await Promise.all([knob(box, "Voice", "Brightness").boundingBox(), knob(box, "Voice", "Macro").boundingBox()]);
  expect(Math.abs((first?.y ?? 0) - (second?.y ?? 100))).toBeLessThan(2);
  expect((await heights(box, 2)).drawn).toBe((await heights(box, 2)).said);
  // Renamed in the open module, the name is the knob's on the box again.
  await page.getByRole("button", { name: "Open Voice" }).click();
  await rename(page, "Macro", "Cutoff");
  await page.getByRole("button", { name: "Close Voice" }).click();
  await expect(knob(box, "Voice", "Cutoff")).toBeVisible();
  await expect(knobs(box, "Voice").getByRole("slider")).toHaveCount(2);
});

mock("the knob of a macro on a box has a port under it that a cable is dragged from, to a parameter of a node outside the module: the cable is drawn from the knob, is the macro's own, and goes with Undo; the knob's menu draws one as well, for a keyboard", async ({ page }) => {
  await open(page);
  await macroOn(node(page, "Filter"), "resonance");
  await showAll(page);
  await rename(page, "Macro", "Brightness");
  await collapse(page, ["LFO", "Brightness", "Oscillator", "Filter"], "Voice");
  await addFree(page, "Reverb", "Reverb");
  await showAll(page);
  const box = node(page, "Voice");
  const reverb = node(page, "Reverb");
  // The macro's cable to the filter, and the LFO's, are inside the box and are not drawn: the box has the one port, which is the knob's.
  await expect(cables(page)).toHaveCount(0);
  await expect(box.locator('.react-flow__handle[data-handleid^="modOut@"]')).toHaveCount(1);
  const dot = knobPort(box, "Voice", "Brightness");
  await expect(dot).toBeVisible();
  // The port is under the knob, at the middle of it, and the box is as tall as the layout says with it there (a handle takes no room).
  const [dial, at] = await Promise.all([knob(box, "Voice", "Brightness").boundingBox(), dot.boundingBox()]);
  if (!dial || !at) throw new Error("the knob and its port are on the page");
  expect(at.y).toBeGreaterThan(dial.y + dial.height);
  expect(Math.abs(at.x + at.width / 2 - (dial.x + dial.width / 2))).toBeLessThan(8);
  const shown = await heights(box, 1);
  expect(shown.drawn).toBe(shown.said);

  // Dragged to a parameter of the reverb, which is outside the module: one cable, which the page says is the macro's, and which starts at the knob's port.
  await drag(page, dot, port(reverb, "decay"));
  await expect(cables(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Reverb: decay" })).toBeVisible();
  const start = (await cablePoints(page, 4))[0];
  const now = await dot.boundingBox();
  if (!start || !now) throw new Error("the cable and the port are on the page");
  expect(Math.hypot(start.x - (now.x + now.width / 2), start.y - (now.y + now.height))).toBeLessThan(8);
  // (It is a step of its own that Undo takes away and Redo gives back.)
  await page.getByRole("button", { name: "Undo draw a cable" }).click();
  await expect(cables(page)).toHaveCount(0);
  await page.getByRole("button", { name: "Redo draw a cable" }).click();
  await expect(cables(page)).toHaveCount(1);

  // The menu in the corner of the knob lists what the macro can still drive, plugin by plugin (the parameter that it drives already is not among them), and a choice is a cable from the same port.
  await knob(box, "Voice", "Brightness").hover();
  await box.getByRole("button", { name: "Add a cable from Brightness" }).click();
  const menu = page.getByRole("menu", { name: "Add a cable from Brightness" });
  await expect(menu.getByRole("group", { name: "Reverb" }).getByRole("menuitem", { name: "decay", exact: true })).toHaveCount(0);
  await menu.getByRole("group", { name: "Reverb" }).getByRole("menuitem", { name: "size", exact: true }).click();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Reverb: size" })).toBeVisible();
  await expect(box.getByRole("button", { name: "Add a cable from Brightness" })).toBeFocused();

  // Open, the macro is its node, with its cables; closed again, they are drawn from the knob once more.
  await page.getByRole("button", { name: "Open Voice" }).click();
  await expect(node(page, "Brightness")).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Reverb: decay" })).toBeVisible();
  await page.getByRole("button", { name: "Close Voice" }).click();
  await expect(knobPort(node(page, "Voice"), "Voice", "Brightness")).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
});

mock("a knob that comes to a box that is as tall as it was has its port found: Undo shows the knob that was hidden, and a cable is dragged from its port at once", async ({ page }) => {
  await open(page);
  await macroOn(node(page, "Filter"), "resonance");
  await showAll(page);
  await rename(page, "Macro", "Brightness");
  await macroOn(node(page, "Filter"), "cutoff");
  await collapse(page, ["LFO", "Brightness", "Macro", "Oscillator", "Filter"], "Voice");
  await addFree(page, "Reverb", "Reverb");
  await showAll(page);
  const box = node(page, "Voice");
  // One of the two is told, in the open module, not to show its knob: the box that it is closed again to has the one.
  await page.getByRole("button", { name: "Open Voice" }).click();
  await node(page, "Macro").getByRole("combobox", { name: "Expose" }).selectOption("off");
  await expect(page.getByRole("button", { name: "Undo hide a macro from its box" })).toBeVisible();
  await page.getByRole("button", { name: "Close Voice" }).click();
  await expect(knobs(box, "Voice").getByRole("slider")).toHaveCount(1);
  await expect(knobPort(box, "Voice", "Macro")).toHaveCount(0);
  const alone = await heights(box, 1);
  expect(alone.drawn).toBe(alone.said);
  // Undo shows it again, beside the other: the box is as tall as it was (a row of four), so that its size says nothing of the port that came, and the port is found all the same (the canvas hands React Flow a node that it has not measured).
  await page.getByRole("button", { name: "Undo hide a macro from its box" }).click();
  await expect(knobs(box, "Voice").getByRole("slider")).toHaveCount(2);
  const both = await heights(box, 2);
  expect(both.drawn).toBe(alone.drawn);
  await showAll(page);
  await still(box);
  await drag(page, knobPort(box, "Voice", "Macro"), port(node(page, "Reverb"), "decay"));
  await expect(cables(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Depth of Macro → Reverb: decay" })).toBeVisible();
});

test("the real program keeps the name and the flag of a macro in its file, a page that is opened again draws the knob on the box, or does not, and Undo and the box's knob go through the program's history", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
  await macroOn(node(page, "Filter"), "resonance");
  await showAll(page);
  await rename(page, "Macro", "Brightness");
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // The name is in the file, and the flag is not: a macro that shows its knob is the file it always was.
  let parts = modulatorsIn(await save());
  expect(parts).toHaveLength(2);
  expect(parts[1]).toContain('name = "Brightness"');
  expect(parts[1]).not.toContain("expose");

  // Told not to show its knob, the flag is in the file too.
  await node(page, "Brightness").getByRole("combobox", { name: "Expose" }).selectOption("off");
  await expect(node(page, "Brightness").getByRole("combobox", { name: "Expose" })).toHaveValue("0");
  parts = modulatorsIn(await save());
  expect(parts[1]).toContain('name = "Brightness"');
  expect(parts[1]).toContain("expose = false");

  // In a box it has no knob, and a page that is opened again does not draw one, with the module as it was.
  await collapse(page, ["LFO", "Brightness", "Oscillator", "Filter"], "Voice");
  await expect(node(page, "Voice")).toBeVisible();
  await expect(knobs(node(page, "Voice"), "Voice")).toHaveCount(0);
  await save();
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Voice")).toBeVisible();
  await expect(knobs(node(page, "Voice"), "Voice")).toHaveCount(0);

  // Shown again (in the open module, where the macro has its node), the knob is on the box, with the macro's name, and the file has no flag again.
  await page.getByRole("button", { name: "Open Voice" }).click();
  await node(page, "Brightness").getByRole("combobox", { name: "Expose" }).selectOption("on");
  await expect(node(page, "Brightness").getByRole("combobox", { name: "Expose" })).toHaveValue("1");
  await page.getByRole("button", { name: "Close Voice" }).click();
  const box = node(page, "Voice");
  await expect(knob(box, "Voice", "Brightness")).toBeVisible();
  parts = modulatorsIn(await save());
  expect(parts[1]).toContain('name = "Brightness"');
  expect(parts[1]).not.toContain("expose");
  const drawn = await heights(box, 1);
  expect(drawn.drawn).toBe(drawn.said);

  // The knob on the box turns the macro in the program, which the file keeps; and Undo of the name is the program's own step.
  await knob(box, "Voice", "Brightness").focus();
  await page.keyboard.press("End");
  await expect(knob(box, "Voice", "Brightness")).toHaveAttribute("aria-valuenow", "1");
  parts = modulatorsIn(await save());
  expect(parts[1]).toContain("value = 1.0");
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(knob(node(page, "Voice"), "Voice", "Brightness")).toHaveAttribute("aria-valuenow", "1");
  expect(await names(page)).toContain("Voice");
});

test("the real program keeps a cable from the macro on a box to a parameter outside the module: it is the macro's cable in the file, a page that is opened again draws it from the knob of the box, and when the module is taken apart the macro is a node again with both its cables", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
  await macroOn(node(page, "Filter"), "resonance");
  await showAll(page);
  await rename(page, "Macro", "Brightness");
  await collapse(page, ["LFO", "Brightness", "Oscillator", "Filter"], "Voice");
  await addFree(page, "Reverb", "Reverb");
  await showAll(page);
  const box = node(page, "Voice");
  await drag(page, knobPort(box, "Voice", "Brightness"), port(node(page, "Reverb"), "decay"));
  await expect(cables(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Reverb: decay" })).toBeVisible();

  // The file has the cable among the macro's: the one to the filter that is in the module, and the one to the reverb that is not (a lane of its own that has no instrument).
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const parts = modulatorsIn(readFileSync(own.file, "utf8"));
  expect(parts).toHaveLength(2);
  expect(parts[1]).toContain('name = "Brightness"');
  expect(parts[1]).toContain('param = "resonance"\nto = "instrument[0].effect[0]"');
  expect(parts[1]).toContain('param = "decay"\nto = "instrument[1].effect[0]"');

  // A page that is opened again has the module as a box, and the cable is drawn from the knob of the macro on it.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Voice")).toBeVisible();
  await expect(cables(page)).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Reverb: decay" })).toBeVisible();
  const dot = knobPort(node(page, "Voice"), "Voice", "Brightness");
  const [start, at] = await Promise.all([cablePoints(page, 4).then((points) => points[0]), dot.boundingBox()]);
  if (!start || !at) throw new Error("the cable and the port are on the page");
  expect(Math.hypot(start.x - (at.x + at.width / 2), start.y - (at.y + at.height))).toBeLessThan(8);

  // Taken apart, the box is gone and the macro is a node again with both its cables: the one to the reverb is drawn from the node.
  await node(page, "Voice").getByRole("button", { name: "Remove Voice" }).click();
  await page.getByRole("menuitem", { name: /^Ungroup Voice/ }).click();
  await expect(node(page, "Voice")).toHaveCount(0);
  await expect(node(page, "Brightness")).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Reverb: decay" })).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of Brightness → Filter: resonance" })).toBeVisible();
});
