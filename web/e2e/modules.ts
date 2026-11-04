import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { node, nodeBoxes, still } from "./canvas";

// What the tests of modules have in common (docs/web-ui.md D104 to D108 and D111): the bar's "+ Effect" and "+ Module", saving the chain of a lane or the nodes that a box is drawn round, and adding what was saved.

/** The button in the bar that adds an effect. */
export const addEffect = (page: Page) => page.getByRole("button", { name: "Add an effect", exact: true });

/** The button in the bar that adds a module (step 111: the modules that were saved are listed here, and not in the effects'). */
export const addModuleButton = (page: Page) => page.getByRole("button", { name: "Add a module", exact: true });

/** The names of the nodes of the canvas, in the order of the page. */
export const names = (page: Page): Promise<string[]> => page.locator(".tang-node__name").allTextContents();

/** Save the chain of the lane called `lane` as `name`, with the field that its button opens. */
export async function save(page: Page, lane: string, name: string): Promise<void> {
  await page.getByRole("button", { name: `Save the chain of ${lane} as a module` }).click();
  const dialog = page.getByRole("dialog", { name: `Save the chain of ${lane} as a module` });
  await expect(dialog.getByRole("textbox", { name: "Module name" })).toBeFocused();
  await dialog.getByRole("textbox", { name: "Module name" }).fill(name);
  await dialog.getByRole("textbox", { name: "Module name" }).press("Enter");
  await expect(page.getByText(`Saved the module "${name}": + Module adds it`)).toBeVisible();
  await expect(dialog).toHaveCount(0);
}

/** The button that turns choosing on, and the one that saves what is chosen. */
export const choose = (page: Page) => page.getByRole("button", { name: "Choose nodes to save as a module" });

/** Add the effect called `plugin` from the bar's "+ Effect", and wait for its node (`name`) once the canvas has stopped moving. */
export async function addFree(page: Page, plugin: string, name: string): Promise<void> {
  await addEffect(page).click();
  await page.getByRole("menuitem", { name: plugin, exact: true }).click();
  await expect(node(page, name)).toBeVisible();
  await still(node(page, name));
}

/** Draw a box on the canvas round the nodes called `names`, from a place above and to the left of them to one below and to the right, which has no other node in it (choosing is on). */
export async function boxRound(page: Page, names: string[]): Promise<void> {
  const boxes = (await nodeBoxes(page)).filter((box) => names.includes(box.name));
  expect(boxes).toHaveLength(names.length);
  const left = Math.min(...boxes.map((box) => box.left)) - 12;
  const top = Math.min(...boxes.map((box) => box.top)) - 12;
  const right = Math.max(...boxes.map((box) => box.left + box.width)) + 12;
  const bottom = Math.max(...boxes.map((box) => box.top + box.height)) + 12;
  await page.mouse.move(left, top);
  await page.mouse.down();
  await page.mouse.move((left + right) / 2, (top + bottom) / 2, { steps: 6 });
  await page.mouse.move(right, bottom, { steps: 6 });
  await page.mouse.up();
}

/** Save what is chosen as `name` from the field that the bar's button opens. */
export async function saveChosen(page: Page, name: string): Promise<void> {
  await page.getByRole("button", { name: "Save the chosen nodes as a module" }).click();
  const dialog = page.getByRole("dialog", { name: "Save the chosen nodes as a module" });
  await expect(dialog.getByRole("textbox", { name: "Module name" })).toBeFocused();
  await dialog.getByRole("textbox", { name: "Module name" }).fill(name);
  await dialog.getByRole("textbox", { name: "Module name" }).press("Enter");
  await expect(page.getByText(`Saved the module "${name}": + Module adds it`)).toBeVisible();
  await expect(dialog).toHaveCount(0);
}
