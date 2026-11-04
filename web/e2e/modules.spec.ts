import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { expect, test as mock } from "@playwright/test";

import { audioIn, audioOut, boxIn, boxOut, cables, centre, drag, node, open, showAll } from "./canvas";
import { addEffect, addFree, addModuleButton, boxRound, choose, names, save, saveChosen } from "./modules";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// A chain of effects saved as a module and added again (docs/web-ui.md D104): the button in the cell of a lane that saves its chain under a name, and the bar's "+ Module" (until step 111 "+ Effect"), which lists what was saved and adds it
// as one box, with the effects it was saved with. The first test is against the mock engine in the page (what the canvas draws and says), the second against the real program, which keeps the module in a file.

mock("a lane that has a chain of effects saves it as a module from its cell, and the bar's + Module adds it: a lane of its own with the effects it was saved with, which Undo takes out and Redo gives back", async ({ page }) => {
  await open(page);
  // The button is in the cell of the lane that has a chain, beside its mute and its solo.
  const cell = node(page, "Oscillator").locator(".tang-cell").filter({ has: page.getByRole("button", { name: "Mute Oscillator" }) });
  await expect(cell.getByRole("button")).toHaveCount(3);
  await expect(addEffect(page)).toBeEnabled();

  // The field takes a name; Escape leaves it and asks nothing; a name that is refused is said, and the field stays.
  await page.getByRole("button", { name: "Save the chain of Oscillator as a module" }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Save the chain of Oscillator as a module" })).toBeFocused();
  await page.getByRole("button", { name: "Save the chain of Oscillator as a module" }).click();
  await page.getByRole("textbox", { name: "Module name" }).fill("a/b");
  // (The bar has a Save of its own: the field's is the one in the dialog.)
  await page.getByRole("dialog").getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/Could not save the chain of Oscillator as the module "a\/b": a file name has no folder in it/)).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Module name" })).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();

  // Saved: the page says so, and the bar's button for modules has it, under no heading (it is shut until one is saved); the menu of effects does not have it, and the lane's own menu does not either.
  await expect(addModuleButton(page)).toBeDisabled();
  await save(page, "Oscillator", "Tracer chain");
  await expect(addModuleButton(page)).toBeEnabled();
  await addModuleButton(page).click();
  const menu = page.getByRole("menu", { name: "Add a module" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Tracer chain"]);
  await expect(menu.getByRole("group")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await addEffect(page).click();
  await expect(page.getByRole("menu", { name: "Add an effect" }).getByRole("menuitem", { name: "Tracer chain" })).toHaveCount(0);
  await expect(page.getByRole("menu", { name: "Add an effect" }).getByRole("group", { name: "Module" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await expect(page.getByRole("menu", { name: "Add an effect to Oscillator" }).getByRole("menuitem", { name: "Tracer chain" })).toHaveCount(0);
  await page.keyboard.press("Escape");

  // Added: one box, with the lane of its own that holds a filter, as the tracer's lane has one, in it. The filter is on the canvas once the box is opened, with the lane's cell on it (and its button, since the chain is there).
  const before = await names(page);
  await addModuleButton(page).click();
  await page.getByRole("menuitem", { name: "Tracer chain", exact: true }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
  await expect(node(page, "Tracer chain")).toBeVisible();
  await expect(page.getByRole("button", { name: "Save the chain of Filter 2 as a module" })).toHaveCount(0);
  await page.getByRole("button", { name: "Open Tracer chain" }).click();
  await expect(page.getByRole("button", { name: "Save the chain of Filter 2 as a module" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Undo add Tracer chain" })).toBeVisible();
  await page.getByRole("button", { name: "Undo add Tracer chain" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length);
  await page.getByRole("button", { name: "Redo add Tracer chain" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
});

test("a chain of effects is saved as a file of the library of the real program, and added from it: heard through cables only, with the effects as they were saved, and the library is the program's: another program on it has the module", async ({ page, own, browser }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await heardAt(page, "the lane");

  // The tracer's lane has an oscillator and a filter whose cutoff an LFO moves: its chain, the filter, is saved as a module. The file is in the library, with the filter and no oscillator, and the session's own file has nothing of it.
  await save(page, "Oscillator", "Tracer filter");
  const module = readFileSync(join(own.library, "Tracer filter.toml"), "utf8");
  expect(module).toContain('plugin = "builtin:filter"');
  expect(module).not.toContain("builtin:osc");
  expect(existsSync(join(own.library, "Tracer filter.toml"))).toBe(true);

  // Cut the lane's way out (its filter is the link to the master): it is silent. The module is added in a lane of its own, bare: silent still, until cables say where. Then it is heard through them, and only through them.
  await showAll(page);
  await page.getByRole("button", { name: "Remove cable Filter → Master", exact: true }).click();
  // (The page has the cut when the program has made it, and the engine has been told by then: a note is pressed after the cut and not beside it.)
  await expect(page.getByRole("button", { name: "Remove cable Filter → Master", exact: true })).toHaveCount(0);
  await silentAt(page, "the lane, with its way out cut");
  await addModuleButton(page).click();
  await page.getByRole("menuitem", { name: "Tracer filter", exact: true }).click();
  // (One box: the filter that is in it is not on the canvas, and the cables are drawn to the ports of the box, which are the filter's.)
  await expect(node(page, "Tracer filter")).toBeVisible();
  await showAll(page);
  await silentAt(page, "the module that is added, with no cable");
  await drag(page, audioOut(node(page, "Oscillator")), boxIn(node(page, "Tracer filter")));
  await drag(page, boxOut(node(page, "Tracer filter")), audioIn(node(page, "Master")));
  await heardAt(page, "the lane through the module");

  // The file of the session keeps the lane that the module made: no instrument, the filter, and the cable into it.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.split("[[instrument]]")).toHaveLength(3);
  expect(saved).toContain('to = "instrument[1].effect[0]"');
  // ...and the module, as a record of what came with it: the file says which lane is in the box.
  expect(saved).toContain("[[module]]");
  expect(saved).toContain('name = "Tracer filter"');
  expect(saved).toContain('members = ["instrument[1]"]');

  // The program's library is the one that it was started with, and a page that is opened again lists the module: it is a file, and not something the page has.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Tracer filter")).toBeVisible();
  await addModuleButton(page).click();
  await expect(page.getByRole("menuitem", { name: "Tracer filter", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  const second = await browser.newPage();
  await second.goto(own.url);
  await expect(second.getByRole("status")).toHaveText("Connected");
  await second.getByRole("button", { name: "Add a module", exact: true }).click();
  await expect(second.getByRole("menuitem", { name: "Tracer filter", exact: true })).toBeVisible();
  await second.close();
});

mock("effects that stand by themselves, joined by cables, are chosen with a box and saved as one module, which + Module adds as the same effects with the cables between them: one step that Undo takes out and Redo gives back", async ({ page }) => {
  await open(page);
  // Three effects, each in a lane of its own, and a cable from the first to the second and from the second to the third: what a person builds on the canvas.
  await addFree(page, "Drive", "Drive");
  await addFree(page, "Reverb", "Reverb");
  await addFree(page, "Delay", "Delay");
  await showAll(page);
  await drag(page, audioOut(node(page, "Drive")), audioIn(node(page, "Reverb")));
  await drag(page, audioOut(node(page, "Reverb")), audioIn(node(page, "Delay")));
  await expect(page.locator(".tang-cable--send")).toHaveCount(2);

  // Nothing is chosen until choosing is on, and then a box round the three effects chooses them and nothing else.
  await expect(choose(page)).toHaveAttribute("aria-pressed", "false");
  await choose(page).click();
  await expect(page.getByRole("status").filter({ hasText: "Drag a box round the nodes to keep" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Save the chosen nodes as a module" })).toBeDisabled();
  await boxRound(page, ["Drive", "Reverb", "Delay"]);
  await expect(page.getByRole("status").filter({ hasText: "3 chosen" })).toBeVisible();
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(3);

  // A name that is refused is said, and the field stays; then it is saved, and the page says so.
  await page.getByRole("button", { name: "Save the chosen nodes as a module" }).click();
  await page.getByRole("textbox", { name: "Module name" }).fill("a/b");
  await page.getByRole("dialog").getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/Could not save .* as the module "a\/b": a file name has no folder in it/)).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();
  await saveChosen(page, "Dub chain");

  // Choosing is turned off, and the effects are added as a module: three lanes of their own, with the two cables between them, in one step.
  await choose(page).click();
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);
  const before = await page.locator(".tang-node__name").allTextContents();
  await addModuleButton(page).click();
  await expect(page.getByRole("menu", { name: "Add a module" }).getByRole("menuitem")).toHaveText(["Dub chain"]);
  await page.getByRole("menuitem", { name: "Dub chain", exact: true }).click();
  // One box, with the three lanes and the two cables between them in it: they are on the canvas when it is opened.
  await expect.poll(async () => (await page.locator(".tang-node__name").allTextContents()).length).toBe(before.length + 1);
  await expect(page.getByRole("button", { name: "Undo add Dub chain" })).toBeVisible();
  await showAll(page);
  await expect(page.locator(".tang-cable--send")).toHaveCount(2);
  await page.getByRole("button", { name: "Open Dub chain" }).click();
  await expect.poll(async () => (await page.locator(".tang-node__name").allTextContents()).length).toBe(before.length + 3);
  await showAll(page);
  await expect(page.locator(".tang-cable--send")).toHaveCount(4);
  await page.getByRole("button", { name: "Undo add Dub chain" }).click();
  await expect.poll(async () => (await page.locator(".tang-node__name").allTextContents()).length).toBe(before.length);
  await expect(page.locator(".tang-cable--send")).toHaveCount(2);
  // (The module comes back under the id it had, and the page had it open: it is open still.)
  await page.getByRole("button", { name: "Redo add Dub chain" }).click();
  await expect.poll(async () => (await page.locator(".tang-node__name").allTextContents()).length).toBe(before.length + 3);
  await expect(page.locator(".tang-cable--send")).toHaveCount(4);
});

mock("a node that a module cannot hold is left out of what is saved and said so, a node is chosen by a click, and Escape turns choosing off", async ({ page }) => {
  await open(page);
  await choose(page).click();
  // The oscillator's card is chosen by a click; the master is not a plugin, and a module is not made of it: it is left out and counted. (The LFO is held: it goes with the plugins that it drives.)
  await node(page, "Oscillator").locator(".tang-node__header").click();
  await expect(page.getByRole("status").filter({ hasText: "1 chosen" })).toBeVisible();
  await page.keyboard.down("Shift");
  await node(page, "LFO").locator(".tang-node__header").click();
  await expect(page.getByRole("status").filter({ hasText: /^2 chosen$/ })).toBeVisible();
  await node(page, "Master").locator(".tang-node__header").click();
  await page.keyboard.up("Shift");
  await expect(page.getByRole("status").filter({ hasText: "2 chosen, 1 that a module cannot hold left out" })).toBeVisible();
  await saveChosen(page, "Tone");
  await addModuleButton(page).click();
  await expect(page.getByRole("menuitem", { name: "Tone", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  // (A click did not move the node: nodes are not moved while choosing is on.)
  const where = await centre(node(page, "Oscillator"));
  await page.mouse.move(where.x, where.y);
  await page.keyboard.press("Escape");
  await expect(choose(page)).toHaveAttribute("aria-pressed", "false");
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);
});

mock("a modulator that is chosen with the plugins that it drives is saved in the module and added with its cable to the new plugin: it hears the new lane, and Undo takes it out with the lane, and Redo gives it back", async ({ page }) => {
  await open(page);
  await showAll(page);
  // The tracer's LFO hears the oscillator's lane and moves the filter's cutoff.
  const wired = await cables(page).count();
  expect(wired).toBeGreaterThan(0);
  await choose(page).click();
  await boxRound(page, ["LFO", "Oscillator", "Filter"]);
  await expect(page.getByRole("status").filter({ hasText: /^3 chosen$/ })).toBeVisible();
  await saveChosen(page, "Tracer voice");
  await choose(page).click();

  // Added: a lane with the oscillator and the filter, and a modulator, and the cable from the modulator to the new filter, in one step.
  const before = (await names(page)).length;
  await addModuleButton(page).click();
  await page.getByRole("menuitem", { name: "Tracer voice", exact: true }).click();
  // (One box: the new LFO and its cable are in it, and are on the canvas when it is opened.)
  await expect.poll(async () => (await names(page)).length).toBe(before + 1);
  await showAll(page);
  await expect(cables(page)).toHaveCount(wired);
  await page.getByRole("button", { name: "Open Tracer voice" }).click();
  await expect.poll(async () => (await names(page)).length).toBe(before + 3);
  await expect(node(page, "LFO 2")).toBeVisible();
  await showAll(page);
  await expect(cables(page)).toHaveCount(wired * 2);
  await expect(page.getByRole("button", { name: "Undo add Tracer voice" })).toBeVisible();
  await page.getByRole("button", { name: "Undo add Tracer voice" }).click();
  await expect.poll(async () => (await names(page)).length).toBe(before);
  await expect(cables(page)).toHaveCount(wired);
  await page.getByRole("button", { name: "Redo add Tracer voice" }).click();
  await expect.poll(async () => (await names(page)).length).toBe(before + 3);
  await expect(cables(page)).toHaveCount(wired * 2);
});

test("the tracer's LFO is chosen with the oscillator and the filter that it hears and drives, saved in the real program's library with its cable, and added from it as a modulator that hears the new lane, which the session's file then keeps", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await showAll(page);
  await choose(page).click();
  await boxRound(page, ["LFO", "Oscillator", "Filter"]);
  await expect(page.getByRole("status").filter({ hasText: /^3 chosen$/ })).toBeVisible();
  await saveChosen(page, "Tracer voice");
  // The file is in the library: the oscillator and the filter in one lane, and the LFO that hears that lane, with its cable to the cutoff of the filter.
  const module = readFileSync(join(own.library, "Tracer voice.toml"), "utf8");
  expect(module).toContain('plugin = "builtin:osc"');
  expect(module).toContain('plugin = "builtin:filter"');
  expect(module.split("[[modulator]]")).toHaveLength(2);
  expect(module).toContain('hears = "instrument[0]"');
  expect(module).toContain('to = "instrument[0].effect[0]"');
  expect(module).toContain('param = "cutoff"');
  await choose(page).click();

  // Added: the oscillator and the filter in a lane of their own, and a modulator of the session's own that hears that lane and moves the new filter's cutoff.
  await addModuleButton(page).click();
  await page.getByRole("menuitem", { name: "Tracer voice", exact: true }).click();
  await page.getByRole("button", { name: "Open Tracer voice" }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(node(page, "Filter 2")).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.split("[[modulator]]")).toHaveLength(3);
  expect(saved).toContain('hears = "instrument[1]"');
  expect(saved).toContain('to = "instrument[1].effect[0]"');
});

test("effects joined by a cable are saved as one file of the real program's library with the cable in it, and added from it as lanes of their own with the cable between them, which the session's file then keeps", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await addFree(page, "Drive", "Drive");
  await addFree(page, "Reverb", "Reverb");
  await showAll(page);
  await drag(page, audioOut(node(page, "Drive")), audioIn(node(page, "Reverb")));
  await expect(page.locator(".tang-cable--send")).toHaveCount(1);

  await choose(page).click();
  await boxRound(page, ["Drive", "Reverb"]);
  await expect(page.getByRole("status").filter({ hasText: "2 chosen" })).toBeVisible();
  await saveChosen(page, "Dub pair");
  // The file is in the library: a lane for each effect, no instrument, and the cable between them.
  const module = readFileSync(join(own.library, "Dub pair.toml"), "utf8");
  expect(module.split("[[instrument]]")).toHaveLength(3);
  expect(module).toContain('plugin = "builtin:drive"');
  expect(module).toContain('plugin = "builtin:reverb"');
  expect(module).not.toContain("builtin:osc");
  expect(module).toContain('from = "instrument[0].effect[0]"');
  expect(module).toContain('to = "instrument[1].effect[0]"');
  await choose(page).click();

  // Added: two lanes more and the cable between them (the session has the two that it was made from and their cable, and the two that the module made).
  await addModuleButton(page).click();
  await page.getByRole("menuitem", { name: "Dub pair", exact: true }).click();
  await page.getByRole("button", { name: "Open Dub pair" }).click();
  await expect(node(page, "Drive 2")).toBeVisible();
  await expect(node(page, "Reverb 2")).toBeVisible();
  await showAll(page);
  await expect(page.locator(".tang-cable--send")).toHaveCount(2);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.split("[[instrument]]")).toHaveLength(6);
  expect(saved.split("[[cable]]")).toHaveLength(3);
  // The cable that the module made is between two lanes that the module made: the fourth and the fifth.
  expect(saved).toContain('from = "instrument[3].effect[0]"');
  expect(saved).toContain('to = "instrument[4].effect[0]"');
});
