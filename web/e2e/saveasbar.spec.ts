import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { open } from "./canvas";

// The bar at the top of the page with the field of Save as open, against the mock engine in the page: it fits the windows the page is used in (the page does not
// scroll sideways, what the bar says stays on a line and nothing in it is over anything else), and in a window that is wide enough the field takes no room from the
// rest: the bar is as tall as it was. (With the field open the bar needs 1,561 pixels in a row: 1,467 before the bar had the button that adds a module (step 111), 1,555 before it lost the button that adds a group (step 103), 1,444 before the button that adds a modulator that is in no lane, 1,358 before the one that adds an effect that stands by itself, and 1,265 before the one that adds a source of notes.)

interface Fit {
  /** How far the page scrolls sideways: more than the window is wide, or not. */
  overflow: number;
  barHeight: number;
  /** The height of the bar's texts: a text that wraps is more than a line. */
  keysHeight: number;
  engineHeight: number;
  /** The pairs of the bar's items (by class or tag) that are one over the other. */
  overlaps: string[];
}

/** What the bar is like now. */
function fit(page: Page): Promise<Fit> {
  return page.evaluate(() => {
    const bar = document.querySelector(".tang-app__bar");
    if (!bar) throw new Error("no bar");
    const box = (selector: string): DOMRect => (document.querySelector(selector) ?? bar).getBoundingClientRect();
    const items = Array.from(bar.children).map((child) => ({ name: child.className || child.tagName, box: child.getBoundingClientRect() }));
    const overlaps: string[] = [];
    items.forEach((a, i) => {
      items.slice(i + 1).forEach((b) => {
        const across = Math.min(a.box.right, b.box.right) - Math.max(a.box.left, b.box.left);
        const down = Math.min(a.box.bottom, b.box.bottom) - Math.max(a.box.top, b.box.top);
        if (across > 1 && down > 1) overlaps.push(`${a.name} / ${b.name}`);
      });
    });
    return {
      overflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      barHeight: Math.round(bar.getBoundingClientRect().height),
      keysHeight: Math.round(box(".tang-app__keys").height),
      engineHeight: Math.round(box(".tang-app__engine").height),
      overlaps,
    };
  });
}

/** The width from which the open bar is as tall as the closed one: it needs 1,561 pixels in a row (1,467 before the bar had the button that adds a module, 1,555 before it lost the button that adds a group, 1,444 before the button that adds a modulator that is in no lane, 1,358 before the one that adds an effect that stands by itself, 1,265 before the one that adds a source of notes). */
const ONE_ROW = 1600;

for (const width of [1700, ONE_ROW, 1280, 1100, 1000, 900]) {
  test(`with the field of Save as open the bar fits a window ${String(width)} pixels wide: no sideways scrolling, texts on a line, nothing over anything`, async ({ page }) => {
    await open(page);
    await page.setViewportSize({ width, height: 700 });
    const closed = await fit(page);
    await page.keyboard.press("Control+Shift+S");
    await expect(page.getByRole("textbox", { name: "File name" })).toBeFocused();
    const opened = await fit(page);
    expect(opened.overflow, "sideways scrolling").toBeLessThanOrEqual(0);
    expect(opened.keysHeight, "the keys' hint wraps").toBeLessThan(24);
    expect(opened.engineHeight, "the engine's name wraps").toBeLessThan(24);
    expect(opened.overlaps).toEqual([]);
    // Where the closed bar fits on one row, so does the open one in a window that is wide enough for the field too.
    if (width >= ONE_ROW) expect(opened.barHeight, "the bar grows when the field opens").toBe(closed.barHeight);
  });
}

test("the closed bar fits a window 1000 pixels wide too, as it did before Save as was in it", async ({ page }) => {
  await open(page);
  await page.setViewportSize({ width: 1000, height: 700 });
  const closed = await fit(page);
  expect(closed.overflow).toBeLessThanOrEqual(0);
  expect(closed.keysHeight).toBeLessThan(24);
  expect(closed.overlaps).toEqual([]);
});
