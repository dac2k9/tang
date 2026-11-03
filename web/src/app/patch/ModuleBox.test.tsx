// @vitest-environment jsdom
import { ReactFlow, ReactFlowProvider } from "@xyflow/react";
import type { NodeTypes } from "@xyflow/react";
import { cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { rig } from "../testing";
import type { Box } from "./boxes";
import { ConnectingContext } from "./connection";
import { ModuleNode } from "./ModuleBox";
import { OpenModulesContext } from "./openModules";
import type { PatchNode } from "./toFlow";

afterEach(cleanup);

// A module that is closed, as one node (step 108): the ports of the module on its edges, each the port of a node inside, and what its buttons ask of the canvas.

const box: Box = {
  node: 1,
  plugins: 3,
  modulators: 1,
  inputs: [
    { member: 5, handle: "audioIn", label: "Filter", kind: "audio" },
    { member: 7, handle: "eventsIn", label: "Oscillator", kind: "notes" },
  ],
  outputs: [{ member: 6, handle: "audioOut", label: "Delay", kind: "audio" }],
  modIn: [
    { member: 5, handle: "param-1", label: "Filter: cutoff", kind: "modulation" },
    { member: 5, handle: "param-3", label: "Filter: resonance", kind: "modulation" },
  ],
  modOut: [{ member: 9, handle: "modOut", label: "LFO", kind: "modulation" }],
};

const node: PatchNode = {
  id: "1",
  type: "module",
  position: { x: 0, y: 0 },
  data: { node: 1, name: "Room", format: null, knobs: [], modulated: [], targets: [], drivenSettings: [], cables: [], notes: false, groupCell: false, scope: false, source: null, heldIn: null, side: null, box, module: null },
};

const NODE_TYPES: NodeTypes = { module: ModuleNode };

async function drawn(connecting: ReadonlySet<string> | null = null, toggle: (node: number) => void = () => undefined) {
  const { view } = await rig(
    <ReactFlowProvider>
      <OpenModulesContext.Provider value={{ open: new Set(), toggle }}>
        <ConnectingContext.Provider value={connecting}>
          <div style={{ width: 800, height: 600 }}>
            <ReactFlow nodes={[node]} edges={[]} nodeTypes={NODE_TYPES} />
          </div>
        </ConnectingContext.Provider>
      </OpenModulesContext.Provider>
    </ReactFlowProvider>,
  );
  await screen.findByTestId("rf__node-1");
  const ports = Array.from(view.container.querySelectorAll<HTMLElement>(".react-flow__handle"));
  const named = (id: string): HTMLElement => {
    const found = ports.find((port) => port.getAttribute("data-handleid") === id);
    if (!found) throw new Error(`no port ${id}`);
    return found;
  };
  return { view, ports, named };
}

describe("the ports of a box", () => {
  test("are a handle for each port of the module, named after the port and the node inside that has it, on the edge that its kind is on: inputs left, outputs right, modulation along the top and the bottom", async () => {
    const { ports } = await drawn();
    expect(ports.map((port) => [port.getAttribute("data-handleid"), port.getAttribute("data-handlepos")])).toEqual([
      ["audioIn@5", "left"],
      ["eventsIn@7", "left"],
      ["audioOut@6", "right"],
      ["param-1@5", "top"],
      ["param-3@5", "top"],
      ["modOut@9", "bottom"],
    ]);
  });

  test("take the cables that end at an input, and start one at an output, and a cable of modulation is started at none (it is drawn from the node inside, once the module is open)", async () => {
    const { named } = await drawn();
    for (const id of ["audioIn@5", "eventsIn@7", "param-1@5", "param-3@5"]) {
      expect(named(id).classList.contains("connectableend"), id).toBe(true);
      expect(named(id).classList.contains("connectablestart"), id).toBe(false);
    }
    expect(named("audioOut@6").classList.contains("connectablestart")).toBe(true);
    expect(named("audioOut@6").classList.contains("connectableend")).toBe(false);
    expect(named("modOut@9").classList.contains("connectablestart")).toBe(false);
    expect(named("modOut@9").classList.contains("connectableend")).toBe(false);
  });

  test("have the looks of the ports that are of their kind: audio, notes, and the cables of modulation", async () => {
    const { named } = await drawn();
    expect(named("audioIn@5").classList.contains("tang-port--audio")).toBe(true);
    expect(named("audioIn@5").classList.contains("tang-port--in")).toBe(true);
    expect(named("audioOut@6").classList.contains("tang-port--out")).toBe(true);
    expect(named("eventsIn@7").classList.contains("tang-port--notes")).toBe(true);
    expect(named("param-1@5").classList.contains("tang-port--param")).toBe(true);
    expect(named("param-1@5").classList.contains("tang-port--driven")).toBe(true);
    expect(named("modOut@9").classList.contains("tang-port--mod")).toBe(true);
  });

  test("are each in the row of its label, a row of 28 px under the header, at the middle of it, and the output beside the first of the inputs", async () => {
    const { named } = await drawn();
    // (The first row's middle is the header (40) and half a row (14): 54; the second 28 further.)
    expect(named("audioIn@5").style.top).toBe("54px");
    expect(named("eventsIn@7").style.top).toBe("82px");
    expect(named("audioOut@6").style.top).toBe("54px");
  });

  test("are told by the node inside that has them, and not by the box: the box takes a cable that the node inside would, and the port of another node with the same handle does not", async () => {
    const { named } = await drawn(new Set(["5:audioIn", "7:eventsIn", "5:param-3", "6:audioIn", "1:audioIn@5", "1:param-1@5", "7:audioIn"]));
    const takes = (id: string): boolean => named(id).classList.contains("tang-port--takes");
    expect([takes("audioIn@5"), takes("eventsIn@7"), takes("param-1@5"), takes("param-3@5")]).toEqual([true, true, false, true]);
    // (An output takes no cable, a cable that is dragged ends at an input.)
    expect([takes("audioOut@6"), takes("modOut@9")]).toEqual([false, false]);
    // The input that takes the cable is the row, along the left edge, as an input is on any node.
    expect([named("audioIn@5").style.top, named("audioIn@5").style.height]).toEqual(["40px", "28px"]);
    expect([named("eventsIn@7").style.top, named("eventsIn@7").style.height]).toEqual(["68px", "28px"]);
    expect(named("param-1@5").style.height).toBe("");
  });

  test("are spread along the top edge and the bottom edge, a share of the width each, as many as there are", async () => {
    const { named } = await drawn();
    expect(Number.parseFloat(named("param-1@5").style.left)).toBeCloseTo(100 / 3, 3);
    expect(Number.parseFloat(named("param-3@5").style.left)).toBeCloseTo(200 / 3, 3);
    expect(Number.parseFloat(named("modOut@9").style.left)).toBeCloseTo(50, 3);
  });
});

describe("a box", () => {
  test("says what is in it and has a row for each input and each output, the notes marked, a title for each port and the badge of what it is", async () => {
    const { view, named } = await drawn();
    expect(within(view.container).getByText("Room")).toBeTruthy();
    expect(within(view.container).getByText("Module")).toBeTruthy();
    expect(within(view.container).getByText("3 plugins · 1 modulator")).toBeTruthy();
    const rows = within(within(view.container).getByRole("list", { name: "Ports of Room" })).getAllByRole("listitem");
    expect(rows.map((row) => row.textContent)).toEqual(["FilterDelay", "♪ Oscillator"]);
    expect(named("audioIn@5").getAttribute("title")).toBe("Filter");
    expect(named("param-3@5").getAttribute("title")).toBe("Filter: resonance");
    expect(named("modOut@9").getAttribute("title")).toBe("LFO");
    // (The number of rows is what the style says it is: as tall as the layout assumes.)
    expect(view.container.querySelector<HTMLElement>(".tang-node--module")?.style.getPropertyValue("--box-rows")).toBe("2");
  });

  test("is opened by its button, which asks the canvas for the module, and says so to a screen reader", async () => {
    const asked: number[] = [];
    const { view } = await drawn(null, (module) => asked.push(module));
    const button = within(view.container).getByRole("button", { name: "Open Room" });
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button.getAttribute("title")).toBe("Show what is in Room: 3 plugins · 1 modulator");
    fireEvent.click(button);
    expect(asked).toEqual([1]);
  });

  test("is nothing at all when there is no box to draw: a node of the kind that has none", async () => {
    const empty: PatchNode = { ...node, data: { ...node.data, box: null } };
    const { view } = await rig(
      <ReactFlowProvider>
        <div style={{ width: 800, height: 600 }}>
          <ReactFlow nodes={[empty]} edges={[]} nodeTypes={NODE_TYPES} />
        </div>
      </ReactFlowProvider>,
    );
    await screen.findByTestId("rf__node-1");
    expect(view.container.querySelector(".tang-node--module")).toBeNull();
  });
});
