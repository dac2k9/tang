import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { flowBoxes, node, open, showAll } from "./canvas";
import { boxRound, choose, names } from "./modules";
import { test } from "./own";

// Collapsing what is chosen into one module where it is (docs/web-ui.md D109; the user's "compact/collapse them into a module in place ... to clean up the ui a bit"): the nodes that a box is drawn round become one box, at the top left
// of where they were, which opens into them where they were and is ungrouped again, each a step that Undo takes back. The first tests are against the mock engine in the page, the last against the real program, whose session keeps the record.

const collapseButton = (page: Page): Locator => page.getByRole("button", { name: "Collapse the chosen nodes into a module" });

/** The nodes of the tracer that make a voice: the LFO that hears the oscillator and drives its filter, the oscillator and the filter. */
const VOICE = ["LFO", "Oscillator", "Filter"];

/** A box is drawn round the nodes called `chosen` and what is chosen is collapsed under `name`, in the field that the bar's button opens, which starts as `Module`. Choosing is turned off when it is done. */
async function collapse(page: Page, chosen: string[], name: string): Promise<void> {
  await choose(page).click();
  await boxRound(page, chosen);
  await expect(page.getByRole("status").filter({ hasText: new RegExp(`^${String(chosen.length)} chosen`) })).toBeVisible();
  await collapseButton(page).click();
  const dialog = page.getByRole("dialog", { name: "Collapse the chosen nodes into a module" });
  const field = dialog.getByRole("textbox", { name: "Module name" });
  await expect(field).toBeFocused();
  await expect(field).toHaveValue("Module");
  await field.fill(name);
  await field.press("Enter");
  await expect(dialog).toHaveCount(0);
  await choose(page).click();
}

/** Where the nodes called `called` are on the canvas, in the canvas's own units (the view may move and zoom, and these do not). */
async function placesOf(page: Page, called: string[]): Promise<Map<string, { x: number; y: number }>> {
  const boxes = await flowBoxes(page);
  return new Map(
    called.map((name) => {
      const found = boxes.find((box) => box.name === name);
      if (!found) throw new Error(`no node called ${name} on the canvas`);
      return [name, { x: found.x, y: found.y }];
    }),
  );
}

/** The top left of the places of the nodes called `called`. */
const topLeft = (places: Map<string, { x: number; y: number }>) => ({ x: Math.min(...[...places.values()].map((at) => at.x)), y: Math.min(...[...places.values()].map((at) => at.y)) });

mock("nodes that are chosen are collapsed into one box at the top left of where they were, which opens into them where they were, is ungrouped and is taken back by Undo, step by step", async ({ page }) => {
  await open(page);
  await showAll(page);
  const before = await placesOf(page, VOICE);
  const others = await names(page);
  expect(others).toHaveLength(4);
  await collapse(page, VOICE, "Voice");

  // The box is there, with what is in it said, and what it took is not on the canvas.
  const box = node(page, "Voice");
  await expect(box).toBeVisible();
  await expect.poll(() => names(page)).toHaveLength(2);
  for (const name of VOICE) await expect(node(page, name)).toHaveCount(0);
  await expect(box.locator(".tang-box__detail")).toHaveText("2 plugins · 1 modulator");
  // Where the nodes were: the box has the top left of them.
  const corner = topLeft(before);
  const at = (await placesOf(page, ["Voice"])).get("Voice");
  expect(at?.x).toBeCloseTo(corner.x, 0);
  expect(at?.y).toBeCloseTo(corner.y, 0);
  await expect(page.getByRole("button", { name: "Undo collapse into Voice" })).toBeVisible();

  // Opened, it is the nodes, each where it was drawn; closed at its tag, it is the box again.
  await page.getByRole("button", { name: "Open Voice" }).click();
  await expect.poll(() => names(page)).toHaveLength(4);
  const opened = await placesOf(page, VOICE);
  for (const name of VOICE) {
    expect(opened.get(name)?.x, name).toBeCloseTo(before.get(name)?.x ?? Number.NaN, 0);
    expect(opened.get(name)?.y, name).toBeCloseTo(before.get(name)?.y ?? Number.NaN, 0);
  }
  await page.getByRole("button", { name: "Close Voice" }).click();
  await expect(node(page, "Voice")).toBeVisible();

  // One step takes the box away and leaves the nodes where they were; one gives it back.
  await page.getByRole("button", { name: "Undo collapse into Voice" }).click();
  await expect(node(page, "Voice")).toHaveCount(0);
  await expect.poll(() => names(page)).toHaveLength(4);
  await page.getByRole("button", { name: "Redo collapse into Voice" }).click();
  await expect(node(page, "Voice")).toBeVisible();
  await expect.poll(() => names(page)).toHaveLength(2);

  // Ungrouped by the menu of its ×, which keeps what is in it: the nodes are where they were, and the box is gone.
  await page.getByRole("button", { name: "Remove Voice" }).click();
  await page.getByRole("menuitem", { name: /^Ungroup Voice/ }).click();
  await expect(node(page, "Voice")).toHaveCount(0);
  await expect.poll(() => names(page)).toHaveLength(4);
  const apart = await placesOf(page, VOICE);
  for (const name of VOICE) {
    expect(apart.get(name)?.x, name).toBeCloseTo(before.get(name)?.x ?? Number.NaN, 0);
    expect(apart.get(name)?.y, name).toBeCloseTo(before.get(name)?.y ?? Number.NaN, 0);
  }
  await expect(page.getByRole("button", { name: "Undo ungroup Voice" })).toBeVisible();
  await page.getByRole("button", { name: "Undo ungroup Voice" }).click();
  await expect(node(page, "Voice")).toBeVisible();
  await expect.poll(() => names(page)).toHaveLength(2);
});

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

mock("what is chosen is collapsed from the menu of the canvas too: the field opens where the pointer was, and the box is where the nodes were and not where the pointer was", async ({ page }) => {
  await open(page);
  await showAll(page);
  const before = await placesOf(page, VOICE);
  await choose(page).click();
  await boxRound(page, VOICE);
  await expect(page.getByRole("status").filter({ hasText: /^3 chosen$/ })).toBeVisible();
  const spot = await emptySpot(page);
  await page.mouse.click(spot.x, spot.y, { button: "right" });
  const menu = page.getByRole("menu", { name: "the canvas" });
  await expect(menu.getByRole("menuitem", { name: /^Collapse / })).toHaveText("Collapse the 1 lane and 1 modulator into a module…");
  await menu.getByRole("menuitem", { name: /^Collapse / }).click();
  const dialog = page.getByRole("dialog", { name: "Collapse the chosen nodes into a module" });
  const field = dialog.getByRole("textbox", { name: "Module name" });
  await expect(field).toBeFocused();
  await expect(field).toHaveValue("Module");
  const place = await dialog.boundingBox();
  expect(Math.abs((place?.x ?? 0) - spot.x)).toBeLessThan(40);
  await field.fill("From the menu");
  await field.press("Enter");
  const box = node(page, "From the menu");
  await expect(box).toBeVisible();
  const at = (await placesOf(page, ["From the menu"])).get("From the menu");
  const corner = topLeft(before);
  expect(at?.x).toBeCloseTo(corner.x, 0);
  expect(at?.y).toBeCloseTo(corner.y, 0);
  // A right click on the box ungroups it as well.
  await box.click({ button: "right" });
  await page.getByRole("menu", { name: "From the menu" }).getByRole("menuitem", { name: /^Ungroup From the menu/ }).click();
  await expect(node(page, "From the menu")).toHaveCount(0);
  await expect.poll(() => names(page)).toHaveLength(4);
});

mock("a box that is open has the item that ungroups it in the menu of its lanes, and the bar's button cannot be pressed with nothing that a module can take", async ({ page }) => {
  await open(page);
  await showAll(page);
  await collapse(page, VOICE, "Voice");
  await page.getByRole("button", { name: "Open Voice" }).click();
  await expect(page.getByRole("button", { name: "Close Voice" })).toBeVisible();
  await node(page, "Oscillator").click({ button: "right", position: { x: 20, y: 12 } });
  const menu = page.getByRole("menu", { name: "Oscillator" });
  await expect(menu.getByRole("menuitem").first()).toHaveText("Close Voice");
  await menu.getByRole("menuitem", { name: "Ungroup Voice" }).click();
  await expect(page.getByRole("button", { name: "Close Voice" })).toHaveCount(0);
  await expect.poll(() => names(page)).toHaveLength(4);
  // With the master alone chosen there is nothing that a module can take.
  await choose(page).click();
  await boxRound(page, ["Master"]);
  await expect(collapseButton(page)).toBeDisabled();
  await expect(collapseButton(page)).toHaveAttribute("title", "None of what is chosen can be collapsed: a module holds lanes and modulators that are in no module");
});

test("the real program's session keeps the record of what was collapsed, with the place of the box, and a page that is opened again draws the box; ungrouping takes the record out of the file and leaves the nodes", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await showAll(page);
  const before = await placesOf(page, VOICE);
  await collapse(page, VOICE, "Voice");
  await expect(node(page, "Voice")).toBeVisible();

  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.split("[[module]]")).toHaveLength(2);
  const record = saved.slice(saved.indexOf("[[module]]"));
  expect(record).toContain('name = "Voice"');
  // The lane and the LFO, which the file names by their places among the lanes and the modulators.
  expect(record).toContain('members = ["instrument[0]", "modulator[0]"]');
  const corner = topLeft(before);
  const place = /ui = \{ x = (-?[\d.]+), y = (-?[\d.]+) \}/.exec(record);
  expect(Number(place?.[1])).toBeCloseTo(corner.x, 0);
  expect(Number(place?.[2])).toBeCloseTo(corner.y, 0);

  // A page that is opened again draws the box, closed, where it was put.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Voice")).toBeVisible();
  await expect.poll(() => names(page)).toHaveLength(2);
  const again = (await placesOf(page, ["Voice"])).get("Voice");
  expect(again?.x).toBeCloseTo(corner.x, 0);
  expect(again?.y).toBeCloseTo(corner.y, 0);

  // Ungrouped, the nodes are on the canvas as they were and the file has no record.
  await page.getByRole("button", { name: "Remove Voice" }).click();
  await page.getByRole("menuitem", { name: /^Ungroup Voice/ }).click();
  await expect(node(page, "Voice")).toHaveCount(0);
  await expect.poll(() => names(page)).toHaveLength(4);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).not.toContain("[[module]]");

  // And Undo gives the box back.
  await page.getByRole("button", { name: "Undo ungroup Voice" }).click();
  await expect(node(page, "Voice")).toBeVisible();
});
