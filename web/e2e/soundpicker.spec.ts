import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

// The picker of sounds (step 116) against the mock engine in the page, which says (`?open=1`) that its program can open another session and lists two libraries of sounds: in a real browser, what the picker is like in a window of the
// size that a plugin's is (a window that a host gives it can be small), that the list scrolls and keeps the sound that an arrow goes to in view, and that what is typed in it is not the page's (a letter is a note on the piano).

const dialog = (page: Page) => page.getByRole("dialog", { name: "Open a sound" });

async function opened(page: Page, width: number, height: number): Promise<void> {
  await page.setViewportSize({ width, height });
  await page.goto("/?engine=mock&open=1");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await page.getByRole("button", { name: "Open…" }).click();
  await expect(page.getByRole("dialog", { name: "Open a sound" })).toBeVisible();
  await expect(dialog(page).getByRole("option")).toHaveCount(16);
}

for (const [width, height] of [
  [1280, 800],
  [900, 600],
  [640, 420],
] as const) {
  test(`fits a window of ${String(width)} by ${String(height)}: the box and its buttons are in the window, the page does not scroll sideways, and the list scrolls when it is longer than the room`, async ({ page }) => {
    await opened(page, width, height);
    const box = await dialog(page).boundingBox();
    expect(box).not.toBeNull();
    if (!box) return;
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(width);
    expect(box.y + box.height).toBeLessThanOrEqual(height);
    for (const name of ["Cancel", "Open", "From this computer…"]) {
      const button = await dialog(page).getByRole("button", { name, exact: true }).boundingBox();
      expect(button, name).not.toBeNull();
      expect((button?.y ?? 0) + (button?.height ?? 0), name).toBeLessThanOrEqual(height);
      expect((button?.x ?? 0) + (button?.width ?? 0), name).toBeLessThanOrEqual(width);
    }
    expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)).toBe(0);
    const list = await dialog(page).locator(".tang-picker__libraries").evaluate((element) => ({ scrolls: element.scrollHeight > element.clientHeight, sideways: element.scrollWidth > element.clientWidth }));
    expect(list.sideways, "the list does not scroll sideways").toBe(false);
    if (height <= 600) expect(list.scrolls, "and scrolls when there is not the room").toBe(true);
  });
}

test("brings the sound that an arrow goes to into view, and the first and the last of the list can both be reached by the keys", async ({ page }) => {
  await opened(page, 900, 600);
  const list = dialog(page).locator(".tang-picker__libraries");
  const inView = async (name: string): Promise<boolean> => {
    const row = await dialog(page).getByRole("option").filter({ hasText: name }).first().boundingBox();
    const area = await list.boundingBox();
    if (!row || !area) return false;
    return row.y >= area.y - 1 && row.y + row.height <= area.y + area.height + 1;
  };
  expect(await inView("session-19eb859829d")).toBe(true);
  expect(await inView("lost-bird")).toBe(false);
  for (let at = 0; at < 14; at += 1) await page.keyboard.press("ArrowDown");
  await expect(dialog(page).getByRole("option", { selected: true })).toContainText("lost-bird");
  expect(await inView("lost-bird")).toBe(true);
  for (let at = 0; at < 20; at += 1) await page.keyboard.press("ArrowUp");
  await expect(dialog(page).getByRole("option", { selected: true })).toContainText("session-19eb859829d");
  expect(await inView("session-19eb859829d")).toBe(true);
});

test("finds a sound by what is typed in the field, which has the focus when the picker opens, and Enter opens it and closes the picker", async ({ page }) => {
  await opened(page, 900, 600);
  await expect(dialog(page).getByRole("searchbox", { name: "Search sounds" })).toBeFocused();
  await page.keyboard.type("wobble");
  await expect(dialog(page).getByRole("option")).toHaveCount(1);
  await expect(dialog(page).getByRole("option")).toContainText("wobble");
  await page.keyboard.press("Enter");
  await expect(dialog(page)).toHaveCount(0);
  // (A press on the button with the mouse gives it the focus, and the picker gives it back.)
  await expect(page.getByRole("button", { name: "Open…" })).toBeFocused();
});

test("is left by Escape and by a press beside the box, with nothing opened", async ({ page }) => {
  await opened(page, 900, 600);
  await page.keyboard.press("Escape");
  await expect(dialog(page)).toHaveCount(0);
  await page.getByRole("button", { name: "Open…" }).click();
  await expect(dialog(page)).toBeVisible();
  await page.mouse.click(8, 8);
  await expect(dialog(page)).toHaveCount(0);
});

test("keeps the keys typed in it from the page: a letter pressed on one of its buttons is no note on the piano", async ({ page }) => {
  await opened(page, 900, 600);
  // (Q is the C of the piano of the computer's keys: C4, which the piano of the page lights while it is held.)
  const c4 = page.locator('[data-note="60"]');
  await page.keyboard.press("Escape");
  await page.keyboard.down("q");
  await expect(c4, "with the picker away a letter is a note").toHaveAttribute("data-held", "");
  await page.keyboard.up("q");
  await expect(c4).not.toHaveAttribute("data-held", "");
  // With it open, a letter on one of its buttons is not: a button takes no text, so the page would have played it.
  await page.getByRole("button", { name: "Open…" }).click();
  await dialog(page).getByRole("button", { name: "Cancel" }).focus();
  await page.keyboard.down("q");
  await page.waitForTimeout(400);
  await expect(c4).not.toHaveAttribute("data-held", "");
  await page.keyboard.up("q");
});

test("a press on a sound marks it, and the sound that cannot be opened cannot be marked", async ({ page }) => {
  await opened(page, 900, 600);
  const kick = dialog(page).getByRole("option").filter({ hasText: "session-q" });
  await kick.click();
  await expect(kick).toHaveAttribute("aria-selected", "true");
  const notes = dialog(page).getByRole("option").filter({ hasText: "notes" });
  await expect(notes).toHaveAttribute("aria-disabled", "true");
  // (Forced: Playwright does not click what says that it is disabled, and a person can.)
  await notes.click({ force: true });
  await expect(notes).toHaveAttribute("aria-selected", "false");
  await expect(kick).toHaveAttribute("aria-selected", "true");
});
