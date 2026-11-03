import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

// What the tests of the canvas against the mock engine in the page have in common: the page opened on a scene, the nodes, their
// ports, and a cable dragged by hand.

export async function open(page: Page, scene = "tracer"): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(`/?engine=mock&scene=${scene}`);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator(".react-flow__node").first()).toBeVisible();
}

/** The node of the canvas whose name is exactly `name` (the text of a regular expression: the name of a lane that has no instrument, `(empty)`, is written `\\(empty\\)`, as `MIDI \\(learning…\\)` is). */
export const node = (page: Page, name: string): Locator => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });

/** The output of a modulator, where a cable is started. */
export const output = (modulator: Locator): Locator => modulator.locator('.react-flow__handle[data-handleid="modOut"]');

/** The port of the control of a parameter, where a cable ends. */
export const port = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) }).locator(".react-flow__handle");

export async function centre(target: Locator): Promise<{ x: number; y: number }> {
  const box = await target.boundingBox();
  if (!box) throw new Error("not on the page");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Press on `from` and move to `to`, and let go there unless `release` is false: a cable dragged by hand. */
export async function drag(page: Page, from: Locator, to: Locator | { x: number; y: number }, release = true): Promise<void> {
  const start = await centre(from);
  const end = "x" in to ? to : await centre(to);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  await page.mouse.move((start.x + end.x) / 2, (start.y + end.y) / 2, { steps: 8 });
  await page.mouse.move(end.x, end.y, { steps: 8 });
  if (release) await page.mouse.up();
}

export const cables = (page: Page): Locator => page.locator(".tang-cable--modulation");

/** The output of audio of a node, where an audio cable is started. */
export const audioOut = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid="audioOut"]');

/** The input of audio of a node (an effect's, a group's or the master's), where an audio cable ends. */
export const audioIn = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid="audioIn"]');

/**
 * The input of audio of a box (a module that is closed, step 108): the handle that stands for the input of a node inside it, which is named after the port and that node (`audioIn@12`), where an audio cable ends. The first
 * of them if the box has more than one.
 */
export const boxIn = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid^="audioIn@"]').first();

/** The output of audio of a box, where an audio cable is started: the handle that stands for the output of a node inside it (`audioOut@12`). The first of them if the box has more than one. */
export const boxOut = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid^="audioOut@"]').first();

/** The second input of audio of an effect that has one (a Math's B), where an audio cable ends that only a cable feeds. */
export const audioInSide = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid="audioInSide"]');

/** The audio cables that have been drawn (the links of the audio path, which are not drawn by anybody, are not among them). */
export const sends =(page: Page): Locator => page.locator(".tang-cable--send");

/** The output of events of a source of notes, where a cable of notes is started. */
export const notesOut = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid="eventsOut"]');

/** The input of events of a lane, where a cable of notes ends. */
export const notesIn = (target: Locator): Locator => target.locator('.react-flow__handle[data-handleid="eventsIn"]');

/** The cables of notes that have been drawn, from a source to a lane (the links by which a lane hears every source, which nobody draws, are not among them). */
export const notesCables = (page: Page): Locator => page.locator(".tang-cable--notes");

/** The links by which a lane that no cable of notes ends at hears every source, which the session has by itself. */
export const hearsLinks = (page: Page): Locator => page.locator(".tang-cable--hears");

/** The port of a setting of a modulator (an LFO's rate, a stage of an envelope), where a cable from another modulator ends. */
export const setting = (modulator: Locator, name: string): Locator => modulator.locator(`.react-flow__handle[data-handleid="${name}"]`);

/** Wait until a node has stopped moving: the canvas pans to a node that was added (it takes a quarter of a second), and a cable dragged to a port that is on its way is missed. */
export async function still(target: Locator): Promise<void> {
  let last = "";
  await expect
    .poll(
      async () => {
        const now = JSON.stringify(await target.boundingBox());
        const same = now === last;
        last = now;
        return same;
      },
      { intervals: [150] },
    )
    .toBe(true);
}

/**
 * An envelope put in the oscillator's lane from its menu (the tracer's LFO is in it already), once the canvas has stopped moving: the nodes stay where they are when one is
 * added, and the view moves to show it if it is not all in it (above the others, say), so that a cable dragged at once would start where its port was a moment before.
 */
export async function addEnvelope(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await still(envelope);
  return envelope;
}

/** The view moved to show every node, by the button of the canvas's controls, once it has stopped moving (it moves to show a node that was added, and may leave the others out of it). */
export async function showAll(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Fit View" }).click();
  await still(page.locator(".react-flow__node").first());
}

/** The nodes arranged again, with the button of the canvas, which then moves the view to show them all: waits until it has stopped moving. */
export async function arrange(page: Page): Promise<void> {
  const button = page.getByRole("button", { name: "Arrange the nodes again" });
  await expect(button).toBeEnabled();
  await button.click();
  await expect(button).toBeDisabled();
  await still(page.locator(".react-flow__node").first());
}

/** Where each node of the canvas is, as the browser draws it (`name` is the node's own, as it is in its header). */
export async function nodeBoxes(page: Page): Promise<{ name: string; left: number; top: number; width: number; height: number }[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll(".react-flow__node")].map((element) => {
      const box = element.getBoundingClientRect();
      return { name: element.querySelector(".tang-node__name")?.textContent ?? "?", left: box.left, top: box.top, width: box.width, height: box.height };
    }),
  );
}

/** Where each node is on the canvas in the canvas's own units, from its transform (the view, which moves to show a node that was added and may zoom, changes none of them). */
export async function flowBoxes(page: Page): Promise<{ name: string; x: number; y: number; width: number; height: number }[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>(".react-flow__node")].flatMap((element) => {
      const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(element.style.transform);
      if (!found) return [];
      return [{ name: element.querySelector(".tang-node__name")?.textContent ?? "?", x: Number(found[1]), y: Number(found[2]), width: element.offsetWidth, height: element.offsetHeight }];
    }),
  );
}

/** No node of the canvas lies over another (a pixel of slack: the browser rounds). */
export async function expectNodesApart(page: Page): Promise<void> {
  const nodes = await nodeBoxes(page);
  for (const a of nodes) {
    for (const b of nodes) {
      if (a === b) continue;
      const apart = a.left + a.width <= b.left + 1 || b.left + b.width <= a.left + 1 || a.top + a.height <= b.top + 1 || b.top + b.height <= a.top + 1;
      expect(apart, `${a.name} and ${b.name}`).toBe(true);
    }
  }
}

/** Points along the last modulation cable of the canvas (the newest, in the order of the graph), in the page's pixels, from its start to its end. */
export async function cablePoints(page: Page, count = 240): Promise<{ x: number; y: number }[]> {
  return page.evaluate((n) => {
    const paths = [...document.querySelectorAll<SVGPathElement>('path[id^="mod:"]')];
    const path = paths.at(-1);
    const matrix = path?.getScreenCTM();
    if (!path || !matrix) throw new Error("no cable on the canvas");
    const length = path.getTotalLength();
    return Array.from({ length: n + 1 }, (_, i) => {
      const at = path.getPointAtLength((length * i) / n);
      return { x: matrix.a * at.x + matrix.c * at.y + matrix.e, y: matrix.b * at.x + matrix.d * at.y + matrix.f };
    });
  }, count);
}

/** Whether a point is inside a box, by more than `margin` pixels from its edge. */
export const within = (point: { x: number; y: number }, box: { left: number; top: number; width: number; height: number }, margin = 3): boolean =>
  point.x > box.left + margin && point.x < box.left + box.width - margin && point.y > box.top + margin && point.y < box.top + box.height - margin;
