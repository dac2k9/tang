import { describe, expect, test } from "vitest";

import type { Edge, Node, PatternInfo, Snapshot, StampedEvent } from "../protocol";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { applyEvent } from "./reducers";

const filter = nodeNamed(tracerSnapshot, "Filter");
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const lfo = nodeNamed(tracerSnapshot, "LFO");
/** The id of the cable from the LFO to the filter's cutoff, in the tracer. */
const CABLE = 0;
/** A pattern of two beats and one note, in key, that plays when a key is held and goes round while it is. */
const A_PATTERN: PatternInfo = { recording: false, enabled: true, looping: true, inKey: true, lengthBeats: 2, bpm: 90, notes: 1 };

function paramOf(snapshot: Snapshot, node: Node, index: number) {
  const found = snapshot.graph.nodes.find((n) => n.id === node.id);
  if (found?.kind !== "instrument" && found?.kind !== "effect") throw new Error("no plugin");
  const param = found.plugin?.params.find((p) => p.index === index);
  if (!param) throw new Error(`no parameter ${index}`);
  return param;
}

function applied(snapshot: Snapshot, event: StampedEvent): Snapshot {
  const result = applyEvent(snapshot, event);
  if (!result.ok) throw new Error(result.reason);
  return result.snapshot;
}

/** The id of an audio cable that `withSend` draws, from the oscillator to the filter: the id that a cable of modulation could have, in the series they share. */
const SEND = 31;

/** The tracer with an audio cable drawn from the oscillator to the filter (a send), at a level of 0.5. */
function withSend(): Snapshot {
  const cable: Edge = { kind: "cable", id: SEND, gain: 0.5, from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: filter.id, port: { type: "audioIn" } } };
  return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, edges: [...tracerSnapshot.graph.edges, cable] } };
}

/** The level of the audio cable `edge`. */
function gainOf(snapshot: Snapshot, edge: number): number {
  const cable = snapshot.graph.edges.find((e) => e.kind === "cable" && e.id === edge);
  if (cable?.kind !== "cable") throw new Error(`no audio cable ${edge}`);
  return cable.gain;
}

/** The tracer with a group node (and a bus effect in it) added, for the gains that are about groups. */
function withGroup(): { snapshot: Snapshot; group: Node } {
  const group: Node = { id: 40, parent: null, name: "Pad", kind: "group", volume: 1, pan: 0, muted: false, solo: false, route: "out" };
  return { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, group] } }, group };
}

/** The tracer with an envelope and a MIDI-learn control added, for the settings an LFO does not have. */
function withEnvelope(): { snapshot: Snapshot; envelope: Node; learn: Node } {
  const envelope: Node = {
    id: 41,
    parent: oscillator.id,
    name: "ADSR",
    kind: "modulator",
    source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
  };
  const learn: Node = { id: 42, parent: oscillator.id, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
  return { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, envelope, learn] } }, envelope, learn };
}

/** The tracer with a macro at 0.25 added, for the setting that only a macro has. */
function withMacro(): { snapshot: Snapshot; knob: Node } {
  const knob: Node = { id: 43, parent: oscillator.id, name: "Macro", kind: "modulator", source: { type: "macro", value: 0.25 } };
  return { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, knob] } }, knob };
}

/** The tracer with a step sequencer of four steps added: at 6 steps a second, a glide of a quarter, and a note does not start it again. */
function withSequencer(): { snapshot: Snapshot; sequencer: Node } {
  const values = [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)];
  const sequencer: Node = { id: 44, parent: oscillator.id, name: "Steps", kind: "modulator", source: { type: "steps", values, length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 0 } };
  return { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, sequencer] } }, sequencer };
}

function sourceOf(snapshot: Snapshot, node: Node) {
  const found = snapshot.graph.nodes.find((n) => n.id === node.id);
  if (found?.kind !== "modulator") throw new Error("not a modulator");
  return found.source;
}

function cableOf(snapshot: Snapshot): Extract<Edge, { kind: "modulation" }> {
  const found = snapshot.graph.edges.find((e): e is Extract<Edge, { kind: "modulation" }> => e.kind === "modulation");
  if (!found) throw new Error("no cable");
  return found;
}

function amountOf(snapshot: Snapshot, edge: number): number {
  const found = snapshot.graph.edges.find((e) => e.kind === "modulation" && e.id === edge);
  if (found?.kind !== "modulation") throw new Error(`no cable ${edge}`);
  return found.amount;
}

describe("a parameter set", () => {
  const event: StampedEvent = { rev: 1, type: "paramSet", node: filter.id, param: 0, value: 0.9 };

  test("changes that parameter of that node, and the revision", () => {
    const after = applied(tracerSnapshot, event);
    expect(after.rev).toBe(1);
    expect(paramOf(after, filter, 0).value).toBe(0.9);
  });

  test("leaves the default of the parameter as it was: only a preset moves that", () => {
    const before = paramOf(tracerSnapshot, filter, 0).default;
    expect(before).not.toBe(0.9);
    expect(paramOf(applied(tracerSnapshot, event), filter, 0).default).toBe(before);
  });

  test("changes nothing else, and shares what it did not change", () => {
    const after = applied(tracerSnapshot, event);
    const before = paramOf(tracerSnapshot, filter, 1);
    expect(paramOf(after, filter, 1)).toBe(before);
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.edges).toBe(tracerSnapshot.graph.edges);
    expect(after.telemetry).toBe(tracerSnapshot.telemetry);
  });

  test("leaves the snapshot it was applied to as it was", () => {
    const before = paramOf(tracerSnapshot, filter, 0).value;
    applied(tracerSnapshot, event);
    expect(paramOf(tracerSnapshot, filter, 0).value).toBe(before);
    expect(tracerSnapshot.rev).toBe(0);
  });

  test("is the next event, whatever the revision of the first one was", () => {
    const first = applied(tracerSnapshot, event);
    const second = applied(first, { rev: 2, type: "paramSet", node: filter.id, param: 0, value: 0.3 });
    expect(second.rev).toBe(2);
    expect(paramOf(second, filter, 0).value).toBe(0.3);
  });
});

describe("any event", () => {
  const events: [string, StampedEvent][] = [
    ["a parameter set", { rev: 1, type: "paramSet", node: filter.id, param: 0, value: 0.9 }],
    ["a gain set", { rev: 1, type: "gainSet", node: oscillator.id, gain: "volume", value: 0.5 }],
    ["a preset loaded", { rev: 1, type: "presetLoaded", node: filter.id, preset: "Dark", values: [{ param: 0, value: 0.2 }] }],
    ["a range set", { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 36, high: 84 } }],
    ["a mute set", { rev: 1, type: "muteSet", node: oscillator.id, muted: true }],
    ["a solo set", { rev: 1, type: "soloSet", node: oscillator.id, solo: true }],
    ["a pattern set", { rev: 1, type: "patternSet", node: oscillator.id, pattern: A_PATTERN }],
    ["a node's place set", { rev: 1, type: "nodeUiSet", node: oscillator.id, ui: { x: 80, y: 120 } }],
    ["pinned parameters set", { rev: 1, type: "pinnedSet", node: filter.id, params: [1] }],
    ["a modulator field set", { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "rate", value: 4 }],
    ["an edge depth set", { rev: 1, type: "edgeDepthSet", edge: CABLE, depth: -0.5 }],
    ["an audio cable's level set", { rev: 1, type: "cableGainSet", edge: SEND, gain: 0.25 }],
  ];

  test.each(events)("%s leaves the session with changes that are not saved, and nothing else of its meta changed", (_, event) => {
    const tracerSnapshot = withSend();
    expect(tracerSnapshot.meta.dirty).toBe(false);
    const after = applied(tracerSnapshot, event);
    expect(after.meta).toEqual({ ...tracerSnapshot.meta, dirty: true });
  });

  test.each(events)("%s shares the meta of a session that was not saved already", (_, event) => {
    const tracerSnapshot = withSend();
    const dirty = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, dirty: true } };
    expect(applied(dirty, event).meta).toBe(dirty.meta);
  });
});

describe("a saved event", () => {
  const unsaved = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, dirty: true } };

  test("leaves the session with nothing unsaved, names its file, and changes nothing of the graph", () => {
    const after = applied(unsaved, { rev: 1, type: "saved", file: "song.toml" });
    expect(after.meta).toEqual({ ...tracerSnapshot.meta, dirty: false, file: "song.toml" });
    expect(after.rev).toBe(1);
    expect(after.graph).toBe(unsaved.graph);
    expect(after.telemetry).toBe(unsaved.telemetry);
  });

  test("says when the session has no file", () => {
    expect(applied(unsaved, { rev: 1, type: "saved", file: null }).meta.file).toBeNull();
  });

  test("is an event like the others: one that skips a revision means events were missed, and the next change is unsaved again", () => {
    expect(applyEvent(unsaved, { rev: 2, type: "saved", file: "song.toml" }).ok).toBe(false);
    const saved = applied(unsaved, { rev: 1, type: "saved", file: "song.toml" });
    expect(applied(saved, { rev: 2, type: "gainSet", node: oscillator.id, gain: "volume", value: 0.5 }).meta.dirty).toBe(true);
  });

  test("is nothing to do for a snapshot that has it already", () => {
    expect(applyEvent(unsaved, { rev: 0, type: "saved", file: "song.toml" })).toEqual({ ok: true, snapshot: unsaved });
  });
});

describe("an editor opened event (step 112)", () => {
  const unsaved = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, dirty: true } };
  const told: StampedEvent = { rev: 1, type: "editorOpened", node: 3 };

  test("is a window on the server's machine and no change of the session: the revision goes on and nothing else does, as saved or as unsaved as it was", () => {
    const after = applied(unsaved, told);
    expect(after.rev).toBe(1);
    expect(after.meta).toBe(unsaved.meta);
    expect(after.graph).toBe(unsaved.graph);
    const clean = applied(tracerSnapshot, told);
    expect(clean.meta.dirty).toBe(false);
    expect(clean.graph).toBe(tracerSnapshot.graph);
  });

  test("is an event like the others: one that skips a revision means events were missed", () => {
    expect(applyEvent(tracerSnapshot, { ...told, rev: 5 })).toMatchObject({ ok: false });
  });
});

describe("a module saved event", () => {
  const unsaved = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, dirty: true } };
  const told: StampedEvent = { rev: 1, type: "moduleSaved", name: "Space" };

  test("is a file of the library and no change of the session: the revision goes on and nothing else does, as saved or as unsaved as it was", () => {
    const after = applied(unsaved, told);
    expect(after.rev).toBe(1);
    expect(after.meta).toBe(unsaved.meta);
    expect(after.graph).toBe(unsaved.graph);
    expect(after.telemetry).toBe(unsaved.telemetry);
    const clean = applied(tracerSnapshot, told);
    expect(clean.meta).toBe(tracerSnapshot.meta);
    expect(clean.meta.dirty).toBe(false);
  });

  test("is an event like the others: one that skips a revision means events were missed, and one the snapshot has already is nothing to do", () => {
    expect(applyEvent(unsaved, { ...told, rev: 2 }).ok).toBe(false);
    expect(applyEvent(unsaved, { ...told, rev: 0 })).toEqual({ ok: true, snapshot: unsaved });
  });
});

describe("a scale set", () => {
  const dorian: StampedEvent = { rev: 1, type: "scaleSet", scale: "F# dorian", root: 6, kind: "dorian", notes: [1, 3, 4, 6, 8, 9, 11] };
  const locked = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, piano: { ...tracerSnapshot.meta.piano, locked: true } } };

  test("sets the scale of the piano as the controller tells it, and keeps the lock", () => {
    expect(tracerSnapshot.meta.piano).toEqual({ scale: "C major", root: 0, kind: "major", notes: [0, 2, 4, 5, 7, 9, 11], locked: false });
    expect(applied(tracerSnapshot, dorian).meta.piano).toEqual({ scale: "F# dorian", root: 6, kind: "dorian", notes: [1, 3, 4, 6, 8, 9, 11], locked: false });
    expect(applied(locked, dorian).meta.piano.locked).toBe(true);
  });

  test("leaves the session with changes that are not saved, and the rest of its meta as it was", () => {
    const after = applied(tracerSnapshot, dorian);
    expect(after.meta.dirty).toBe(true);
    expect({ ...after.meta, piano: null }).toEqual({ ...tracerSnapshot.meta, dirty: true, piano: null });
    expect(after.rev).toBe(1);
  });

  test("changes nothing of the graph or the layout: they are the objects they were", () => {
    const after = applied(tracerSnapshot, dorian);
    expect(after.graph).toBe(tracerSnapshot.graph);
    expect(after.telemetry).toBe(tracerSnapshot.telemetry);
  });

  test("leaves the snapshot it was applied to as it was", () => {
    const before = structuredClone(tracerSnapshot);
    applied(tracerSnapshot, dorian);
    expect(tracerSnapshot).toEqual(before);
  });

  test("is an event like the others: one that skips a revision means events were missed", () => {
    expect(applyEvent(tracerSnapshot, { ...dorian, rev: 2 }).ok).toBe(false);
  });
});

describe("a lock set", () => {
  const locked = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, piano: { ...tracerSnapshot.meta.piano, locked: true } } };

  test("locks the piano and lets it go again, and keeps the scale", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "lockedSet", locked: true });
    expect(after.meta.piano).toEqual({ ...tracerSnapshot.meta.piano, locked: true });
    expect(applied(after, { rev: 2, type: "lockedSet", locked: false }).meta.piano).toEqual(tracerSnapshot.meta.piano);
  });

  test("leaves the session with changes that are not saved, and changes nothing of the graph", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "lockedSet", locked: true });
    expect(after.meta.dirty).toBe(true);
    expect(after.graph).toBe(tracerSnapshot.graph);
    expect(applied(locked, { rev: 1, type: "lockedSet", locked: false }).graph).toBe(locked.graph);
  });
});

describe("a tempo set", () => {
  const faster = { rev: 1, type: "bpmSet", bpm: 96.5 } as const;

  test("sets the tempo of the session, and leaves it with changes that are not saved", () => {
    expect(tracerSnapshot.meta.bpm).toBe(120);
    const after = applied(tracerSnapshot, faster);
    expect(after.meta.bpm).toBe(96.5);
    expect(after.meta.dirty).toBe(true);
    expect(after.rev).toBe(1);
  });

  test("changes nothing else of the session: the graph and the layout are the objects they were", () => {
    const after = applied(tracerSnapshot, faster);
    expect({ ...after.meta, bpm: 0, dirty: false }).toEqual({ ...tracerSnapshot.meta, bpm: 0, dirty: false });
    expect(after.graph).toBe(tracerSnapshot.graph);
    expect(after.telemetry).toBe(tracerSnapshot.telemetry);
  });

  test("is an event like the others: one that skips a revision means events were missed", () => {
    expect(applyEvent(tracerSnapshot, { ...faster, rev: 2 }).ok).toBe(false);
  });
});

describe("a gain set", () => {
  test("sets the volume of an instrument", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "gainSet", node: oscillator.id, gain: "volume", value: 0.5 });
    const node = after.graph.nodes.find((n) => n.id === oscillator.id);
    expect(node?.kind === "instrument" && node.volume).toBe(0.5);
  });

  test("sets the mix of an effect", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "gainSet", node: filter.id, gain: "mix", value: 0.25 });
    const node = after.graph.nodes.find((n) => n.id === filter.id);
    expect(node?.kind === "effect" && node.mix).toBe(0.25);
  });

  test("sets the volume and the pan of a group", () => {
    const { snapshot, group } = withGroup();
    const louder = applied(snapshot, { rev: 1, type: "gainSet", node: group.id, gain: "groupVolume", value: 2 });
    const panned = applied(louder, { rev: 2, type: "gainSet", node: group.id, gain: "groupPan", value: -0.5 });
    const node = panned.graph.nodes.find((n) => n.id === group.id);
    expect(node?.kind === "group" && [node.volume, node.pan]).toEqual([2, -0.5]);
  });

  test("sets the volume of the output mix and the ceiling of its limiter", () => {
    const louder = applied(tracerSnapshot, { rev: 1, type: "gainSet", node: MASTER, gain: "masterVolume", value: 1.5 });
    const lower = applied(louder, { rev: 2, type: "gainSet", node: MASTER, gain: "limiterCeiling", value: -6 });
    const master = lower.graph.nodes.find((n) => n.id === MASTER);
    expect(master?.kind === "master" && [master.volume, master.ceiling, master.limiter]).toEqual([1.5, -6, true]);
  });

  test("is refused by a node that does not have that gain", () => {
    const refused = [
      { node: filter.id, gain: "volume" as const },
      { node: oscillator.id, gain: "mix" as const },
      { node: oscillator.id, gain: "groupVolume" as const },
      { node: MASTER, gain: "groupPan" as const },
      { node: MASTER, gain: "volume" as const },
      { node: oscillator.id, gain: "masterVolume" as const },
      { node: filter.id, gain: "limiterCeiling" as const },
    ];
    for (const { node, gain } of refused) {
      const result = applyEvent(tracerSnapshot, { rev: 1, type: "gainSet", node, gain, value: 1 });
      expect(result.ok, `${gain} on ${node}`).toBe(false);
    }
  });
});

describe("a limiter set", () => {
  const master = (snapshot: Snapshot) => {
    const node = snapshot.graph.nodes.find((n) => n.kind === "master");
    if (node?.kind !== "master") throw new Error("no master");
    return node;
  };

  test("puts the limiter on the output mix in the sound or takes it out, and changes nothing else of it", () => {
    expect(master(tracerSnapshot).limiter).toBe(true);
    const out = applied(tracerSnapshot, { rev: 1, type: "limiterSet", on: false });
    expect(master(out)).toEqual({ ...master(tracerSnapshot), limiter: false });
    expect(master(applied(out, { rev: 2, type: "limiterSet", on: true }))).toEqual(master(tracerSnapshot));
    expect(out.graph.nodes.filter((n) => n.kind !== "master")).toEqual(tracerSnapshot.graph.nodes.filter((n) => n.kind !== "master"));
    expect(out.meta.dirty).toBe(true);
  });

  test("is refused by a session that has no output mix", () => {
    const without: Snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: tracerSnapshot.graph.nodes.filter((n) => n.kind !== "master") } };
    expect(applyEvent(without, { rev: 1, type: "limiterSet", on: false }).ok).toBe(false);
  });
});

describe("a range set", () => {
  const keys = (snapshot: Snapshot) => {
    const node = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument") throw new Error("not a lane");
    return node.range;
  };

  test("sets the keys of a lane, and none is every note", () => {
    expect(keys(tracerSnapshot)).toBeNull();
    const after = applied(tracerSnapshot, { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 36, high: 84 } });
    expect(keys(after)).toEqual({ low: 36, high: 84 });
    expect(keys(applied(after, { rev: 2, type: "rangeSet", node: oscillator.id, range: null }))).toBeNull();
  });

  test("changes that lane and nothing else: every other node is the object it was", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 36, high: 84 } });
    for (const node of tracerSnapshot.graph.nodes) {
      const now = after.graph.nodes.find((n) => n.id === node.id);
      if (node.id === oscillator.id) expect(now).not.toBe(node);
      else expect(now).toBe(node);
    }
  });

  test("keeps the lane's own plugin, volume and the rest", () => {
    const before = tracerSnapshot.graph.nodes.find((n) => n.id === oscillator.id);
    const after = applied(tracerSnapshot, { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 60, high: 60 } }).graph.nodes.find((n) => n.id === oscillator.id);
    expect(after).toEqual({ ...before, range: { low: 60, high: 60 } });
  });

  test("is refused by a node that is not a lane, and by one that is not there", () => {
    for (const node of [filter.id, lfo.id, MASTER]) {
      expect(applyEvent(tracerSnapshot, { rev: 1, type: "rangeSet", node, range: null }).ok, `node ${node}`).toBe(false);
    }
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "rangeSet", node: 4242, range: null });
    expect(result).toEqual({ ok: false, reason: "there is no node 4242" });
  });
});

describe("a mute and a solo set", () => {
  const flagsOf = (snapshot: Snapshot, id: number) => {
    const node = snapshot.graph.nodes.find((n) => n.id === id);
    if (node?.kind !== "instrument" && node?.kind !== "group") throw new Error("neither a lane nor a group");
    return { muted: node.muted, solo: node.solo };
  };

  test("mutes and unmutes a lane, and says nothing of its solo", () => {
    expect(flagsOf(tracerSnapshot, oscillator.id)).toEqual({ muted: false, solo: false });
    const muted = applied(tracerSnapshot, { rev: 1, type: "muteSet", node: oscillator.id, muted: true });
    expect(flagsOf(muted, oscillator.id)).toEqual({ muted: true, solo: false });
    const soloed = applied(muted, { rev: 2, type: "soloSet", node: oscillator.id, solo: true });
    expect(flagsOf(soloed, oscillator.id)).toEqual({ muted: true, solo: true });
    const brought = applied(soloed, { rev: 3, type: "muteSet", node: oscillator.id, muted: false });
    expect(flagsOf(brought, oscillator.id)).toEqual({ muted: false, solo: true });
    expect(flagsOf(applied(brought, { rev: 4, type: "soloSet", node: oscillator.id, solo: false }), oscillator.id)).toEqual({ muted: false, solo: false });
  });

  test("does the same for a group", () => {
    const { snapshot, group } = withGroup();
    const muted = applied(snapshot, { rev: 1, type: "muteSet", node: group.id, muted: true });
    expect(flagsOf(muted, group.id)).toEqual({ muted: true, solo: false });
    expect(flagsOf(applied(muted, { rev: 2, type: "soloSet", node: group.id, solo: true }), group.id)).toEqual({ muted: true, solo: true });
  });

  test("changes that node and nothing else: every other node is the object it was, and the node is what it was but for the flag", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "muteSet", node: oscillator.id, muted: true });
    for (const node of tracerSnapshot.graph.nodes) {
      const now = after.graph.nodes.find((n) => n.id === node.id);
      if (node.id === oscillator.id) expect(now).toEqual({ ...node, muted: true });
      else expect(now).toBe(node);
    }
    expect(after.graph.edges).toBe(tracerSnapshot.graph.edges);
  });

  test("is refused by a node that is neither a lane nor a group, and by one that is not there", () => {
    for (const node of [filter.id, lfo.id, MASTER]) {
      expect(applyEvent(tracerSnapshot, { rev: 1, type: "muteSet", node, muted: true }).ok, `mute of node ${node}`).toBe(false);
      expect(applyEvent(tracerSnapshot, { rev: 1, type: "soloSet", node, solo: true }).ok, `solo of node ${node}`).toBe(false);
    }
    expect(applyEvent(tracerSnapshot, { rev: 1, type: "muteSet", node: 4242, muted: true })).toEqual({ ok: false, reason: "there is no node 4242" });
    expect(applyEvent(tracerSnapshot, { rev: 1, type: "soloSet", node: filter.id, solo: true })).toEqual({ ok: false, reason: `node ${filter.id} is neither a lane nor a group` });
  });
});

describe("a pattern set", () => {
  const patternOf = (snapshot: Snapshot) => {
    const found = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
    if (found?.kind !== "instrument") throw new Error("no lane");
    return found.pattern;
  };
  const set = (node: number, pattern: PatternInfo | null, rev = 1): StampedEvent => ({ rev, type: "patternSet", node, pattern });

  test("gives the lane the pattern the event tells, and none takes it away", () => {
    expect(patternOf(tracerSnapshot)).toBeNull();
    const after = applied(tracerSnapshot, set(oscillator.id, A_PATTERN));
    expect(patternOf(after)).toEqual(A_PATTERN);
    const replaced = applied(after, set(oscillator.id, { ...A_PATTERN, recording: true, lengthBeats: 8 }, 2));
    expect(patternOf(replaced)).toEqual({ ...A_PATTERN, recording: true, lengthBeats: 8 });
    expect(patternOf(applied(replaced, set(oscillator.id, null, 3)))).toBeNull();
  });

  test("changes that lane and nothing else: every other node is the object it was, and the lane keeps its plugin, its keys and its volume", () => {
    const after = applied(tracerSnapshot, set(oscillator.id, A_PATTERN));
    for (const node of tracerSnapshot.graph.nodes) {
      const now = after.graph.nodes.find((n) => n.id === node.id);
      if (node.id === oscillator.id) expect(now).toEqual({ ...node, pattern: A_PATTERN });
      else expect(now).toBe(node);
    }
    expect(after.graph.edges).toBe(tracerSnapshot.graph.edges);
  });

  test("is refused by a node that is not a lane, and by one that is not there", () => {
    for (const node of [filter.id, lfo.id, MASTER]) {
      expect(applyEvent(tracerSnapshot, set(node, A_PATTERN)).ok, `node ${node}`).toBe(false);
    }
    expect(applyEvent(tracerSnapshot, set(filter.id, A_PATTERN))).toEqual({ ok: false, reason: `node ${filter.id} is not a lane` });
    expect(applyEvent(tracerSnapshot, set(4242, null))).toEqual({ ok: false, reason: "there is no node 4242" });
  });
});

describe("pinned parameters set", () => {
  const pinnedOf = (snapshot: Snapshot, node: Node) => {
    const found = snapshot.graph.nodes.find((n) => n.id === node.id);
    if (found?.kind !== "instrument" && found?.kind !== "effect") throw new Error("no plugin");
    return found.plugin?.pinned;
  };
  const pin = (node: number, params: number[], rev = 1): StampedEvent => ({ rev, type: "pinnedSet", node, params });

  test("pins the parameters of the node's plugin in the order the event gives, and a list with none gives the node back to the front-end", () => {
    expect(pinnedOf(tracerSnapshot, filter)).toEqual([]);
    const after = applied(tracerSnapshot, pin(filter.id, [1, 0]));
    expect(pinnedOf(after, filter)).toEqual([1, 0]);
    const replaced = applied(after, pin(filter.id, [3], 2));
    expect(pinnedOf(replaced, filter)).toEqual([3]);
    expect(pinnedOf(applied(replaced, pin(filter.id, [], 3)), filter)).toEqual([]);
  });

  test("is for an instrument as it is for an effect", () => {
    const after = applied(tracerSnapshot, pin(oscillator.id, [2, 1]));
    expect(pinnedOf(after, oscillator)).toEqual([2, 1]);
    expect(pinnedOf(after, filter)).toEqual([]);
  });

  test("changes that node and nothing else: every other node is the object it was, and so is the plugin's own list of parameters", () => {
    const after = applied(tracerSnapshot, pin(filter.id, [0]));
    for (const node of tracerSnapshot.graph.nodes) {
      if (node.id === filter.id) continue;
      expect(after.graph.nodes.find((n) => n.id === node.id), `node ${node.id}`).toBe(node);
    }
    const before = tracerSnapshot.graph.nodes.find((n) => n.id === filter.id);
    const now = after.graph.nodes.find((n) => n.id === filter.id);
    if (before?.kind !== "effect" || now?.kind !== "effect") throw new Error("no filter");
    expect(now.plugin.params).toBe(before.plugin.params);
    expect(now.plugin.presets).toBe(before.plugin.presets);
  });

  test("does not fit a node that is not there, a node that has no plugin or a parameter the plugin has not, and changes nothing", () => {
    const cases: [string, StampedEvent, string][] = [
      ["a node that is not there", pin(9999, []), "there is no node 9999"],
      ["a modulator", pin(lfo.id, []), `node ${lfo.id} has no plugin`],
      ["the master", pin(MASTER, []), `node ${MASTER} has no plugin`],
      ["a parameter the plugin has not", pin(filter.id, [0, 4000]), `node ${filter.id} has no parameter 4000`],
    ];
    for (const [what, event, why] of cases) {
      const result = applyEvent(tracerSnapshot, event);
      expect(result.ok, what).toBe(false);
      expect(!result.ok && result.reason, what).toContain(why);
    }
  });
});

describe("a group renamed", () => {
  const nameOf = (snapshot: Snapshot, node: Node) => snapshot.graph.nodes.find((n) => n.id === node.id)?.name;
  const rename = (node: number, name: string, rev = 1): StampedEvent => ({ rev, type: "groupRenamed", node, name });

  test("gives the group the name the event says, and a second event another", () => {
    const { snapshot, group } = withGroup();
    expect(nameOf(snapshot, group)).toBe("Pad");
    const after = applied(snapshot, rename(group.id, "Strings"));
    expect(nameOf(after, group)).toBe("Strings");
    expect(nameOf(applied(after, rename(group.id, "Group 1", 2)), group)).toBe("Group 1");
  });

  test("changes that group and nothing else: every other node is the object it was, the group is still a group of the same volume and pan", () => {
    const { snapshot: plain, group: first } = withGroup();
    // (A group that is not at its defaults, so that a volume and a pan that were lost would show.)
    const group: Node = { ...first, kind: "group", volume: 0.5, pan: -0.25, muted: false, solo: false, route: "out" };
    const snapshot = { ...plain, graph: { ...plain.graph, nodes: plain.graph.nodes.map((n) => (n.id === group.id ? group : n)) } };
    const after = applied(snapshot, rename(group.id, "Strings"));
    for (const node of snapshot.graph.nodes) {
      if (node.id === group.id) continue;
      expect(after.graph.nodes.find((n) => n.id === node.id), `node ${node.id}`).toBe(node);
    }
    expect(after.graph.nodes.find((n) => n.id === group.id)).toEqual({ ...group, name: "Strings" });
    expect(after.graph.edges).toBe(snapshot.graph.edges);
  });

  test("leaves the session with changes that are not saved, and nothing else of its meta changed", () => {
    const { snapshot, group } = withGroup();
    expect(applied(snapshot, rename(group.id, "Strings")).meta).toEqual({ ...snapshot.meta, dirty: true });
  });

  test("does not fit a node that is not there or a node that is not a group, and changes nothing", () => {
    const { snapshot } = withGroup();
    const cases: [string, StampedEvent, string][] = [
      ["a node that is not there", rename(9999, "x"), "there is no node 9999"],
      ["an instrument", rename(oscillator.id, "x"), `node ${oscillator.id} is not a group`],
      ["an effect", rename(filter.id, "x"), `node ${filter.id} is not a group`],
      ["the master", rename(MASTER, "x"), `node ${MASTER} is not a group`],
    ];
    for (const [what, event, why] of cases) {
      const result = applyEvent(snapshot, event);
      expect(result.ok, what).toBe(false);
      expect(!result.ok && result.reason, what).toContain(why);
    }
  });
});

describe("a node's place set", () => {
  const placeOf = (snapshot: Snapshot, id: number) => snapshot.graph.nodes.find((n) => n.id === id)?.ui;
  const place = (node: number, ui: { x: number; y: number } | null, rev = 1): StampedEvent => ({ rev, type: "nodeUiSet", node, ui });

  test("puts the node at the place, and no place gives it back to the layout", () => {
    expect(placeOf(tracerSnapshot, filter.id)).toBeUndefined();
    const after = applied(tracerSnapshot, place(filter.id, { x: 376, y: -40.5 }));
    expect(placeOf(after, filter.id)).toEqual({ x: 376, y: -40.5 });
    const back = applied(after, place(filter.id, null, 2));
    expect(placeOf(back, filter.id)).toBeUndefined();
    // (A node the server never placed has no `ui` at all, and neither has one that was given back.)
    expect("ui" in (back.graph.nodes.find((n) => n.id === filter.id) ?? { ui: 1 })).toBe(false);
  });

  test("is for every node, the master too", () => {
    for (const node of tracerSnapshot.graph.nodes) {
      const after = applied(tracerSnapshot, place(node.id, { x: 1, y: 2 }));
      expect(placeOf(after, node.id), `node ${node.id}`).toEqual({ x: 1, y: 2 });
    }
    expect(tracerSnapshot.graph.nodes.some((n) => n.id === MASTER)).toBe(true);
  });

  test("changes that node and nothing else: every other node is the object it was", () => {
    const after = applied(tracerSnapshot, place(filter.id, { x: 5, y: 6 }));
    for (const node of tracerSnapshot.graph.nodes) {
      const now = after.graph.nodes.find((n) => n.id === node.id);
      if (node.id === filter.id) expect(now).not.toBe(node);
      else expect(now).toBe(node);
    }
    expect(after.graph.edges).toBe(tracerSnapshot.graph.edges);
  });

  test("keeps the rest of the node as it was", () => {
    const before = tracerSnapshot.graph.nodes.find((n) => n.id === oscillator.id);
    const after = applied(tracerSnapshot, place(oscillator.id, { x: 9, y: 9 })).graph.nodes.find((n) => n.id === oscillator.id);
    expect(after).toEqual({ ...before, ui: { x: 9, y: 9 } });
  });

  test("moves a node that has a place, and the place is the one the event says whatever it was", () => {
    const first = applied(tracerSnapshot, place(filter.id, { x: 1, y: 1 }));
    const second = applied(first, place(filter.id, { x: 2, y: 3 }, 2));
    expect(placeOf(second, filter.id)).toEqual({ x: 2, y: 3 });
  });

  test("is refused for a node that is not there", () => {
    expect(applyEvent(tracerSnapshot, place(4242, { x: 1, y: 1 }))).toEqual({ ok: false, reason: "there is no node 4242" });
  });
});

describe("a preset loaded", () => {
  test("names the preset and sets the parameters it set", () => {
    const event: StampedEvent = {
      rev: 1,
      type: "presetLoaded",
      node: filter.id,
      preset: "Dark",
      values: [
        { param: 0, value: 0.2 },
        { param: 1, value: 4 },
      ],
    };
    const after = applied(tracerSnapshot, event);
    const node = after.graph.nodes.find((n) => n.id === filter.id);
    expect(node?.kind === "effect" && node.plugin.currentPreset).toBe("Dark");
    expect([paramOf(after, filter, 0).value, paramOf(after, filter, 1).value]).toEqual([0.2, 4]);
    expect(paramOf(after, filter, 2)).toBe(paramOf(tracerSnapshot, filter, 2));
  });

  test("makes the values it set the parameters' defaults, which is what the controller keeps them as", () => {
    const event: StampedEvent = {
      rev: 1,
      type: "presetLoaded",
      node: filter.id,
      preset: "Dark",
      values: [
        { param: 0, value: 0.2 },
        { param: 1, value: 4 },
      ],
    };
    const after = applied(tracerSnapshot, event);
    expect([paramOf(after, filter, 0).default, paramOf(after, filter, 1).default]).toEqual([0.2, 4]);
    expect(paramOf(after, filter, 2).default).toBe(paramOf(tracerSnapshot, filter, 2).default);
  });

  test("is refused if it sets a parameter the plugin does not have", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "presetLoaded", node: filter.id, preset: "x", values: [{ param: 99, value: 1 }] });
    expect(result.ok).toBe(false);
  });
});

describe("a modulator field set", () => {
  test("sets the rate of an LFO", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "rate", value: 4 });
    expect(sourceOf(after, lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 4, retrigger: false, fade: 0, sync: 0 });
  });

  test("sets the waveform of an LFO by its index: sine, triangle, saw, square, random, smooth", () => {
    const waveforms = ["sine", "triangle", "saw", "square", "random", "smooth"];
    waveforms.forEach((name, index) => {
      const after = applied(tracerSnapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "waveform", value: index });
      expect(sourceOf(after, lfo), `index ${index}`).toEqual({ type: "lfo", waveform: name, rate: 0.5, retrigger: false, fade: 0, sync: 0 });
    });
  });

  test("sets each stage of an envelope and leaves the others", () => {
    const { snapshot, envelope } = withEnvelope();
    const stages = [
      ["attack", { attack: 0.5, decay: 0.3, sustain: 0.7, release: 0.5 }],
      ["decay", { attack: 0.01, decay: 1.5, sustain: 0.7, release: 0.5 }],
      ["sustain", { attack: 0.01, decay: 0.3, sustain: 0.25, release: 0.5 }],
      ["release", { attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 }],
    ] as const;
    const values = { attack: 0.5, decay: 1.5, sustain: 0.25, release: 2 };
    for (const [field, expected] of stages) {
      const after = applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: envelope.id, field, value: values[field] });
      expect(sourceOf(after, envelope), field).toEqual({ type: "envelope", ...expected });
    }
  });

  test("sets the value of a macro, and changes nothing else of it or of the graph", () => {
    const { snapshot, knob } = withMacro();
    const after = applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: knob.id, field: "value", value: 0.8 });
    expect(sourceOf(after, knob)).toEqual({ type: "macro", value: 0.8 });
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.nodes.find((n) => n.id === lfo.id)).toBe(lfo);
    expect(after.graph.edges).toBe(snapshot.graph.edges);
    expect(sourceOf(snapshot, knob), "the snapshot it was applied to").toEqual({ type: "macro", value: 0.25 });
    expect(after.graph.nodes.find((n) => n.id === knob.id)).toMatchObject({ name: "Macro", parent: oscillator.id });
  });

  test("sets the rate, the glide, the length and the switch of a step sequencer, one at a time, and changes nothing else of it or of the graph", () => {
    const { snapshot, sequencer } = withSequencer();
    const values = [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)];
    const set = (field: "rate" | "glide" | "length" | "retrigger", value: number) => applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: sequencer.id, field, value });
    expect(sourceOf(set("rate", 12.5), sequencer)).toEqual({ type: "steps", values, length: 4, rate: 12.5, glide: 0.25, retrigger: false, sync: 0 });
    expect(sourceOf(set("glide", 0.75), sequencer)).toEqual({ type: "steps", values, length: 4, rate: 6, glide: 0.75, retrigger: false, sync: 0 });
    expect(sourceOf(set("length", 9), sequencer)).toEqual({ type: "steps", values, length: 9, rate: 6, glide: 0.25, retrigger: false, sync: 0 });
    expect(sourceOf(set("retrigger", 1), sequencer)).toEqual({ type: "steps", values, length: 4, rate: 6, glide: 0.25, retrigger: true, sync: 0 });
    expect(sourceOf(set("retrigger", 0), sequencer)).toMatchObject({ retrigger: false });
    const after = set("glide", 0.5);
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.edges).toBe(snapshot.graph.edges);
    expect(sourceOf(snapshot, sequencer), "the snapshot it was applied to").toMatchObject({ glide: 0.25 });
  });

  test("sets the retrigger and the fade of an LFO, one at a time, and changes nothing else of it or of the graph", () => {
    const set = (field: "retrigger" | "fade", value: number) => applied(tracerSnapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field, value });
    expect(sourceOf(set("retrigger", 1), lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 0.5, retrigger: true, fade: 0, sync: 0 });
    expect(sourceOf(set("fade", 0.75), lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 0.5, retrigger: false, fade: 0.75, sync: 0 });
    expect(sourceOf(set("retrigger", 0), lfo)).toMatchObject({ retrigger: false });
    const after = set("fade", 2);
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.edges).toBe(tracerSnapshot.graph.edges);
    expect(sourceOf(tracerSnapshot, lfo), "the snapshot it was applied to").toMatchObject({ fade: 0 });
  });

  test("sets the note length that an LFO and a step sequencer follow the tempo by, as a place in the list, and keeps the rate they have", () => {
    const { snapshot, sequencer } = withSequencer();
    const setLfo = (value: number) => applied(tracerSnapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "sync", value });
    expect(sourceOf(setLfo(11), lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 0.5, retrigger: false, fade: 0, sync: 11 });
    expect(sourceOf(setLfo(0), lfo)).toMatchObject({ rate: 0.5, sync: 0 });
    const after = applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: sequencer.id, field: "sync", value: 14 });
    expect(sourceOf(after, sequencer)).toMatchObject({ type: "steps", rate: 6, sync: 14 });
    expect(after.graph.edges).toBe(snapshot.graph.edges);
    expect(sourceOf(tracerSnapshot, lfo), "the snapshot it was applied to").toMatchObject({ sync: 0 });
  });

  test("a note length is a place in the list, from 0 for none to the last, and nothing else is one", () => {
    const { snapshot, sequencer } = withSequencer();
    for (const node of [lfo, sequencer]) {
      for (const value of [-1, 17, 2.5, Number.NaN]) {
        expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: node.id, field: "sync", value }), `${node.name} ${String(value)}`).toEqual({ ok: false, reason: `${String(value)} is not a note length` });
      }
      for (const value of [0, 1, 16]) expect(sourceOf(applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: node.id, field: "sync", value }), node), `${node.name} ${String(value)}`).toMatchObject({ sync: value });
    }
  });

  test("is refused by an envelope, a control and a macro, which have no rate to follow the tempo", () => {
    const { snapshot: withKnob, knob } = withMacro();
    const { snapshot: withOthers, envelope: adsr, learn } = withEnvelope();
    for (const [snapshot, node] of [[withOthers, adsr], [withOthers, learn], [withKnob, knob]] as const) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: node.id, field: "sync", value: 3 }), node.name).toEqual({ ok: false, reason: `sync does not apply to node ${node.id}` });
    }
  });

  test("a length is a whole number of the steps that there are, and nothing else is one", () => {
    const { snapshot, sequencer } = withSequencer();
    for (const value of [0, 17, 2.5, -1, Number.NaN]) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: sequencer.id, field: "length", value }), String(value)).toEqual({ ok: false, reason: `${String(value)} is not a length` });
    }
    for (const value of [1, 16]) expect(sourceOf(applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: sequencer.id, field: "length", value }), sequencer), String(value)).toMatchObject({ length: value });
  });

  test("is refused by a step sequencer for the settings of the other kinds, and by the other kinds for the settings of a step sequencer", () => {
    const { snapshot, sequencer } = withSequencer();
    for (const field of ["waveform", "attack", "decay", "sustain", "release", "value"] as const) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: sequencer.id, field, value: 0.5 }), field).toEqual({ ok: false, reason: `${field} does not apply to node ${sequencer.id}` });
    }
    for (const field of ["glide", "length"] as const) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field, value: 1 }), field).toEqual({ ok: false, reason: `${field} does not apply to node ${lfo.id}` });
    }
    // (An LFO has the switch, which is its key's, and a fade that a step sequencer has not.)
    expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: sequencer.id, field: "fade", value: 1 }), "fade").toEqual({ ok: false, reason: `fade does not apply to node ${sequencer.id}` });
  });

  test("gives a step sequencer all of its steps, and changes nothing else of it or of the graph", () => {
    const { snapshot, sequencer } = withSequencer();
    const values = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1, -0.9, -0.8, -0.7, -0.6, -0.5, -0.4];
    const after = applied(snapshot, { rev: 1, type: "stepsSet", node: sequencer.id, values });
    expect(sourceOf(after, sequencer)).toEqual({ type: "steps", values, length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 0 });
    expect(after.graph.nodes.find((n) => n.id === lfo.id)).toBe(lfo);
    expect(after.graph.edges).toBe(snapshot.graph.edges);
    expect(sourceOf(snapshot, sequencer), "the snapshot it was applied to").toMatchObject({ values: [0.5, -0.25, 1, -1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] });
  });

  test("a list of steps is refused for what is not a step sequencer, for a node that is not there and for a list of the wrong length", () => {
    const { snapshot, sequencer } = withSequencer();
    const sixteen = Array<number>(16).fill(0);
    expect(applyEvent(snapshot, { rev: 1, type: "stepsSet", node: lfo.id, values: sixteen })).toEqual({ ok: false, reason: `node ${lfo.id} is not a step sequencer` });
    expect(applyEvent(snapshot, { rev: 1, type: "stepsSet", node: oscillator.id, values: sixteen })).toEqual({ ok: false, reason: `node ${oscillator.id} is not a step sequencer` });
    expect(applyEvent(snapshot, { rev: 1, type: "stepsSet", node: 12345, values: sixteen })).toEqual({ ok: false, reason: "there is no node 12345" });
    for (const length of [0, 15, 17]) {
      expect(applyEvent(snapshot, { rev: 1, type: "stepsSet", node: sequencer.id, values: Array<number>(length).fill(0) }), String(length)).toEqual({
        ok: false,
        reason: "a step sequencer has 16 steps, and " + String(length) + " were told",
      });
    }
  });

  test("is refused by a macro for each of the settings that it has not, and by every other kind for its value", () => {
    const { snapshot, knob } = withMacro();
    const { snapshot: more, envelope, learn } = withEnvelope();
    for (const field of ["rate", "waveform", "attack", "decay", "sustain", "release"] as const) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node: knob.id, field, value: 0.5 }), field).toEqual({ ok: false, reason: `${field} does not apply to node ${knob.id}` });
    }
    for (const node of [lfo.id, envelope.id, learn.id]) {
      const snapshotOf = node === lfo.id ? snapshot : more;
      expect(applyEvent(snapshotOf, { rev: 1, type: "modulatorFieldSet", node, field: "value", value: 0.5 }), `node ${node}`).toEqual({ ok: false, reason: `value does not apply to node ${node}` });
    }
  });

  test("changes only the modulator it names, and shares the rest of the graph", () => {
    const { snapshot, envelope } = withEnvelope();
    const after = applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "rate", value: 4 });
    expect(after.graph.nodes.find((n) => n.id === envelope.id)).toBe(envelope);
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.edges).toBe(snapshot.graph.edges);
    expect(sourceOf(snapshot, lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 0.5, retrigger: false, fade: 0, sync: 0 });
  });

  test("is refused by what it does not fit, and says why", () => {
    const { snapshot, envelope, learn } = withEnvelope();
    const refused = [
      { node: 12345, field: "rate" as const, value: 1, reason: "there is no node 12345" },
      { node: oscillator.id, field: "rate" as const, value: 1, reason: `node ${oscillator.id} is not a modulator` },
      { node: MASTER, field: "rate" as const, value: 1, reason: `node ${MASTER} is not a modulator` },
      { node: lfo.id, field: "attack" as const, value: 1, reason: `attack does not apply to node ${lfo.id}` },
      { node: lfo.id, field: "sustain" as const, value: 1, reason: `sustain does not apply to node ${lfo.id}` },
      { node: envelope.id, field: "rate" as const, value: 1, reason: `rate does not apply to node ${envelope.id}` },
      { node: envelope.id, field: "waveform" as const, value: 1, reason: `waveform does not apply to node ${envelope.id}` },
      { node: learn.id, field: "rate" as const, value: 1, reason: `rate does not apply to node ${learn.id}` },
      { node: lfo.id, field: "waveform" as const, value: 6, reason: "6 is not a waveform" },
      { node: lfo.id, field: "waveform" as const, value: 1.5, reason: "1.5 is not a waveform" },
      { node: lfo.id, field: "waveform" as const, value: -1, reason: "-1 is not a waveform" },
      { node: lfo.id, field: "waveform" as const, value: Number.NaN, reason: "NaN is not a waveform" },
    ];
    for (const { node, field, value, reason } of refused) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node, field, value }), `${field} of ${node}`).toEqual({ ok: false, reason });
    }
  });
});

describe("an edge depth set", () => {
  test("sets the depth of that cable", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "edgeDepthSet", edge: CABLE, depth: -0.5 });
    expect(amountOf(after, CABLE)).toBe(-0.5);
    expect(amountOf(tracerSnapshot, CABLE)).toBe(0.4);
  });

  test("changes only that cable, and shares the nodes and the other edges", () => {
    const second = { ...cableOf(tracerSnapshot), id: 7, amount: 0.9 };
    const snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, edges: [...tracerSnapshot.graph.edges, second] } };
    const after = applied(snapshot, { rev: 1, type: "edgeDepthSet", edge: CABLE, depth: 0.1 });
    expect(after.graph.nodes).toBe(snapshot.graph.nodes);
    expect(after.graph.edges.filter((e) => e.kind !== "modulation")).toEqual(snapshot.graph.edges.filter((e) => e.kind !== "modulation"));
    expect(after.graph.edges.at(-1)).toBe(second);
    expect([amountOf(after, CABLE), amountOf(after, 7)]).toEqual([0.1, 0.9]);
  });

  test("is refused for a cable that is not there, and says so", () => {
    expect(applyEvent(tracerSnapshot, { rev: 1, type: "edgeDepthSet", edge: 99, depth: 0.5 })).toEqual({ ok: false, reason: "there is no cable 99" });
  });
});

describe("a cable gain set", () => {
  test("sets the level of that audio cable", () => {
    const snapshot = withSend();
    const after = applied(snapshot, { rev: 1, type: "cableGainSet", edge: SEND, gain: 1.5 });
    expect(gainOf(after, SEND)).toBe(1.5);
    expect(gainOf(snapshot, SEND)).toBe(0.5);
  });

  test("changes only that cable, shares the nodes and the other edges, and leaves a cable of modulation with the id it has alone", () => {
    const snapshot = withSend();
    const other: Edge = { kind: "cable", id: 32, gain: 1, from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } } };
    const both = { ...snapshot, graph: { ...snapshot.graph, edges: [...snapshot.graph.edges, other] } };
    const after = applied(both, { rev: 1, type: "cableGainSet", edge: SEND, gain: 0 });
    expect(after.graph.nodes).toBe(both.graph.nodes);
    expect(after.graph.edges.at(-1)).toBe(other);
    expect([gainOf(after, SEND), gainOf(after, 32)]).toEqual([0, 1]);
    expect(amountOf(after, CABLE)).toBe(amountOf(both, CABLE));
  });

  test("is refused for an audio cable that is not there, and for a cable of modulation, and says so", () => {
    const snapshot = withSend();
    expect(applyEvent(snapshot, { rev: 1, type: "cableGainSet", edge: 99, gain: 0.5 })).toEqual({ ok: false, reason: "there is no audio cable 99" });
    expect(applyEvent(snapshot, { rev: 1, type: "cableGainSet", edge: CABLE, gain: 0.5 })).toEqual({ ok: false, reason: `there is no audio cable ${CABLE}` });
    expect(applyEvent(snapshot, { rev: 1, type: "edgeDepthSet", edge: SEND, depth: 0.5 })).toEqual({ ok: false, reason: `there is no cable ${SEND}` });
  });
});

describe("an event that does not fit", () => {
  test("for a node that is not there says so", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "paramSet", node: 12345, param: 0, value: 1 });
    expect(result).toEqual({ ok: false, reason: "there is no node 12345" });
  });

  test("for a parameter the plugin does not have says so", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "paramSet", node: filter.id, param: 99, value: 1 });
    expect(result.ok).toBe(false);
    expect(!result.ok && result.reason).toContain("no parameter 99");
  });

  test("for a node with no plugin says so", () => {
    for (const node of [MASTER, nodeNamed(tracerSnapshot, "LFO").id]) {
      const result = applyEvent(tracerSnapshot, { rev: 1, type: "paramSet", node, param: 0, value: 1 });
      expect(!result.ok && result.reason, `node ${node}`).toContain("has no plugin");
    }
  });

  test("that skips a revision says the client has missed something", () => {
    const result = applyEvent(tracerSnapshot, { rev: 3, type: "paramSet", node: filter.id, param: 0, value: 1 });
    expect(result).toEqual({ ok: false, reason: "the session is at revision 0 and the next event is 3" });
  });
});

describe("an event the snapshot already has", () => {
  test("changes nothing, and gives the same snapshot back", () => {
    const at5 = { ...tracerSnapshot, rev: 5 };
    for (const rev of [5, 4, 0]) {
      const result = applyEvent(at5, { rev, type: "paramSet", node: filter.id, param: 0, value: 0.1 });
      expect(result.ok && result.snapshot).toBe(at5);
    }
  });
});
