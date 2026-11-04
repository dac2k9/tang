import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { node, open, showAll } from "./canvas";
import { boxRound, choose as chooseNodes, names } from "./modules";
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
