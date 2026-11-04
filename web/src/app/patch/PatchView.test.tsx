// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { builtinCatalog, builtinEffects, builtinPresets, fieldsFixture, MASTER, nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { decodeFrame } from "../../client/telemetry";
import type { Port, Snapshot } from "../../protocol";
import { angleOf, arcPath, pointAt } from "../../ui/knob-geometry";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { callsOf, recordingContext } from "../../ui/testing";
import { rig } from "../testing";
import * as describeModule from "./describe";
import { cableWidth } from "./describe";
import { drivesId } from "./controls";
import { PatchView } from "./PatchView";
import { sameGraphStructure } from "./toFlow";

// Wrapped, to count how often a node says what kind of modulator it is: once a render.
vi.mock("./describe", async (importOriginal) => {
  const original = await importOriginal<typeof import("./describe")>();
  return { ...original, kindOf: vi.fn(original.kindOf) };
});

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
if (filter.kind !== "effect") throw new Error("the tracer's filter is not an effect");
const filterPlugin = filter.plugin;

/** The node of the canvas that is the picture of this node of the session. */
const nodeOf = async (id: number): Promise<HTMLElement> => {
  const found = await screen.findByTestId(`rf__node-${String(id)}`);
  return found;
};

/** The button of the list of the modulator `id`, which draws a cable from it. */
const pickerOf = async (id: number): Promise<HTMLButtonElement> => {
  const node = await nodeOf(id);
  return within(node).getByRole<HTMLButtonElement>("button", { name: /^Add a cable from / });
};

/** What is on the list that is open, by the plugin or the modulator it is of (a node of the canvas is a group too). */
const listed = (): [string | null, string[]][] =>
  within(screen.getByRole("menu"))
    .getAllByRole("group")
    .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);

/** The names of the controls in a node, sliders and choices (not the list that draws a cable, which is a button), in the order they are in. */
// (The bars of a step sequencer are sliders too: they are its steps, and not its controls.)
const controlsIn = (node: HTMLElement): string[] =>
  Array.from(node.querySelectorAll('[role="slider"], select'))
    .filter((control) => !control.closest(".tang-stepbars"))
    .map((control) => control.getAttribute("aria-label") ?? control.closest("label")?.querySelector(".tang-choice__label")?.textContent ?? "?");

describe("the patch", () => {
  test("has a node for each node of the session, of its kind, with its name", async () => {
    await rig(<PatchView />);
    for (const [node, kind] of [
      [oscillator, "instrument"],
      [filter, "effect"],
      [lfo, "modulator"],
    ] as const) {
      const placed = await nodeOf(node.id);
      expect(placed.classList.contains(`react-flow__node-${kind}`)).toBe(true);
      expect(placed.querySelector(".tang-node")?.classList.contains(`tang-node--${kind}`)).toBe(true);
      expect(within(placed).getByText(node.name, { selector: ".tang-node__name" })).toBeTruthy();
    }
    expect(within(await nodeOf(oscillator.id)).getByText("Built-in")).toBeTruthy();
    expect(within(await nodeOf(4294967295)).getByText("Master")).toBeTruthy();
  });

  test("puts what kind of plugin a node has under its name in the header, in a title of its own that the buttons leave what room is left", async () => {
    await rig(<PatchView />);
    for (const [id, name, badge, buttons] of [
      // (A lane has a button after the one that sends its audio: the one that takes notes into it, from the sources; an effect has no more.)
      [oscillator.id, "Oscillator", "Built-in", ["tang-menu-button nodrag nopan tang-node__send", "tang-menu-button nodrag nopan tang-node__notes"]],
      [filter.id, "Filter", "Built-in", ["tang-menu-button nodrag nopan tang-node__send"]],
    ] as const) {
      const header = (await nodeOf(id)).querySelector(".tang-node__header");
      const title = header?.querySelector(".tang-node__title");
      expect(title?.querySelector(".tang-node__name")?.textContent, name).toBe(name);
      // (The badge is the second thing in the title, under the name, and the buttons that send the node's audio and take notes into it are the next, on the badge's line, which a row of buttons
      // that is full has no room for; the title is the first in the header, before the buttons.)
      expect(Array.from(title?.children ?? [], (child) => child.className), name).toEqual(["tang-node__name", "tang-node__badge", ...buttons]);
      expect(title?.querySelector(".tang-node__badge")?.textContent, name).toBe(badge);
      expect(header?.firstElementChild, name).toBe(title);
    }
    // A node that has no badge has a title that is the name, and for a modulator the menu that chooses whose notes it hears (lit: the tracer's LFO hears its lane).
    const lfoTitle = (await nodeOf(lfo.id)).querySelector(".tang-node__title");
    expect(Array.from(lfoTitle?.children ?? [], (child) => child.className)).toEqual(["tang-node__name", "tang-menu-button nodrag nopan tang-node__notes tang-node__notes--set"]);
  });

  test("shows each plugin node's parameters as controls, and its own gain after them (and a lane's keys after that, and its group only while the session has one)", async () => {
    await rig(<PatchView />);
    expect(controlsIn(await nodeOf(oscillator.id))).toEqual(["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "Volume", "Lowest key of Oscillator", "Highest key of Oscillator"]);
    expect(controlsIn(await nodeOf(filter.id))).toEqual(["cutoff", "resonance", "type", "drive", "Mix"]);
  });

  test("has a knob for what has a scale and a choice for what has names", async () => {
    await rig(<PatchView />);
    const osc = await nodeOf(oscillator.id);
    expect(within(osc).getByRole("combobox", { name: "waveform" })).toBeTruthy();
    expect(within(osc).getAllByRole("slider")).toHaveLength(8);
    const fil = await nodeOf(filter.id);
    expect(within(fil).getByRole("combobox", { name: "type" })).toBeTruthy();
    expect(within(fil).getAllByRole("slider")).toHaveLength(4);
  });

  test("draws the cables: the audio between the nodes, and the modulation, which is thicker the deeper it is", async () => {
    const { view } = await rig(<PatchView />);
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
    });
    expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(2);
    expect(Array.from(view.container.querySelectorAll(".tang-cable")).map((path) => path.id).sort()).toEqual(["audio:0>1", `audio:1>${String(MASTER)}`, "mod:0"]);
    const modulation = view.container.querySelectorAll<SVGPathElement>(".tang-cable--modulation");
    expect(modulation).toHaveLength(1);
    expect(modulation[0]?.style.strokeWidth).toBe(String(cableWidth(0.4)));
  });

  test("draws a cable as thick as it is deep as it is turned, without laying the canvas out again", async () => {
    const { view, server } = await rig(<PatchView />);
    const width = () => view.container.querySelector<SVGPathElement>(".tang-cable--modulation")?.style.strokeWidth;
    await waitFor(() => {
      expect(width()).toBe(String(cableWidth(0.4)));
    });
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: -1 });
    });
    await waitFor(() => {
      expect(width()).toBe(String(cableWidth(-1)));
    });
    expect(cableWidth(-1)).not.toBe(cableWidth(0.4));
  });

  test("puts a slider for its depth on each modulation cable, named for what the cable joins", async () => {
    await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    expect([depth.min, depth.max, depth.step, depth.value]).toEqual(["-1", "1", "0.01", "0.4"]);
    expect(depth.getAttribute("aria-valuetext")).toBe("40%");
    expect(screen.getAllByRole("slider", { name: /^Depth of / })).toHaveLength(1); // none on the audio cables
  });

  test("turns the depth of a cable on the cable: shown at once, sent on the frame as a setting of the cable, and the cable follows", async () => {
    const { clock, requests, view } = await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    fireEvent.change(depth, { target: { value: "-0.75" } });
    expect(depth.value).toBe("-0.75");
    expect(depth.getAttribute("aria-valuetext")).toBe("-75%");
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setEdgeDepth", edge: 0, depth: -0.75 } }]);
    });
    await waitFor(() => {
      expect(view.container.querySelector<SVGPathElement>(".tang-cable--modulation")?.style.strokeWidth).toBe(String(cableWidth(-0.75)));
    });
  });

  test("sends a depth when the gesture that set it ends: the key let go, the pointer let go, the focus gone (one intent a frame)", async () => {
    const { requests, clock } = await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    const ends: [string, () => void][] = [
      ["0.5", () => fireEvent.keyUp(depth, { key: "ArrowUp" })],
      ["0.6", () => fireEvent.pointerUp(depth)],
      ["0.7", () => fireEvent.blur(depth)],
    ];
    for (const [value, end] of ends) {
      fireEvent.change(depth, { target: { value } });
      end(); // (not waiting for the frame)
      await waitFor(() => {
        expect(requests().at(-1)).toEqual({ intent: { type: "setEdgeDepth", edge: 0, depth: Number(value) } });
      });
      act(() => {
        clock.tick(16 * (requests().length + 1)); // the next frame, which is the one that is allowed the next intent
      });
    }
    expect(requests()).toHaveLength(3);
  });

  test("follows a depth that somebody else sets", async () => {
    const { server } = await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: -0.2 });
    });
    await waitFor(() => {
      expect(depth.value).toBe("-0.2");
    });
    expect(depth.getAttribute("aria-valuetext")).toBe("-20%");
    // (A whole percent, though 0.29 times 100 is not quite 29.)
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: 0.29 });
    });
    await waitFor(() => {
      expect(depth.getAttribute("aria-valuetext")).toBe("29%");
    });
  });

  test("marks the port of the control that a cable ends at, and no other", async () => {
    await rig(<PatchView />);
    const fil = await nodeOf(filter.id);
    const ports = Array.from(fil.querySelectorAll(".tang-port--param"));
    expect(ports).toHaveLength(4 + 1); // one for each of the filter's parameters and one for its mix, which the host keeps
    expect(fil.querySelectorAll(".tang-port--driven")).toHaveLength(1);
    expect(fil.querySelector(".tang-port--driven")?.getAttribute("data-handleid")).toBe("param-0");
    expect((await nodeOf(oscillator.id)).querySelectorAll(".tang-port--driven")).toHaveLength(0);
  });

  test("shows the modulator for what it is, with a trace of what it puts out", async () => {
    await rig(<PatchView />);
    const mod = await nodeOf(lfo.id);
    // (An LFO is called what it is, so its kind is not said twice.)
    expect(within(mod).getByText("LFO", { selector: ".tang-node__name" })).toBeTruthy();
    expect(mod.querySelector(".tang-node__badge")).toBeNull();
    // Its settings are controls, not words: a knob for the rate, a choice for the waveform, a choice for whether a note starts it again and a knob for the fade.
    expect(controlsIn(mod)).toEqual(["Sync", "Rate", "Wave", "Retrigger", "Fade"]);
    // (The note length it follows the tempo by is chosen in the header, in a line with the name, and is not a cell of the grid: a fifth would make a second row.)
    const sync = within(mod).getByRole("combobox", { name: "Sync" });
    expect(mod.querySelector(".tang-node__header")?.contains(sync)).toBe(true);
    expect(mod.querySelectorAll(".tang-node__controls > .tang-cell")).toHaveLength(4);
    // (A choice in a line with the name and the button that takes the node out, which the canvas leaves alone when it is used and not drags the node by, and says what it is for.)
    expect(sync.closest("label")?.classList.contains("tang-choice--inline")).toBe(true);
    expect(sync.closest(".nodrag")).not.toBeNull();
    expect(sync.closest("label")?.getAttribute("title")).toMatch(/tempo/);
    expect(within(mod).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuetext")).toBe("0.5 Hz");
    expect(within(mod).getByRole<HTMLSelectElement>("combobox", { name: "Wave" }).value).toBe("0");
    expect(within(mod).getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" }).value).toBe("0");
    expect(within(mod).getByRole("slider", { name: "Fade" }).getAttribute("aria-valuetext")).toBe("none");
    expect(mod.querySelector(".tang-node__detail")).toBeNull();
    const bar = within(mod).getByRole("meter", { name: `${lfo.name} output` });
    expect(bar.getAttribute("aria-valuemin")).toBe("-1");
    expect(bar.getAttribute("aria-valuemax")).toBe("1");
    expect(bar.hasAttribute("data-centred")).toBe(true);
  });

  test("says what kind of modulator an envelope and a MIDI control are, which their names do not", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push(
      { id: 90, parent: 1, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } },
      { id: 91, parent: 1, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } },
    );
    await rig(<PatchView />, { snapshot });
    expect(within(await nodeOf(90)).getByText("Envelope", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(90)).getByText("ADSR", { selector: ".tang-node__name" })).toBeTruthy();
    expect(within(await nodeOf(91)).getByText("MIDI", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(91)).getByText("CC 74", { selector: ".tang-node__name" })).toBeTruthy();
  });

  test("tells two modulators of one name apart: each node is a group of its own name, and so are the controls in it and the cables", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    // A second LFO on the same lane, driving the same cutoff.
    snapshot.graph.nodes.push({ id: 90, parent: null, hears: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 7, retrigger: false, fade: 0, sync: 0 } });
    snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } });
    await rig(<PatchView />, { snapshot });
    const first = await screen.findByRole("group", { name: "LFO 1" });
    const second = await screen.findByRole("group", { name: "LFO 2" });
    expect(first.getAttribute("data-id")).toBe(String(lfo.id));
    expect(second.getAttribute("data-id")).toBe("90");
    // Each has its own Rate, which is told by the node it is in.
    expect(within(first).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuenow")).toBe("0.5");
    expect(within(second).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuenow")).toBe("7");
    expect(within(second).getByText("LFO 2", { selector: ".tang-node__name" })).toBeTruthy();
    expect(within(second).queryByText("LFO", { selector: ".tang-node__badge" })).toBeNull(); // (it is said by the name)
    // And each cable has a slider that says whose it is.
    expect(screen.getByRole("slider", { name: "Depth of LFO 1 → Filter: cutoff" })).toBeTruthy();
    expect(screen.getByRole("slider", { name: "Depth of LFO 2 → Filter: cutoff" })).toBeTruthy();
    expect(screen.getByRole("meter", { name: "LFO 2 output" })).toBeTruthy();
  });

  test("gives an envelope the four stages as knobs, and a MIDI control no settings but what it is bound to", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push(
      { id: 90, parent: 1, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 } },
      { id: 91, parent: 1, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } },
    );
    await rig(<PatchView />, { snapshot });
    const envelope = await nodeOf(90);
    expect(controlsIn(envelope)).toEqual(["Attack", "Decay", "Sustain", "Release"]);
    expect(Array.from(envelope.querySelectorAll('[role="slider"]')).map((knob) => knob.getAttribute("aria-valuetext"))).toEqual(["10 ms", "300 ms", "0.7", "2 s"]);
    const control = await nodeOf(91);
    expect(controlsIn(control)).toEqual([]);
    expect(control.querySelector(".tang-node__controls")).toBeNull(); // (not an empty row of them)
    expect(within(control).getByText("CC 74", { selector: ".tang-node__detail" })).toBeTruthy();
  });

  test("gives a macro a knob for its value, which is turned on its node, with no place for a cable on it and no button that modulates it, and a choice of whether the box of its module shows the knob (step 110), and says nothing of its kind twice", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push({ id: 90, parent: 1, name: "Macro", kind: "modulator", source: { type: "macro", value: 0.25 } });
    const { requests, clock } = await rig(<PatchView />, { snapshot });
    const knob = await nodeOf(90);
    expect(controlsIn(knob)).toEqual(["Value", "Expose"]);
    expect(Array.from(knob.querySelectorAll('[role="slider"]')).map((control) => control.getAttribute("aria-valuetext"))).toContain("0.25");
    // (Its name is its kind: no badge says it again.)
    expect(knob.querySelector(".tang-node__badge")).toBeNull();
    // What drives a setting of an LFO or an envelope has a place on the knob and a button that modulates it; a macro's value is turned by a person.
    expect(knob.querySelector('.react-flow__handle[data-handleid="value"]')).toBeNull();
    expect(within(knob).queryByRole("button", { name: /^Modulate/ })).toBeNull();
    expect(within(await nodeOf(lfo.id)).getByRole("button", { name: "Modulate rate" })).toBeTruthy();
    // It draws what it puts out, like the others.
    expect(within(knob).getByRole("meter", { name: "Macro output" })).toBeTruthy();
    // Turned, it sends the value as a setting of the macro.
    fireEvent.keyDown(within(knob).getByRole("slider", { name: "Value" }), { key: "ArrowUp" });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "setModulatorField", node: 90, field: "value", value: expect.closeTo(0.26, 9) as unknown }]);
    });
  });

  test("gives an LFO a retrigger and a fade, which a person sets and no cable drives, as it has the rate that one drives", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push({ id: 90, parent: 1, name: "LFO 2", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 3, retrigger: true, fade: 1.5, sync: 0 } });
    const { requests, clock } = await rig(<PatchView />, { snapshot });
    const keyed = await nodeOf(90);
    expect(controlsIn(keyed)).toEqual(["Sync", "Rate", "Wave", "Retrigger", "Fade"]);
    expect(within(keyed).getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" }).value).toBe("1");
    expect(within(keyed).getByRole("slider", { name: "Fade" }).getAttribute("aria-valuetext")).toBe("1.5 s");
    // A cable ends at the rate; no cable ends at the switch, at the note length or at the fade, and no modulator is made for them by a button.
    expect(keyed.querySelector('.react-flow__handle[data-handleid="rate"]')).not.toBeNull();
    for (const handle of ["retrigger", "fade", "sync"]) expect(keyed.querySelector(`.react-flow__handle[data-handleid="${handle}"]`), handle).toBeNull();
    expect(within(keyed).queryByRole("button", { name: /^Modulate (retrigger|fade|sync)/ })).toBeNull();
    // The fade, turned, is a setting of the LFO; so is the switch.
    fireEvent.keyDown(within(keyed).getByRole("slider", { name: "Fade" }), { key: "ArrowUp" });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "setModulatorField", node: 90, field: "fade", value: expect.closeTo(1.55, 9) as unknown }]);
    });
    fireEvent.change(within(keyed).getByRole("combobox", { name: "Retrigger" }), { target: { value: "0" } });
    await waitFor(() => {
      expect(requests().at(-1)?.intent).toEqual({ type: "setModulatorField", node: 90, field: "retrigger", value: 0 });
    });
    // The note length that it follows the tempo by is chosen from a list; once it is, the knob of the rate shows what the tempo makes of it (an eighth note at the tracer's 120
    // beats a minute is 4 Hz) and cannot be turned.
    expect(within(keyed).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuetext")).toBe("3 Hz");
    fireEvent.change(within(keyed).getByRole("combobox", { name: "Sync" }), { target: { value: "11" } });
    await waitFor(() => {
      expect(requests().at(-1)?.intent).toEqual({ type: "setModulatorField", node: 90, field: "sync", value: 11 });
    });
    await waitFor(() => {
      expect(within(keyed).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuetext")).toBe("4 Hz");
    });
    expect(within(keyed).getByRole("slider", { name: "Rate" }).getAttribute("aria-disabled")).toBe("true");
  });

  test("gives a step sequencer its rate, which a cable can drive, and its glide, its length and its retrigger, which a person sets, and says nothing of its kind twice", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    const values = [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)];
    snapshot.graph.nodes.push({ id: 90, parent: 1, name: "Steps", kind: "modulator", source: { type: "steps", values, length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 0 } });
    const { requests, clock } = await rig(<PatchView />, { snapshot });
    const sequencer = await nodeOf(90);
    expect(controlsIn(sequencer)).toEqual(["Sync", "Rate", "Glide", "Length", "Retrigger"]);
    expect(sequencer.querySelector(".tang-node__header")?.contains(within(sequencer).getByRole("combobox", { name: "Sync" }))).toBe(true);
    expect(sequencer.querySelectorAll(".tang-node__controls > .tang-cell")).toHaveLength(4);
    expect(Array.from(sequencer.querySelectorAll('.tang-knob__dial[role="slider"]')).map((control) => control.getAttribute("aria-valuetext"))).toEqual(["6 Hz", "0.25", "4"]);
    expect(within(sequencer).getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" }).value).toBe("0");
    // (Its name is its kind: no badge says it again.)
    expect(sequencer.querySelector(".tang-node__badge")).toBeNull();
    // A cable ends at the rate, as at an LFO's, and a modulator is made for it by a button; the glide and the length are set by a person.
    expect(sequencer.querySelector('.react-flow__handle[data-handleid="rate"]')).not.toBeNull();
    expect(within(sequencer).getByRole("button", { name: "Modulate rate" })).toBeTruthy();
    for (const handle of ["glide", "length", "retrigger"]) expect(sequencer.querySelector(`.react-flow__handle[data-handleid="${handle}"]`), handle).toBeNull();
    expect(within(sequencer).queryByRole("button", { name: /^Modulate (glide|length)/ })).toBeNull();
    // It draws what it puts out, from -1 to 1 as an LFO's output is.
    expect(within(sequencer).getByRole("meter", { name: "Steps output" }).hasAttribute("data-centred")).toBe(true);
    // It has its steps as bars, of which the four in use are bright; and nothing drives a bar, so there is no port or button for one.
    const bars = within(within(sequencer).getByRole("group", { name: "Steps of Steps" })).getAllByRole("slider");
    expect(bars.map((bar) => bar.getAttribute("aria-valuenow")).slice(0, 5)).toEqual(["0.5", "-0.25", "1", "-1", "0"]);
    expect(bars.filter((bar) => !bar.hasAttribute("data-unused"))).toHaveLength(4);
    // The glide, turned, is a setting of the step sequencer.
    fireEvent.keyDown(within(sequencer).getByRole("slider", { name: "Glide" }), { key: "ArrowUp" });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "setModulatorField", node: 90, field: "glide", value: expect.closeTo(0.26, 9) as unknown }]);
    });
  });

  test("says what a control is bound to as soon as it is bound, though the settings of a modulator do not render its node", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push({ id: 91, parent: 1, name: "MIDI (learning…)", kind: "modulator", source: { type: "midiLearn", control: null, learning: true } });
    const { client } = await rig(<PatchView />, { snapshot });
    const control = await nodeOf(91);
    expect(within(control).getByText("waiting for a control…", { selector: ".tang-node__detail" })).toBeTruthy();
    const bound = structuredClone(snapshot);
    const node = bound.graph.nodes.find((candidate) => candidate.id === 91);
    if (node?.kind === "modulator") node.source = { type: "midiLearn", control: { type: "cc", number: 21 }, learning: false };
    act(() => {
      client.store.setState({ snapshot: { ...bound, rev: 5 } });
    });
    await waitFor(() => {
      expect(within(control).getByText("CC 21", { selector: ".tang-node__detail" })).toBeTruthy();
    });
  });

  test("turns a modulator's rate on its node, which sends it as a setting and shows it", async () => {
    const { clock, requests } = await rig(<PatchView />);
    const rate = within(await nodeOf(lfo.id)).getByRole("slider", { name: "Rate" });
    fireEvent.keyDown(rate, { key: "ArrowUp" });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toMatchObject({ type: "setModulatorField", node: lfo.id, field: "rate" });
    await waitFor(() => {
      expect(Number(rate.getAttribute("aria-valuenow"))).toBeCloseTo(0.5 * 5000 ** 0.01, 6);
    });
  });

  test("does not render a modulator's node again for a change of its settings, only the control", async () => {
    const { server, client } = await rig(<PatchView />);
    const node = await nodeOf(lfo.id);
    // A node renders from its own selection of the store, what it is made of and not the values of its settings, and says what kind it is each time it does.
    const renders = () => vi.mocked(describeModule.kindOf).mock.calls.length;
    const settled = renders();
    expect(settled).toBeGreaterThan(0);
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 9 });
      server.apply({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 1 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 2);
    });
    await waitFor(() => {
      expect(within(node).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuenow")).toBe("9");
    });
    expect(within(node).getByRole<HTMLSelectElement>("combobox", { name: "Wave" }).value).toBe("1");
    expect(renders()).toBe(settled);
  });

  test("gives each node the ports it has: an effect takes audio in and puts it out, an instrument takes notes in and puts audio out, a modulator puts out modulation, the master takes audio in", async () => {
    await rig(<PatchView />);
    const handles = async (id: number) =>
      Array.from((await nodeOf(id)).querySelectorAll(".react-flow__handle:not(.tang-port--param)")).map(
        (handle) => `${handle.getAttribute("data-handleid") ?? "?"}:${handle.classList.contains("source") ? "source" : "target"}:${handle.getAttribute("data-handlepos") ?? "?"}`,
      );
    // (Audio goes in on the left and out on the right, and notes come in on the left of a lane; a modulator's output is at its bottom, over what it drives.)
    expect(await handles(oscillator.id)).toEqual(["eventsIn:target:left", "audioOut:source:right"]);
    expect(await handles(filter.id)).toEqual(["audioIn:target:left", "audioOut:source:right"]);
    // (The tracer's LFO has a cable, and its row has a port at its left edge, for a cable to end at and drive how deep this one goes.)
    expect(await handles(lfo.id)).toEqual(["depth-0:target:left", "modOut:source:bottom"]);
    expect(await handles(MASTER)).toEqual(["audioIn:target:left"]);
    // And the ports of the controls are on their top edges.
    const ports = Array.from((await nodeOf(filter.id)).querySelectorAll(".tang-port--param")).map((handle) => handle.getAttribute("data-handlepos"));
    expect(new Set(ports)).toEqual(new Set(["top"]));
  });

  test("can be moved but not selected, and takes a cable only at an output (to start one) and at a parameter's, a setting's, a depth's or an input of audio's or of notes' port (to end one)", async () => {
    const { view } = await rig(<PatchView />);
    await nodeOf(filter.id);
    for (const node of Array.from(view.container.querySelectorAll(".react-flow__node"))) {
      expect(node.classList.contains("draggable")).toBe(true);
      expect(node.classList.contains("selectable")).toBe(false);
    }
    const connectable = Array.from(view.container.querySelectorAll(".react-flow__handle.connectable"));
    const ports = connectable.map((handle) => handle.getAttribute("data-handleid") ?? "?");
    expect(ports.filter((port) => port === "modOut")).toHaveLength(1);
    expect(ports.filter((port) => port.startsWith("param-"))).toHaveLength(12); // the oscillator's eight and the filter's four
    expect(ports.filter((port) => port === "rate")).toHaveLength(1); // the LFO's rate, which another modulator's cable can end at
    expect(ports.filter((port) => port.startsWith("depth-"))).toHaveLength(1); // and the depth of its cable, which another modulator's cable can end at too
    expect(ports.filter((port) => port === "audioOut")).toHaveLength(2); // the oscillator's and the filter's: audio that is sent starts there
    expect(ports.filter((port) => port === "audioIn")).toHaveLength(2); // the filter's and the master's: audio that is sent ends there
    expect(ports.filter((port) => port === "eventsIn")).toHaveLength(1); // the oscillator's: notes from a source end there
    expect(ports.filter((port) => port === "volume")).toHaveLength(1); // the oscillator's output volume, which the host keeps: a cable from a modulator ends there
    expect(ports.filter((port) => port === "mix")).toHaveLength(1); // and the filter's mix
    expect(ports).toHaveLength(22);
    // A cable is started at an output (a modulator's, or one of audio) and ends at a parameter, a setting, a depth or an input of audio, never the other way.
    for (const output of Array.from(view.container.querySelectorAll(".react-flow__handle[data-handleid='modOut'], .react-flow__handle[data-handleid='audioOut']"))) {
      expect(output.classList.contains("connectablestart"), output.getAttribute("data-handleid") ?? "?").toBe(true);
    }
    for (const output of Array.from(view.container.querySelectorAll(".react-flow__handle[data-handleid='audioOut']"))) {
      expect(output.classList.contains("connectableend"), "audioOut").toBe(false);
    }
    const endsCable = (handle: Element): boolean => {
      const id = handle.getAttribute("data-handleid") ?? "";
      return id.startsWith("param-") || id.startsWith("depth-") || id === "rate" || id === "audioIn" || id === "eventsIn";
    };
    for (const handle of connectable.filter(endsCable)) {
      expect(handle.classList.contains("connectablestart")).toBe(false);
      expect(handle.classList.contains("connectableend")).toBe(true);
    }
  });

  describe("adding a modulator", () => {
    const addButtons = async () => (await screen.findAllByRole("button", { name: /^Add a modulator to / })).map((button) => button.getAttribute("aria-label"));

    test("is offered on every lane and group, by a button that names whose it is, and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const names = (await addButtons()).sort();
      expect(names).toEqual(
        [
          "Add a modulator to (empty)",
          "Add a modulator to Group 1",
          "Add a modulator to Group 2",
          "Add a modulator to Oscillator 1",
          "Add a modulator to Oscillator 2",
          "Add a modulator to Oscillator 3",
          "Add a modulator to Oscillator 4",
        ].sort(),
      );
      // None on an effect, a modulator or the master.
      for (const id of [5, 12, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Add a modulator/ })).toBeNull();
    });

    test("opens a menu of the kinds of modulator", async () => {
      await rig(<PatchView />);
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      const menu = screen.getByRole("menu", { name: "Add a modulator to Oscillator" });
      expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["LFO", "Envelope", "Macro", "MIDI control", "Velocity", "Key", "Step sequencer"]);
    });

    test("asks the server for a modulator of the kind that was chosen, of the session's own and hearing that lane, and the node is on the canvas with its controls when it has", async () => {
      const { requests } = await rig(<PatchView />);
      expect(screen.queryByRole("group", { name: "ADSR" })).toBeNull();
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Envelope" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addFreeModulator", kind: "envelope", hears: oscillator.id }]);
      });
      const added = await screen.findByRole("group", { name: "ADSR" });
      expect(controlsIn(added)).toEqual(["Attack", "Decay", "Sustain", "Release"]);
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a modulator to Oscillator" }));
      });
    });

    test.each([
      ["Velocity", "velocity"],
      ["Key", "key"],
    ] as const)("asks for a control that follows the notes when %s is chosen, and the node on the canvas is bound at once and has no controls", async (item, kind) => {
      const { requests } = await rig(<PatchView />);
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      fireEvent.click(screen.getByRole("menuitem", { name: item }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addFreeModulator", kind, hears: oscillator.id }]);
      });
      const added = await screen.findByRole("group", { name: item });
      expect(controlsIn(added)).toEqual([]);
      expect(added.textContent).toContain(kind === "velocity" ? "velocity of the last note" : "pitch of the last note");
    });

    test("asks for a step sequencer when Step sequencer is chosen, and the node on the canvas has its four controls, the choice of a note length in its header and its sixteen bars", async () => {
      const { requests } = await rig(<PatchView />);
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Step sequencer" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addFreeModulator", kind: "steps", hears: oscillator.id }]);
      });
      const added = await screen.findByRole("group", { name: "Steps" });
      expect(controlsIn(added)).toEqual(["Sync", "Rate", "Glide", "Length", "Retrigger"]);
      const bars = within(within(added).getByRole("group", { name: "Steps of Steps" })).getAllByRole("slider");
      expect(bars).toHaveLength(16);
      // (A staircase, all in use.)
      expect(bars.map((bar) => bar.getAttribute("aria-valuenow"))[0]).toBe("-1");
      expect(bars.filter((bar) => bar.hasAttribute("data-unused"))).toHaveLength(0);
    });

    test("to a group asks for one that hears the group, whichever kind", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      for (const [item, kind] of [["LFO", "lfo"], ["Macro", "macro"], ["MIDI control", "midiLearn"], ["Velocity", "velocity"], ["Key", "key"], ["Step sequencer", "steps"]] as const) {
        fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Group 2" }));
        fireEvent.click(screen.getByRole("menuitem", { name: item }));
        await waitFor(() => {
          expect(requests().at(-1)?.intent).toEqual({ type: "addFreeModulator", kind, hears: 2 });
        });
      }
      await waitFor(() => {
        expect(requests()).toHaveLength(6);
      });
    });

    test("says why when it is refused, on the page's notices, and the canvas is as it was", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client, view } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const before = view.container.querySelectorAll(".react-flow__node").length;
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "LFO" }));
      expect(await screen.findByText("Could not add an LFO that hears Oscillator: Not connected to the server.")).toBeTruthy();
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(before);
    });
  });

  describe("adding an effect", () => {
    const addButtons = async () => (await screen.findAllByRole("button", { name: /^Add an effect to / })).map((button) => button.getAttribute("aria-label"));
    const opened = async (name: string): Promise<HTMLElement> => {
      fireEvent.click(await screen.findByRole("button", { name: `Add an effect to ${name}` }));
      return screen.getByRole("menu", { name: `Add an effect to ${name}` });
    };

    test("is offered on every lane and group, by a button that names whose it is, and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      expect((await addButtons()).sort()).toEqual(
        [
          "Add an effect to (empty)",
          "Add an effect to Group 1",
          "Add an effect to Group 2",
          "Add an effect to Oscillator 1",
          "Add an effect to Oscillator 2",
          "Add an effect to Oscillator 3",
          "Add an effect to Oscillator 4",
        ].sort(),
      );
      for (const id of [5, 12, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Add an effect/ })).toBeNull();
    });

    test("opens a menu of the effects that are installed, under the format each is of, and not of the instruments", async () => {
      await rig(<PatchView />);
      const menu = await opened("Oscillator");
      const groups = within(menu).getAllByRole("group").map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
      expect(groups).toEqual([["Built-in", ["Delay", "Drive", "Filter", "Math", "Reverb"]]]);
    });

    test("has what the scan finds as it finds it, while it is open", async () => {
      const { server } = await rig(<PatchView />, { catalog: { entries: builtinCatalog, scanning: true } });
      const menu = await opened("Oscillator");
      expect(within(menu).getAllByRole("menuitem")).toHaveLength(5);
      act(() => {
        server.announceCatalog(
          [
            ...builtinCatalog,
            { id: "vst3:Acme Verb", name: "Acme Verb", format: "VST3", isInstrument: false, params: 4, presets: 0 },
            { id: "clap:com.acme.chorus", name: "Chorus", format: "CLAP", isInstrument: false, params: 3, presets: 0 },
            { id: "vst3:Acme Synth", name: "Acme Synth", format: "VST3", isInstrument: true, params: 40, presets: 0 },
          ],
          false,
        );
      });
      await waitFor(() => {
        expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Delay", "Drive", "Filter", "Math", "Reverb", "Chorus", "Acme Verb"]);
      });
      expect(within(screen.getByRole("menu")).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["Built-in", "CLAP", "VST3"]);
    });

    test("asks the server to put the one that was chosen in that lane's chain, and the node is on the canvas with its controls when it has", async () => {
      const { requests } = await rig(<PatchView />);
      expect(screen.queryByRole("group", { name: "Reverb" })).toBeNull();
      await opened("Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" }]);
      });
      const added = await screen.findByRole("group", { name: "Reverb" });
      const names = (builtinEffects["builtin:reverb"]?.plugin.params ?? []).slice(0, 8).map((param) => param.name);
      expect(controlsIn(added)).toEqual([...names, "Mix"]);
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect to Oscillator" }));
      });
    });

    test("takes an audio cable to the master from the new node, and the lane's own goes into it", async () => {
      const { view } = await rig(<PatchView />);
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
      });
      await opened("Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
      });
      const audio = Array.from(view.container.querySelectorAll(".react-flow__edge")).filter((edge) => edge.querySelector(".tang-cable--audio"));
      expect(audio).toHaveLength(3);
    });

    test("to a group asks for one on its bus", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await opened("Group 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Filter" }));
      await waitFor(() => {
        expect(requests().at(-1)?.intent).toEqual({ type: "addEffect", parent: 2, plugin: "builtin:filter" });
      });
    });

    test("has nothing to open when nothing that is installed can be an effect, or the server has not said what is", async () => {
      const instrument = { id: "builtin:osc", name: "Oscillator", format: "Built-in", isInstrument: true, params: 8, presets: 0 };
      for (const catalog of [{ entries: [], scanning: true }, { entries: [instrument], scanning: false }]) {
        await rig(<PatchView />, { catalog });
        const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add an effect to Oscillator" });
        expect(button.disabled).toBe(true);
        cleanup();
      }
    });

    test("says why when it is refused, on the page's notices, by the name the catalog gives it", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client, view } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const before = view.container.querySelectorAll(".react-flow__node").length;
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      await opened("Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
      expect(await screen.findByText("Could not add Reverb to Oscillator: Not connected to the server.")).toBeTruthy();
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(before);
    });
  });

  describe("changing an instrument", () => {
    const label = /^(Change the instrument of|Choose an instrument for) /;
    const buttons = async () => (await screen.findAllByRole("button", { name: label })).map((button) => button.getAttribute("aria-label"));
    const opened = async (name: string): Promise<HTMLElement> => {
      fireEvent.click(await screen.findByRole("button", { name }));
      return screen.getByRole("menu", { name });
    };
    const waveform = async (id: number): Promise<string> => within(await nodeOf(id)).getByRole<HTMLSelectElement>("combobox", { name: "waveform" }).value;

    test("is offered on every lane, by a button that names whose it is (an empty lane is asked to be given one), and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      expect((await buttons()).sort()).toEqual(
        [
          "Change the instrument of Oscillator 1",
          "Change the instrument of Oscillator 2",
          "Change the instrument of Oscillator 3",
          "Change the instrument of Oscillator 4",
          "Choose an instrument for (empty)",
        ].sort(),
      );
      for (const id of [0, 1, 2, 3, 5, 12, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: label })).toBeNull();
    });

    test("opens a menu of the instruments that are installed, under the format each is of, and not of the effects", async () => {
      await rig(<PatchView />);
      const menu = await opened("Change the instrument of Oscillator");
      const groups = within(menu).getAllByRole("group").map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
      expect(groups).toEqual([["Built-in", ["Audio Input", "Oscillator"]]]);
    });

    test("has what the scan finds as it finds it, while it is open", async () => {
      const { server } = await rig(<PatchView />, { catalog: { entries: builtinCatalog, scanning: true } });
      const menu = await opened("Change the instrument of Oscillator");
      expect(within(menu).getAllByRole("menuitem")).toHaveLength(2);
      act(() => {
        server.announceCatalog(
          [
            ...builtinCatalog,
            { id: "vst3:Acme Synth", name: "Acme Synth", format: "VST3", isInstrument: true, params: 40, presets: 0 },
            { id: "vst3:Acme Verb", name: "Acme Verb", format: "VST3", isInstrument: false, params: 4, presets: 0 },
          ],
          false,
        );
      });
      await waitFor(() => {
        expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Audio Input", "Oscillator", "Acme Synth"]);
      });
    });

    test("asks the server to put the one that was chosen in that lane, which keeps its node and has the new instrument's parameters", async () => {
      const { requests } = await rig(<PatchView />);
      expect(await waveform(oscillator.id)).toBe("3"); // the tracer's is a saw
      await opened("Change the instrument of Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "replaceInstrument", node: oscillator.id, plugin: "builtin:osc" }]);
      });
      await waitFor(async () => {
        expect(await waveform(oscillator.id)).toBe("0"); // a new instrument starts as a sine
      });
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Change the instrument of Oscillator" }));
      });
    });

    test("fills an empty lane, which is called by its instrument then and has a button that changes it", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await opened("Choose an instrument for (empty)");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      await waitFor(() => {
        expect(requests().at(-1)?.intent).toEqual({ type: "replaceInstrument", node: 11, plugin: "builtin:osc" });
      });
      expect(await screen.findByRole("button", { name: "Change the instrument of Oscillator 5" })).toBeTruthy();
      expect(screen.queryByRole("button", { name: "Choose an instrument for (empty)" })).toBeNull();
      expect(controlsIn(await nodeOf(11))).toEqual(["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "Volume", "Lowest key of Oscillator 5", "Highest key of Oscillator 5", "Group of Oscillator 5"]);
    });

    test("takes the cables that drove the old instrument off the canvas, and leaves the others", async () => {
      const { server, view } = await rig(<PatchView />);
      server.apply({ type: "addModEdge", modulator: lfo.id, to: { node: oscillator.id, port: { type: "param", index: 1 } } });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
      });
      await opened("Change the instrument of Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
      });
    });

    test("has nothing to open when nothing that is installed can be an instrument, or the server has not said what is", async () => {
      const effect = { id: "builtin:filter", name: "Filter", format: "Built-in", isInstrument: false, params: 4, presets: 0 };
      for (const catalog of [{ entries: [], scanning: true }, { entries: [effect], scanning: false }]) {
        await rig(<PatchView />, { catalog });
        const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Change the instrument of Oscillator" });
        expect(button.disabled).toBe(true);
        cleanup();
      }
    });

    test("says why when it is refused, on the page's notices, by the names the catalog gives", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      await opened("Change the instrument of Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      expect(await screen.findByText("Could not change the instrument of Oscillator to Oscillator: Not connected to the server.")).toBeTruthy();
    });
  });

  describe("loading a preset", () => {
    const label = /^Load a preset into /;
    const buttons = async () => (await screen.findAllByRole("button", { name: label })).map((button) => button.getAttribute("aria-label"));
    const opened = async (name: string): Promise<HTMLElement> => {
      fireEvent.click(await screen.findByRole("button", { name }));
      return screen.getByRole("menu", { name });
    };
    /** What a knob of a node says it is at. */
    const knob = async (node: number, name: string): Promise<string | null> => within(await nodeOf(node)).getByRole("slider", { name }).getAttribute("aria-valuenow");
    const PRESETS = ["Arcadia Dream Hall", "Cathedral", "Vintage Plate", "Small Room", "Ambient Wash"];

    test("is offered on every instrument and effect of a lane that has presets, by a button that names whose it is, and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      // (Of the two reverbs, the one on a group's bus has none to load into: the audio thread addresses a preset by a lane's slot.)
      expect(await buttons()).toEqual(["Load a preset into Reverb 2"]);
      for (const id of [0, 1, 2, 3, 4, 5, 6, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: label })).toBeNull();
    });

    test("is offered on an instrument that has presets too, and not on one that has none", async () => {
      const snapshot = structuredClone(tracerSnapshot);
      const osc = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
      if (osc?.kind !== "instrument" || !osc.plugin) throw new Error("no instrument");
      osc.plugin.presets = [
        { id: "pad", name: "Pad" },
        { id: "lead", name: "Lead" },
      ];
      await rig(<PatchView />, { snapshot });
      expect(await buttons()).toEqual(["Load a preset into Oscillator"]);
      expect(within(await nodeOf(filter.id)).queryByRole("button", { name: label })).toBeNull();
    });

    test("opens a menu of the preset names, in the plugin's order, none marked while none is loaded", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const menu = await opened("Load a preset into Reverb 2");
      const entries = within(menu).getAllByRole("menuitem");
      expect(entries.map((entry) => entry.textContent)).toEqual(PRESETS);
      expect(entries.map((entry) => entry.getAttribute("aria-current"))).toEqual([null, null, null, null, null]);
    });

    test("asks the server to load the one that was chosen, and the knobs of the node show what it sets", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const before = await knob(8, "decay");
      await opened("Load a preset into Reverb 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Cathedral" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "loadPreset", node: 8, preset: "cathedral" }]);
      });
      // (The reverb's decay is the first of a preset's values: a long one in Cathedral.)
      const decay = (builtinPresets["builtin:reverb"]?.cathedral ?? []).find((v) => v.param === 0)?.value;
      await waitFor(async () => {
        expect(Number(await knob(8, "decay"))).toBeCloseTo(decay ?? Number.NaN, 5);
      });
      expect(Number(before)).not.toBeCloseTo(decay ?? 0, 5);
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Load a preset into Reverb 2" }));
      });
    });

    test("has the preset that was loaded marked, and the menu opens on it", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await opened("Load a preset into Reverb 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Vintage Plate" }));
      await waitFor(() => {
        expect(screen.queryByRole("menu")).toBeNull();
      });
      const menu = await opened("Load a preset into Reverb 2");
      const marked = within(menu).getAllByRole("menuitem").filter((entry) => entry.getAttribute("aria-current") === "true");
      expect(marked.map((entry) => entry.textContent)).toEqual(["Vintage Plate"]);
      expect(document.activeElement).toBe(marked[0]);
    });

    test("is told of a preset that another front-end loads: the mark moves, and so do the knobs", async () => {
      const { server } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await screen.findByRole("button", { name: "Load a preset into Reverb 2" });
      act(() => {
        server.apply({ type: "loadPreset", node: 8, preset: "room" });
      });
      const room = (builtinPresets["builtin:reverb"]?.room ?? []).find((v) => v.param === 0)?.value;
      await waitFor(async () => {
        expect(Number(await knob(8, "decay"))).toBeCloseTo(room ?? Number.NaN, 5);
      });
      const menu = await opened("Load a preset into Reverb 2");
      expect(within(menu).getAllByRole("menuitem").filter((entry) => entry.getAttribute("aria-current") === "true").map((entry) => entry.textContent)).toEqual(["Small Room"]);
    });

    test("says why when it is refused, on the page's notices, by the preset's name", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
        { snapshot: reachFixture.snapshot },
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      await opened("Load a preset into Reverb 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Cathedral" }));
      expect(await screen.findByText("Could not load Cathedral into Reverb 2: Not connected to the server.")).toBeTruthy();
    });
  });

  describe("drawing a cable to a parameter, from a list", () => {
    test("every modulator has a list of what it can be drawn to: its plugins' parameters, by plugin, but what it drives already", async () => {
      await rig(<PatchView />);
      const picker = await pickerOf(lfo.id);
      expect(picker.getAttribute("aria-label")).toBe("Add a cable from LFO");
      expect(picker.textContent).toBe("Add a cable…");
      expect(picker.hasAttribute("disabled")).toBe(false);
      // (A menu, which does nothing until a parameter is chosen: not a native list, whose arrow keys change it on some platforms.)
      expect(picker.getAttribute("aria-haspopup")).toBe("menu");
      fireEvent.click(picker);
      expect(screen.getByRole("menu", { name: "Add a cable from LFO" })).toBeTruthy();
      // (After the parameters of each plugin, the value that the host keeps: the oscillator's output volume, which is not its `volume`, and the filter's mix.)
      expect(listed()).toEqual([
        ["Oscillator", ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "unison", "spread", "width", "pulse", "fm", "fm_ratio", "sub", "sync", "drift", "velocity", "bend", "quantize", "output volume"]],
        ["Filter", ["resonance", "type", "drive", "mix"]],
      ]);
    });

    test("draws nothing by being opened, moved through or left: only choosing a parameter is a cable", async () => {
      const { requests } = await rig(<PatchView />);
      const picker = await pickerOf(lfo.id);
      fireEvent.keyDown(picker, { key: "ArrowDown" });
      for (const key of ["ArrowDown", "ArrowDown", "End", "ArrowUp", "d"]) fireEvent.keyDown(document.activeElement ?? document.body, { key });
      fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
      expect(screen.queryByRole("menu")).toBeNull();
      expect(requests()).toEqual([]);
    });

    test("asks the server for the cable that was picked, and it is drawn, and the parameter is off the list", async () => {
      const { requests, view } = await rig(<PatchView />);
      const detune = oscillator.kind === "instrument" ? oscillator.plugin?.params.find((param) => param.name === "detune")?.index : undefined;
      const picker = await pickerOf(lfo.id);
      fireEvent.click(picker);
      fireEvent.click(screen.getByRole("menuitem", { name: "detune" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addModEdge", modulator: lfo.id, to: { node: oscillator.id, port: { type: "param", index: detune } } }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(2);
      });
      // The cable has its depth to turn and its button to remove; the menu is closed, with the focus on its button, and the parameter is not offered again.
      expect(screen.getByRole("slider", { name: "Depth of LFO → Oscillator: detune" })).toBeTruthy();
      expect(screen.queryByRole("menu")).toBeNull();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(listed()[0]).toEqual(["Oscillator", ["waveform", "volume", "attack", "decay", "sustain", "release", "pan", "unison", "spread", "width", "pulse", "fm", "fm_ratio", "sub", "sync", "drift", "velocity", "bend", "quantize", "output volume"]]);
      // And the oscillator's detune knob has its port driven now.
      const knob = within(await nodeOf(oscillator.id)).getByRole("slider", { name: "detune" });
      expect(knob.closest(".tang-cell")?.querySelector(".tang-port--driven")).toBeTruthy();
    });

    test("says why when the cable was not drawn, and the list is as it was", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const picker = await pickerOf(lfo.id);
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(picker);
      fireEvent.click(screen.getByRole("menuitem", { name: "detune" }));
      expect(await screen.findByText("Could not draw a cable from LFO to Oscillator: detune: Not connected to the server.")).toBeTruthy();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(screen.getByRole("menuitem", { name: "detune" })).toBeTruthy();
    });

    test("is for a modulator that hears a group the parameters of every plugin of the session, then the settings of the other modulators", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      fireEvent.click(await pickerOf(15));
      // (In the order of the graph: the groups, which have a volume and a pan, and the plugins, the buses' and the lanes', then the modulators that have settings, but not the one that is drawn from.)
      expect(listed().map(([plugin]) => plugin)).toEqual(["Group 1", "Filter 1", "Group 2", "Reverb 1", "Oscillator 1", "Filter 2", "Oscillator 2", "Oscillator 3", "Reverb 2", "Oscillator 4", "Filter 3", "LFO 1", "ADSR 1", "ADSR 2", "LFO 3"]);
    });

    test("is disabled for a modulator with nothing to be drawn to, and says so", async () => {
      // (A session that has a modulator and no plugin and no other modulator: every modulator reaches every plugin and the others, so it takes a session with none of them.)
      const hearing = tracerSnapshot.graph.nodes.filter((node) => node.kind === "modulator" || node.kind === "master");
      const nodes = hearing.map((node) => {
        const copy = { ...node };
        if (copy.kind === "modulator") delete copy.hears;
        return copy;
      });
      await rig(<PatchView />, { snapshot: { ...tracerSnapshot, graph: { nodes, edges: [] }, telemetry: { ...tracerSnapshot.telemetry, modulation: tracerSnapshot.telemetry.modulation.filter((entry) => entry.kind === "modOutput") } } });
      const picker = await pickerOf(lfo.id);
      expect(picker.hasAttribute("disabled")).toBe(true);
      expect(picker.textContent).toBe("Nothing to drive");
      fireEvent.click(picker);
      expect(screen.queryByRole("menu")).toBeNull();
    });

    test("tells two modulators of one name apart", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      // (The graph lists them in the order of the session's own rack, so the one that hears the first lane is the first.)
      expect((await pickerOf(12)).getAttribute("aria-label")).toBe("Add a cable from LFO 1");
      expect((await pickerOf(15)).getAttribute("aria-label")).toBe("Add a cable from LFO 2");
    });

    test("has an id the focus can be given back to, one for each modulator", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      for (const id of [15, 12]) expect((await pickerOf(id)).id).toBe(drivesId(id));
      expect(drivesId(15)).not.toBe(drivesId(12));
    });
  });

  describe("drawing a cable to a setting of another modulator", () => {
    // The first lane of a group is heard by an LFO ("LFO 1"), an envelope ("ADSR 1") and a MIDI control ("CC 1"), its second by an LFO ("LFO 2"), a lane in no group by one
    // ("LFO 3") and a lane of another group by one ("LFO 4"); the group is heard by an LFO ("LFO 5"), an envelope ("ADSR 2") and a MIDI control ("CC 74").
    const { snapshot } = fieldsFixture;
    const [groupLfo, laneLfo, laneEnvelope, laneControl] = [13, 7, 8, 9];
    const cables = snapshot.graph.edges.filter((edge) => edge.kind === "modulation").length;
    /** What the first oscillator's plugin calls its parameter of this index. */
    const oscillatorParam = (index: number): string => {
      const found = snapshot.graph.nodes.find((node) => node.id === 3);
      return (found?.kind === "instrument" ? found.plugin?.params.find((param) => param.index === index)?.name : undefined) ?? "?";
    };
    /** The session with a cable (the 50th) from a modulator to a setting of another. */
    const withCable = (from: number, to: number, port: Port): Snapshot => ({
      ...snapshot,
      graph: { ...snapshot.graph, edges: [...snapshot.graph.edges, { kind: "modulation", id: 50, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: to, port } }] },
    });
    const portsOf = async (id: number): Promise<(string | null)[]> => Array.from((await nodeOf(id)).querySelectorAll(".tang-port--param")).map((port) => port.getAttribute("data-handleid"));

    test("has a port at the top of each setting that takes a cable: an LFO's rate and an envelope's stages, but not an LFO's waveform nor a MIDI control", async () => {
      await rig(<PatchView />, { snapshot });
      expect(await portsOf(laneLfo)).toEqual(["rate"]);
      expect(await portsOf(laneEnvelope)).toEqual(["attack", "decay", "sustain", "release"]);
      expect(await portsOf(laneControl)).toEqual([]);
      // At the top edge of the control it is for, in its cell, as a parameter's is; a cable ends there, and is not started there.
      const rate = (await nodeOf(laneLfo)).querySelector(".tang-port--param");
      expect(rate?.getAttribute("data-handlepos")).toBe("top");
      expect(rate?.closest(".tang-cell")?.querySelector('[role="slider"]')?.getAttribute("aria-label")).toBe("Rate");
      expect(rate?.classList.contains("connectablestart")).toBe(false);
      expect(rate?.classList.contains("connectableend")).toBe(true);
    });

    test("marks the port of a setting that a cable ends at, and no other, and draws the cable from the modulator's output with a slider for its depth", async () => {
      const { view } = await rig(<PatchView />, { snapshot: withCable(laneEnvelope, laneLfo, { type: "rate" }) });
      const driven = async (id: number) => Array.from((await nodeOf(id)).querySelectorAll(".tang-port--driven")).map((port) => port.getAttribute("data-handleid"));
      expect(await driven(laneLfo)).toEqual(["rate"]);
      expect(await driven(laneEnvelope)).toEqual([]);
      expect(await driven(laneControl)).toEqual([]);
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables + 1);
      expect(screen.getByRole("slider", { name: "Depth of ADSR 1 → LFO 1: rate" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Remove cable ADSR 1 → LFO 1: rate" })).toBeTruthy();
    });

    test("is drawn to the depth of a cable, to the port of its row, with a slider for its depth that is named by the whole of what it joins", async () => {
      // The lane's LFO drives how deep the envelope's cable 1 goes (it ends at the second parameter of the oscillator that the session names).
      const { view } = await rig(<PatchView />, { snapshot: withCable(laneLfo, laneEnvelope, { type: "depth", edge: 1 }) });
      await nodeOf(laneLfo);
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables + 1);
      const words = `LFO 1 → depth of ADSR 1 → Oscillator 1: ${oscillatorParam(2)}`;
      expect(screen.getByRole("slider", { name: `Depth of ${words}` })).toBeTruthy();
      expect(screen.getByRole("button", { name: `Remove cable ${words}` })).toBeTruthy();
      // It ends at the row of that cable, in the node of the envelope that it starts at, which says that a cable ends there.
      const row = (await nodeOf(laneEnvelope)).querySelector(".tang-cable-row--driven");
      expect(row?.querySelector(".react-flow__handle")?.getAttribute("data-handleid")).toBe("depth-1");
      expect(row?.querySelector(".tang-cable-row__label")?.textContent).toBe(`Oscillator 1: ${oscillatorParam(2)}`);
    });

    test("is on the list of a modulator, after the parameters, modulator by modulator: the settings of the others, but what it drives already", async () => {
      await rig(<PatchView />, { snapshot: withCable(laneControl, laneEnvelope, { type: "decay" }) });
      fireEvent.click(await pickerOf(laneControl));
      const groups = listed();
      // (Every plugin's parameters, then every other modulator that has a setting or a cable: the LFO's rate and the depth of its cable, the envelope's three stages, for the control drives its decay,
      // and the depths of the cables that start at it, each by where the cable goes.)
      expect(groups.map(([name]) => name)).toEqual(["Group 1", "Reverb", "Group 2", "Oscillator 1", "Oscillator 2", "Oscillator 3", "Oscillator 4", "LFO 1", "ADSR 1", "LFO 2", "LFO 3", "LFO 4", "LFO 5", "ADSR 2", "CC 74"]);
      expect(groups.filter(([name]) => name === "LFO 1" || name === "ADSR 1")).toEqual([
        ["LFO 1", ["rate", `depth → Oscillator 1: ${oscillatorParam(1)}`]],
        ["ADSR 1", ["attack", "sustain", "release", `depth → Oscillator 1: ${oscillatorParam(2)}`, `depth → Oscillator 1: ${oscillatorParam(1)}`]],
      ]);
    });

    test("is on the list of an LFO as the envelope's four stages and the depths of the cables of the envelope and of the MIDI control, which has no setting, among those of all the others", async () => {
      await rig(<PatchView />, { snapshot });
      fireEvent.click(await pickerOf(laneLfo));
      expect(listed().map(([name]) => name)).toEqual(["Group 1", "Reverb", "Group 2", "Oscillator 1", "Oscillator 2", "Oscillator 3", "Oscillator 4", "ADSR 1", "CC 1", "LFO 2", "LFO 3", "LFO 4", "LFO 5", "ADSR 2", "CC 74"]);
      expect(listed().find(([name]) => name === "ADSR 1")).toEqual(["ADSR 1", ["attack", "decay", "sustain", "release", `depth → Oscillator 1: ${oscillatorParam(2)}`, `depth → Oscillator 1: ${oscillatorParam(1)}`]]);
      expect(listed().find(([name]) => name === "CC 1")).toEqual(["CC 1", [`depth → Oscillator 1: ${oscillatorParam(7)}`]]);
    });

    test("is for a modulator that hears a group the same: the settings and the cables' depths of every other modulator, those that hear the members and those that hear the group", async () => {
      await rig(<PatchView />, { snapshot });
      fireEvent.click(await pickerOf(groupLfo));
      // (The plugins in the order of the graph; then the other modulators, in the order of the session's own rack.)
      expect(listed().map(([name]) => name)).toEqual(["Group 1", "Reverb", "Group 2", "Oscillator 1", "Oscillator 2", "Oscillator 3", "Oscillator 4", "LFO 1", "ADSR 1", "CC 1", "LFO 2", "LFO 3", "LFO 4", "ADSR 2", "CC 74"]);
    });

    test("asks the server for the cable that was picked, and it is drawn, and the setting is off the list", async () => {
      const { requests, view } = await rig(<PatchView />, { snapshot });
      const picker = await pickerOf(laneEnvelope);
      fireEvent.click(picker);
      fireEvent.click(within(within(screen.getByRole("menu")).getByRole("group", { name: "LFO 1" })).getByRole("menuitem", { name: "rate" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addModEdge", modulator: laneEnvelope, to: { node: laneLfo, port: { type: "rate" } } }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables + 1);
      });
      expect(screen.getByRole("slider", { name: "Depth of ADSR 1 → LFO 1: rate" })).toBeTruthy();
      // The menu is closed, with the focus on its button; the LFO has nothing more to take, and its port is marked as driven.
      expect(screen.queryByRole("menu")).toBeNull();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      // (Its rate is taken, and the depth of its cable is not.)
      expect(listed().find(([name]) => name === "LFO 1")?.[1]).toEqual([`depth → Oscillator 1: ${oscillatorParam(1)}`]);
      expect((await nodeOf(laneLfo)).querySelector(".tang-port--driven")?.getAttribute("data-handleid")).toBe("rate");
    });

    test("asks the server for the cable to a depth that was picked, under the modulator whose cable it is, and it is drawn, and that depth is off the list", async () => {
      const { requests, view } = await rig(<PatchView />, { snapshot });
      const picker = await pickerOf(laneLfo);
      fireEvent.click(picker);
      fireEvent.click(within(within(screen.getByRole("menu")).getByRole("group", { name: "ADSR 1" })).getByRole("menuitem", { name: `depth → Oscillator 1: ${oscillatorParam(2)}` }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addModEdge", modulator: laneLfo, to: { node: laneEnvelope, port: { type: "depth", edge: 1 } } }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables + 1);
      });
      expect(screen.getByRole("slider", { name: `Depth of LFO 1 → depth of ADSR 1 → Oscillator 1: ${oscillatorParam(2)}` })).toBeTruthy();
      // The menu is closed, with the focus on its button; the depth it drives is off the list and the envelope's other one is not, and the row of the
      // cable is marked as driven.
      expect(screen.queryByRole("menu")).toBeNull();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(listed().find(([name]) => name === "ADSR 1")?.[1].filter((item) => item.startsWith("depth"))).toEqual([`depth → Oscillator 1: ${oscillatorParam(1)}`]);
      expect((await nodeOf(laneEnvelope)).querySelector(".tang-port--driven")?.getAttribute("data-handleid")).toBe("depth-1");
    });

    test("says why when the cable was not drawn, in the names of the modulators and the setting, and the list is as it was", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
        { snapshot },
      );
      const picker = await pickerOf(laneEnvelope);
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(picker);
      fireEvent.click(within(within(screen.getByRole("menu")).getByRole("group", { name: "LFO 1" })).getByRole("menuitem", { name: "rate" }));
      expect(await screen.findByText("Could not draw a cable from ADSR 1 to LFO 1: rate: Not connected to the server.")).toBeTruthy();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(listed().map(([name]) => name)).toContain("LFO 1");
    });

    test("takes the cable out with its button, and the focus goes to the list of the modulator that drove it", async () => {
      const { requests, view } = await rig(<PatchView />, { snapshot: withCable(laneEnvelope, laneLfo, { type: "rate" }) });
      await nodeOf(laneLfo);
      fireEvent.click(screen.getByRole("button", { name: "Remove cable ADSR 1 → LFO 1: rate" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeEdge", edge: 50 }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables);
      });
      expect((await nodeOf(laneLfo)).querySelectorAll(".tang-port--driven")).toHaveLength(0);
      await waitFor(() => {
        expect(document.activeElement?.id).toBe(drivesId(laneEnvelope));
      });
    });
  });

  describe("the cables a modulator's node lists", () => {
    // The fixture of the settings: the envelope that hears the first lane (node 8, "ADSR 1") has two cables, to the second and the first parameters of the oscillator
    // (node 3, "Oscillator 1") that are named in the session; the LFO that hears it (7, "LFO 1") has one, to the first; the MIDI control (9) one; the LFO that hears
    // the lane that is in no group (11) has none.
    const { snapshot } = fieldsFixture;
    const [laneLfo, laneEnvelope, laneControl, loneLfo] = [7, 8, 9, 11];
    const oscillatorNode = snapshot.graph.nodes.find((node) => node.id === 3);
    const paramNamed = (index: number): string => (oscillatorNode?.kind === "instrument" ? oscillatorNode.plugin?.params.find((param) => param.index === index)?.name : undefined) ?? "?";
    const rowsOf = async (id: number): Promise<string[]> => Array.from((await nodeOf(id)).querySelectorAll(".tang-cable-row__label")).map((label) => label.textContent);
    /** The session with a cable (the 50th) from a modulator to a port of a node. */
    const withCable = (from: number, to: number, port: Port): Snapshot => ({
      ...snapshot,
      graph: { ...snapshot.graph, edges: [...snapshot.graph.edges, { kind: "modulation", id: 50, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: to, port } }] },
    });

    test("has a row for each cable that starts at the modulator, in the order of the graph, saying what it goes to", async () => {
      await rig(<PatchView />, { snapshot });
      expect(await rowsOf(laneEnvelope)).toEqual([`Oscillator 1: ${paramNamed(2)}`, `Oscillator 1: ${paramNamed(1)}`]);
      expect(await rowsOf(laneLfo)).toEqual([`Oscillator 1: ${paramNamed(1)}`]);
      expect(await rowsOf(laneControl)).toEqual([`Oscillator 1: ${paramNamed(7)}`]);
    });

    test("is a list that is named after the modulator, and has none for a modulator that drives nothing", async () => {
      await rig(<PatchView />, { snapshot });
      const list = within(await nodeOf(laneEnvelope)).getByRole("list", { name: "Cables of ADSR 1" });
      expect(within(list).getAllByRole("listitem")).toHaveLength(2);
      expect((await nodeOf(loneLfo)).querySelector(".tang-node__cables")).toBeNull();
      expect(within(await nodeOf(loneLfo)).queryByRole("list")).toBeNull();
    });

    test("has a button on each row that modulates the depth of that cable, named after the cable and the modulator it starts at", async () => {
      await rig(<PatchView />, { snapshot });
      const names = async (id: number) => Array.from((await nodeOf(id)).querySelectorAll(".tang-cable-row .tang-modulate")).map((button) => button.getAttribute("aria-label"));
      expect(await names(laneEnvelope)).toEqual([`Modulate depth of ADSR 1 → Oscillator 1: ${paramNamed(2)}`, `Modulate depth of ADSR 1 → Oscillator 1: ${paramNamed(1)}`]);
      expect(await names(laneLfo)).toEqual([`Modulate depth of LFO 1 → Oscillator 1: ${paramNamed(1)}`]);
      expect(await names(loneLfo)).toEqual([]);
    });

    test("makes a modulator for the depth of the cable of a row with that button, one that hears what the modulator the cable starts at hears", async () => {
      const { requests, server } = await rig(<PatchView />, { snapshot });
      await nodeOf(laneEnvelope);
      fireEvent.click(screen.getByRole("button", { name: `Modulate depth of ADSR 1 → Oscillator 1: ${paramNamed(1)}` }));
      fireEvent.click(screen.getByRole("menuitem", { name: "New LFO" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "modulate", to: { node: laneEnvelope, port: { type: "depth", edge: 2 } }, kind: "lfo", depth: null }]);
      });
      await waitFor(() => {
        const made = server.snapshot.graph.edges.filter((e) => e.kind === "modulation" && e.to.port.type === "depth");
        expect(made).toHaveLength(1);
      });
      const envelope = snapshot.graph.nodes.find((node) => node.id === laneEnvelope);
      const lane = envelope?.kind === "modulator" ? envelope.hears : undefined;
      expect(lane).toBe(3);
      expect(server.snapshot.graph.nodes.filter((node) => node.kind === "modulator" && node.hears === lane)).toHaveLength(4);
    });

    test("tells the whole of a row that the node has no room for, by its title", async () => {
      await rig(<PatchView />, { snapshot });
      const rows = Array.from((await nodeOf(laneEnvelope)).querySelectorAll(".tang-cable-row__label"));
      expect(rows.map((row) => row.getAttribute("title"))).toEqual([`Oscillator 1: ${paramNamed(2)}`, `Oscillator 1: ${paramNamed(1)}`]);
    });

    test("has a port at the left edge of each row, named after the cable, in the node of the modulator that the cable starts at", async () => {
      await rig(<PatchView />, { snapshot });
      const ports = async (id: number) =>
        Array.from((await nodeOf(id)).querySelectorAll(".tang-cable-row .react-flow__handle")).map(
          (handle) => `${handle.getAttribute("data-handleid") ?? "?"}:${handle.classList.contains("target") ? "target" : "source"}:${handle.getAttribute("data-handlepos") ?? "?"}`,
        );
      expect(await ports(laneEnvelope)).toEqual(["depth-1:target:left", "depth-2:target:left"]);
      expect(await ports(laneLfo)).toEqual(["depth-0:target:left"]);
      expect(await ports(loneLfo)).toEqual([]);
      // Each is in its row, which is the row of that cable.
      const rows = Array.from((await nodeOf(laneEnvelope)).querySelectorAll(".tang-cable-row"));
      expect(rows.map((row) => row.querySelector(".react-flow__handle")?.getAttribute("data-handleid"))).toEqual(["depth-1", "depth-2"]);
    });

    test("marks the row of a cable that another cable ends at the depth of, and no other", async () => {
      await rig(<PatchView />, { snapshot: withCable(laneEnvelope, laneLfo, { type: "depth", edge: 0 }) });
      const driven = async (id: number) => Array.from((await nodeOf(id)).querySelectorAll(".tang-cable-row--driven .react-flow__handle")).map((port) => port.getAttribute("data-handleid"));
      expect(await driven(laneLfo)).toEqual(["depth-0"]);
      expect(await driven(laneEnvelope)).toEqual([]);
      // (The port is the one that says it: the same mark as the ports of the controls have.)
      const marked = Array.from((await nodeOf(laneLfo)).querySelectorAll(".tang-port--driven")).map((port) => port.getAttribute("data-handleid"));
      expect(marked).toEqual(["depth-0"]);
    });

    test("says what a cable that ends at the depth of another is, in the row of the one that drives it", async () => {
      await rig(<PatchView />, { snapshot: withCable(laneEnvelope, laneLfo, { type: "depth", edge: 0 }) });
      expect(await rowsOf(laneEnvelope)).toEqual([`Oscillator 1: ${paramNamed(2)}`, `Oscillator 1: ${paramNamed(1)}`, `depth of LFO 1 → Oscillator 1: ${paramNamed(1)}`]);
    });

    test("has ports that take a cable and start none: a row's port is for another modulator's cable to end at", async () => {
      await rig(<PatchView />, { snapshot });
      const ports = Array.from((await nodeOf(laneEnvelope)).querySelectorAll(".tang-cable-row .react-flow__handle"));
      expect(ports).toHaveLength(2);
      for (const port of ports) {
        expect(port.classList.contains("connectableend"), port.getAttribute("data-handleid") ?? "?").toBe(true);
        expect(port.classList.contains("connectablestart"), port.getAttribute("data-handleid") ?? "?").toBe(false);
      }
    });
  });

  describe("taking a modulator or a cable out", () => {
    test("is offered on every modulator and every effect, by a button that names it, and on no other node", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      // (A lane and a group are taken out by a menu that asks: `PatchView.groups.test.tsx`.)
      const names = (await screen.findAllByRole("button", { name: /^Remove / }))
        .map((button) => button.getAttribute("aria-label"))
        .filter((name) => !name?.startsWith("Remove cable") && !name?.startsWith("Remove the lane of ") && !/^Remove Group \d+$/.test(name ?? ""));
      // The fixture's modulators: two LFOs of lanes and one of a group (numbered), two envelopes, a control; and its effects: three filters (one on a bus), two reverbs (one on a bus).
      expect(names.sort()).toEqual(
        ["Remove ADSR 1", "Remove ADSR 2", "Remove LFO 1", "Remove LFO 2", "Remove LFO 3", "Remove MIDI (learning…)", "Remove Filter 1", "Remove Filter 2", "Remove Filter 3", "Remove Reverb 1", "Remove Reverb 2"].sort(),
      );
      // None on the master. A lane and a group have a menu that asks, which is not the button of an effect or a modulator.
      expect(within(await nodeOf(MASTER)).queryByRole("button", { name: /^Remove / })).toBeNull();
      for (const id of [0, 2, 4, 6, 11]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Remove (the lane of .+|Group \d+)$/ }), `node ${String(id)}`).toBeTruthy();
    });

    test("takes an effect out when its button is pressed, with the cable that drives it, and the focus goes to the button that adds one to its lane", async () => {
      const { requests, view } = await rig(<PatchView />);
      fireEvent.click(within(await nodeOf(filter.id)).getByRole("button", { name: "Remove Filter" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: filter.id }]);
      });
      await waitFor(() => {
        expect(view.container.querySelector(`[data-id="${String(filter.id)}"]`)).toBeNull();
      });
      // The LFO has nothing to drive now; the audio goes from the oscillator to the master.
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(0);
      expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(1);
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect to Oscillator" }));
      });
    });

    test("takes a bus effect out, and the focus goes to the button that adds one to its group", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      fireEvent.click(within(await nodeOf(3)).getByRole("button", { name: "Remove Reverb 1" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: 3 }]);
      });
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect to Group 2" }));
      });
    });

    test("says why when an effect could not be taken out, and it stays", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(within(await nodeOf(filter.id)).getByRole("button", { name: "Remove Filter" }));
      expect(await screen.findByText("Could not remove Filter: Not connected to the server.")).toBeTruthy();
      expect(await nodeOf(filter.id)).toBeTruthy();
    });

    test("takes a modulator out when its button is pressed, with its cable, and the focus goes to the button that adds one to its lane", async () => {
      const { requests, view } = await rig(<PatchView />);
      fireEvent.click(within(await nodeOf(lfo.id)).getByRole("button", { name: "Remove LFO" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: lfo.id }]);
      });
      await waitFor(() => {
        expect(view.container.querySelector(`[data-id="${String(lfo.id)}"]`)).toBeNull();
      });
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(0);
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a modulator to Oscillator" }));
      });
    });

    test("says why when the modulator could not be taken out, and it stays", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(within(await nodeOf(lfo.id)).getByRole("button", { name: "Remove LFO" }));
      expect(await screen.findByText("Could not remove LFO: Not connected to the server.")).toBeTruthy();
      expect(await nodeOf(lfo.id)).toBeTruthy();
    });

    test("has a button on each cable that takes it out, which names the cable", async () => {
      const { requests, view } = await rig(<PatchView />);
      const button = await screen.findByRole("button", { name: "Remove cable LFO → Filter: cutoff" });
      fireEvent.click(button);
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeEdge", edge: 0 }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(0);
      });
      // The modulator stays, and there is no depth to turn.
      expect(await nodeOf(lfo.id)).toBeTruthy();
      expect(screen.queryByRole("slider", { name: /^Depth of/ })).toBeNull();
    });

    test("gives the focus to the list of the modulator that drove the cable, where the next cable is drawn from, and not to the page", async () => {
      await rig(<PatchView />);
      const button = await screen.findByRole("button", { name: "Remove cable LFO → Filter: cutoff" });
      button.focus();
      fireEvent.click(button);
      await waitFor(() => {
        expect(screen.queryByRole("button", { name: /^Remove cable LFO/ })).toBeNull();
      });
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a cable from LFO" }));
      });
    });

    test("gives it to the list of the modulator of that cable, whichever modulators there are", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: null, hears: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 7, retrigger: false, fade: 0, sync: 0 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } });
      // (A second cable of the second modulator, so that it still has one cable when the first is taken out.)
      snapshot.graph.edges.push({ kind: "modulation", id: 2, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: oscillator.id, port: { type: "param", index: 1 } } });
      await rig(<PatchView />, { snapshot });
      for (const [cable, picker] of [
        ["Remove cable LFO 2 → Filter: cutoff", "Add a cable from LFO 2"],
        ["Remove cable LFO 1 → Filter: cutoff", "Add a cable from LFO 1"],
      ] as const) {
        fireEvent.click(await screen.findByRole("button", { name: cable }));
        await waitFor(() => {
          expect(document.activeElement).toBe(screen.getByRole("button", { name: picker }));
        });
      }
    });

    test("leaves the focus where it was when the cable could not be taken out, and says why", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const button = await screen.findByRole("button", { name: "Remove cable LFO → Filter: cutoff" });
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      button.focus();
      fireEvent.click(button);
      expect(await screen.findByText(/^Could not take out the cable LFO → Filter: cutoff: /)).toBeTruthy();
      expect(document.activeElement).toBe(button);
    });

    test("names two cables of two modulators of one name apart", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: null, hears: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 7, retrigger: false, fade: 0, sync: 0 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } });
      await rig(<PatchView />, { snapshot });
      expect(await screen.findByRole("button", { name: "Remove cable LFO 1 → Filter: cutoff" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Remove cable LFO 2 → Filter: cutoff" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Remove LFO 2" })).toBeTruthy();
    });
  });

  test("gives the master a level meter with a clip light", async () => {
    await rig(<PatchView />);
    const master = await nodeOf(4294967295);
    expect(within(master).getByRole("meter", { name: "Master level" }).getAttribute("aria-valuemin")).toBe("-60");
    expect(within(master).getByText("CLIP")).toBeTruthy();
  });

  describe("the scope of the master", () => {
    const optionsOf = (select: HTMLElement): string[] => Array.from(select.querySelectorAll("option"), (option) => option.textContent);

    test("is a scope of the output, a wave to begin with, with a choice of the view and of the time", async () => {
      await rig(<PatchView />);
      const master = await nodeOf(4294967295);
      expect(within(master).getByRole("meter", { name: "Output scope" }).getAttribute("data-view")).toBe("wave");
      // (The scope's two choices, and the volume and the limiter's ceiling, which are knobs: step 102.)
      expect(controlsIn(master)).toEqual(["View", "Time", "Master volume", "Ceiling"]);
      expect(optionsOf(within(master).getByRole("combobox", { name: "View" }))).toEqual(["Wave", "Spectrum"]);
      expect(optionsOf(within(master).getByRole("combobox", { name: "Time" }))).toEqual(["5 ms", "11 ms", "21 ms", "43 ms"]);
      expect(within(master).getByRole<HTMLSelectElement>("combobox", { name: "Time" }).value).toBe("2");
      // (The choices are not where the node is dragged from, nor panned or scrolled.)
      const choices = master.querySelector(".tang-scope-choices");
      expect(["nodrag", "nopan", "nowheel"].every((name) => choices?.classList.contains(name))).toBe(true);
    });

    test("says how long the times are at the sample rate of the engine", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.meta.sampleRate = 44_100;
      await rig(<PatchView />, { snapshot });
      const master = await nodeOf(4294967295);
      expect(optionsOf(within(master).getByRole("combobox", { name: "Time" }))).toEqual(["6 ms", "12 ms", "23 ms", "46 ms"]);
    });

    test("is the spectrum when that is chosen, which has no time base, and the wave again when that is", async () => {
      await rig(<PatchView />);
      const master = await nodeOf(4294967295);
      const scope = () => within(master).getByRole("meter", { name: "Output scope" });
      fireEvent.change(within(master).getByRole("combobox", { name: "View" }), { target: { value: "1" } });
      expect(scope().getAttribute("data-view")).toBe("spectrum");
      expect(controlsIn(master)).toEqual(["View", "Master volume", "Ceiling"]);
      fireEvent.change(within(master).getByRole("combobox", { name: "View" }), { target: { value: "0" } });
      expect(scope().getAttribute("data-view")).toBe("wave");
      expect(controlsIn(master)).toEqual(["View", "Time", "Master volume", "Ceiling"]);
    });

    test("keeps the time that was chosen when the view goes to the spectrum and back", async () => {
      await rig(<PatchView />);
      const master = await nodeOf(4294967295);
      fireEvent.change(within(master).getByRole("combobox", { name: "Time" }), { target: { value: "0" } });
      fireEvent.change(within(master).getByRole("combobox", { name: "View" }), { target: { value: "1" } });
      fireEvent.change(within(master).getByRole("combobox", { name: "View" }), { target: { value: "0" } });
      expect(within(master).getByRole<HTMLSelectElement>("combobox", { name: "Time" }).value).toBe("0");
    });

    test("is asked of the server for as long as the master is on the page, and no longer", async () => {
      // (The patch is taken off the page, and not the client: the client goes on, and so does what it asks for if nothing lets go.)
      let take: () => void = () => undefined;
      const Switch = () => {
        const [there, setThere] = useState(true);
        take = () => {
          setThere(false);
        };
        return there ? <PatchView /> : null;
      };
      const { client } = await rig(<Switch />);
      await waitFor(() => {
        expect(client.telemetry.framesOf("scope")).toBeGreaterThan(2);
      });
      act(() => {
        take();
      });
      await new Promise((resolve) => setTimeout(resolve, 30)); // (frames that were on their way)
      const frames = client.telemetry.framesOf("scope");
      await new Promise((resolve) => setTimeout(resolve, 80));
      expect(client.telemetry.framesOf("scope")).toBe(frames);
    });

    test("shows what the engine puts out: a note that is held is a value on the scope's meter", async () => {
      const { client, clock } = await rig(<PatchView />);
      const master = await nodeOf(4294967295);
      client.noteOn(69);
      let time = 0;
      await waitFor(() => {
        time += 16;
        clock.tick(time);
        expect(Number(within(master).getByRole("meter", { name: "Output scope" }).getAttribute("aria-valuenow"))).toBeGreaterThan(0.1);
      });
    });
  });
});

describe("a patch with a group", () => {
  /** The tracer's lane in a group, with the group between the filter and the master. */
  function grouped(): Snapshot {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.unshift({ id: 77, parent: null, name: "Pad", kind: "group", volume: 0.8, pan: -0.25, muted: false, solo: false, route: "out" });
    for (const node of snapshot.graph.nodes) if (node.id === oscillator.id) node.parent = 77;
    snapshot.graph.edges = snapshot.graph.edges.flatMap((edge) => {
      if (edge.kind === "audio" && edge.to.node === MASTER) {
        return [
          { kind: "member" as const, from: edge.from, to: { node: 77, port: { type: "audioIn" as const } } },
          { kind: "audio" as const, from: { node: 77, port: { type: "audioOut" as const } }, to: edge.to },
        ];
      }
      return [edge];
    });
    return snapshot;
  }

  test("has the badge of a group under its name, in the title of the header, and the name in a field when it is renamed, with the badge beside it", async () => {
    await rig(<PatchView />, { snapshot: grouped() });
    const group = await nodeOf(77);
    const title = group.querySelector(".tang-node__header > .tang-node__title");
    expect(Array.from(title?.children ?? [], (child) => child.className)).toEqual(["tang-node__name", "tang-node__badge", "tang-menu-button nodrag nopan tang-node__send"]);
    fireEvent.click(within(group).getByRole("button", { name: "Rename Pad" }));
    // (A name that is edited is a field in place of the title, and the badge goes beside it as it did.)
    const header = group.querySelector(".tang-node__header");
    expect(header?.querySelector(".tang-node__title")).toBeNull();
    expect(Array.from(header?.children ?? [], (child) => child.className.split(" ")[0])).toEqual(["tang-node__name-field", "tang-node__badge", "tang-node__actions"]);
  });

  test("has the group's volume and pan on its node, with ports for the lanes that feed it and the master", async () => {
    const { view } = await rig(<PatchView />, { snapshot: grouped() });
    const group = await nodeOf(77);
    expect(group.classList.contains("react-flow__node-group")).toBe(true);
    expect(within(group).getByText("Pad")).toBeTruthy();
    expect(within(group).getByText("Group", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(group).getByRole("slider", { name: "Volume" }).getAttribute("aria-valuenow")).toBe("0.8");
    expect(within(group).getByRole("slider", { name: "Pan" }).getAttribute("aria-valuenow")).toBe("-0.25");
    // (The audio ports, and the places where a cable from a modulator ends at the volume and the pan.)
    expect(Array.from(group.querySelectorAll(".react-flow__handle")).map((handle) => handle.getAttribute("data-handleid"))).toEqual(["audioIn", "audioOut", "volume", "pan"]);
    // oscillator → filter, filter → group, group → master, and the modulation.
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
    });
  });
});

describe("the master's meter", () => {
  test("lights the clip light when the count of clipped buffers goes up, from the telemetry", async () => {
    // The mock makes no telemetry here (its period is long), so that the frames are the test's own.
    const { client, clock, view } = await rig(<PatchView />, { telemetryPeriod: 3_600_000 });
    const { layout } = { layout: client.state.snapshot?.telemetry };
    if (!layout) throw new Error("no layout");
    const frame = (clips: number): ArrayBuffer => {
      const values = layout.meters.map((entry) => (entry.kind === "clips" ? clips : 0.25));
      const buffer = new ArrayBuffer(8 + 4 * values.length);
      const data = new DataView(buffer);
      data.setUint16(2, values.length, true);
      data.setUint32(4, layout.rev, true);
      values.forEach((value, i) => {
        data.setFloat32(8 + 4 * i, value, true);
      });
      return buffer;
    };
    const accept = (clips: number) => {
      const decoded = decodeFrame(frame(clips));
      if (decoded) client.telemetry.accept(decoded);
    };
    const light = view.container.querySelector(".tang-clip");
    accept(0);
    act(() => {
      clock.tick(0);
    });
    expect(light?.hasAttribute("data-lit")).toBe(false);
    accept(1);
    act(() => {
      clock.tick(100);
    });
    expect(light?.hasAttribute("data-lit")).toBe(true);
    act(() => {
      clock.tick(2200);
    });
    expect(light?.hasAttribute("data-lit")).toBe(false);
    // And the level meter shows the 0.25 of full scale the frames carried: about -12 dB.
    expect(Number(within(await nodeOf(MASTER)).getByRole("meter", { name: "Master level" }).getAttribute("aria-valuenow"))).toBeCloseTo(-12.04, 1);
  });
});

describe("whether the canvas is the same", () => {
  test("it is when both have no session, or when what differs is only values", () => {
    expect(sameGraphStructure(null, null)).toBe(true);
    const changed = structuredClone(tracerSnapshot.graph);
    for (const node of changed.nodes) if (node.kind === "instrument") node.volume = 0.5;
    expect(sameGraphStructure(tracerSnapshot.graph, changed)).toBe(true);
  });

  test("it is not when one has a session and the other has not, or they differ in structure", () => {
    expect(sameGraphStructure(null, tracerSnapshot.graph)).toBe(false);
    expect(sameGraphStructure(tracerSnapshot.graph, null)).toBe(false);
    expect(sameGraphStructure(tracerSnapshot.graph, { nodes: tracerSnapshot.graph.nodes.slice(1), edges: tracerSnapshot.graph.edges })).toBe(false);
  });
});

describe("the patch while it plays", () => {
  test("has the cutoff's dot and ring where the LFO takes it, the LFO's bar where it is, and the master's level", async () => {
    const { client, clock, time } = await rig(<PatchView />);
    const cutoff = within(await nodeOf(filter.id)).getByRole("slider", { name: "cutoff" }).closest(".tang-knob");
    const bar = within(await nodeOf(lfo.id)).getByRole("meter");
    const level = within(await nodeOf(4294967295)).getByRole("meter", { name: "Master level" });
    expect(cutoff?.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.1, 0.9));
    time.seconds = 0.5; // the LFO is at its peak
    client.noteOn(60); // and a note is held
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      const dot = cutoff?.querySelector(".tang-knob__dot");
      expect(dot?.getAttribute("visibility")).toBe("visible");
      expect(Number(dot?.getAttribute("cx"))).toBeCloseTo(pointAt(24, 18, angleOf(0.9)).x, 3);
      expect(bar.getAttribute("aria-valuenow")).toBe("1");
      expect(level.getAttribute("aria-valuenow")).not.toBeNull();
    });
    const fill = (element: Element) => element.querySelector<HTMLElement>(".tang-livebar__fill")?.style;
    expect(Number(fill(level)?.getPropertyValue("--to"))).toBeGreaterThan(0.7); // 0.35 of full scale is about -9 dB
  });

  test("draws what the LFO puts out as a trace on its node's canvas, at the frame, and a dot at the newest value", async () => {
    const context = recordingContext();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation((() => context) as never);
    const { clock, time } = await rig(<PatchView />);
    const node = await nodeOf(lfo.id);
    const canvas = node.querySelector("canvas");
    if (!canvas) throw new Error("the LFO's node has no canvas");
    canvas.style.width = "280px";
    canvas.style.height = "44px";
    time.seconds = 0.5; // the LFO is at its peak
    await waitFor(() => {
      context.calls.length = 0;
      act(() => {
        clock.tick(performance.now());
      });
      // (The newest value is at the top of the 44 px less the room above it, once the engine's frame with the LFO at its peak is in.)
      expect(callsOf(context, "arc")[0]?.[1]).toBe(3);
    });
    // And at the right of the 280 px, with the mark across the middle drawn.
    expect(callsOf(context, "arc")[0]?.[0]).toBe(277);
    expect(callsOf(context, "moveTo")[0]).toEqual([0, 22.5]);
  });

  describe("a cable that is lit by what its modulator puts out", () => {
    /** The session with a modulator's output in the layout of the telemetry too (a modulator put in by hand has no slot to read). */
    const withOutput = (snapshot: Snapshot, node: number): Snapshot => {
      const modulation = snapshot.telemetry.modulation;
      modulation.splice(modulation.findLastIndex((entry) => entry.kind === "modOutput") + 1, 0, { slot: 500 + node, node, port: { type: "modOut" }, kind: "modOutput" });
      return snapshot;
    };

    /** What the cables of the canvas read as the signal that goes through them, by cable, as a number (or nothing). */
    const signals = (container: HTMLElement): (number | null)[] =>
      Array.from(container.querySelectorAll<SVGGElement>(".tang-cable__light")).map((light) => {
        const value = light.style.getPropertyValue("--signal");
        return value === "" ? null : Number(value);
      });

    test("has the output of an LFO across its range, from -1 at nothing to 1 at all: the cable follows it", async () => {
      const { clock, time, view } = await rig(<PatchView />);
      await nodeOf(lfo.id);
      // Nothing is known before a frame of the engine's telemetry comes: the cable is as it was.
      expect(signals(view.container)).toEqual([null]);
      const lit = async (seconds: number, expected: number) => {
        time.seconds = seconds;
        await waitFor(() => {
          act(() => {
            clock.tick(performance.now());
          });
          expect(signals(view.container)[0]).toBeCloseTo(expected, 5);
        });
      };
      await lit(0.5, 1); // 0.5 Hz: the peak
      await lit(0, 0.5); // a crossing
      await lit(1.5, 0); // the trough
    });

    test("has the output of an envelope across 0 to 1, which an LFO's range would give another number for", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: null, hears: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 1 } } });
      const { client, clock, view } = await rig(<PatchView />, { snapshot: withOutput(snapshot, 90) });
      await nodeOf(90);
      client.noteOn(60); // the mock's envelope is at its sustain while a note is held
      await waitFor(() => {
        act(() => {
          clock.tick(performance.now());
        });
        // (The cables are the LFO's first, then the envelope's.)
        expect(signals(view.container)[1]).toBeCloseTo(0.7, 5);
      });
    });

    test("is lit by the modulator that drives it: two cables of two modulators have each their own", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: null, hears: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 1, retrigger: false, fade: 0, sync: 0 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 1 } } });
      const { clock, time, view } = await rig(<PatchView />, { snapshot: withOutput(snapshot, 90) });
      await nodeOf(90);
      time.seconds = 0.25; // the first (a sine at 0.5 Hz) is at sin(π/4) of -1…1, the second (a square at 1 Hz) is at 1
      await waitFor(() => {
        act(() => {
          clock.tick(performance.now());
        });
        const [first, second] = signals(view.container);
        expect(first).toBeCloseTo((1 + Math.sin(2 * Math.PI * 0.5 * 0.25)) / 2, 5);
        expect(second).toBe(1);
      });
    });

    test("is lit by an LFO's cable to the setting of another modulator as by a cable to a parameter", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: null, hears: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.5, from: { node: lfo.id, port: { type: "modOut" } }, to: { node: 90, port: { type: "release" } } });
      const { clock, time, view } = await rig(<PatchView />, { snapshot });
      await nodeOf(90);
      time.seconds = 0.5;
      await waitFor(() => {
        act(() => {
          clock.tick(performance.now());
        });
        expect(signals(view.container)).toEqual([1, 1]);
      });
    });
  });

  test("follows a parameter somebody else changes without making the canvas again", async () => {
    const { server } = await rig(<PatchView />);
    const fil = await nodeOf(filter.id);
    const nodes = Array.from(document.querySelectorAll(".react-flow__node"));
    const cutoff = within(fil).getByRole("slider", { name: "cutoff" });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.8 });
    });
    await waitFor(() => {
      expect(cutoff.getAttribute("aria-valuenow")).toBe("0.8");
    });
    // The same elements, not new ones: nothing was laid out again.
    expect(Array.from(document.querySelectorAll(".react-flow__node"))).toEqual(nodes);
  });

  test("is laid out again when the session changes in structure: a node comes", async () => {
    const { client, view } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(filter.id)}`);
    expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(4);
    const bigger: Snapshot = structuredClone(tracerSnapshot);
    bigger.rev = 1;
    bigger.graph.nodes.splice(2, 0, { id: 88, parent: 0, name: "Second filter", kind: "effect", plugin: structuredClone(filterPlugin), mix: 1, feed: "chain" });
    act(() => {
      client.store.setState({ snapshot: bigger });
    });
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(within(await nodeOf(88)).getByText("Second filter")).toBeTruthy();
  });

  test("has nothing on it before there is a session", async () => {
    const { view, client } = await rig(<PatchView />);
    act(() => {
      client.store.setState({ snapshot: null });
    });
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(0);
    });
  });
});
