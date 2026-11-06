import { describe, expect, test } from "vitest";

import type { ClientState } from "./connection";
import { MASTER, audioCablesFixture, groupsFixture, movesFixture, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { audibleAt, cableDepthOf, canOpenSession, cableGainOf, cableSourceOf, chainLengthOf, chainPlaceOf, envelopeStagesOf, flagsOf, gainOf, groupChoicesOf, hasPins, hasWindowOf, isKnob, limiterOf, modulatorFieldOf, nodeById, paramCountOf, paramListOf, paramOf, patternOf, presetsOf, rangeOf, removalOf, sameChainPlace, sameFlags, sameGroupChoices, sameParamList, sameRemoval, sameStages, stepsOf } from "./selectors";
import type { Node, Snapshot } from "../protocol";
import type { Removal } from "./selectors";

const stateOf = (snapshot: Snapshot | null): ClientState => ({ status: "open", problem: null, hello: null, snapshot, catalog: null, inputs: [], history: { undo: null, redo: null }, reconnects: 0, probed: [] });

const state = stateOf(tracerSnapshot);
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");

describe("finding what a view shows", () => {
  test("a node by its id", () => {
    expect(nodeById(state, filter.id)).toBe(filter);
    expect(nodeById(state, MASTER)?.kind).toBe("master");
    expect(nodeById(state, 12345)).toBeUndefined();
  });

  test("nothing, before there is a session", () => {
    const empty = stateOf(null);
    expect(nodeById(empty, filter.id)).toBeUndefined();
    expect(paramOf(empty, filter.id, 0)).toBeUndefined();
    expect(gainOf(empty, filter.id, "mix")).toBeUndefined();
  });

  test("a parameter of a plugin by its own index, in an instrument and in an effect", () => {
    expect(paramOf(state, filter.id, 0)?.name).toBe("cutoff");
    expect(paramOf(state, filter.id, 1)?.name).toBe("resonance");
    expect(paramOf(state, oscillator.id, 1)?.name).toBe("detune");
    expect(paramOf(state, oscillator.id, 0)?.labels).toEqual(["sine", "triangle", "square", "saw", "noise"]);
  });

  test("no parameter of a node that has none, or of one the plugin does not have", () => {
    expect(paramOf(state, filter.id, 99)).toBeUndefined();
    expect(paramOf(state, lfo.id, 0)).toBeUndefined();
    expect(paramOf(state, MASTER, 0)).toBeUndefined();
    expect(paramOf(state, 12345, 0)).toBeUndefined();
  });

  test("an instrument that has no plugin has no parameters", () => {
    const emptied = structuredClone(tracerSnapshot);
    const lane = emptied.graph.nodes.find((node) => node.kind === "instrument");
    if (lane?.kind === "instrument") lane.plugin = null;
    expect(paramOf(stateOf(emptied), oscillator.id, 0)).toBeUndefined();
  });

  test("a gain, of the node it belongs to", () => {
    expect(gainOf(state, oscillator.id, "volume")).toBeCloseTo(0.1, 6);
    expect(gainOf(state, filter.id, "mix")).toBe(1);
  });

  test("the volume of the output mix and the ceiling of its limiter, which the master has and nothing else does, and whether the limiter is in the sound", () => {
    expect(gainOf(state, MASTER, "masterVolume")).toBe(1);
    expect(gainOf(state, MASTER, "limiterCeiling")).toBe(-1);
    expect(gainOf(state, oscillator.id, "masterVolume")).toBeUndefined();
    expect(gainOf(state, filter.id, "limiterCeiling")).toBeUndefined();
    expect(gainOf(state, MASTER, "volume")).toBeUndefined();
    expect(limiterOf(state)).toBe(true);
    const out = structuredClone(tracerSnapshot);
    const master = out.graph.nodes.find((node) => node.kind === "master");
    if (master?.kind === "master") master.limiter = false;
    expect(limiterOf(stateOf(out))).toBe(false);
    expect(limiterOf(stateOf(null))).toBeUndefined();
  });

  test("no gain that the node does not have", () => {
    expect(gainOf(state, oscillator.id, "mix")).toBeUndefined();
    expect(gainOf(state, filter.id, "volume")).toBeUndefined();
    expect(gainOf(state, filter.id, "groupVolume")).toBeUndefined();
    expect(gainOf(state, oscillator.id, "groupPan")).toBeUndefined();
    expect(gainOf(state, lfo.id, "volume")).toBeUndefined();
    expect(gainOf(state, MASTER, "mix")).toBeUndefined();
  });

  test("a group's volume and pan", () => {
    const grouped = structuredClone(tracerSnapshot);
    grouped.graph.nodes.push({ id: 77, parent: null, name: "Group 1", kind: "group", volume: 0.8, pan: -0.25, muted: false, solo: false, route: "out" });
    const withGroup = stateOf(grouped);
    expect(gainOf(withGroup, 77, "groupVolume")).toBeCloseTo(0.8, 6);
    expect(gainOf(withGroup, 77, "groupPan")).toBeCloseTo(-0.25, 6);
    expect(gainOf(withGroup, 77, "volume")).toBeUndefined();
    expect(gainOf(withGroup, 77, "mix")).toBeUndefined();
  });
});

describe("the settings of a modulator and the depths of its cables", () => {
  const envelope: Node = {
    id: 41,
    parent: oscillator.id,
    name: "ADSR",
    kind: "modulator",
    source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
  };
  const control: Node = { id: 42, parent: oscillator.id, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
  const macro: Node = { id: 43, parent: oscillator.id, name: "Macro", kind: "modulator", source: { type: "macro", value: 0.25 } };
  const withMore = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, envelope, control, macro] } });

  test("an LFO's rate, and its waveform as the index the intent takes", () => {
    expect(modulatorFieldOf(state, lfo.id, "rate")).toBe(0.5);
    expect(modulatorFieldOf(state, lfo.id, "waveform")).toBe(0);
    const saw = structuredClone(tracerSnapshot);
    const node = saw.graph.nodes.find((n) => n.id === lfo.id);
    if (node?.kind === "modulator" && node.source.type === "lfo") node.source.waveform = "saw";
    expect(modulatorFieldOf(stateOf(saw), lfo.id, "waveform")).toBe(2);
  });

  test("the value of a macro, which is the only setting it has", () => {
    expect(modulatorFieldOf(withMore, macro.id, "value")).toBe(0.25);
    for (const field of ["rate", "waveform", "attack", "decay", "sustain", "release"] as const) expect(modulatorFieldOf(withMore, macro.id, field), field).toBeUndefined();
    // (And an LFO, an envelope and a control have no value.)
    expect([lfo.id, envelope.id, control.id].map((id) => modulatorFieldOf(withMore, id, "value"))).toEqual([undefined, undefined, undefined]);
  });

  const steps = [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)];
  const sequencer: Node = { id: 44, parent: oscillator.id, name: "Steps", kind: "modulator", source: { type: "steps", values: steps, length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 0 } };
  const withSequencer = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, sequencer, macro] } });

  test("the rate, the glide, the length and the switch of a step sequencer, as numbers, and none of the settings of the other kinds", () => {
    expect((["rate", "glide", "length", "retrigger"] as const).map((field) => modulatorFieldOf(withSequencer, sequencer.id, field))).toEqual([6, 0.25, 4, 0]);
    for (const field of ["waveform", "attack", "decay", "sustain", "release", "value"] as const) expect(modulatorFieldOf(withSequencer, sequencer.id, field), field).toBeUndefined();
    // (And the others have no glide or length, and a macro no switch: an LFO has one, which is its key's.)
    for (const id of [lfo.id, macro.id]) {
      for (const field of ["glide", "length"] as const) expect(modulatorFieldOf(withSequencer, id, field), `${field} of ${id}`).toBeUndefined();
    }
    expect(modulatorFieldOf(withSequencer, macro.id, "retrigger")).toBeUndefined();
  });

  test("the retrigger and the fade of an LFO, as numbers, and none for the other kinds but the switch of a step sequencer", () => {
    const keyed: Node = { id: lfo.id, parent: lfo.parent, name: lfo.name, kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 2, retrigger: true, fade: 0.75, sync: 0 } };
    const state = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes.filter((n) => n.id !== lfo.id), keyed, sequencer, macro] } });
    expect((["retrigger", "fade"] as const).map((field) => modulatorFieldOf(state, keyed.id, field))).toEqual([1, 0.75]);
    expect(modulatorFieldOf(withSequencer, lfo.id, "retrigger"), "a free LFO's switch is off").toBe(0);
    expect(modulatorFieldOf(withSequencer, lfo.id, "fade")).toBe(0);
    expect(modulatorFieldOf(state, sequencer.id, "fade"), "a step sequencer has no fade").toBeUndefined();
    expect(modulatorFieldOf(state, macro.id, "fade")).toBeUndefined();
    expect(modulatorFieldOf(state, macro.id, "retrigger")).toBeUndefined();
  });

  test("the note length that an LFO and a step sequencer follow the tempo by, as a place in the list (0 for none), and none for the other kinds", () => {
    const synced: Node = { id: lfo.id, parent: lfo.parent, name: lfo.name, kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 2, retrigger: false, fade: 0, sync: 11 } };
    const timed: Node = { ...sequencer, source: { type: "steps", values: steps, length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 14 } };
    const state = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes.filter((n) => n.id !== lfo.id), synced, timed, macro, envelope, control] } });
    expect([synced.id, timed.id].map((id) => modulatorFieldOf(state, id, "sync"))).toEqual([11, 14]);
    expect([lfo.id, sequencer.id].map((id) => modulatorFieldOf(withSequencer, id, "sync")), "the ones that are free").toEqual([0, 0]);
    expect([macro.id, envelope.id, control.id].map((id) => modulatorFieldOf(state, id, "sync")), "an envelope, a macro and a control have none").toEqual([undefined, undefined, undefined]);
  });

  test("the switch of a step sequencer that a note starts again is 1", () => {
    const on: Node = { ...sequencer, source: { type: "steps", values: steps, length: 4, rate: 6, glide: 0.25, retrigger: true, sync: 0 } };
    expect(modulatorFieldOf(stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, on] } }), on.id, "retrigger")).toBe(1);
  });

  test("the steps of a step sequencer, all of them and the very list that the node has, and none for a node that is not one", () => {
    expect(stepsOf(withSequencer, sequencer.id)).toBe(steps);
    expect([lfo.id, macro.id, oscillator.id, 12345].map((id) => stepsOf(withSequencer, id))).toEqual([undefined, undefined, undefined, undefined]);
    expect(stepsOf(stateOf(null), sequencer.id)).toBeUndefined();
  });

  test("the stages of an envelope", () => {
    expect(["attack", "decay", "sustain", "release"].map((field) => modulatorFieldOf(withMore, envelope.id, field as "attack"))).toEqual([0.01, 0.3, 0.7, 0.5]);
  });

  test("the index of each of the waveforms", () => {
    const names = ["sine", "triangle", "saw", "square", "random", "smooth"] as const;
    const indexes = names.map((name) => {
      const copy = structuredClone(tracerSnapshot);
      const node = copy.graph.nodes.find((n) => n.id === lfo.id);
      if (node?.kind === "modulator" && node.source.type === "lfo") node.source.waveform = name;
      return modulatorFieldOf(stateOf(copy), lfo.id, "waveform");
    });
    expect(indexes).toEqual([0, 1, 2, 3, 4, 5]);
  });

  test("no setting that the modulator does not have, or of a node that is not one", () => {
    expect(modulatorFieldOf(state, lfo.id, "attack")).toBeUndefined();
    expect(modulatorFieldOf(withMore, envelope.id, "rate")).toBeUndefined();
    expect(modulatorFieldOf(withMore, envelope.id, "waveform")).toBeUndefined();
    expect(modulatorFieldOf(withMore, control.id, "rate")).toBeUndefined();
    expect(modulatorFieldOf(state, filter.id, "rate")).toBeUndefined();
    expect(modulatorFieldOf(state, 12345, "rate")).toBeUndefined();
    expect(modulatorFieldOf(stateOf(null), lfo.id, "rate")).toBeUndefined();
  });

  test("the depth of a cable by its id", () => {
    expect(cableDepthOf(state, 0)).toBe(0.4);
    expect(cableDepthOf(state, 12345)).toBeUndefined();
    expect(cableDepthOf(stateOf(null), 0)).toBeUndefined();
  });

  test("the modulator a cable starts at, by the id of the cable: the node whose depth setting the cable's slot is named at", () => {
    expect(cableSourceOf(state, 0)).toBe(lfo.id);
    expect(cableSourceOf(state, 12345)).toBeUndefined();
    expect(cableSourceOf(stateOf(null), 0)).toBeUndefined();
    // (And not of an edge that is not a cable of modulation.)
    expect(cableSourceOf(state, 1)).toBeUndefined();
  });

  test("no depth of an edge that is not a cable of modulation", () => {
    // The audio edges have no id of the kind: the number that is the id of a cable is not theirs.
    expect(tracerSnapshot.graph.edges.filter((edge) => edge.kind !== "modulation")).not.toHaveLength(0);
    expect(cableDepthOf(state, 1)).toBeUndefined();
  });
});

describe("finding the level of an audio cable", () => {
  const withSends = stateOf(audioCablesFixture.snapshot);

  test("by its id, and none for an id that is no audio cable's: a cable of modulation's, one that is not there, or a session that is not there", () => {
    expect(cableGainOf(withSends, 1)).toBe(0.5);
    expect(cableGainOf(withSends, 2)).toBe(0.75);
    expect(cableGainOf(withSends, 3)).toBe(1.5);
    expect(cableGainOf(withSends, 4)).toBe(1);
    expect(cableGainOf(withSends, 0)).toBeUndefined(); // the LFO's cable
    expect(cableGainOf(withSends, 12345)).toBeUndefined();
    expect(cableGainOf(stateOf(null), 1)).toBeUndefined();
    expect(cableGainOf(state, 0)).toBeUndefined();
  });

  test("and the depth of a cable is not found for an audio cable", () => {
    expect(cableDepthOf(withSends, 1)).toBeUndefined();
    expect(cableDepthOf(withSends, 0)).toBe(0.5);
  });
});

describe("finding the presets a plugin can be given", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("those of an effect of a lane, with none loaded to begin with", () => {
    const found = presetsOf(reach, 8);
    expect(found?.presets.map((p) => p.id)).toEqual(["arcadia", "cathedral", "plate", "room", "ambient"]);
    expect(found?.current).toBeNull();
  });

  test("and the one that was loaded last, by its name", () => {
    const loaded = structuredClone(reachFixture.snapshot);
    const reverb = loaded.graph.nodes.find((n) => n.id === 8);
    if (reverb?.kind !== "effect") throw new Error("no reverb");
    reverb.plugin.currentPreset = "Cathedral";
    expect(presetsOf(stateOf(loaded), 8)?.current).toBe("Cathedral");
  });

  test("those of an instrument", () => {
    const given = structuredClone(tracerSnapshot);
    const osc = given.graph.nodes.find((n) => n.id === oscillator.id);
    if (osc?.kind !== "instrument" || !osc.plugin) throw new Error("no instrument");
    osc.plugin.presets = [{ id: "pad", name: "Pad" }];
    expect(presetsOf(stateOf(given), oscillator.id)?.presets).toEqual([{ id: "pad", name: "Pad" }]);
  });

  test("none for an effect on a group's bus (a preset is loaded by a lane's slot), though it has them", () => {
    expect(paramOf(reach, 3, 0)).toBeDefined();
    expect(presetsOf(reach, 3)).toBeNull();
  });

  test("none for a node that has no plugin, a lane that has none, a node that is not there, or before there is a session", () => {
    expect(presetsOf(reach, 15)).toBeNull(); // a modulator
    expect(presetsOf(reach, 0)).toBeNull(); // a group
    expect(presetsOf(reach, 11)).toBeNull(); // a lane with no instrument
    expect(presetsOf(reach, 424242)).toBeNull();
    expect(presetsOf(reach, MASTER)).toBeNull();
    expect(presetsOf(stateOf(null), 8)).toBeNull();
  });

  test("a plugin that has no presets has an empty list: the caller decides what that is worth", () => {
    expect(presetsOf(state, filter.id)).toEqual({ presets: [], current: null });
  });
});

describe("finding the parameters of a plugin to look through", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("those of an instrument and of an effect, each by the plugin's own index, with the node's name and the plugin's format", () => {
    expect(paramListOf(state, oscillator.id)).toEqual({
      name: "Oscillator",
      format: "Built-in",
      params: ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "unison", "spread", "width", "pulse", "fm", "fm_ratio", "sub", "sync", "drift", "velocity", "bend", "quantize"].map((name, index) => ({ index, name })),
    });
    expect(paramListOf(state, filter.id)?.params).toEqual([
      { index: 0, name: "cutoff" },
      { index: 1, name: "resonance" },
      { index: 2, name: "type" },
      { index: 3, name: "drive" },
    ]);
  });

  test("by the plugin's own index, which is not its place in the list: a plugin can number its parameters with gaps", () => {
    const gaps = structuredClone(tracerSnapshot);
    const node = gaps.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = node.plugin.params.map((param, position) => ({ ...param, index: [3, 8, 20, 21][position] ?? 99 }));
    expect(paramListOf(stateOf(gaps), filter.id)?.params.map((param) => param.index)).toEqual([3, 8, 20, 21]);
  });

  test("those of an effect on a group's bus too: it has parameters to turn, whatever its presets", () => {
    expect(paramListOf(reach, 3)?.params.length).toBeGreaterThan(0);
    expect(presetsOf(reach, 3)).toBeNull();
  });

  test("the node's name is the one the canvas gives it: numbered when two nodes are called the same", () => {
    const twins = structuredClone(tracerSnapshot);
    const copy = structuredClone(oscillator);
    twins.graph.nodes.push({ ...copy, id: 777 });
    const twinned = stateOf(twins);
    expect(paramListOf(twinned, oscillator.id)?.name).toBe("Oscillator 1");
    expect(paramListOf(twinned, 777)?.name).toBe("Oscillator 2");
  });

  test("none for a node that has no plugin, a lane that has none, a node that is not there, or before there is a session", () => {
    expect(paramListOf(reach, 15)).toBeNull(); // a modulator
    expect(paramListOf(reach, 0)).toBeNull(); // a group
    expect(paramListOf(reach, 14)).toBeNull(); // a lane with no instrument
    expect(paramListOf(reach, 424242)).toBeNull();
    expect(paramListOf(reach, MASTER)).toBeNull();
    expect(paramListOf(stateOf(null), 10)).toBeNull();
  });

  test("a plugin that has no parameters has an empty list", () => {
    const bare = structuredClone(tracerSnapshot);
    const node = bare.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = [];
    expect(paramListOf(stateOf(bare), filter.id)?.params).toEqual([]);
  });

  test("is the same list when only a value changed, and another when a name, an index, the format, the node's name or the number of parameters did", () => {
    const base = paramListOf(state, filter.id);
    if (!base) throw new Error("no list");
    const moved = structuredClone(tracerSnapshot);
    const node = moved.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = node.plugin.params.map((param, position) => (position === 0 ? { ...param, value: 0.9 } : param));
    expect(sameParamList(base, paramListOf(stateOf(moved), filter.id))).toBe(true);
    expect(sameParamList(base, base)).toBe(true);
    expect(sameParamList(null, null)).toBe(true);
    expect(sameParamList(base, null)).toBe(false);
    expect(sameParamList(null, base)).toBe(false);

    const other = (change: (list: { name: string; format: string; params: { index: number; name: string }[] }) => void) => {
      const copy = structuredClone(base) as { name: string; format: string; params: { index: number; name: string }[] };
      change(copy);
      return sameParamList(base, copy);
    };
    expect(other((l) => (l.params[1] = { index: 1, name: "q" }))).toBe(false);
    expect(other((l) => (l.params[1] = { index: 5, name: "resonance" }))).toBe(false);
    expect(other((l) => (l.format = "VST3"))).toBe(false);
    expect(other((l) => (l.name = "Filter 2"))).toBe(false);
    expect(other((l) => l.params.pop())).toBe(false);
    expect(other((l) => l.params.push({ index: 4, name: "extra" }))).toBe(false);
  });
});

describe("counting the parameters of a plugin", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("is how many the plugin of an instrument or an effect has, whatever the effect is in", () => {
    expect(paramCountOf(state, oscillator.id)).toBe(20);
    expect(paramCountOf(state, filter.id)).toBe(4);
    expect(paramCountOf(reach, 3)).toBe(paramListOf(reach, 3)?.params.length);
  });

  test("is none for a node that has no plugin, a node that is not there, or before there is a session", () => {
    expect(paramCountOf(reach, 15)).toBe(0); // a modulator
    expect(paramCountOf(reach, 0)).toBe(0); // a group
    expect(paramCountOf(reach, 14)).toBe(0); // a lane with no instrument
    expect(paramCountOf(reach, MASTER)).toBe(0);
    expect(paramCountOf(reach, 424242)).toBe(0);
    expect(paramCountOf(stateOf(null), filter.id)).toBe(0);
  });
});

describe("whether a plugin has a window of its own that can be opened (step 112)", () => {
  const shows = { protocol: 1, rev: 0, features: ["notes", "telemetry", "windows"], needsToken: false };
  /** The tracer with the oscillator's plugin saying that it has a window of its own (the filter's does not); and what the server's hello says. */
  const windowed = (hello: ClientState["hello"], also: readonly number[] = []): ClientState => {
    const copy = structuredClone(tracerSnapshot);
    for (const found of copy.graph.nodes) {
      if ((found.kind === "instrument" || found.kind === "effect") && found.plugin && (found.id === oscillator.id || also.includes(found.id))) found.plugin.hasEditor = true;
    }
    return { ...stateOf(copy), hello };
  };

  test("is so for an instrument or an effect whose plugin says so, on a server whose machine can show a window", () => {
    const on = windowed(shows, [filter.id]);
    expect(hasWindowOf(on, oscillator.id)).toBe(true);
    expect(hasWindowOf(on, filter.id)).toBe(true);
  });

  test("is not so for a plugin that says it has none, nor for a node that has no plugin, a node that is not there or before there is a session", () => {
    const on = windowed(shows);
    expect(hasWindowOf(on, filter.id)).toBe(false);
    expect(hasWindowOf(on, lfo.id)).toBe(false);
    expect(hasWindowOf(on, MASTER)).toBe(false);
    expect(hasWindowOf(on, 424242)).toBe(false);
    expect(hasWindowOf({ ...stateOf(null), hello: shows }, oscillator.id)).toBe(false);
  });

  test("is not so on a server whose machine cannot show a window, or before it has said hello, whatever the plugin says", () => {
    expect(hasWindowOf(windowed({ ...shows, features: ["notes", "telemetry"] }), oscillator.id)).toBe(false);
    expect(hasWindowOf(windowed(null), oscillator.id)).toBe(false);
  });
});

describe("whether the page may offer to open a session file (step 115)", () => {
  const hello = (features: readonly string[]): ClientState["hello"] => ({ protocol: 1, rev: 0, features: [...features], needsToken: false });

  test("is so on a server that says openSession among what it can do, before or after the others", () => {
    expect(canOpenSession({ ...state, hello: hello(["notes", "telemetry", "openSession"]) })).toBe(true);
    expect(canOpenSession({ ...state, hello: hello(["notes", "telemetry", "windows", "openSession"]) })).toBe(true);
  });

  test("is not so on a server that does not say it (a program that is started with its file), nor before it has said hello", () => {
    expect(canOpenSession({ ...state, hello: hello(["notes", "telemetry", "windows"]) })).toBe(false);
    expect(canOpenSession({ ...state, hello: hello([]) })).toBe(false);
    expect(canOpenSession({ ...state, hello: null })).toBe(false);
  });

  test("is not so for a word that only contains it: the features are whole words", () => {
    expect(canOpenSession({ ...state, hello: hello(["openSessions", "openSessio"]) })).toBe(false);
  });
});

describe("finding the settings of an envelope", () => {
  // The reach fixture's modulators: LFOs (12, 15, 17), envelopes (13, 16) and a MIDI control (14), the envelopes at the engine's defaults.
  const reach = stateOf(reachFixture.snapshot);

  test("are its four stages, by name: the three times and the sustain level", () => {
    expect(envelopeStagesOf(reach, 13)).toEqual({ attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 });
    expect(envelopeStagesOf(reach, 16)).toEqual({ attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 });
  });

  test("are none for what is no envelope: an LFO, a MIDI control, a plugin, a node that is not there, or before there is a session", () => {
    for (const id of [12, 14, 15, 17, 0, 12345, MASTER]) expect(envelopeStagesOf(reach, id), String(id)).toBeNull();
    expect(envelopeStagesOf(stateOf(null), 13)).toBeNull();
  });

  test("follow a stage that changes, and are the same settings while nothing of them does", () => {
    const changed = structuredClone(reachFixture.snapshot);
    const envelope = changed.graph.nodes.find((n) => n.id === 13);
    if (envelope?.kind === "modulator" && envelope.source.type === "envelope") envelope.source.release = 4;
    expect(envelopeStagesOf(stateOf(changed), 13)).toEqual({ attack: 0.01, decay: 0.3, sustain: 0.7, release: 4 });
    expect(sameStages(envelopeStagesOf(reach, 13), envelopeStagesOf(stateOf(structuredClone(reachFixture.snapshot)), 13))).toBe(true);
    for (const stage of ["attack", "decay", "sustain", "release"] as const) {
      const a = { attack: 1, decay: 2, sustain: 0.5, release: 3 };
      expect(sameStages(a, { ...a, [stage]: a[stage] + 1 }), stage).toBe(false);
    }
    expect(sameStages(null, null)).toBe(true);
    expect(sameStages(null, { attack: 1, decay: 2, sustain: 0.5, release: 3 })).toBe(false);
    expect(sameStages({ attack: 1, decay: 2, sustain: 0.5, release: 3 }, null)).toBe(false);
  });
});

describe("finding where an effect is in its chain", () => {
  // The moves fixture: a bus of four effects (1 to 4) of the group 0, a lane (5) in it with four effects (6 to 9) and a lane (10) in none with three (11 to 13).
  const moves = stateOf(movesFixture.snapshot);

  test("is its place, counted from 0, and how many effects the chain has: of a lane's chain and of a bus", () => {
    expect(chainPlaceOf(moves, 6)).toEqual({ index: 0, count: 4 });
    expect(chainPlaceOf(moves, 8)).toEqual({ index: 2, count: 4 });
    expect(chainPlaceOf(moves, 9)).toEqual({ index: 3, count: 4 });
    expect(chainPlaceOf(moves, 11)).toEqual({ index: 0, count: 3 });
    expect(chainPlaceOf(moves, 13)).toEqual({ index: 2, count: 3 });
    expect(chainPlaceOf(moves, 1)).toEqual({ index: 0, count: 4 });
    expect(chainPlaceOf(moves, 3)).toEqual({ index: 2, count: 4 });
  });

  test("counts the effects of the chain it is in, and not those of the other chains of the session", () => {
    // The tracer's lane has one effect.
    expect(chainPlaceOf(state, filter.id)).toEqual({ index: 0, count: 1 });
  });

  test("is none for a node that is no effect, a node that is not there, or before there is a session", () => {
    for (const id of [0, 5, 10, 14, 16, MASTER, 12345]) expect(chainPlaceOf(moves, id), String(id)).toBeNull();
    expect(chainPlaceOf(stateOf(null), 6)).toBeNull();
  });

  test("follows the effect when it moves, and is the same answer while only a value changes", () => {
    // (The first two effects of the lane's chain change places in the graph.)
    const nodes = [...movesFixture.snapshot.graph.nodes];
    const first = nodes.findIndex((n) => n.id === 6);
    nodes.splice(first, 2, ...nodes.slice(first, first + 2).reverse());
    const moved: Snapshot = { ...movesFixture.snapshot, graph: { ...movesFixture.snapshot.graph, nodes } };
    expect(chainPlaceOf(stateOf(moved), 6)).toEqual({ index: 1, count: 4 });
    expect(chainPlaceOf(stateOf(moved), 7)).toEqual({ index: 0, count: 4 });

    expect(sameChainPlace(chainPlaceOf(moves, 6), chainPlaceOf(stateOf(structuredClone(movesFixture.snapshot)), 6))).toBe(true);
    expect(sameChainPlace(chainPlaceOf(moves, 6), chainPlaceOf(stateOf(moved), 6))).toBe(false);
    expect(sameChainPlace({ index: 1, count: 4 }, { index: 1, count: 5 })).toBe(false);
    expect(sameChainPlace(null, null)).toBe(true);
    expect(sameChainPlace(null, { index: 0, count: 1 })).toBe(false);
    expect(sameChainPlace({ index: 0, count: 1 }, null)).toBe(false);
  });
});

describe("finding the keys of a lane", () => {
  const reach = stateOf(reachFixture.snapshot);

  test("is none for a lane that takes every note, and its range for one that does not", () => {
    expect(rangeOf(state, oscillator.id)).toBeNull();
    const ranged = structuredClone(tracerSnapshot);
    const node = ranged.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument") throw new Error("no lane");
    node.range = { low: 48, high: 71 };
    expect(rangeOf(stateOf(ranged), oscillator.id)).toEqual({ low: 48, high: 71 });
  });

  test("is there for a lane that has no instrument", () => {
    expect(rangeOf(reach, 11)).toBeNull();
  });

  test("is not there for what is not a lane, a node that is not there, or before there is a session", () => {
    expect(rangeOf(state, filter.id)).toBeUndefined();
    expect(rangeOf(state, lfo.id)).toBeUndefined();
    expect(rangeOf(state, MASTER)).toBeUndefined();
    expect(rangeOf(reach, 0)).toBeUndefined(); // a group
    expect(rangeOf(state, 424242)).toBeUndefined();
    expect(rangeOf(stateOf(null), oscillator.id)).toBeUndefined();
  });
});


describe("the pattern of a lane", () => {
  const pattern = { recording: false, enabled: true, looping: false, inKey: true, lengthBeats: 2, bpm: 90, notes: 1 };
  const withPattern = (): Snapshot => {
    const snapshot = structuredClone(tracerSnapshot);
    const node = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument") throw new Error("no lane");
    node.pattern = pattern;
    return snapshot;
  };

  test("is none for a lane that has none, and what it is for one that has", () => {
    expect(patternOf(state, oscillator.id)).toBeNull();
    expect(patternOf(stateOf(withPattern()), oscillator.id)).toEqual(pattern);
  });

  test("is there for a lane that has no instrument", () => {
    expect(patternOf(stateOf(reachFixture.snapshot), 11)).toBeNull();
  });

  test("is not there for what is not a lane, a node that is not there, or before there is a session", () => {
    expect(patternOf(state, filter.id)).toBeUndefined();
    expect(patternOf(state, lfo.id)).toBeUndefined();
    expect(patternOf(state, MASTER)).toBeUndefined();
    expect(patternOf(stateOf(reachFixture.snapshot), 0)).toBeUndefined(); // a group
    expect(patternOf(state, 424242)).toBeUndefined();
    expect(patternOf(stateOf(null), oscillator.id)).toBeUndefined();
  });

  test("is the same object while the pattern is: a view that selects it does not draw again for what is not its own", () => {
    const first = patternOf(stateOf(withPattern()), oscillator.id);
    expect(first).toEqual(pattern);
    const snapshot = withPattern();
    expect(patternOf(stateOf(snapshot), oscillator.id)).toBe(patternOf(stateOf(snapshot), oscillator.id));
  });
});

describe("which parameters a node shows as knobs", () => {
  /** The tracer with these parameters pinned to the plugin of the node. */
  const pinned = (node: Node, params: number[]): ClientState => {
    const nodes = tracerSnapshot.graph.nodes.map((n) => {
      if (n.id !== node.id || (n.kind !== "instrument" && n.kind !== "effect") || !n.plugin) return n;
      return { ...n, plugin: { ...n.plugin, pinned: params } };
    });
    return stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } });
  };

  test("a parameter is one when it is among the first few and nothing is pinned, or when it is pinned", () => {
    expect(isKnob(state, filter.id, 0)).toBe(true);
    expect(isKnob(state, oscillator.id, 7)).toBe(true);
    // (The first eight: the tracer's plugins have no more.)
    const some = pinned(filter, [2, 0]);
    expect(isKnob(some, filter.id, 2)).toBe(true);
    expect(isKnob(some, filter.id, 0)).toBe(true);
    expect(isKnob(some, filter.id, 1)).toBe(false);
    expect(isKnob(some, filter.id, 3)).toBe(false);
    expect(isKnob(some, oscillator.id, 1)).toBe(true);
  });

  test("a parameter the plugin has not, a node that has no plugin and a session that is not there have none", () => {
    expect(isKnob(state, filter.id, 99)).toBe(false);
    expect(isKnob(state, lfo.id, 0)).toBe(false);
    expect(isKnob(state, MASTER, 0)).toBe(false);
    expect(isKnob(state, 12345, 0)).toBe(false);
    expect(isKnob(stateOf(null), filter.id, 0)).toBe(false);
  });

  test("a node has pins when its plugin has a list that is not empty", () => {
    expect(hasPins(state, filter.id)).toBe(false);
    expect(hasPins(pinned(filter, [1]), filter.id)).toBe(true);
    expect(hasPins(pinned(filter, [1]), oscillator.id)).toBe(false);
    expect(hasPins(state, lfo.id)).toBe(false);
    expect(hasPins(state, 12345)).toBe(false);
    expect(hasPins(stateOf(null), filter.id)).toBe(false);
  });
});

// The session of the groups fixture: three groups (0, 1 and 2, the last empty and called Lead; the first has two modulators and no effect, the second one
// modulator), four lanes (3 and 10 in the first group, 9 in the second, 5 in none, which has a filter and two modulators of its own).
describe("which group a lane is in, and which there are to choose from", () => {
  const groups = stateOf(groupsFixture.snapshot);
  const everyGroup = [
    { id: 0, name: "Group 1" },
    { id: 1, name: "Group 2" },
    { id: 2, name: "Lead" },
  ];

  test("the group a lane is in, and every group of the session in the order of the graph", () => {
    expect(groupChoicesOf(groups, 3)).toEqual({ current: 0, groups: everyGroup });
    expect(groupChoicesOf(groups, 6)).toEqual({ current: 1, groups: everyGroup });
    expect(groupChoicesOf(groups, 7)).toEqual({ current: 0, groups: everyGroup });
  });

  test("none, for a lane that is in no group", () => {
    expect(groupChoicesOf(groups, 4)).toEqual({ current: null, groups: everyGroup });
    expect(groupChoicesOf(state, oscillator.id)).toEqual({ current: null, groups: [] });
  });

  test("nothing for what is no lane: a group, an effect, a modulator, the master, a node that is not there, and before there is a session", () => {
    for (const node of [0, 5, 8, 13, MASTER, 9999]) expect(groupChoicesOf(groups, node), `node ${String(node)}`).toBeUndefined();
    expect(groupChoicesOf(stateOf(null), 3)).toBeUndefined();
  });

  test("says what a group is called now: the name it was given, or the one it has by default", () => {
    const named: Snapshot = {
      ...groupsFixture.snapshot,
      graph: { ...groupsFixture.snapshot.graph, nodes: groupsFixture.snapshot.graph.nodes.map((n) => (n.id === 1 ? { ...n, name: "Strings" } : n)) },
    };
    expect(groupChoicesOf(stateOf(named), 4)?.groups.map((g) => g.name)).toEqual(["Group 1", "Strings", "Lead"]);
  });

  test("is the same choice when it is the same group and the same groups by the same names, and not otherwise", () => {
    const a = groupChoicesOf(groups, 3);
    const b = groupChoicesOf(groups, 3);
    expect(a).not.toBe(b);
    expect(sameGroupChoices(a, b)).toBe(true);
    expect(sameGroupChoices(a, groupChoicesOf(groups, 7))).toBe(true);
    expect(sameGroupChoices(a, groupChoicesOf(groups, 6))).toBe(false);
    expect(sameGroupChoices(a, groupChoicesOf(groups, 4))).toBe(false);
    expect(sameGroupChoices(a, { current: 0, groups: everyGroup.slice(0, 2) })).toBe(false);
    expect(sameGroupChoices(a, { current: 0, groups: everyGroup.map((g) => ({ ...g, name: g.id === 2 ? "Pad" : g.name })) })).toBe(false);
    // The same names at the same places, but other nodes: an option's value is its node, so the choice that was drawn is stale.
    expect(sameGroupChoices(a, { current: 0, groups: everyGroup.map((g) => ({ ...g, id: g.id + 100 })) })).toBe(false);
    expect(sameGroupChoices(undefined, undefined)).toBe(true);
    expect(sameGroupChoices(a, undefined)).toBe(false);
    expect(sameGroupChoices(undefined, a)).toBe(false);
  });
});

describe("what goes when a lane or a group is taken out", () => {
  const groups = stateOf(groupsFixture.snapshot);

  test("counts the modulators that hear a lane or a group, which stay when it goes: not the ones that hear another or every note", () => {
    const base = groupsFixture.snapshot;
    const own = (id: number, hears?: number): Node => ({ id, parent: null, name: `LFO ${String(id)}`, kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 }, ...(hears === undefined ? {} : { hears }) });
    const master = base.graph.nodes.findIndex((node) => node.kind === "master");
    // (The fixture's own: one hears lane 3, two hear lane 4, one lane 7, two group 0 and one group 1.)
    const nodes = [...base.graph.nodes.slice(0, master), own(100, 4), own(101, 0), own(102, 6), own(103), ...base.graph.nodes.slice(master)];
    const heard = stateOf({ ...base, graph: { ...base.graph, nodes } });
    expect(removalOf(heard, 4)).toMatchObject({ kind: "lane", hearing: 3 });
    expect(removalOf(heard, 7)).toMatchObject({ kind: "lane", hearing: 1 });
    expect(removalOf(heard, 6)).toMatchObject({ kind: "lane", hearing: 1 });
    expect(removalOf(heard, 0)).toMatchObject({ kind: "group", hearing: 3 });
    expect(removalOf(heard, 1)).toMatchObject({ kind: "group", hearing: 1 });
    expect(removalOf(heard, 2)).toMatchObject({ kind: "group", hearing: 0 });
  });

  test("is not the same removal when the number that hear it is not", () => {
    const base = removalOf(groups, 4);
    if (!base) throw new Error("a lane");
    expect(sameRemoval(base, { ...base })).toBe(true);
    expect(sameRemoval(base, { ...base, hearing: base.hearing + 1 })).toBe(false);
  });

  test("for a group: what it is called, the effects of its bus that go with it, the modulators that hear it and the lanes that stay", () => {
    expect(removalOf(groups, 0)).toEqual({ kind: "group", name: "Group 1", instrument: false, effects: 0, hearing: 2, lanes: 2, last: false });
    expect(removalOf(groups, 1)).toEqual({ kind: "group", name: "Group 2", instrument: false, effects: 0, hearing: 1, lanes: 1, last: false });
    expect(removalOf(groups, 2)).toEqual({ kind: "group", name: "Lead", instrument: false, effects: 0, hearing: 0, lanes: 0, last: false });
  });

  test("for a lane: what it is called, its effects that go with it, the modulators that hear it, whether it is in a group, and whether it is the last", () => {
    expect(removalOf(groups, 4)).toEqual({ kind: "lane", name: "Oscillator 2", instrument: true, effects: 1, hearing: 2, lanes: 0, last: false });
    expect(removalOf(groups, 7)).toEqual({ kind: "lane", name: "Oscillator 4", instrument: true, effects: 1, hearing: 1, lanes: 0, last: false });
    expect(removalOf(state, oscillator.id)).toEqual({ kind: "lane", name: "Oscillator", instrument: true, effects: 1, hearing: 1, lanes: 0, last: true });
  });

  test("says whether a lane has an instrument, which can be taken out alone: a lane that has none does not, and a group never does", () => {
    const emptied = stateOf({
      ...tracerSnapshot,
      graph: { ...tracerSnapshot.graph, nodes: tracerSnapshot.graph.nodes.map((node) => (node.id === oscillator.id && node.kind === "instrument" ? { ...node, name: "(empty)", plugin: null } : node)) },
    });
    // (The lane keeps what else it has: its filter goes with it, and its LFO still hears it. It has nothing else that a card would show, so the canvas draws no card for it and it is called by its
    // filter; with a key range it has a card, and is called as the server calls a lane that has no instrument.)
    expect(removalOf(emptied, oscillator.id)).toEqual({ kind: "lane", name: "Filter", instrument: false, effects: 1, hearing: 1, lanes: 0, last: true });
    const base = emptied.snapshot;
    if (base === null) throw new Error("the session is there");
    const keyed = stateOf({
      ...base,
      graph: { ...base.graph, nodes: base.graph.nodes.map((node) => (node.id === oscillator.id && node.kind === "instrument" ? { ...node, range: { low: 36, high: 60 } } : node)) },
    });
    expect(removalOf(keyed, oscillator.id)).toEqual({ kind: "lane", name: "(empty)", instrument: false, effects: 1, hearing: 1, lanes: 0, last: true });
    expect([0, 1, 2].map((group) => removalOf(groups, group)?.instrument)).toEqual([false, false, false]);
    expect([3, 4, 6, 7].map((lane) => removalOf(groups, lane)?.instrument)).toEqual([true, true, true, true]);
  });

  test("nothing for what is neither: an effect, a modulator, the master, a node that is not there, and before there is a session", () => {
    for (const node of [5, 8, 13, MASTER, 9999]) expect(removalOf(groups, node), `node ${String(node)}`).toBeUndefined();
    expect(removalOf(stateOf(null), 0)).toBeUndefined();
  });

  test("a lane is the last when it is the only one, whatever else the session has, and a group never is", () => {
    // (The tracer with a second lane: neither is the last. And with a group: the group is not a lane, and the lane is still the last.)
    const second: Node = { ...oscillator, id: 9000, parent: null };
    const twoLanes = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, second] } });
    expect([removalOf(twoLanes, oscillator.id)?.last, removalOf(twoLanes, second.id)?.last]).toEqual([false, false]);
    const found = groupsFixture.snapshot.graph.nodes.find((node) => node.id === 2);
    if (found?.kind !== "group") throw new Error("no group Lead");
    const lead: Node = { ...found, id: 9001 };
    const withGroup = stateOf({ ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, lead] } });
    expect([removalOf(withGroup, oscillator.id)?.last, removalOf(withGroup, lead.id)?.last]).toEqual([true, false]);
  });

  test("is the same removal when it says the same, and not when any one thing it says is different", () => {
    const a = removalOf(groups, 0);
    if (!a) throw new Error("no removal");
    expect(sameRemoval(a, removalOf(groups, 0))).toBe(true);
    expect(sameRemoval(a, removalOf(groups, 1))).toBe(false);
    const differing: Removal[] = [
      { ...a, kind: "lane" },
      { ...a, name: "Pad" },
      { ...a, instrument: !a.instrument },
      { ...a, effects: a.effects + 1 },
      { ...a, hearing: a.hearing + 1 },
      { ...a, lanes: a.lanes + 1 },
      { ...a, last: !a.last },
    ];
    for (const other of differing) expect(sameRemoval(a, other), JSON.stringify(other)).toBe(false);
    expect(sameRemoval(undefined, undefined)).toBe(true);
    expect(sameRemoval(a, undefined)).toBe(false);
    expect(sameRemoval(undefined, a)).toBe(false);
  });
});

describe("the mute and the solo of a lane or a group", () => {
  const groups = stateOf(groupsFixture.snapshot);
  /** The tracer with the oscillator's flags set, or `snapshot` with those of the node `id`. */
  const flagged = (flags: { muted?: boolean; solo?: boolean }, snapshot: Snapshot = tracerSnapshot, id: number = oscillator.id): ClientState => {
    const copy = structuredClone(snapshot);
    const node = copy.graph.nodes.find((n) => n.id === id);
    if (node?.kind !== "instrument" && node?.kind !== "group") throw new Error("no lane or group");
    Object.assign(node, flags);
    return stateOf(copy);
  };

  test("are both off for a lane or a group that was not touched, and for a lane that has no instrument", () => {
    expect(flagsOf(state, oscillator.id)).toEqual({ muted: false, solo: false });
    expect(flagsOf(groups, 0)).toEqual({ muted: false, solo: false }); // a group
    expect(flagsOf(stateOf(reachFixture.snapshot), 11)).toEqual({ muted: false, solo: false }); // a lane that has no instrument
  });

  test("are what the node says of them, each by itself: of a lane and of a group", () => {
    expect(flagsOf(flagged({ muted: true }), oscillator.id)).toEqual({ muted: true, solo: false });
    expect(flagsOf(flagged({ solo: true }), oscillator.id)).toEqual({ muted: false, solo: true });
    expect(flagsOf(flagged({ muted: true, solo: true }), oscillator.id)).toEqual({ muted: true, solo: true });
    expect(flagsOf(flagged({ solo: true }, groupsFixture.snapshot, 1), 1)).toEqual({ muted: false, solo: true });
    expect(flagsOf(flagged({ muted: true }, groupsFixture.snapshot, 1), 0)).toEqual({ muted: false, solo: false });
  });

  test("are none for what is neither a lane nor a group: an effect, a modulator, the master, a node that is not there, and before there is a session", () => {
    for (const node of [filter.id, lfo.id, MASTER, 424242]) expect(flagsOf(state, node), `node ${String(node)}`).toBeUndefined();
    expect(flagsOf(stateOf(null), oscillator.id)).toBeUndefined();
  });

  test("are the same flags when both are as they were, and not otherwise", () => {
    const a = flagsOf(state, oscillator.id);
    const b = flagsOf(state, oscillator.id);
    expect(a).not.toBe(b);
    expect(sameFlags(a, b)).toBe(true);
    expect(sameFlags(a, flagsOf(flagged({ muted: true }), oscillator.id))).toBe(false);
    expect(sameFlags(a, flagsOf(flagged({ solo: true }), oscillator.id))).toBe(false);
    expect(sameFlags(flagsOf(flagged({ muted: true }), oscillator.id), flagsOf(flagged({ solo: true }), oscillator.id))).toBe(false);
    expect(sameFlags(undefined, undefined)).toBe(true);
    expect(sameFlags(a, undefined)).toBe(false);
    expect(sameFlags(undefined, a)).toBe(false);
  });
});

describe("how many effects the chain of a lane has (what a module of it is made of)", () => {
  const reach = stateOf(reachFixture.snapshot);
  /** The tracer with `extra` effects more in the chain of the oscillator's lane, and one that is in the chain of the node `elsewhere`, which is not the lane's. */
  const chained = (extra: number, elsewhere?: number): ClientState => {
    const copy = structuredClone(tracerSnapshot);
    const effect = nodeNamed(tracerSnapshot, "Filter");
    if (effect.kind !== "effect") throw new Error("no effect");
    for (let i = 0; i < extra; i += 1) copy.graph.nodes.push({ ...effect, id: 1000 + i });
    if (elsewhere !== undefined) copy.graph.nodes.push({ ...effect, id: 2000, parent: elsewhere });
    return stateOf(copy);
  };

  test("is how many effects the lane has: one for the tracer's, none for a lane that has none and for a lane that has no instrument either", () => {
    expect(chainLengthOf(state, oscillator.id)).toBe(1);
    expect(chainLengthOf(reach, 4)).toBe(1);
    expect(chainLengthOf(reach, 9)).toBe(1);
    expect(chainLengthOf(reach, 6)).toBe(0);
    expect(chainLengthOf(reach, 11)).toBe(0); // no instrument, and no effect
  });

  test("grows with each effect that is put in the chain, and is not moved by an effect that is in the chain of another node", () => {
    expect(chainLengthOf(chained(1), oscillator.id)).toBe(2);
    expect(chainLengthOf(chained(2), oscillator.id)).toBe(3);
    expect(chainLengthOf(chained(2, 4242), oscillator.id)).toBe(3);
    expect(chainLengthOf(chained(0, 4242), oscillator.id)).toBe(1);
  });

  test("counts the lane's own effects and not those of the bus of the group that it is in: a lane in a group has its own chain, and a group has none to save", () => {
    expect(chainLengthOf(reach, 7)).toBe(1); // a lane in group 2, whose bus has an effect too
    expect(chainLengthOf(reach, 2)).toBe(0);
    expect(chainLengthOf(reach, 0)).toBe(0); // a group whose bus has an effect
  });

  test("is none for what is no lane: an effect, a modulator, the master, a node that is not there, and before there is a session", () => {
    for (const node of [filter.id, lfo.id, MASTER, 424242]) expect(chainLengthOf(state, node), `node ${String(node)}`).toBe(0);
    expect(chainLengthOf(stateOf(null), oscillator.id)).toBe(0);
  });
});

describe("whether a node can be heard at the master", () => {
  const muted = (): ClientState => {
    const copy = structuredClone(tracerSnapshot);
    const node = copy.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument") throw new Error("no lane");
    node.muted = true;
    return stateOf(copy);
  };

  test("is yes for everything of a session that has no mute and no solo: a lane and the effects of its chain", () => {
    expect(audibleAt(state, oscillator.id)).toBe(true);
    expect(audibleAt(state, filter.id)).toBe(true);
  });

  test("is no for a lane that is muted, and for the effects of its chain, which have nothing to say", () => {
    expect(audibleAt(muted(), oscillator.id)).toBe(false);
    expect(audibleAt(muted(), filter.id)).toBe(false);
  });

  test("is the engine's for a group: a muted group is not heard, nor are the lanes and the effects of the lanes that are in it", () => {
    const copy = structuredClone(groupsFixture.snapshot);
    const group = copy.graph.nodes.find((n) => n.id === 0);
    if (group?.kind !== "group") throw new Error("no group");
    group.muted = true;
    const cut = stateOf(copy);
    expect(audibleAt(cut, 0)).toBe(false);
    expect(audibleAt(cut, 3)).toBe(false); // a lane of the group
    expect(audibleAt(cut, 8)).toBe(false); // an effect of another lane of the group
    expect(audibleAt(cut, 7)).toBe(false);
    expect(audibleAt(cut, 4)).toBe(true); // a lane that is in no group
    expect(audibleAt(cut, 5)).toBe(true);
    expect(audibleAt(cut, 6)).toBe(true); // a lane of the other group
    expect(audibleAt(cut, 1)).toBe(true);
  });

  describe("with cables that carry audio", () => {
    // The audio cables fixture: the group Pad (0) with the lane 4 in it, that has the effects 5 and 6, the lane 7 (effect 8) that is in none, and the lane 9, which is empty. Cable 1 goes from the
    // instrument of lane 7 to the effect 5 of lane 4.
    const soloed = (lane: number): ClientState => {
      const copy = structuredClone(audioCablesFixture.snapshot);
      const node = copy.graph.nodes.find((n) => n.id === lane);
      if (node?.kind !== "instrument") throw new Error("no lane");
      node.solo = true;
      return stateOf(copy);
    };

    test("says nothing of an effect that is in no lane or group's chain", () => {
      const copy = structuredClone(audioCablesFixture.snapshot);
      const effect = copy.graph.nodes.find((n) => n.kind === "effect");
      if (effect?.kind !== "effect") throw new Error("no effect");
      copy.graph.nodes.push({ ...effect, id: 777, parent: null });
      expect(audibleAt(stateOf(copy), 777)).toBeUndefined();
    });

    test("is yes for every lane, group and effect when nothing is soloed", () => {
      const quiet = stateOf(audioCablesFixture.snapshot);
      for (const node of [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]) expect(audibleAt(quiet, node), `node ${String(node)}`).toBe(true);
    });

    test("is yes for the effects of a lane that a lane that is soloed sends to, and for its group, and no for the instrument of that lane, which is not heard for what it is", () => {
      const state = soloed(7);
      expect(audibleAt(state, 7)).toBe(true);
      expect(audibleAt(state, 8)).toBe(true);
      expect(audibleAt(state, 5)).toBe(true);
      expect(audibleAt(state, 6)).toBe(true);
      expect(audibleAt(state, 0)).toBe(true);
      expect(audibleAt(state, 4)).toBe(false);
      // (Nothing is sent to the empty lane.)
      expect(audibleAt(state, 9)).toBe(false);
    });

    test("is yes for the effect that the output of a group that is heard goes to and no for the instrument of the lane that has it: with the lane of the group soloed, cable 3 carries the group into lane 7", () => {
      const state = soloed(4);
      expect(audibleAt(state, 4)).toBe(true);
      expect(audibleAt(state, 8)).toBe(true);
      expect(audibleAt(state, 7)).toBe(false);
      expect(audibleAt(state, 9)).toBe(false);
    });
  });

  test("says nothing of what is no lane, no group and no effect: a modulator, the master, a node that is not there, and a session that is not there", () => {
    for (const node of [lfo.id, MASTER, 424242]) expect(audibleAt(state, node), `node ${String(node)}`).toBeUndefined();
    expect(audibleAt(stateOf(null), oscillator.id)).toBeUndefined();
  });
});
