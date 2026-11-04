import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { audioIn, audioOut, boxIn, boxOut, centre, drag, expectNodesApart, flowBoxes, node, open, showAll, still } from "./canvas";
import { addModuleButton, boxRound, choose, names, save, saveChosen } from "./modules";
import { test } from "./own";

// A module that is added is one box on the canvas (docs/web-ui.md D108; the user's "the goal when we re-add a module is that it gets rendered like just a single box"): the box says what is in it and has the ports of the module, a cable
// dragged to one is a cable to the node inside, the module opens into its nodes (with a tag over each lane that closes it) and closes again, and the session keeps the record of what is in the box. The first tests are against the
// mock engine in the page, the last against the real program, which writes the record in the session's file.

/** The tracer's LFO, its oscillator and its filter, which an LFO hears and drives, saved as the module called `name`, with the library of the page's mock engine. */
async function saveVoice(page: Page, name: string): Promise<void> {
  await showAll(page);
  await choose(page).click();
  await boxRound(page, ["LFO", "Oscillator", "Filter"]);
  await expect(page.getByRole("status").filter({ hasText: /^3 chosen$/ })).toBeVisible();
  await saveChosen(page, name);
  await choose(page).click();
}

/** Add the module called `name` from the bar's "+ Module", and wait for its box to be where it stays. */
async function addModule(page: Page, name: string) {
  await addModuleButton(page).click();
  await page.getByRole("menuitem", { name, exact: true }).click();
  const box = node(page, name);
  await expect(box).toBeVisible();
  await still(box);
  return box;
}

mock("a module that is added is one box that says what is in it and has the module's ports, as tall as the layout says, and it opens into the nodes that are in it, with a tag that closes it, and closes again", async ({ page }) => {
  await open(page);
  await saveVoice(page, "Tracer voice");
  const before = await names(page);
  const box = await addModule(page, "Tracer voice");
  // One box, and the oscillator, the filter and the LFO that are in it are not on the canvas.
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
  await expect(box.locator(".tang-box__detail")).toHaveText("2 plugins · 1 modulator");
  await expect(box.getByText("Module", { exact: true })).toBeVisible();
  // The ports of the module: the notes that its instrument takes in, and the filter's output of audio; nothing feeds audio in.
  await expect(box.locator(".tang-port--notes.tang-port--in")).toHaveCount(1);
  await expect(box.locator(".tang-port--audio.tang-port--out")).toHaveCount(1);
  await expect(box.locator(".tang-port--audio.tang-port--in")).toHaveCount(0);
  await expect(box.getByRole("list", { name: "Ports of Tracer voice" }).getByRole("listitem")).toHaveText(["♪ Oscillator 2Filter 2"]);
  // As tall as the layout says (the header, a row, the line that says what is in it, the room under it), and no node is on another.
  const found = (await flowBoxes(page)).find((each) => each.name === "Tracer voice");
  expect(found).toMatchObject({ width: 304, height: 44 + 28 + 20 + 16 });
  // The rows are as tall as the layout says, and each port is level with the middle of its row: the ports are put by numbers that the stylesheet's rows must agree with.
  expect(await box.locator(".tang-box__row").evaluateAll((rows) => rows.map((row) => (row as HTMLElement).offsetHeight))).toEqual([28]);
  const row = await box.locator(".tang-box__row").first().boundingBox();
  if (!row) throw new Error("the row is on the page");
  for (const port of [box.locator(".tang-port--notes.tang-port--in"), box.locator(".tang-port--audio.tang-port--out")]) {
    const at = await port.boundingBox();
    if (!at) throw new Error("the port is on the page");
    expect(Math.abs(at.y + at.height / 2 - (row.y + row.height / 2))).toBeLessThan(row.height * 0.1);
  }
  await showAll(page);
  await expectNodesApart(page);

  // Opened, it is its nodes: its oscillator, its filter and its LFO are on the canvas with the tracer's, and the tag over its lane closes it.
  await page.getByRole("button", { name: "Open Tracer voice" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 3);
  await expect(node(page, "Tracer voice")).toHaveCount(0);
  for (const name of ["Oscillator 2", "Filter 2", "LFO 2"]) await expect(node(page, name)).toBeVisible();
  const tag = page.getByRole("button", { name: "Close Tracer voice" });
  await expect(tag).toBeVisible();
  await expect(page.locator(".tang-node__module-tag")).toHaveCount(1);
  await expect(node(page, "Oscillator 2").locator(".tang-node__module-tag")).toBeVisible();
  await showAll(page);
  await expectNodesApart(page);

  // Closed again at the tag: the box, where it was drawn.
  await tag.click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
  await expect(node(page, "Tracer voice")).toBeVisible();
  await expect(page.locator(".tang-node__module-tag")).toHaveCount(0);

  // One step takes it out, and one gives it back, as a box (it was closed when it went).
  await page.getByRole("button", { name: "Undo add Tracer voice" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length);
  await page.getByRole("button", { name: "Redo add Tracer voice" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
  await expect(node(page, "Tracer voice")).toBeVisible();
});

mock("a cable dragged to the input of a box and from its output is a cable to the node inside, drawn from and to the box while it is closed and to the node inside once it is open, which Undo takes out", async ({ page }) => {
  await open(page);
  await save(page, "Oscillator", "Tracer chain");
  const box = await addModule(page, "Tracer chain");
  await showAll(page);
  // The filter is the module's one effect, and what feeds it is what cables bring: the box has one input and one output.
  await expect(box.locator(".tang-port--audio.tang-port--in")).toHaveCount(1);
  await expect(box.locator(".tang-port--audio.tang-port--out")).toHaveCount(1);
  await drag(page, audioOut(node(page, "Oscillator")), boxIn(box));
  await expect(page.locator(".tang-cable--send")).toHaveCount(1);
  // (The cable is the session's, between the oscillator and the filter inside: its words are theirs.)
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Filter 2" })).toBeVisible();
  await drag(page, boxOut(box), audioIn(node(page, "Master")));
  await expect(page.getByRole("button", { name: "Remove cable Filter 2 → Master" })).toBeVisible();

  // Opened, the cables are at the filter itself; closed, at the box.
  await page.getByRole("button", { name: "Open Tracer chain" }).click();
  await expect(node(page, "Filter 2")).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Filter 2" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove cable Filter 2 → Master" })).toBeVisible();
  await page.getByRole("button", { name: "Close Tracer chain" }).click();
  await expect(node(page, "Tracer chain")).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Filter 2" })).toBeVisible();

  // The cable is one step, as any is.
  await page.getByRole("button", { name: /^Undo give .* its way out$/ }).click();
  await expect(page.getByRole("button", { name: "Remove cable Filter 2 → Master" })).toHaveCount(0);
  await page.getByRole("button", { name: "Undo draw an audio cable" }).click();
  await expect(page.locator(".tang-cable--send")).toHaveCount(0);
});

mock("a box that is chosen is what is in it, a right click on it opens its own menu and its × asks before it takes it out with all that is in it, in one step", async ({ page }) => {
  await open(page);
  await saveVoice(page, "Tracer voice");
  const box = await addModule(page, "Tracer voice");
  const before = await names(page);
  await showAll(page);
  // Chosen by a box drawn round it: what a module is made of is what is in it (the oscillator, the filter and the LFO).
  await choose(page).click();
  await boxRound(page, ["Tracer voice"]);
  await expect(page.getByRole("status").filter({ hasText: /^3 chosen$/ })).toBeVisible();
  await choose(page).click();
  await expect(page.locator(".react-flow__node.selected")).toHaveCount(0);

  // Its own menu: it opens it, or takes it out with all that is in it.
  await box.click({ button: "right" });
  const menu = page.getByRole("menu", { name: "Tracer voice" });
  await expect(menu.getByRole("menuitem")).toHaveText([
    "Open Tracer voice",
    "Ungroup Tracer voice: keep the 2 plugins · 1 modulator, take the box away",
    "Remove Tracer voice for good: the 2 plugins · 1 modulator in it go with it",
  ]);
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);

  // The × asks first.
  await page.getByRole("button", { name: "Remove Tracer voice" }).click();
  await page.getByRole("menuitem", { name: "Keep Tracer voice" }).click();
  await expect(node(page, "Tracer voice")).toBeVisible();
  await page.getByRole("button", { name: "Remove Tracer voice" }).click();
  await page.getByRole("menuitem", { name: /^Remove Tracer voice for good/ }).click();
  await expect(node(page, "Tracer voice")).toHaveCount(0);
  await expect.poll(() => names(page)).toHaveLength(before.length - 1);
  await page.getByRole("button", { name: "Undo remove Tracer voice" }).click();
  await expect(node(page, "Tracer voice")).toBeVisible();
  await expect.poll(() => names(page)).toHaveLength(before.length);
});

test("the real program's session keeps the record of a module that is a box, with the place that the box was put at, and a page that is opened again draws the box, closed; taking it out takes out what is in it, and Undo puts it back", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await save(page, "Oscillator", "Tracer filter");
  const box = await addModule(page, "Tracer filter");
  await showAll(page);

  // Put somewhere else: the page asks for the place, and the session says so.
  const from = await centre(box.locator(".tang-node__header"));
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 80, from.y + 60, { steps: 8 });
  await page.mouse.up();
  await expect(page.getByRole("button", { name: /^Undo move / })).toBeVisible();

  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.split("[[module]]")).toHaveLength(2);
  expect(saved).toContain('name = "Tracer filter"');
  expect(saved).toContain('members = ["instrument[1]"]');
  const record = saved.slice(saved.indexOf("[[module]]"));
  expect(record).toMatch(/ui = \{ x = -?[\d.]+, y = -?[\d.]+ \}/);

  // Opened, and a page that is opened again has it closed (that is the page's own, and not the session's), where the box was put.
  await page.getByRole("button", { name: "Open Tracer filter" }).click();
  await expect(page.getByRole("button", { name: "Close Tracer filter" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  const again = node(page, "Tracer filter");
  await expect(again).toBeVisible();
  await expect(page.getByRole("button", { name: "Close Tracer filter" })).toHaveCount(0);

  // Taken out with what is in it (one step), and put back by Undo.
  const count = (await names(page)).length;
  await page.getByRole("button", { name: "Remove Tracer filter" }).click();
  await page.getByRole("menuitem", { name: /^Remove Tracer filter for good/ }).click();
  await expect(node(page, "Tracer filter")).toHaveCount(0);
  await page.getByRole("button", { name: "Undo remove Tracer filter" }).click();
  await expect(node(page, "Tracer filter")).toBeVisible();
  await expect.poll(async () => (await names(page)).length).toBe(count);
});
