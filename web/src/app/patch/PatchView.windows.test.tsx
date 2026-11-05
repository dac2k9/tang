// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Intent, NodeId, Snapshot } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useEditor, useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The button that opens a plugin's own window (step 112): on a node whose plugin has one, on a server whose machine can show it. The tracer has an oscillator (an instrument) and a filter (an effect) as plugins;
// each test says which of them has a window of its own.
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

/** The tracer, with the plugins of the nodes `ids` saying that they have a window of their own. */
function windowed(...ids: NodeId[]): Snapshot {
  const copy = structuredClone(tracerSnapshot);
  for (const found of copy.graph.nodes) {
    if ((found.kind === "instrument" || found.kind === "effect") && found.plugin && ids.includes(found.id)) found.plugin.hasEditor = true;
  }
  return copy;
}

const nodeOf = (id: NodeId): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const opener = (node: HTMLElement, plugin: string): HTMLElement => within(node).getByRole("button", { name: `Open the window of ${plugin}` });
const noOpener = (node: HTMLElement): boolean => within(node).queryByRole("button", { name: /^Open the window of / }) === null;

/** What the page says of what it could not do: a notice on the board (the board is a live region that has no role of its own to ask for). */
const noticeTexts = (): string[] => Array.from(document.querySelectorAll(".tang-notice__text")).map((notice) => notice.textContent);

/** The board of notices, and a button that asks for a window directly, as the page's code would: the way to have a refusal to see what the board shows of one. */
function Notices({ ask }: { ask: NodeId }) {
  const editor = useEditor();
  return (
    <>
      <NoticeBoard notices={useNotices()} />
      <button
        type="button"
        onClick={() => {
          void editor.openEditor(ask);
        }}
      >
        ask directly
      </button>
    </>
  );
}

const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);

/** Let what the page is going to do have its turn: a request leaves a moment after the click that makes it, and its answer comes a moment after. */
const settle = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

describe("the button that opens a plugin's own window", () => {
  test("is on the node of an instrument whose plugin has a window, and not on the node of an effect whose plugin has none", async () => {
    await rig(<PatchView />, { snapshot: windowed(oscillator.id), windows: true });
    expect(opener(await nodeOf(oscillator.id), "Oscillator").id).toBe(`window-${String(oscillator.id)}`);
    expect(noOpener(await nodeOf(filter.id))).toBe(true);
  });

  test("is on the node of an effect whose plugin has a window, and not on the node of an instrument whose plugin has none", async () => {
    await rig(<PatchView />, { snapshot: windowed(filter.id), windows: true });
    expect(opener(await nodeOf(filter.id), "Filter").id).toBe(`window-${String(filter.id)}`);
    expect(noOpener(await nodeOf(oscillator.id))).toBe(true);
  });

  test("is on no node of the modulators and the master, whatever the session says", async () => {
    const lfo = nodeNamed(tracerSnapshot, "LFO");
    await rig(<PatchView />, { snapshot: windowed(oscillator.id, filter.id, lfo.id), windows: true });
    expect(noOpener(await nodeOf(lfo.id))).toBe(true);
    expect(noOpener(await nodeOf(4294967295))).toBe(true);
  });

  test("is on no node while the machine that runs Tang cannot show a window, whatever the plugins say", async () => {
    await rig(<PatchView />, { snapshot: windowed(oscillator.id, filter.id) });
    expect(noOpener(await nodeOf(oscillator.id))).toBe(true);
    expect(noOpener(await nodeOf(filter.id))).toBe(true);
  });

  test("asks for the plugin's window each time it is pressed, and says nothing of a window that opened", async () => {
    const { requests } = await rig(
      <>
        <PatchView />
        <Notices ask={filter.id} />
      </>,
      { snapshot: windowed(oscillator.id), windows: true },
    );
    const button = opener(await nodeOf(oscillator.id), "Oscillator");
    fireEvent.click(button);
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "openEditor", node: oscillator.id }]);
    });
    await settle();
    expect(noticeTexts()).toEqual([]);
    // Pressed again, it asks again (the server brings a window that is open to the front).
    fireEvent.click(button);
    await waitFor(() => {
      expect(asked(requests)).toEqual([
        { type: "openEditor", node: oscillator.id },
        { type: "openEditor", node: oscillator.id },
      ]);
    });
    await settle();
    expect(noticeTexts()).toEqual([]);
    // (The board does show a refusal: the filter's plugin has no window, and the page asked for it anyway.)
    fireEvent.click(screen.getByRole("button", { name: "ask directly" }));
    await waitFor(() => {
      expect(noticeTexts()).toEqual(["Could not open the window of Filter: Filter has no window of its own"]);
    });
  });

  test("is in the menu of the node's right click too, before its panel of parameters", async () => {
    await rig(<PatchView />, { snapshot: windowed(oscillator.id), windows: true });
    fireEvent.contextMenu(within(await nodeOf(oscillator.id)).getByText("Oscillator", { selector: ".tang-node__name" }));
    // (A switch that is on or off, "Parameters…", is a menu item that can be checked: its role is not the plain one.)
    const menu = await screen.findByRole("menu");
    const items = Array.from(menu.querySelectorAll('[role^="menuitem"]')).map((item) => item.textContent);
    expect(items).toContain("Open its own window");
    expect(items).toContain("Parameters…");
    expect(items.indexOf("Open its own window")).toBeLessThan(items.indexOf("Parameters…"));
  });

  test("is not in the menu of a node whose plugin has no window", async () => {
    await rig(<PatchView />, { snapshot: windowed(oscillator.id), windows: true });
    fireEvent.contextMenu(within(await nodeOf(filter.id)).getByText("Filter", { selector: ".tang-node__name" }));
    const menu = await screen.findByRole("menu");
    const items = Array.from(menu.querySelectorAll('[role^="menuitem"]')).map((item) => item.textContent);
    expect(items).toContain("Parameters…");
    expect(items).not.toContain("Open its own window");
  });
});
