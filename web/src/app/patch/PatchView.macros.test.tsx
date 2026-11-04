// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { macrosFixture, nodeNamed } from "../../client/fixtures";
import { MockServer } from "../../client/mock";
import type { Intent } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// A macro that has a name and a choice of whether the box of its module shows its knob (step 110): what its node says and asks, and what the box of a module that holds macros shows. The session of the macros fixture: a lane
// (0) with a filter (1) that an LFO (2) drives, a macro that is not called anything (3: `Macro`) and a macro that the file calls `Wet` and hides from a box (4).
const LFO = 2;
const KNOB = 3;
const WET = 4;
const session = { snapshot: macrosFixture.snapshot };

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

function Notices() {
  return <NoticeBoard notices={useNotices()} />;
}

const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);

/** Let what the page is going to do have its turn: a request leaves a moment after the click that makes it, so that "nothing was asked" is true of a moment that is too short. */
const settle = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

describe("the node of a macro", () => {
  test("has a button that renames it and a choice of whether a box shows its knob, which is on unless the file says not, and no other modulator has either", async () => {
    await rig(<PatchView />, session);
    const knob = within(await nodeOf(KNOB));
    expect(knob.getByRole("button", { name: "Rename Macro" })).toBeTruthy();
    expect(knob.getByRole<HTMLSelectElement>("combobox", { name: "Expose" }).value).toBe("1");
    expect(Array.from(knob.getByRole<HTMLSelectElement>("combobox", { name: "Expose" }).options).map((option) => option.textContent)).toEqual(["off", "on"]);
    // The one that the file hides is off, and is called by the name that the file gives it, with the kind of modulator it is as the badge.
    const wet = within(await nodeOf(WET));
    expect(wet.getByRole<HTMLSelectElement>("combobox", { name: "Expose" }).value).toBe("0");
    expect(wet.getByRole("button", { name: "Rename Wet" })).toBeTruthy();
    expect(wet.getByText("Macro", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(wet.getByText("Wet", { selector: ".tang-node__name" })).toBeTruthy();
    // (An LFO is called what it is, and nothing renames it.)
    const lfo = within(await nodeOf(LFO));
    expect(lfo.queryByRole("button", { name: /^Rename / })).toBeNull();
    expect(lfo.queryByRole("combobox", { name: "Expose" })).toBeNull();
    expect(screen.getAllByRole("button", { name: /^Rename / })).toHaveLength(2);
    expect(screen.getAllByRole("combobox", { name: "Expose" })).toHaveLength(2);
  });

  test("turns its name into a field with the name in it, selected, that takes as many characters as a name can have, and asks nothing until a name is given", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(KNOB)).getByRole("button", { name: "Rename Macro" }));
    const field = await within(await nodeOf(KNOB)).findByRole<HTMLInputElement>("textbox", { name: "Name of Macro" });
    expect(field.value).toBe("Macro");
    expect(field.maxLength).toBe(64);
    expect(document.activeElement).toBe(field);
    expect([field.selectionStart, field.selectionEnd]).toEqual([0, 5]);
    await settle();
    expect(requests()).toEqual([]);
  });

  test("is called by the name that was typed when Enter is pressed, and the kind of modulator it is is its badge from then on", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(KNOB)).getByRole("button", { name: "Rename Macro" }));
    const field = await within(await nodeOf(KNOB)).findByRole<HTMLInputElement>("textbox", { name: "Name of Macro" });
    fireEvent.change(field, { target: { value: "Brightness" } });
    // (The key is taken, so that the button that has the focus then is not pressed by it.)
    expect(fireEvent.keyDown(field, { key: "Enter" })).toBe(false);
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "renameMacro", node: KNOB, name: "Brightness" }]);
    });
    const renamed = await within(await nodeOf(KNOB)).findByRole("button", { name: "Rename Brightness" });
    expect(within(await nodeOf(KNOB)).getByText("Brightness", { selector: ".tang-node__name" })).toBeTruthy();
    expect(within(await nodeOf(KNOB)).getByText("Macro", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(KNOB)).queryByRole("textbox")).toBeNull();
    await waitFor(() => {
      expect(document.activeElement).toBe(renamed);
    });
  });

  test("asks nothing when Escape is pressed or the name is the one it was, and asks for the name when the field is left, without taking the focus back", async () => {
    const { requests } = await rig(<PatchView />, session);
    const open = async (name: string): Promise<HTMLInputElement> => {
      fireEvent.click(within(await nodeOf(KNOB)).getByRole("button", { name: `Rename ${name}` }));
      return within(await nodeOf(KNOB)).findByRole<HTMLInputElement>("textbox", { name: `Name of ${name}` });
    };
    let field = await open("Macro");
    fireEvent.change(field, { target: { value: "Brightness" } });
    expect(fireEvent.keyDown(field, { key: "Escape" })).toBe(false);
    expect(await within(await nodeOf(KNOB)).findByRole("button", { name: "Rename Macro" })).toBeTruthy();
    field = await open("Macro");
    fireEvent.keyDown(field, { key: "Enter" });
    expect(await within(await nodeOf(KNOB)).findByRole("button", { name: "Rename Macro" })).toBeTruthy();
    await settle();
    expect(requests()).toEqual([]);
    field = await open("Macro");
    fireEvent.change(field, { target: { value: "Wet 2" } });
    fireEvent.blur(field);
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "renameMacro", node: KNOB, name: "Wet 2" }]);
    });
    const renamed = await within(await nodeOf(KNOB)).findByRole("button", { name: "Rename Wet 2" });
    await settle();
    expect(document.activeElement).not.toBe(renamed);
  });

  test("is called by what it was before when the controller refuses the name, and the page says why", async () => {
    const { requests } = await rig(
      <>
        <PatchView />
        <Notices />
      </>,
      session,
    );
    fireEvent.click(within(await nodeOf(KNOB)).getByRole("button", { name: "Rename Macro" }));
    const field = await within(await nodeOf(KNOB)).findByRole<HTMLInputElement>("textbox", { name: "Name of Macro" });
    // (A tab: a field of one line drops a line break from what is typed in it, and the name would not be what is asked.)
    fireEvent.change(field, { target: { value: "tab\there" } });
    fireEvent.keyDown(field, { key: "Enter" });
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "renameMacro", node: KNOB, name: "tab\there" }]);
    });
    expect(await screen.findByText(/a name has no control characters/)).toBeTruthy();
    expect(within(await nodeOf(KNOB)).getByText("Macro", { selector: ".tang-node__name" })).toBeTruthy();
  });

  test("is shown on a box or not as the choice says, which asks the controller and shows what it answers", async () => {
    const { requests } = await rig(<PatchView />, session);
    const choice = async (id: number): Promise<HTMLSelectElement> => within(await nodeOf(id)).findByRole<HTMLSelectElement>("combobox", { name: "Expose" });
    fireEvent.change(await choice(KNOB), { target: { value: "0" } });
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "exposeMacro", node: KNOB, expose: false }]);
    });
    await waitFor(async () => {
      expect((await choice(KNOB)).value).toBe("0");
    });
    fireEvent.change(await choice(WET), { target: { value: "1" } });
    await waitFor(() => {
      expect(asked(requests)).toEqual([
        { type: "exposeMacro", node: KNOB, expose: false },
        { type: "exposeMacro", node: WET, expose: true },
      ]);
    });
    await waitFor(async () => {
      expect((await choice(WET)).value).toBe("1");
    });
  });

  test("shows a name and a flag that another client or an undo gave it", async () => {
    const { server } = await rig(<PatchView />, session);
    server.apply({ type: "renameMacro", node: KNOB, name: "Cutoff" });
    expect(await within(await nodeOf(KNOB)).findByRole("button", { name: "Rename Cutoff" })).toBeTruthy();
    server.apply({ type: "exposeMacro", node: KNOB, expose: false });
    await waitFor(async () => {
      expect((await within(await nodeOf(KNOB)).findByRole<HTMLSelectElement>("combobox", { name: "Expose" })).value).toBe("0");
    });
    server.apply({ type: "undo" });
    server.apply({ type: "undo" });
    expect(await within(await nodeOf(KNOB)).findByRole("button", { name: "Rename Macro" })).toBeTruthy();
    await waitFor(async () => {
      expect((await within(await nodeOf(KNOB)).findByRole<HTMLSelectElement>("combobox", { name: "Expose" })).value).toBe("1");
    });
  });
});

describe("the box of a module that has macros in it", () => {
  /** The session with the filter and both macros collapsed into a module called Room. */
  const collapsed = (): { snapshot: typeof macrosFixture.snapshot } => {
    const before = new MockServer({ snapshot: macrosFixture.snapshot });
    before.apply({ type: "collapseIntoModule", nodes: [1, KNOB, WET], name: "Room", ui: null });
    return { snapshot: before.snapshot };
  };
  const knobs = (): HTMLElement | null => screen.queryByRole("group", { name: "Knobs of Room" });
  /** The group of knobs that the box has: it is there when this is asked for. */
  const grid = (): HTMLElement => {
    const found = knobs();
    if (!found) throw new Error("the box has no knobs");
    return found;
  };
  const sliders = (): string[] => (knobs() ? within(grid()).getAllByRole("slider").map((slider) => slider.getAttribute("aria-label") ?? "?") : []);

  test("shows the knob of each macro in it that is not hidden, in a group of its own, called by the macro's name, and the macros themselves are not on the canvas", async () => {
    await rig(<PatchView />, collapsed());
    await screen.findByRole("button", { name: "Open Room" });
    expect(sliders()).toEqual(["Macro"]);
    // (The one that the file hides is not among them, and neither macro is a node of the canvas: they are in the box.)
    expect(screen.queryByTestId(`rf__node-${String(KNOB)}`)).toBeNull();
    expect(screen.queryByTestId(`rf__node-${String(WET)}`)).toBeNull();
    // The knob shows the macro's value, which the file gave it.
    const value = within(grid()).getByRole("slider", { name: "Macro" });
    expect(value.getAttribute("aria-valuetext")).toBe("0.5");
  });

  test("turns the macro's own value when the knob is turned, by the macro's node, as the knob of its own node does", async () => {
    const { requests, clock } = await rig(<PatchView />, collapsed());
    await screen.findByRole("button", { name: "Open Room" });
    fireEvent.keyDown(within(grid()).getByRole("slider", { name: "Macro" }), { key: "ArrowUp" });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "setModulatorField", node: KNOB, field: "value", value: expect.closeTo(0.51, 9) as unknown }]);
    });
  });

  test("follows what the macros are called and whether they are shown: a name is the knob's, a macro that is shown again has its knob, and a box with none has no grid", async () => {
    const { server } = await rig(<PatchView />, collapsed());
    await screen.findByRole("button", { name: "Open Room" });
    server.apply({ type: "renameMacro", node: KNOB, name: "Brightness" });
    await waitFor(() => {
      expect(sliders()).toEqual(["Brightness"]);
    });
    server.apply({ type: "exposeMacro", node: WET, expose: true });
    await waitFor(() => {
      expect(sliders()).toEqual(["Brightness", "Wet"]);
    });
    server.apply({ type: "exposeMacro", node: KNOB, expose: false });
    await waitFor(() => {
      expect(sliders()).toEqual(["Wet"]);
    });
    server.apply({ type: "exposeMacro", node: WET, expose: false });
    await waitFor(() => {
      expect(knobs()).toBeNull();
    });
    // The box says what is in it, which the macros that it does not show are still part of.
    expect(screen.getByText("2 plugins · 2 modulators")).toBeTruthy();
  });

  test("is as tall as the layout says, with the knob in the grid that a node's controls are in: a row of them under the ports", async () => {
    await rig(<PatchView />, collapsed());
    await screen.findByRole("button", { name: "Open Room" });
    const group = grid();
    expect(group.classList.contains("tang-node__controls")).toBe(true);
    expect(group.closest(".tang-node--module")).not.toBeNull();
    // (The ports, and then the knobs, and then the line that says what is in the box.)
    const parts = Array.from(group.closest(".tang-node--module")?.children ?? []).map((child) => child.className.split(" ")[0]);
    expect(parts.slice(-3)).toEqual(["tang-box__ports", "tang-node__controls", "tang-node__detail"]);
  });

  test("has a port under each knob that a cable is started at, and a menu in its corner that draws one from the macro, to what the macro can still drive, which the session says and the closed box does not draw (step 111)", async () => {
    const { requests } = await rig(<PatchView />, collapsed());
    await screen.findByRole("button", { name: "Open Room" });
    const cell = grid().querySelector<HTMLElement>(".tang-cell");
    if (!cell) throw new Error("the knob has a cell");
    // The port is the macro's own output, named for the macro: the one handle that the knob has, and the box has no other for it.
    const port = cell.querySelector<HTMLElement>(".react-flow__handle");
    expect(port?.getAttribute("data-handleid")).toBe(`modOut@${String(KNOB)}`);
    expect(port?.classList.contains("connectablestart")).toBe(true);
    expect(port?.classList.contains("connectableend")).toBe(false);
    expect(document.querySelectorAll(`[data-handleid="modOut@${String(KNOB)}"]`)).toHaveLength(1);
    // The macro that does not show its knob has no port on the box (its cables, if it had any, would be drawn from the bottom edge).
    expect(document.querySelectorAll(`[data-handleid="modOut@${String(WET)}"]`)).toHaveLength(0);
    // The menu lists the plugins of the session, the filter that is in the module as well, and the settings of the LFO that is not.
    fireEvent.click(within(cell).getByRole("button", { name: "Add a cable from Macro" }));
    const menu = await screen.findByRole("menu", { name: "Add a cable from Macro" });
    const groups = within(menu).getAllByRole("group").map((group) => group.getAttribute("aria-label"));
    expect(groups).toEqual(expect.arrayContaining(["Filter", "LFO"]));
    const resonance = nodeNamed(macrosFixture.snapshot, "Filter");
    const index = resonance.kind === "effect" ? resonance.plugin.params.find((param) => param.name === "resonance")?.index : undefined;
    expect(index).toBeDefined();
    fireEvent.click(within(within(menu).getByRole("group", { name: "Filter" })).getByRole("menuitem", { name: "resonance" }));
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "addModEdge", modulator: KNOB, to: { node: resonance.id, port: { type: "param", index } } }]);
    });
  });

  test("opens into the nodes that are in it, where each macro has its own knob and choice, and the box has none; it closes again with the knobs back", async () => {
    await rig(<PatchView />, collapsed());
    fireEvent.click(await screen.findByRole("button", { name: "Open Room" }));
    const macro = await nodeOf(KNOB);
    expect(within(macro).getByRole("slider", { name: "Value" })).toBeTruthy();
    expect(within(macro).getByRole("combobox", { name: "Expose" })).toBeTruthy();
    expect(knobs()).toBeNull();
    expect(screen.queryByRole("button", { name: "Open Room" })).toBeNull();
  });
});
