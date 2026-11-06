import { afterEach, describe, expect, test } from "vitest";

import type { Edge, Node, Port, Snapshot } from "../protocol";
import { Client } from "./connection";
import { MASTER, groupsFixture, macrosFixture, movesFixture, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import type { MockOptions } from "./mock";
import type { Module } from "./modules";
import { Editor, MAX_SESSION_FILE_BYTES, refusal } from "./editor";
import { Notices } from "./notices";
import { samePort } from "./reach";
import { Reveals } from "./reveals";

const clients: Client[] = [];
afterEach(() => {
  for (const client of clients.splice(0)) client.stop();
});

async function until(condition: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 2000;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}

/** An editor of a session on a mock engine, with its notices. */
async function open(options: MockOptions = {}) {
  const server = new MockServer(options);
  const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
  clients.push(client);
  client.start();
  await until(() => client.state.status === "open", "the session");
  const notices = new Notices();
  const reveals = new Reveals();
  const shown: number[] = [];
  reveals.subscribe((node) => {
    // (What is shown is in the session the client has by then: the canvas has something to look for.)
    expect(client.state.snapshot?.graph.nodes.some((n) => n.id === node), `node ${String(node)} is shown before the client has it`).toBe(true);
    shown.push(node);
  });
  return { server, client, notices, shown, editor: new Editor(client, notices, reveals) };
}

const said = (notices: Notices): string[] => notices.getItems().map((notice) => notice.text);
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
const param = (node: typeof filter, name: string): number => {
  if (node.kind !== "instrument" && node.kind !== "effect") throw new Error("no plugin");
  const found = node.plugin?.params.find((p) => p.name === name);
  if (!found) throw new Error(`no ${name}`);
  return found.index;
};
/** The port of a parameter of a node, by its name. */
const paramPort = (node: typeof filter, name: string): Port => ({ type: "param", index: param(node, name) });

describe("adding a modulator for a lane or a group", () => {
  test("resolves with the node it made, which is in the session's own rack and hears the lane, is in the session then and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addFreeModulator("envelope", oscillator.id);
    expect(id).toBe(3);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === id);
    expect(node).toMatchObject({ kind: "modulator", parent: null, hears: oscillator.id, name: "ADSR" });
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addFreeModulator("lfo", oscillator.id)).toBe(4);
  });

  test("finds its own node when another front-end added one that hears another lane at the same time", async () => {
    const { server, editor } = await open({ snapshot: reachFixture.snapshot });
    server.apply({ type: "addFreeModulator", kind: "lfo", hears: 6 }); // another front-end's, for the second lane, before this one is answered
    const id = await editor.addFreeModulator("lfo", 4);
    const mine = server.snapshot.graph.nodes.find((n) => n.id === id);
    expect(mine).toMatchObject({ kind: "modulator", hears: 4 });
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.hears === 6).length).toBe(2);
  });

  test("takes the last of the modulators that are new and hear the lane, which is its own, when another front-end added one for the same lane just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addFreeModulator", kind: "envelope", hears: oscillator.id });
    const id = await editor.addFreeModulator("lfo", oscillator.id);
    const names = server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.hears === oscillator.id).map((n) => [n.id, n.name]);
    expect(names).toEqual([[2, "LFO"], [3, "ADSR"], [4, "LFO"]]);
    expect(id).toBe(4);
  });

  test("is refused for what is no lane or group: it resolves with null, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addFreeModulator("lfo", filter.id)).toBeNull();
    expect(await editor.addFreeModulator("midiLearn", lfo.id)).toBeNull();
    expect(await editor.addFreeModulator("envelope", MASTER)).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add an LFO that hears Filter: only a lane or a group can be heard",
      "Could not add a MIDI control that hears LFO: only a lane or a group can be heard",
      "Could not add an envelope that hears Master: there is no node 4294967295",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("says a macro that it could not add is a macro", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addFreeModulator("macro", filter.id)).toBeNull();
    expect(said(notices)).toEqual(["Could not add a macro that hears Filter: only a lane or a group can be heard"]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addFreeModulator("lfo", oscillator.id)).toBeNull();
    expect(said(notices)).toEqual(["Could not add an LFO that hears Oscillator: Not connected to the server."]);
  });
});

describe("adding a modulator that is in no lane and no group", () => {
  test("resolves with the node it made, which has no parent, is in the session then and is shown, and says nothing", async () => {
    const { client, notices, shown, editor } = await open();
    const id = await editor.addFreeModulator("envelope");
    expect(id).toBe(3);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "modulator", parent: null, name: "ADSR" });
    expect(shown).toEqual([3]);
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addFreeModulator("lfo")).toBe(4);
    expect(shown).toEqual([3, 4]);
  });

  test("takes the one that is new to the session's own rack, which is its own, when another front-end added one just before, and not one that hears a lane", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addFreeModulator", kind: "envelope", hears: null });
    const id = await editor.addFreeModulator("lfo");
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.hears === undefined).map((n) => [n.id, n.name])).toEqual([[3, "ADSR"], [4, "LFO"]]);
    expect(id).toBe(4);
    server.apply({ type: "addFreeModulator", kind: "macro", hears: oscillator.id });
    expect(await editor.addFreeModulator("steps")).toBe(6);
  });

  test("with no connection says so, and shows nothing", async () => {
    const { client, notices, shown, editor } = await open();
    client.stop();
    expect(await editor.addFreeModulator("lfo")).toBeNull();
    expect(said(notices)).toEqual(["Could not add an LFO of its own: Not connected to the server."]);
    expect(shown).toEqual([]);
  });

  test("asks for the ear with the modulator, in one request, and the node it made hears the lane (the '+' of a lane)", async () => {
    const { client, server, notices, shown, editor } = await open();
    const asked: unknown[] = [];
    const request = client.request.bind(client);
    client.request = (message) => {
      asked.push(message);
      return request(message);
    };
    const id = await editor.addFreeModulator("lfo", oscillator.id);
    expect(id).toBe(3);
    expect(asked).toEqual([{ type: "addFreeModulator", kind: "lfo", hears: oscillator.id }]);
    expect(server.snapshot.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "modulator", parent: null, hears: oscillator.id });
    expect(shown).toEqual([3]);
    expect(said(notices)).toEqual([]);
  });

  test("says why when the ear is no lane or group, in the names the canvas shows, and makes nothing", async () => {
    const { client, notices, shown, editor } = await open();
    expect(await editor.addFreeModulator("lfo", filter.id)).toBeNull();
    expect(said(notices)).toEqual(["Could not add an LFO that hears Filter: only a lane or a group can be heard"]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    expect(shown).toEqual([]);
    // (Nothing was used up: the next one has the id that this one would have had.)
    expect(await editor.addFreeModulator("lfo", oscillator.id)).toBe(3);
  });
});

describe("choosing whose notes a modulator that is in no lane hears", () => {
  const hears = (client: Client, node: number): number | undefined | "no modulator" => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "modulator" ? found.hears : "no modulator";
  };

  test("asks for the lane or the group, and the session says whom the modulator hears: and every note when it is asked for none", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addFreeModulator("envelope");
    expect(id).toBe(3);
    expect(await editor.setModulatorEar(3, oscillator.id)).toBe(true);
    expect(hears(client, 3)).toBe(oscillator.id);
    expect(await editor.setModulatorEar(3, null)).toBe(true);
    expect(hears(client, 3)).toBeUndefined();
    expect(said(notices)).toEqual([]);
  });

  test("is one request, in the intent's words, and puts nothing among the notices", async () => {
    const { client, server, editor } = await open();
    await editor.addFreeModulator("lfo");
    const asked: unknown[] = [];
    const request = client.request.bind(client);
    client.request = (message) => {
      asked.push(message);
      return request(message);
    };
    await editor.setModulatorEar(3, oscillator.id);
    expect(asked).toEqual([{ type: "setModulatorEar", node: 3, ear: oscillator.id }]);
    expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).toMatchObject({ hears: oscillator.id });
  });

  test("says why when it is refused: only a modulator has an ear, and a plugin is no lane or group", async () => {
    const { client, notices, editor } = await open();
    await editor.addFreeModulator("lfo");
    expect(await editor.setModulatorEar(filter.id, oscillator.id)).toBe(false);
    expect(await editor.setModulatorEar(3, filter.id)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not make Filter hear Oscillator: only a modulator has an ear",
      "Could not make LFO 2 hear Filter: only a lane or a group can be heard",
    ]);
    expect(hears(client, 3)).toBeUndefined();
  });

  test("with no connection says so, and changes nothing", async () => {
    const { client, notices, editor } = await open();
    await editor.addFreeModulator("lfo");
    client.stop();
    expect(await editor.setModulatorEar(3, null)).toBe(false);
    expect(said(notices)).toEqual(["Could not make LFO 2 hear every note: Not connected to the server."]);
  });
});

describe("modulating a parameter", () => {
  /** The cables of the session that end at the port `to` of `node`. */
  const cablesTo = (cables: readonly Edge[], node: number, to: Port): Edge[] => cables.filter((e) => e.kind === "modulation" && e.to.node === node && samePort(e.to.port, to));
  const requests = (server: MockServer) => server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []));

  test("resolves with the node of the modulator it made, which is the session's own and hears the lane, with a cable from it to the parameter at half depth, and says nothing; the canvas is asked to show it", async () => {
    const { client, notices, shown, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    const id = await editor.modulate(filter.id, cutoff, "envelope");
    expect(id).toBe(3);
    const graph = client.state.snapshot?.graph;
    expect(graph?.nodes.find((n) => n.id === id)).toMatchObject({ kind: "modulator", parent: null, hears: oscillator.id, name: "ADSR" });
    const [cable] = cablesTo(graph?.edges ?? [], filter.id, cutoff).filter((e) => e.kind === "modulation" && e.from.node === id);
    expect(cable).toMatchObject({ kind: "modulation", from: { node: id, port: { type: "modOut" } }, amount: 0.5 });
    expect(said(notices)).toEqual([]);
    expect(shown).toEqual([id]);
  });

  test("asks for one intent, at the depth the TUI's m gives a cable (none given: the controller's), and the depth the TUI's l gives a control that learns, which is armed", async () => {
    const { server, client, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    const first = await editor.modulate(filter.id, cutoff, "lfo");
    const learner = await editor.modulate(filter.id, cutoff, "midiLearn");
    expect(requests(server)).toEqual([
      { type: "modulate", to: { node: filter.id, port: cutoff }, kind: "lfo", depth: null },
      { type: "modulate", to: { node: filter.id, port: cutoff }, kind: "midiLearn", depth: 1 },
    ]);
    const graph = client.state.snapshot?.graph;
    expect(graph?.nodes.find((n) => n.id === learner)).toMatchObject({ kind: "modulator", source: { type: "midiLearn", control: null, learning: true } });
    // (The cutoff has a cable of the tracer's own, at 0.4.)
    const made = cablesTo(graph?.edges ?? [], filter.id, cutoff).filter((e) => e.kind === "modulation" && (e.from.node === first || e.from.node === learner));
    expect(made.map((e) => (e.kind === "modulation" ? e.amount : null))).toEqual([0.5, 1]);
  });

  test("makes a sibling that drives a setting of a modulator, and shows it", async () => {
    const { client, shown, editor } = await open();
    const id = await editor.modulate(lfo.id, { type: "rate" }, "lfo");
    const graph = client.state.snapshot?.graph;
    expect(graph?.nodes.find((n) => n.id === id)).toMatchObject({ kind: "modulator", parent: lfo.parent });
    expect(cablesTo(graph?.edges ?? [], lfo.id, { type: "rate" }).some((e) => e.kind === "modulation" && e.from.node === id)).toBe(true);
    expect(shown).toEqual([id]);
  });

  test("finds its own node, the one that has the cable to the port, when another front-end made one for the same parameter just before", async () => {
    const { server, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    server.apply({ type: "modulate", to: { node: filter.id, port: cutoff }, kind: "envelope", depth: null });
    const id = await editor.modulate(filter.id, cutoff, "lfo");
    const mine = server.snapshot.graph.nodes.find((n) => n.id === id);
    expect(mine).toMatchObject({ kind: "modulator", name: "LFO" });
    expect(id).toBe(4);
  });

  test("is refused as the controller refuses it: it resolves with null, nothing is made, and the page says what it could not do and why, in the names the canvas shows", async () => {
    const { client, notices, shown, editor } = await open({ snapshot: reachFixture.snapshot });
    const before = client.state.snapshot;
    const group = reachFixture.snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!group) throw new Error("the fixture has a group");
    expect(await editor.modulate(group.id, { type: "param", index: 1 }, "lfo")).toBeNull();
    expect(await editor.modulate(MASTER, { type: "param", index: 1 }, "envelope")).toBeNull();
    expect(await editor.modulate(group.id, { type: "modOut" }, "midiLearn")).toBeNull();
    expect(said(notices)).toEqual([
      `Could not modulate ${group.name} with an LFO: a modulator drives a parameter of a plugin, a volume, a mix or a pan, or a setting of a modulator, and that is none of them`,
      "Could not modulate Master with an envelope: there is no node 4294967295",
      `Could not modulate ${group.name} with a MIDI control: a modulator drives a parameter of a plugin, a volume, a mix or a pan, or a setting of a modulator, and that is none of them`,
    ]);
    expect(client.state.snapshot).toEqual(before);
    expect(shown).toEqual([]);
  });

  test("names what it could not modulate by the port: a setting of a modulator by its name, a parameter that the plugin has not by the node alone", async () => {
    const { notices, editor } = await open();
    expect(await editor.modulate(filter.id, { type: "param", index: 9_999 }, "lfo")).toBeNull();
    expect(await editor.modulate(lfo.id, { type: "attack" }, "envelope")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not modulate Filter with an LFO: the plugin has no parameter 9999",
      "Could not modulate LFO: attack with an envelope: that modulator has no attack",
    ]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.modulate(filter.id, paramPort(filter, "cutoff"), "lfo")).toBeNull();
    expect(said(notices)).toEqual(["Could not modulate Filter: cutoff with an LFO: Not connected to the server."]);
  });
});

describe("adding an effect", () => {
  test("resolves with the node it made, which is in the lane's chain then, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addEffect(oscillator.id, "builtin:reverb");
    expect(id).toBe(3);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === id);
    expect(node).toMatchObject({ kind: "effect", parent: oscillator.id, name: "Reverb", mix: 0.5 });
    expect(said(notices)).toEqual([]);
    // The next is the next, and a filter goes in fully wet.
    expect(await editor.addEffect(oscillator.id, "builtin:filter")).toBe(4);
  });

  test("finds its own node when another front-end added one to another lane at the same time", async () => {
    const { server, editor } = await open({ snapshot: reachFixture.snapshot });
    server.apply({ type: "addEffect", parent: 7, plugin: "builtin:filter" }); // another front-end's, to the second lane of group 1, before this one is answered
    const id = await editor.addEffect(4, "builtin:reverb");
    const mine = server.snapshot.graph.nodes.find((n) => n.id === id);
    expect(mine).toMatchObject({ parent: 4, name: "Reverb" });
  });

  test("takes the last of the effects that are new to the lane, which is its own, when another front-end added one to the same lane just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addEffect", parent: oscillator.id, plugin: "builtin:filter" });
    const id = await editor.addEffect(oscillator.id, "builtin:reverb");
    const names = server.snapshot.graph.nodes.filter((n) => n.parent === oscillator.id && n.kind === "effect").map((n) => [n.id, n.name]);
    expect(names).toEqual([[1, "Filter"], [3, "Filter"], [4, "Reverb"]]);
    expect(id).toBe(4);
  });

  test("is refused for what is no lane or group, or a plugin that is not installed: it resolves with null, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addEffect(filter.id, "builtin:reverb")).toBeNull();
    expect(await editor.addEffect(oscillator.id, "./examples/plugin.clap")).toBeNull();
    expect(await editor.addEffect(oscillator.id, "builtin:osc")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add Reverb to Filter: an effect goes in a lane or in a group",
      "Could not add ./examples/plugin.clap to Oscillator: ./examples/plugin.clap is not a plugin that is installed",
      "Could not add Oscillator to Oscillator: Oscillator is an instrument, not an effect",
    ]);
    // (A node that is not there is named as the page names what it cannot find.)
    expect(await editor.addEffect(MASTER, "builtin:reverb")).toBeNull();
    expect(said(notices).at(-1)).toBe("Could not add Reverb to Master: there is no node 4294967295");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addEffect(oscillator.id, "builtin:reverb")).toBeNull();
    expect(said(notices)).toEqual(["Could not add Reverb to Oscillator: Not connected to the server."]);
  });
});

describe("adding an effect that stands by itself", () => {
  test("resolves with the node of the effect it made, which is the only one of a lane of its own that has no instrument, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addFreeEffect("builtin:reverb");
    // (The lane's node is made first, and the effect's next.)
    expect(id).toBe(4);
    const nodes = client.state.snapshot?.graph.nodes ?? [];
    expect(nodes.find((n) => n.id === 3)).toMatchObject({ kind: "instrument", parent: null, name: "(empty)", plugin: null, range: null, pattern: null, muted: false, solo: false, route: "none" });
    expect(nodes.find((n) => n.id === id)).toMatchObject({ kind: "effect", parent: 3, name: "Reverb", mix: 0.5 });
    expect(said(notices)).toEqual([]);
    // The next goes in a lane of its own again: two nodes more.
    expect(await editor.addFreeEffect("builtin:filter")).toBe(6);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === 5)).toMatchObject({ kind: "instrument", plugin: null });
  });

  test("takes the effect that is new to the session, which is its own, when another front-end added one just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addFreeEffect", plugin: "builtin:filter" });
    const id = await editor.addFreeEffect("builtin:reverb");
    expect(id).toBe(6);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "effect" && n.parent !== oscillator.id).map((n) => [n.id, n.name])).toEqual([[4, "Filter"], [6, "Reverb"]]);
  });

  test("is refused for a plugin that is not installed or is an instrument: it resolves with null, the page says what it could not do and why, and nothing is made", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addFreeEffect("./examples/plugin.clap")).toBeNull();
    expect(await editor.addFreeEffect("builtin:osc")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add ./examples/plugin.clap as an effect of its own: ./examples/plugin.clap is not a plugin that is installed",
      "Could not add Oscillator as an effect of its own: Oscillator is an instrument, not an effect",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addFreeEffect("builtin:reverb")).toBeNull();
    expect(said(notices)).toEqual(["Could not add Reverb as an effect of its own: Not connected to the server."]);
  });

  test("has the words that a request that is refused with nothing more known of it has", () => {
    expect(refusal({ type: "addFreeEffect", plugin: "builtin:reverb" }, new Error("Reverb is not a plugin that is installed"))).toBe("Could not add an effect of its own: Reverb is not a plugin that is installed");
  });
});

describe("adding a module (step 111)", () => {
  const space: Module = {
    lanes: [
      {
        instrument: null,
        effects: [
          { plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true },
          { plugin: "builtin:reverb", preset: null, params: {}, mix: 0.4, pinned: [], fed: true },
        ],
      },
    ],
    cables: [],
    modulators: [],
  };

  test("resolves with the node of the module, which is one box and not the last effect that is in it, asks the canvas to show it, and says nothing", async () => {
    const { client, notices, shown, editor, server } = await open({ modules: { Space: space } });
    const id = await editor.addModule("module:Space");
    expect(client.state.snapshot?.graph.nodes.find((node) => node.id === id)).toMatchObject({ kind: "module", name: "Space" });
    expect(shown).toEqual([id]);
    expect(said(notices)).toEqual([]);
    // (It is the controller's own intent that adds it, one step: the module is what `Undo` takes out.)
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "addFreeEffect", plugin: "module:Space" }]);
    expect(server.history.undo).toBe("add Space");
  });

  test("puts the box where it is told, by a move of its own, and does not ask the canvas to show what it put where the person is looking", async () => {
    const { client, shown, editor, server } = await open({ modules: { Space: space } });
    const at = { x: 40.5, y: -12 };
    const id = await editor.addModule("module:Space", at);
    expect(client.state.snapshot?.graph.nodes.find((node) => node.id === id)).toMatchObject({ kind: "module", ui: at });
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent.type] : []))).toEqual(["addFreeEffect", "setNodeUi"]);
    expect(shown).toEqual([]);
  });

  test("is refused for a module that the library has not (or that has a plugin that is not installed): it resolves with null, the page says what it could not do, in the words of a module, and why, and nothing is made", async () => {
    const needs: Module = { ...space, lanes: [{ instrument: null, effects: [{ plugin: "clap:com.example.nothing", preset: null, params: {}, mix: 1, pinned: [], fed: true }] }] };
    const { client, notices, editor } = await open({ modules: { Needs: needs } });
    expect(await editor.addModule("module:Gone")).toBeNull();
    expect(await editor.addModule("module:Needs")).toBeNull();
    const [gone, plugin] = said(notices);
    expect(gone).toBe("Could not add the module Gone: there is no module called Gone in the library");
    expect(plugin).toBe("Could not add the module Needs: the module Needs cannot be added: clap:com.example.nothing is not a plugin that is installed");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open({ modules: { Space: space } });
    client.stop();
    expect(await editor.addModule("module:Space")).toBeNull();
    expect(said(notices)).toEqual(["Could not add the module Space: Not connected to the server."]);
  });

  test("is not what adds an effect: an effect that stands by itself is said in its own words, and a module that goes in by that way is a module all the same", async () => {
    const { client, notices, editor } = await open({ modules: { Space: space } });
    expect(await editor.addFreeEffect("builtin:osc")).toBeNull();
    expect(said(notices)).toEqual(["Could not add Oscillator as an effect of its own: Oscillator is an instrument, not an effect"]);
    const id = await editor.addFreeEffect("module:Space");
    expect(client.state.snapshot?.graph.nodes.find((node) => node.id === id)?.kind).toBe("module");
  });
});

describe("adding an instrument", () => {
  test("resolves with the node of the lane it made, which is in the session then, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const id = await editor.addInstrument("builtin:osc");
    expect(id).toBe(3);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === id);
    // (A lane that is new has no pattern, as the controller makes it.)
    expect(node).toMatchObject({ kind: "instrument", parent: null, name: "Oscillator", pattern: null });
    expect(said(notices)).toEqual([]);
    // The next is the next.
    expect(await editor.addInstrument("builtin:osc")).toBe(4);
  });

  test("takes the lane that is new to the session, which is its own, when another front-end added one just before", async () => {
    const { server, editor } = await open();
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    const id = await editor.addInstrument("builtin:osc");
    expect(id).toBe(4);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "instrument").map((n) => n.id)).toEqual([0, 3, 4]);
  });

  test("is refused for a plugin that is not installed or is not an instrument: it resolves with null, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addInstrument("./examples/plugin.clap")).toBeNull();
    expect(await editor.addInstrument("builtin:reverb")).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add ./examples/plugin.clap as an instrument: ./examples/plugin.clap is not a plugin that is installed",
      "Could not add Reverb as an instrument: Reverb is an effect, not an instrument",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addInstrument("builtin:osc")).toBeNull();
    expect(said(notices)).toEqual(["Could not add Oscillator as an instrument: Not connected to the server."]);
  });
});

// A menu on the canvas adds a node where the pointer was (step 107): the node is added, and then put there, which is a move of its own in the history, and the canvas, whose person is looking at the place, is not moved to show it.
describe("adding a node where a menu on the canvas was opened", () => {
  const place = { x: 120.5, y: -30 };
  const intentsAsked = (server: MockServer): string[] => server.received.flatMap((message) => (message.type === "request" ? [message.intent.type] : []));

  test("puts an instrument, an effect, a modulator and a source of notes at the place it is given, each by a move of its own after the add, and says nothing", async () => {
    const { client, server, notices, editor } = await open();
    const lane = await editor.addInstrument("builtin:osc", place);
    const effect = await editor.addFreeEffect("builtin:reverb", place);
    const modulator = await editor.addFreeModulator("lfo", null, place);
    const keys = await editor.addSource({ type: "keys" }, place);
    for (const [id, kind] of [
      [lane, "instrument"],
      [effect, "effect"],
      [modulator, "modulator"],
      [keys, "source"],
    ] as const) {
      expect(id).not.toBeNull();
      const node = client.state.snapshot?.graph.nodes.find((candidate) => candidate.id === id);
      expect(node?.kind).toBe(kind);
      expect(node?.ui).toEqual(place);
    }
    expect(intentsAsked(server)).toEqual(["addInstrument", "setNodeUi", "addFreeEffect", "setNodeUi", "addFreeModulator", "setNodeUi", "addSource", "setNodeUi"]);
    expect(said(notices)).toEqual([]);
  });

  test("puts a module where it is told, as the one box it is on the canvas: the node that is placed and shown is the module's, and not the last effect that is in it", async () => {
    const space: Module = {
      lanes: [
        {
          instrument: null,
          effects: [
            { plugin: "builtin:filter", preset: null, params: {}, mix: 1, pinned: [], fed: true },
            { plugin: "builtin:reverb", preset: null, params: {}, mix: 0.4, pinned: [], fed: true },
          ],
        },
      ],
      cables: [],
      modulators: [],
    };
    const { client, server, shown, editor } = await open({ modules: { Space: space } });
    const placed = await editor.addFreeEffect("module:Space", place);
    const nodes = client.state.snapshot?.graph.nodes ?? [];
    expect(nodes.find((candidate) => candidate.id === placed)).toMatchObject({ kind: "module", name: "Space", ui: place });
    expect(intentsAsked(server)).toEqual(["addFreeEffect", "setNodeUi"]);
    expect(shown).toEqual([]);
    // Without a place the canvas is asked to show it, and it is the box that it is asked to show.
    const shownAgain = await editor.addFreeEffect("module:Space");
    expect(nodes.length).toBeLessThan(client.state.snapshot?.graph.nodes.length ?? 0);
    expect(client.state.snapshot?.graph.nodes.find((candidate) => candidate.id === shownAgain)?.kind).toBe("module");
    expect(shown).toEqual([shownAgain]);
  });

  test("does not ask the canvas to show a node that it put where the person is looking", async () => {
    const { shown, editor } = await open();
    await editor.addInstrument("builtin:osc", place);
    await editor.addFreeEffect("builtin:reverb", place);
    await editor.addFreeModulator("lfo", oscillator.id, place);
    await editor.addSource({ type: "keys" }, place);
    expect(shown).toEqual([]);
  });

  test("shows the node, as it does one that is given no place, when the place is refused, and says why the node could not be moved", async () => {
    const { shown, notices, editor } = await open();
    // (A number that is beyond the 32-bit floats of the engine: `NaN` goes over the wire as `null`, which is 0.)
    const id = await editor.addInstrument("builtin:osc", { x: 1e39, y: 0 });
    expect(id).toBe(3);
    expect(shown).toEqual([3]);
    expect(said(notices)).toHaveLength(1);
    expect(said(notices)[0]).toMatch(/^Could not move /);
  });

  test("asks for no move of a node that is given no place, which is shown, and none of a node that was refused", async () => {
    const { server, shown, editor } = await open();
    expect(await editor.addFreeModulator("lfo", null)).not.toBeNull();
    expect(intentsAsked(server)).toEqual(["addFreeModulator"]);
    expect(shown).toHaveLength(1);
    expect(await editor.addInstrument("builtin:reverb", place)).toBeNull();
    expect(intentsAsked(server)).toEqual(["addFreeModulator", "addInstrument"]);
  });

  test("is as it was with no connection: it says so, and moves nothing", async () => {
    const { client, notices, editor, server } = await open();
    client.stop();
    expect(await editor.addFreeEffect("builtin:reverb", place)).toBeNull();
    expect(said(notices)).toEqual(["Could not add Reverb as an effect of its own: Not connected to the server."]);
    expect(intentsAsked(server)).toEqual([]);
  });
});

// The session of the groups fixture: three groups (0, 1 and 2, the last empty and called Lead; two modulators hear the first, 13 and 14, and one the second,
// 15), four lanes (3 and 7 in the first group, 6 in the second, 4 in none; each has an instrument called Oscillator, and the canvas numbers what is called
// alike across the session in the order of its graph), and the cables of the modulators that hear the groups and the lanes.
describe("adding a group", () => {
  const groups = { snapshot: groupsFixture.snapshot };
  const parentOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.parent;

  test("resolves with the node of the group it made, which is in the session then, and says nothing; the canvas is asked to show it", async () => {
    const { client, notices, shown, editor } = await open(groups);
    const id = await editor.addGroup();
    expect(id).toBe(16);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "group", parent: null, name: "Group 4" });
    expect(said(notices)).toEqual([]);
    expect(shown).toEqual([16]);
    // The next is the next.
    expect(await editor.addGroup()).toBe(17);
  });

  test("puts the lanes it is given in the group it makes, whatever group they were in", async () => {
    const { client, editor } = await open(groups);
    const id = await editor.addGroup([4, 7]);
    expect(id).toBe(16);
    expect([3, 4, 6, 7].map((lane) => parentOf(client, lane))).toEqual([0, id, 1, id]);
  });

  test("takes the group that is new to the session, which is its own, when another front-end added one just before", async () => {
    const { server, editor } = await open(groups);
    server.apply({ type: "addGroup", members: [] });
    expect(await editor.addGroup()).toBe(17);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "group").map((n) => n.id)).toEqual([0, 1, 2, 16, 17]);
  });

  test("is refused for a member that is not a lane: it resolves with null, no group is made, and the page says what it could not do and why", async () => {
    const { client, notices, shown, editor } = await open(groups);
    expect(await editor.addGroup([0])).toBeNull();
    expect(await editor.addGroup([4, 9999])).toBeNull();
    expect(said(notices)).toEqual([
      "Could not put Group 1 in a new group: only a lane can be put in a group",
      "Could not put Oscillator 2, that node in a new group: there is no node 9999",
    ]);
    expect(client.state.snapshot).toEqual(groupsFixture.snapshot);
    expect(shown).toEqual([]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(groups);
    client.stop();
    expect(await editor.addGroup()).toBeNull();
    expect(await editor.addGroup([4])).toBeNull();
    expect(said(notices)).toEqual(["Could not add a group: Not connected to the server.", "Could not put Oscillator 2 in a new group: Not connected to the server."]);
  });
});

describe("putting a lane in a group", () => {
  const groups = { snapshot: groupsFixture.snapshot };
  const parentOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.parent;

  test("resolves true when the lane is in the group, which the session shows, and says nothing; and for null when it is in none", async () => {
    const { client, notices, editor } = await open(groups);
    expect(await editor.setLaneGroup(4, 1)).toBe(true);
    expect(parentOf(client, 4)).toBe(1);
    expect(await editor.setLaneGroup(4, null)).toBe(true);
    expect(parentOf(client, 4)).toBeNull();
    expect(said(notices)).toEqual([]);
  });

  test("asks the canvas to show the group the lane was put in, which moves with it, and nothing when the lane is taken out of its group or the lane was refused", async () => {
    const { shown, editor } = await open(groups);
    expect(await editor.setLaneGroup(4, 1)).toBe(true);
    expect(shown).toEqual([1]);
    expect(await editor.setLaneGroup(4, null)).toBe(true);
    expect(await editor.setLaneGroup(4, 4)).toBe(false);
    expect(await editor.setLaneGroup(4, 9999)).toBe(false);
    expect(shown).toEqual([1]);
  });

  test("leaves every cable where it is when a lane leaves its group or is put in another: a cable names the plugin it drives, and the modulator that hears the group goes on hearing it (step 96)", async () => {
    const { client, editor } = await open(groups);
    const cables = () => client.state.snapshot?.graph.edges.flatMap((e) => (e.kind === "modulation" ? [e.id] : []));
    expect(cables()).toEqual([0, 1, 2, 3, 4]);
    expect(await editor.setLaneGroup(3, null)).toBe(true);
    expect(cables()).toEqual([0, 1, 2, 3, 4]);
    expect(await editor.setLaneGroup(7, 1)).toBe(true);
    expect(cables()).toEqual([0, 1, 2, 3, 4]);
  });

  test("is refused for a node that is not a lane and for one that is not a group: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open(groups);
    // (The notices keep the last few: each is read as it is said.)
    const cases: [number, number | null, string][] = [
      [0, 1, "Could not put Group 1 in Group 2: only a lane can be put in a group"],
      [4, 3, "Could not put Oscillator 2 in Oscillator 1: that node is not a group"],
      [4, 9999, "Could not put Oscillator 2 in that node: there is no node 9999"],
      [9999, null, "Could not take that node out of its group: there is no node 9999"],
    ];
    for (const [lane, group, words] of cases) {
      expect(await editor.setLaneGroup(lane, group), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(groupsFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(groups);
    client.stop();
    expect(await editor.setLaneGroup(4, 1)).toBe(false);
    expect(await editor.setLaneGroup(3, null)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not put Oscillator 2 in Group 2: Not connected to the server.",
      "Could not take Oscillator 1 out of its group: Not connected to the server.",
    ]);
  });
});

describe("naming a group", () => {
  const groups = { snapshot: groupsFixture.snapshot };
  const nameOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.name;

  test("resolves true when the group has the name, which the session shows, and says nothing; a name of nothing gives the default one back", async () => {
    const { client, notices, editor } = await open(groups);
    expect(await editor.renameGroup(1, "  Strings ")).toBe(true);
    expect(nameOf(client, 1)).toBe("Strings");
    expect(await editor.renameGroup(1, "")).toBe(true);
    expect(nameOf(client, 1)).toBe("Group 2");
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a name that is too long or has a control character, and for a node that is not a group: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open(groups);
    // (The notices keep the last few: each is read as it is said.)
    const cases: [number, string, string][] = [
      [0, "x".repeat(65), `Could not give Group 1 the name "${"x".repeat(24)}…": a name is at most 64 characters`],
      // (A character is not always one unit of a string: the quote is cut after 24 of them, not after 24 units.)
      [0, "😀".repeat(65), `Could not give Group 1 the name "${"😀".repeat(24)}…": a name is at most 64 characters`],
      [0, "two\nlines", 'Could not give Group 1 the name "two\\nlines": a name has no control characters'],
      [3, "Strings", 'Could not give Oscillator 1 the name "Strings": only a group has a name to change'],
      [9999, "Strings", 'Could not give that node the name "Strings": there is no node 9999'],
    ];
    for (const [node, name, words] of cases) {
      expect(await editor.renameGroup(node, name), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(groupsFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(groups);
    client.stop();
    expect(await editor.renameGroup(0, "Strings")).toBe(false);
    expect(said(notices)).toEqual(['Could not give Group 1 the name "Strings": Not connected to the server.']);
  });
});

describe("naming a macro and having a box show its knob (step 110)", () => {
  // The session of the macros fixture: a lane (0) with a filter (1) that an LFO (2) drives, a macro that is not called anything (3) and one that the file calls Wet and hides from a box (4).
  const macros = { snapshot: macrosFixture.snapshot };
  const nodeOf = (client: Client, node: number): Node | undefined => client.state.snapshot?.graph.nodes.find((n) => n.id === node);

  test("resolves true when the macro has the name, which the session shows as the node's name and in its source, and says nothing; a name of nothing gives it `Macro` back", async () => {
    const { client, notices, editor, server } = await open(macros);
    expect(await editor.renameMacro(3, "  Brightness ")).toBe(true);
    // (What is asked is the name as it was typed: the controller takes the spaces off, and the page does not decide it.)
    expect(server.received.at(-1)).toMatchObject({ type: "request", intent: { type: "renameMacro", node: 3, name: "  Brightness " } });
    expect(nodeOf(client, 3)?.name).toBe("Brightness");
    expect(nodeOf(client, 3)).toMatchObject({ source: { type: "macro", value: 0.5, name: "Brightness" } });
    expect(await editor.renameMacro(3, "")).toBe(true);
    expect(nodeOf(client, 3)?.name).toBe("Macro");
    expect(nodeOf(client, 3)).toMatchObject({ source: { type: "macro", value: 0.5 } });
    expect((nodeOf(client, 3) as { source: object }).source).not.toHaveProperty("name");
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a name that is too long or has a control character, and for a node that is not a macro: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open(macros);
    const cases: [number, string, string][] = [
      [3, "x".repeat(65), `Could not give Macro the name "${"x".repeat(24)}…": a name is at most 64 characters`],
      [3, "two\nlines", 'Could not give Macro the name "two\\nlines": a name has no control characters'],
      [2, "Strings", 'Could not give LFO the name "Strings": only a macro has a name and a knob to show'],
      [1, "Strings", 'Could not give Filter the name "Strings": only a macro has a name and a knob to show'],
      [9999, "Strings", 'Could not give that node the name "Strings": there is no node 9999'],
    ];
    for (const [node, name, words] of cases) {
      expect(await editor.renameMacro(node, name), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(macrosFixture.snapshot);
  });

  test("resolves true when the macro shows its knob on a box or not, as asked, which the session says (and says nothing of a macro that does), and says nothing", async () => {
    const { client, notices, editor } = await open(macros);
    expect(await editor.exposeMacro(3, false)).toBe(true);
    expect(nodeOf(client, 3)).toMatchObject({ source: { type: "macro", value: 0.5, expose: false } });
    expect(await editor.exposeMacro(4, true)).toBe(true);
    expect((nodeOf(client, 4) as { source: object }).source).toEqual({ type: "macro", value: 0, name: "Wet" });
    expect(await editor.exposeMacro(3, true)).toBe(true);
    expect((nodeOf(client, 3) as { source: object }).source).toEqual({ type: "macro", value: 0.5 });
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is not a macro: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open(macros);
    expect(await editor.exposeMacro(2, false)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not hide the knob of LFO on its box: only a macro has a name and a knob to show");
    expect(await editor.exposeMacro(9999, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not show the knob of that node on its box: there is no node 9999");
    expect(client.state.snapshot).toEqual(macrosFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open(macros);
    client.stop();
    expect(await editor.renameMacro(3, "Brightness")).toBe(false);
    expect(await editor.exposeMacro(3, false)).toBe(false);
    expect(said(notices)).toEqual([
      'Could not give Macro the name "Brightness": Not connected to the server.',
      "Could not hide the knob of Macro on its box: Not connected to the server.",
    ]);
  });
});

describe("replacing an instrument", () => {
  test("resolves true when the lane has the instrument, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.replaceInstrument(oscillator.id, "builtin:osc")).toBe(true);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is not a lane, a plugin that is not installed and one that is not an instrument: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.replaceInstrument(filter.id, "builtin:osc")).toBe(false);
    expect(await editor.replaceInstrument(oscillator.id, "./examples/plugin.clap")).toBe(false);
    expect(await editor.replaceInstrument(oscillator.id, "builtin:reverb")).toBe(false);
    expect(said(notices)).toEqual([
      "Could not change the instrument of Filter to Oscillator: an instrument goes in a lane",
      "Could not change the instrument of Oscillator to ./examples/plugin.clap: ./examples/plugin.clap is not a plugin that is installed",
      "Could not change the instrument of Oscillator to Reverb: Reverb is an effect, not an instrument",
    ]);
    expect(await editor.replaceInstrument(MASTER, "builtin:osc")).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not change the instrument of Master to Oscillator: there is no node 4294967295");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.replaceInstrument(oscillator.id, "builtin:osc")).toBe(false);
    expect(said(notices)).toEqual(["Could not change the instrument of Oscillator to Oscillator: Not connected to the server."]);
  });
});

describe("taking the instrument out of a lane", () => {
  const cables = (graph: { edges: Edge[] } | undefined): Edge[] => graph?.edges.filter((e) => e.kind === "modulation") ?? [];

  test("resolves true when the lane has none, with its effect and its modulator and their cables, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.clearInstrument(oscillator.id)).toBe(true);
    const graph = client.state.snapshot?.graph;
    const lane = graph?.nodes.find((n) => n.id === oscillator.id);
    expect(lane?.name).toBe("(empty)");
    expect(lane?.kind === "instrument" ? lane.plugin : "no lane").toBeNull();
    expect(graph?.nodes.map((n) => n.name).filter((name) => name !== "(empty)").sort()).toEqual(["Filter", "LFO", "Master"]);
    expect(cables(graph), "the cables of the modulator are still on the effect").toEqual(cables(tracerSnapshot.graph));
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a lane that has none, a node that is no lane and one that is not there: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.clearInstrument(filter.id)).toBe(false);
    expect(await editor.clearInstrument(MASTER)).toBe(false);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    expect(await editor.clearInstrument(oscillator.id)).toBe(true);
    expect(await editor.clearInstrument(oscillator.id)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not take Filter out of its lane: an instrument is taken out of a lane",
      "Could not take Master out of its lane: there is no node 4294967295",
      // (The lane has nothing but its filter and its LFO now, so the canvas draws no card for it and calls it by its filter.)
      "Could not take Filter out of its lane: the lane has no instrument to take out",
    ]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.clearInstrument(oscillator.id)).toBe(false);
    expect(said(notices)).toEqual(["Could not take Oscillator out of its lane: Not connected to the server."]);
  });

  test("has the words that a request that is refused with nothing more known of it has", () => {
    expect(refusal({ type: "clearInstrument", node: oscillator.id }, new Error("the lane has no instrument to take out"))).toBe(
      "Could not take an instrument out of its lane: the lane has no instrument to take out",
    );
  });
});

describe("moving an effect", () => {
  // The session of the moves fixture: a group (0) with four bus effects (1 to 4), a lane in it (5) with four effects (6 to 9), and a lane
  // in no group (11) with three (12 to 14). The names the canvas gives are numbered across the session in the order of its graph: the bus's
  // are Filter 1, Reverb 1, Filter 2 and Reverb 2, the first lane's Filter 3, Reverb 3, Filter 4 and Reverb 4.
  const chain = (client: Client, parent: number): number[] =>
    (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "effect" && n.parent === parent).map((n) => n.id);

  test("resolves true when the effect is in its new place in its lane's chain, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    expect(await editor.moveEffect(6, 2)).toBe(true);
    expect(chain(client, 5)).toEqual([7, 8, 6, 9]);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    // The first place: the effects it passes move the other way.
    expect(await editor.moveEffect(9, 0)).toBe(true);
    expect(chain(client, 5)).toEqual([9, 7, 8, 6]);
    expect(said(notices)).toEqual([]);
  });

  test("moves a bus effect in its group's bus", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    expect(await editor.moveEffect(1, 3)).toBe(true);
    expect(chain(client, 0)).toEqual([2, 3, 4, 1]);
    expect(chain(client, 5)).toEqual([6, 7, 8, 9]);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for what is no effect, a place the chain has not and the place the effect is at: it resolves false, and the page says what it could not do and why, with the place counted from 1", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    // (The notices keep the last few: each is read as it is said.)
    for (const [node, index, words] of [
      [5, 0, "Could not move Oscillator 1 to place 1 of its chain: only an effect can be moved"],
      [6, 4, "Could not move Filter 3 to place 5 of its chain: the chain has no place 4"],
      [6, 0, "Could not move Filter 3 to place 1 of its chain: that effect is at that place already"],
      [MASTER, 1, "Could not move Master to place 2 of its chain: there is no node 4294967295"],
      [9999, 1, "Could not move that node to place 2 of its chain: there is no node 9999"],
    ] as const) {
      expect(await editor.moveEffect(node, index)).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(movesFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open({ snapshot: movesFixture.snapshot });
    client.stop();
    expect(await editor.moveEffect(6, 2)).toBe(false);
    expect(said(notices)).toEqual(["Could not move Filter 3 to place 3 of its chain: Not connected to the server."]);
  });
});

describe("loading a preset", () => {
  /** The tracer's reverb on a lane that has one (an effect with presets: the reach fixture's reverb of lane 7, in a group). */
  const LANE_REVERB = 8;

  test("resolves true when the plugin has the preset, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open({ snapshot: reachFixture.snapshot });
    expect(await editor.loadPreset(LANE_REVERB, "cathedral")).toBe(true);
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === LANE_REVERB);
    expect(node?.kind === "effect" && node.plugin.currentPreset).toBe("Cathedral");
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a preset the plugin does not have, a bus effect and a node that has no plugin: it resolves false, and the page says what it could not do and why, by the preset's name", async () => {
    const { client, notices, editor } = await open({ snapshot: reachFixture.snapshot });
    expect(await editor.loadPreset(LANE_REVERB, "no-such-preset")).toBe(false);
    expect(await editor.loadPreset(3, "cathedral")).toBe(false); // group 2's bus effect
    expect(await editor.loadPreset(0, "cathedral")).toBe(false); // a group
    expect(said(notices)).toEqual([
      "Could not load no-such-preset into Reverb 2: the plugin has no preset no-such-preset",
      "Could not load Cathedral into Reverb 1: a bus effect has no presets to load",
      "Could not load cathedral into Group 1: that node has no plugin",
    ]);
    expect(client.state.snapshot).toEqual(reachFixture.snapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open({ snapshot: reachFixture.snapshot });
    client.stop();
    expect(await editor.loadPreset(LANE_REVERB, "plate")).toBe(false);
    expect(said(notices)).toEqual(["Could not load Vintage Plate into Reverb 2: Not connected to the server."]);
  });
});

describe("opening a plugin's own window (step 112)", () => {
  /** The tracer, with the oscillator's plugin saying that it has a window of its own (the filter's does not). */
  const windowed = (): Snapshot => {
    const copy = structuredClone(tracerSnapshot);
    const found = copy.graph.nodes.find((n) => n.id === oscillator.id);
    if (found?.kind !== "instrument" || !found.plugin) throw new Error("the oscillator has a plugin");
    found.plugin.hasEditor = true;
    return copy;
  };

  test("resolves true for a plugin that has one, on a machine that can show it: the session is as it was but for its revision, the page says nothing and the history has no step", async () => {
    const snapshot = windowed();
    const { client, notices, editor } = await open({ snapshot, windows: true });
    expect(await editor.openEditor(oscillator.id)).toBe(true);
    expect(said(notices)).toEqual([]);
    expect(client.state.snapshot?.graph).toEqual(snapshot.graph);
    expect(client.state.snapshot?.rev).toBe(snapshot.rev + 1);
    expect(client.state.snapshot?.meta.dirty).toBe(snapshot.meta.dirty);
    expect(client.state.history).toEqual({ undo: null, redo: null });
  });

  test("is refused, in the controller's words, for a plugin that has no window, a machine that has no screen, a lane that has no plugin, a node that has none and one that is not there", async () => {
    const withWindow = await open({ snapshot: windowed(), windows: true });
    expect(await withWindow.editor.openEditor(filter.id)).toBe(false);
    expect(await withWindow.editor.openEditor(lfo.id)).toBe(false);
    expect(await withWindow.editor.openEditor(9999)).toBe(false);
    expect(said(withWindow.notices)).toEqual([
      "Could not open the window of Filter: Filter has no window of its own",
      "Could not open the window of LFO: that node has no plugin",
      "Could not open the window of that node: there is no node 9999",
    ]);
    const noScreen = await open({ snapshot: windowed() });
    expect(await noScreen.editor.openEditor(oscillator.id)).toBe(false);
    expect(said(noScreen.notices)).toEqual(["Could not open the window of Oscillator: this session has no screen to open a window on (a login over ssh, a container, CI)"]);
    expect(noScreen.client.state.snapshot?.rev, "a refusal uses no revision").toBe(windowed().rev);
  });

  test("is refused for the master, which is not a node that a plugin is in, and for a lane that has no plugin", async () => {
    const empty = windowed();
    const lane = empty.graph.nodes.find((n) => n.id === oscillator.id);
    if (lane?.kind !== "instrument") throw new Error("the oscillator is a lane");
    lane.plugin = null;
    const { editor, notices } = await open({ snapshot: empty, windows: true });
    expect(await editor.openEditor(MASTER)).toBe(false);
    expect(await editor.openEditor(oscillator.id)).toBe(false);
    // (A lane that has no instrument is called after what it has left: its filter.)
    expect(said(notices)).toEqual([`Could not open the window of Master: there is no node ${String(MASTER)}`, "Could not open the window of Filter: that instrument has no plugin"]);
  });

  test("is told to every client as the event that says which node's window opened", async () => {
    const server = new MockServer({ snapshot: windowed(), windows: true });
    const heard: { type: string; event?: unknown }[] = [];
    const client = new Client({
      transport: () => {
        const inner = server.transport();
        return {
          connect: (events) => {
            inner.connect({
              ...events,
              text: (text) => {
                heard.push(JSON.parse(text) as { type: string; event?: unknown });
                events.text(text);
              },
            });
          },
          send: (text) => {
            inner.send(text);
          },
          close: () => {
            inner.close();
          },
        };
      },
      reconnectDelay: () => 5,
    });
    clients.push(client);
    client.start();
    await until(() => client.state.status === "open", "the session");
    expect(await new Editor(client, new Notices(), new Reveals()).openEditor(oscillator.id)).toBe(true);
    expect(heard.filter((message) => message.type === "event").map((message) => message.event)).toEqual([{ rev: windowed().rev + 1, type: "editorOpened", node: oscillator.id }]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open({ snapshot: windowed(), windows: true });
    client.stop();
    expect(await editor.openEditor(oscillator.id)).toBe(false);
    expect(said(notices)).toEqual(["Could not open the window of Oscillator: Not connected to the server."]);
  });
});

describe("setting the keys of a lane", () => {
  const keys = (client: Client, node: number = oscillator.id) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" ? found.range : "not a lane";
  };

  test("resolves true when the lane has the keys, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setRange(oscillator.id, { low: 36, high: 84 })).toBe(true);
    expect(keys(client)).toEqual({ low: 36, high: 84 });
    expect(await editor.setRange(oscillator.id, null)).toBe(true);
    expect(keys(client)).toBeNull();
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a range that is not one and for a node that is not a lane: it resolves false, and the page says what it could not do and why, with the keys written as the TUI writes them", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setRange(oscillator.id, { low: 90, high: 40 })).toBe(false);
    expect(await editor.setRange(filter.id, { low: 0, high: 59 })).toBe(false);
    expect(await editor.setRange(lfo.id, null)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not set the keys of Oscillator to F#6-E2: the range goes from F#6 down to E2: its low note has to be the lower",
      "Could not set the keys of Filter to -B3: a key range belongs to a lane",
      "Could not set the keys of LFO to all keys: a key range belongs to a lane",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setRange(oscillator.id, { low: 60, high: 127 })).toBe(false);
    expect(said(notices)).toEqual(["Could not set the keys of Oscillator to C4-: Not connected to the server."]);
  });
});

describe("muting and soloing a lane or a group", () => {
  const flags = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "group" ? { muted: found.muted, solo: found.solo } : "neither";
  };

  test("resolves true when the lane has the flag, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setMute(oscillator.id, true)).toBe(true);
    expect(flags(client, oscillator.id)).toEqual({ muted: true, solo: false });
    expect(await editor.setSolo(oscillator.id, true)).toBe(true);
    expect(flags(client, oscillator.id)).toEqual({ muted: true, solo: true });
    expect(await editor.setMute(oscillator.id, false)).toBe(true);
    expect(await editor.setSolo(oscillator.id, false)).toBe(true);
    expect(flags(client, oscillator.id)).toEqual({ muted: false, solo: false });
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is neither: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setMute(filter.id, true)).toBe(false);
    expect(await editor.setSolo(lfo.id, true)).toBe(false);
    expect(await editor.setMute(filter.id, false)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not mute Filter: only a lane or a group can be muted",
      "Could not solo LFO: only a lane or a group can be soloed",
      "Could not unmute Filter: only a lane or a group can be muted",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setMute(oscillator.id, true)).toBe(false);
    expect(await editor.setSolo(oscillator.id, true)).toBe(false);
    expect(said(notices)).toEqual(["Could not mute Oscillator: Not connected to the server.", "Could not solo Oscillator: Not connected to the server."]);
  });
});

describe("cutting and giving back the way out of a lane or a group", () => {
  const routeOf = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "group" ? found.route : "neither";
  };
  /** Whether the audio path has a link out of the lane or group `node`'s chain into the master, which a cut way out takes away. */
  const linkToMaster = (client: Client): boolean => client.state.snapshot?.graph.edges.some((e) => (e.kind === "audio" || e.kind === "member") && e.to.node === MASTER) ?? false;

  test("resolves true when the lane has the way out, which the session shows with the links of the audio path, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(routeOf(client, oscillator.id)).toBe("out");
    expect(linkToMaster(client)).toBe(true);
    expect(await editor.setRoute(oscillator.id, "none")).toBe(true);
    expect(routeOf(client, oscillator.id)).toBe("none");
    expect(linkToMaster(client), "the link into the master is gone").toBe(false);
    expect(await editor.setRoute(oscillator.id, "out")).toBe(true);
    expect(routeOf(client, oscillator.id)).toBe("out");
    expect(client.state.snapshot?.graph, "the way out given back is the graph that there was").toEqual(tracerSnapshot.graph);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is neither a lane nor a group: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setRoute(filter.id, "none")).toBe(false);
    expect(await editor.setRoute(lfo.id, "out")).toBe(false);
    expect(said(notices)).toEqual([
      "Could not cut the way out of Filter: only a lane or a group has a way out",
      "Could not give LFO its way out: only a lane or a group has a way out",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setRoute(oscillator.id, "none")).toBe(false);
    expect(said(notices)).toEqual(["Could not cut the way out of Oscillator: Not connected to the server."]);
  });
});

describe("cutting and giving back the link into an effect", () => {
  const feedOf = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "effect" ? found.feed : "no effect";
  };
  /** The links of the audio path that end at `node`, and that start at it. */
  const linksInto = (client: Client, node: number): number => client.state.snapshot?.graph.edges.filter((e) => (e.kind === "audio" || e.kind === "member") && e.to.node === node).length ?? 0;
  const linksFrom = (client: Client, node: number): number => client.state.snapshot?.graph.edges.filter((e) => (e.kind === "audio" || e.kind === "member") && e.from.node === node).length ?? 0;

  test("resolves true when the effect has the feed, which the session shows with the links of the audio path, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(feedOf(client, filter.id)).toBe("chain");
    expect([linksInto(client, filter.id), linksFrom(client, filter.id)]).toEqual([1, 1]);
    expect(await editor.setFeed(filter.id, "none")).toBe(true);
    expect(feedOf(client, filter.id)).toBe("none");
    expect([linksInto(client, filter.id), linksFrom(client, filter.id)], "the link into the filter is gone, and the one out of it is not").toEqual([0, 1]);
    expect(await editor.setFeed(filter.id, "chain")).toBe(true);
    expect(feedOf(client, filter.id)).toBe("chain");
    expect(client.state.snapshot?.graph, "the feed given back is the graph that there was").toEqual(tracerSnapshot.graph);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is no effect: it resolves false, and the page says what it could not do and why, in words of the effect's own", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setFeed(oscillator.id, "none")).toBe(false);
    expect(await editor.setFeed(lfo.id, "chain")).toBe(false);
    expect(await editor.setFeed(MASTER, "none")).toBe(false);
    expect(said(notices)).toEqual([
      "Could not cut the input of Oscillator: only an effect has a feed",
      "Could not give LFO its input back: only an effect has a feed",
      "Could not cut the input of Master: there is no node 4294967295",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("is refused where the link would be given back to make the audio go round, by the cables there are, and the page says so", async () => {
    const { client, notices, editor } = await open();
    const delay = await editor.addEffect(oscillator.id, "builtin:delay");
    expect(delay).not.toBeNull();
    if (delay === null) return;
    // The delay is after the filter: a cable back into the filter closes a loop through the link between them, unless the delay is not fed by it.
    expect(await editor.addAudioCable(delay, filter.id)).toBe(false);
    expect(await editor.setFeed(delay, "none")).toBe(true);
    expect(await editor.addAudioCable(delay, filter.id)).toBe(true);
    expect(await editor.setFeed(delay, "chain")).toBe(false);
    expect(feedOf(client, delay)).toBe("none");
    expect(said(notices)).toEqual([
      "Could not draw an audio cable from Delay to Filter: that cable would make the audio go round: it would feed what it comes from",
      "Could not give Delay its input back: giving that effect its input back would make the audio go round, by the cables there are",
    ]);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setFeed(filter.id, "none")).toBe(false);
    expect(said(notices)).toEqual(["Could not cut the input of Filter: Not connected to the server."]);
  });

  test("has the words that a request that is refused with nothing more known of it has", () => {
    expect(refusal({ type: "setFeed", node: filter.id, feed: "none" }, new Error("only an effect has a feed"))).toBe("Could not cut or give back the link into an effect: only an effect has a feed");
  });
});

describe("moving a node", () => {
  const placeOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.ui;

  test("resolves true when the node has the place, which the session shows, and says nothing; none gives it back to the layout", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.moveNode(filter.id, { x: 376, y: -40 })).toBe(true);
    expect(placeOf(client, filter.id)).toEqual({ x: 376, y: -40 });
    expect(await editor.moveNode(MASTER, { x: 900, y: 0 })).toBe(true);
    expect(placeOf(client, MASTER)).toEqual({ x: 900, y: 0 });
    expect(await editor.moveNode(filter.id, null)).toBe(true);
    expect(placeOf(client, filter.id)).toBeUndefined();
    expect(said(notices)).toEqual([]);
  });

  test("is refused for a node that is not there: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.moveNode(424242, { x: 1, y: 1 })).toBe(false);
    expect(await editor.moveNode(424242, null)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not move that node: there is no node 424242",
      "Could not give that node back to the layout: there is no node 424242",
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.moveNode(lfo.id, { x: 1, y: 2 })).toBe(false);
    expect(said(notices)).toEqual(["Could not move LFO: Not connected to the server."]);
  });
});

describe("pinning parameters to a node", () => {
  const pinnedOf = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "effect" ? found.plugin?.pinned : undefined;
  };

  test("resolves true when the plugin has them pinned, in the order given, which the session shows, and says nothing; none gives the node back to the page's choice", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setPinned(filter.id, [param(filter, "resonance"), param(filter, "cutoff")])).toBe(true);
    expect(pinnedOf(client, filter.id)).toEqual([param(filter, "resonance"), param(filter, "cutoff")]);
    expect(await editor.setPinned(oscillator.id, [param(oscillator, "detune")])).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([param(oscillator, "detune")]);
    expect(await editor.setPinned(filter.id, [])).toBe(true);
    expect(pinnedOf(client, filter.id)).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("is refused as the controller refuses it: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    // (Each is read as it is said: the page keeps the last few notices only.)
    const refusals: [number, number[], string][] = [
      [424242, [], "Could not choose the knobs of that node: there is no node 424242"],
      [lfo.id, [0], "Could not choose the knobs of LFO: that node has no plugin"],
      [filter.id, [0, 0], "Could not choose the knobs of Filter: parameter 0 is listed twice"],
      [filter.id, [4000], "Could not choose the knobs of Filter: the plugin has no parameter 4000"],
    ];
    for (const [node, params, notice] of refusals) {
      expect(await editor.setPinned(node, params), notice).toBe(false);
      expect(said(notices).at(-1)).toBe(notice);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setPinned(filter.id, [0])).toBe(false);
    expect(said(notices)).toEqual(["Could not choose the knobs of Filter: Not connected to the server."]);
  });
});

describe("showing a parameter as a knob on its node, or not", () => {
  /** The tracer with eight more parameters (indices 100 to 107) on the oscillator, which has eight: so that there are some the node does not show. */
  const wide = () => {
    const nodes = tracerSnapshot.graph.nodes.map((node) => {
      if (node.id !== oscillator.id || node.kind !== "instrument" || !node.plugin) return node;
      const more = Array.from({ length: 8 }, (_, i) => ({ ...(node.plugin?.params[0] ?? ({} as never)), index: 100 + i, name: `extra ${String(i)}` }));
      return { ...node, plugin: { ...node.plugin, params: [...node.plugin.params, ...more] } };
    });
    return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } };
  };
  const first = Array.from({ length: 8 }, (_, i) => i);
  const pinnedOf = (client: Client, node: number) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "effect" ? found.plugin?.pinned : undefined;
  };

  test("pins one that is not shown after what is shown: the first eight stay, and the next is after them", async () => {
    const { client, notices, editor } = await open({ snapshot: wide() });
    expect(pinnedOf(client, oscillator.id)).toEqual([]);
    expect(await editor.pinParam(oscillator.id, 100, true)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([...first, 100]);
    expect(await editor.pinParam(oscillator.id, 105, true)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([...first, 100, 105]);
    expect(said(notices)).toEqual([]);
  });

  test("takes one off what is shown: with none pinned that is the first eight less that one, and the last one off gives the node back to the first eight", async () => {
    const { client, editor } = await open({ snapshot: wide() });
    expect(await editor.pinParam(oscillator.id, 3, false)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual(first.filter((i) => i !== 3));
    await editor.setPinned(oscillator.id, [101]);
    expect(await editor.pinParam(oscillator.id, 101, false)).toBe(true);
    expect(pinnedOf(client, oscillator.id)).toEqual([]);
  });

  test("resolves true and asks nothing when it is so already", async () => {
    const { client, notices, editor } = await open({ snapshot: wide() });
    const before = client.state.snapshot?.rev;
    expect(await editor.pinParam(oscillator.id, 2, true)).toBe(true); // shown already
    expect(await editor.pinParam(oscillator.id, 104, false)).toBe(true); // not shown
    expect(await editor.pinParam(oscillator.id, 999, false)).toBe(true); // not there
    expect(client.state.snapshot?.rev).toBe(before);
    expect(said(notices)).toEqual([]);
  });

  test("says what it could not do, and why, for a parameter the plugin has not, a node with no plugin and a node that is not there", async () => {
    const { client, notices, editor } = await open({ snapshot: wide() });
    const refusals: [number, number, string][] = [
      [oscillator.id, 999, "Could not pin parameter 999 to Oscillator: the plugin has no parameter 999"],
      [lfo.id, 0, "Could not pin parameter 0 to LFO: that node has no plugin"],
      [424242, 0, "Could not pin parameter 0 to that node: there is no node 424242"],
    ];
    for (const [node, index, notice] of refusals) {
      expect(await editor.pinParam(node, index, true), notice).toBe(false);
      expect(said(notices).at(-1)).toBe(notice);
    }
    expect(client.state.snapshot).toEqual(wide());
  });

  test("names the parameter, and where it is pinned to or taken off, in what it says", async () => {
    const { notices, editor, client } = await open({ snapshot: wide() });
    client.stop();
    expect(await editor.pinParam(oscillator.id, 101, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not pin extra 1 to Oscillator: Not connected to the server.");
    expect(await editor.pinParam(oscillator.id, 3, false)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not unpin attack from Oscillator: Not connected to the server.");
  });

  test("says a node can show only so many, without asking the server, when it shows that many", async () => {
    const huge = (() => {
      const base = wide();
      const nodes = base.graph.nodes.map((node) => {
        if (node.id !== oscillator.id || node.kind !== "instrument" || !node.plugin) return node;
        const params = Array.from({ length: 70 }, (_, i) => ({ ...(node.plugin?.params[0] ?? ({} as never)), index: i, name: `p${String(i)}` }));
        return { ...node, plugin: { ...node.plugin, params, pinned: Array.from({ length: 64 }, (_, i) => i) } };
      });
      return { ...base, graph: { ...base.graph, nodes } };
    })();
    const { client, notices, editor } = await open({ snapshot: huge });
    const before = client.state.snapshot?.rev;
    expect(await editor.pinParam(oscillator.id, 65, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not pin p65 to Oscillator: a node can show at most 64 parameters");
    expect(client.state.snapshot?.rev).toBe(before);
  });
});

describe("arranging the nodes again", () => {
  const placeOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.ui;

  test("gives every node that has a place back to the layout, the master too, and says nothing", async () => {
    const { client, notices, editor } = await open();
    await editor.moveNode(filter.id, { x: 1, y: 2 });
    await editor.moveNode(MASTER, { x: 3, y: 4 });
    await editor.moveNode(lfo.id, { x: 5, y: 6 });
    expect(await editor.resetLayout()).toBe(true);
    for (const node of client.state.snapshot?.graph.nodes ?? []) expect(placeOf(client, node.id), `node ${String(node.id)}`).toBeUndefined();
    expect(said(notices)).toEqual([]);
  });

  test("leaves the nodes that have no place alone: a request for each that has one, and none when there is none", async () => {
    const { server, editor } = await open();
    const sent = (): number => server.received.filter((message) => message.type === "request").length;
    expect(await editor.resetLayout()).toBe(true);
    expect(sent()).toBe(0);
    await editor.moveNode(filter.id, { x: 1, y: 2 });
    expect(sent()).toBe(1);
    await editor.resetLayout();
    expect(sent()).toBe(2);
    expect(server.received.at(-1)).toMatchObject({ type: "request", intent: { type: "setNodeUi", node: filter.id, ui: null } });
  });

  test("with no connection says so, for each node it could not give back (it goes on with the rest), and resolves false", async () => {
    const { client, notices, editor } = await open();
    await editor.moveNode(filter.id, { x: 1, y: 2 });
    await editor.moveNode(lfo.id, { x: 3, y: 4 });
    client.stop();
    expect(await editor.resetLayout()).toBe(false);
    expect(said(notices)).toEqual([
      "Could not give Filter back to the layout: Not connected to the server.",
      "Could not give LFO back to the layout: Not connected to the server.",
    ]);
  });
});

describe("saving the session under another name", () => {
  test("resolves true when the session has that file, which the session shows with nothing unsaved, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(client.state.snapshot?.meta.file).toBe("tracer.toml");
    expect(await editor.saveAs("copy")).toBe(true);
    expect(client.state.snapshot?.meta).toMatchObject({ file: "copy.toml", dirty: false });
    expect(said(notices)).toEqual([]);
  });

  test("asks for the name as it was typed, and says the name without the spaces round it", async () => {
    const { server, editor, notices } = await open();
    expect(await editor.saveAs("  ../x ")).toBe(false);
    expect(server.received.at(-1)).toMatchObject({ type: "request", intent: { type: "saveAs", name: "  ../x " } });
    expect(said(notices)).toEqual(['Could not save the session as "../x": a file name has no folder in it: `/` and `\\` are not allowed']);
  });

  test("says the name it was refused for in quotes and cut after 24 characters, as a group's name is, whatever else the name has", async () => {
    const { notices, editor } = await open();
    const folder = "a file name has no folder in it: `/` and `\\` are not allowed";
    const control = "a file name cannot have `:`, `*`, `?`, `\"`, `<`, `>`, `|` or a control character";
    // (The notices keep the last few: each is read as it is said. Exactly as many characters as it quotes is not cut, and one more is.)
    const cases: [string, string][] = [
      ["😀".repeat(200), `Could not save the session as "${"😀".repeat(24)}…": a file name is at most 255 bytes long`],
      ["two\nlines", `Could not save the session as "two\\nlines": ${control}`],
      ["a/".repeat(12), `Could not save the session as "${"a/".repeat(12)}": ${folder}`],
      [`${"a/".repeat(12)}a`, `Could not save the session as "${"a/".repeat(12)}…": ${folder}`],
    ];
    for (const [name, words] of cases) {
      expect(await editor.saveAs(name), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("is refused as the controller refuses it: false, the page says what it could not do and why, and nothing changes", async () => {
    const { client, notices, editor } = await open({ files: ["other.toml"] });
    expect(await editor.saveAs("other")).toBe(false);
    expect(await editor.saveAs("")).toBe(false);
    expect(await editor.saveAs("con")).toBe(false);
    expect(said(notices)).toEqual([
      'Could not save the session as "other": a file called other.toml is there already',
      'Could not save the session as "": a file name is needed',
      'Could not save the session as "con": con is a name that Windows keeps for itself',
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.saveAs("copy")).toBe(false);
    expect(said(notices)).toEqual(['Could not save the session as "copy": Not connected to the server.']);
  });
});

describe("saving a chain of effects as a module", () => {
  test("resolves true when the module is in the library, which the catalog then lists, says so, and changes nothing of the session: nothing is unsaved because of it, and there is nothing to undo", async () => {
    const { server, client, notices, editor } = await open();
    expect(await editor.saveModule(oscillator.id, "  Tracer chain  ")).toBe(true);
    expect(server.received.at(-1)).toMatchObject({ type: "request", intent: { type: "saveModule", node: oscillator.id, name: "  Tracer chain  " } });
    expect(said(notices)).toEqual(['Saved the module "Tracer chain": + Module adds it']);
    await until(() => client.state.catalog?.entries.some((entry) => entry.id === "module:Tracer chain") === true, "the catalog with the module");
    expect(client.state.catalog?.entries.find((entry) => entry.id === "module:Tracer chain")).toEqual({ id: "module:Tracer chain", name: "Tracer chain", format: "Module", isInstrument: false, params: 1, presets: 0 });
    expect(client.state.snapshot?.graph).toEqual(tracerSnapshot.graph);
    expect(client.state.snapshot?.meta.dirty).toBe(false);
    expect(client.state.history).toEqual({ undo: null, redo: null });
  });

  test("is the chain of the lane whether the node is the lane or an effect of it", async () => {
    const { client, editor } = await open();
    expect(await editor.saveModule(filter.id, "Chain")).toBe(true);
    await until(() => client.state.catalog?.entries.some((entry) => entry.id === "module:Chain") === true, "the catalog with the module");
  });

  test("is refused as the controller refuses it: false, and the page says what it could not do, and why, and nothing changes", async () => {
    const { client, notices, editor } = await open();
    // (The notices keep the last few: each is read as it is said.)
    const cases: [number, string, string][] = [
      [oscillator.id, "", 'Could not save the chain of Oscillator as the module "": a file name is needed'],
      [oscillator.id, "a/b", 'Could not save the chain of Oscillator as the module "a/b": a file name has no folder in it: `/` and `\\` are not allowed'],
      [oscillator.id, "  a/b ", 'Could not save the chain of Oscillator as the module "a/b": a file name has no folder in it: `/` and `\\` are not allowed'],
      [lfo.id, "Room", 'Could not save the chain of LFO as the module "Room": a module is made of the chain of a lane'],
      [9_999, "Room", 'Could not save the chain of that node as the module "Room": there is no node 9999'],
    ];
    for (const [node, name, words] of cases) {
      expect(await editor.saveModule(node, name), words).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    expect(client.state.catalog?.entries.some((entry) => entry.format === "Module")).toBe(false);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.saveModule(oscillator.id, "Room")).toBe(false);
    expect(said(notices)).toEqual(['Could not save the chain of Oscillator as the module "Room": Not connected to the server.']);
  });
});

describe("setting the scale of the piano", () => {
  const piano = (client: Client) => client.state.snapshot?.meta.piano;

  test("resolves true when the piano has the scale, which the session shows, and says nothing", async () => {
    const { server, client, notices, editor } = await open();
    expect(await editor.setScale("F# dorian")).toBe(true);
    // (The words are the server's to read: they go as they were given.)
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "setScale", scale: "F# dorian" }]);
    expect(piano(client)).toEqual({ scale: "F# dorian", root: 6, kind: "dorian", notes: [1, 3, 4, 6, 8, 9, 11], locked: false });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("is refused for what is not a scale: it resolves false, and the page says what it could not do and why", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setScale("banana")).toBe(false);
    expect(said(notices)).toEqual(['Could not set the scale of the piano to banana: "banana" is not a scale: a root and a kind of scale, like C major or F# dorian']);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setScale("A minor")).toBe(false);
    expect(said(notices)).toEqual(["Could not set the scale of the piano to A minor: Not connected to the server."]);
  });
});

describe("setting the tempo", () => {
  test("resolves true when the tempo is set, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(client.state.snapshot?.meta.bpm).toBe(120);
    expect(await editor.setBpm(96.5)).toBe(true);
    expect(client.state.snapshot?.meta.bpm).toBe(96.5);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(said(notices)).toEqual([]);
  });

  test("holds a tempo to the tempos there can be, which the session shows", async () => {
    const { client, editor } = await open();
    expect(await editor.setBpm(1000)).toBe(true);
    expect(client.state.snapshot?.meta.bpm).toBe(300);
    expect(await editor.setBpm(5)).toBe(true);
    expect(client.state.snapshot?.meta.bpm).toBe(20);
  });

  test("with no connection says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setBpm(96.5)).toBe(false);
    expect(said(notices)).toEqual(["Could not set the tempo to 96.5: Not connected to the server."]);
  });
});

describe("the pattern of a lane", () => {
  const patternOf = (client: Client, node: number = oscillator.id) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" ? found.pattern : "not a lane";
  };
  /** The pattern a lane is given when it has none and something is asked of it: four beats, looping, not in key, not heard, at the tempo of the session. */
  const empty = { recording: false, enabled: false, looping: true, inKey: false, lengthBeats: 4, bpm: 120, notes: 0 };

  test("a recording is started and stopped: the session shows the pattern of the lane, recording and then not, and nothing is said", async () => {
    const { client, notices, editor } = await open();
    expect(patternOf(client)).toBeNull();
    expect(await editor.setRecording(oscillator.id, true)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, recording: true });
    expect(await editor.setRecording(oscillator.id, false)).toBe(true);
    expect(patternOf(client)).toEqual(empty);
    expect(said(notices)).toEqual([]);
  });

  test("a setting is set, held to its range and a switch rounded, and a lane that has no pattern is given an empty one with it", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.setPatternField(oscillator.id, "length", 8)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, lengthBeats: 8 });
    expect(await editor.setPatternField(oscillator.id, "length", 100)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, lengthBeats: 32 });
    expect(await editor.setPatternField(oscillator.id, "enabled", 1)).toBe(true);
    expect(await editor.setPatternField(oscillator.id, "looping", 0.4)).toBe(true);
    expect(await editor.setPatternField(oscillator.id, "transpose", 0.6)).toBe(true);
    expect(patternOf(client)).toEqual({ ...empty, lengthBeats: 32, enabled: true, looping: false, inKey: true });
    expect(said(notices)).toEqual([]);
  });

  test("a pattern is cleared, and what has none cannot be cleared: it resolves false, and the page says so", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.clearPattern(oscillator.id)).toBe(false);
    expect(await editor.setPatternField(oscillator.id, "enabled", 1)).toBe(true);
    expect(await editor.clearPattern(oscillator.id)).toBe(true);
    expect(patternOf(client)).toBeNull();
    expect(said(notices)).toEqual(["Could not clear the pattern of Oscillator: that lane has no pattern"]);
  });

  /** What the page says of each thing that is asked and refused, in turn (it keeps three notices at a time, so each is read as it comes). */
  const refusals = async (notices: Notices, asks: readonly (() => Promise<boolean>)[]): Promise<string[]> => {
    const words: string[] = [];
    const stop = notices.subscribe(() => {
      words.push(said(notices).at(-1) ?? "nothing was said");
    });
    for (const ask of asks) expect(await ask()).toBe(false);
    stop();
    return words;
  };

  test("is refused for a node that is not a lane, in the words of what it was to do, and the session is as it was", async () => {
    const { client, notices, editor } = await open();
    expect(
      await refusals(notices, [
        () => editor.setRecording(filter.id, true),
        () => editor.setRecording(lfo.id, false),
        () => editor.clearPattern(filter.id),
        () => editor.setPatternField(lfo.id, "length", 2),
        () => editor.setPatternField(MASTER, "enabled", 1),
      ]),
    ).toEqual([
      "Could not start recording a pattern on Filter: a pattern belongs to a lane",
      "Could not stop recording the pattern of LFO: a pattern belongs to a lane",
      "Could not clear the pattern of Filter: a pattern belongs to a lane",
      "Could not set the length of the pattern of LFO to 2 beats: a pattern belongs to a lane",
      `Could not turn the pattern of Master on: there is no node ${String(MASTER)}`,
    ]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("with no connection says so, in the words of each setting it was to change", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    const why = "Not connected to the server.";
    expect(
      await refusals(notices, [
        () => editor.setRecording(oscillator.id, true),
        () => editor.setRecording(oscillator.id, false),
        () => editor.clearPattern(oscillator.id),
        () => editor.setPatternField(oscillator.id, "length", 2.5),
        () => editor.setPatternField(oscillator.id, "enabled", 1),
        () => editor.setPatternField(oscillator.id, "enabled", 0),
        () => editor.setPatternField(oscillator.id, "looping", 1),
        () => editor.setPatternField(oscillator.id, "looping", 0),
        () => editor.setPatternField(oscillator.id, "transpose", 1),
        () => editor.setPatternField(oscillator.id, "transpose", 0),
      ]),
    ).toEqual([
      `Could not start recording a pattern on Oscillator: ${why}`,
      `Could not stop recording the pattern of Oscillator: ${why}`,
      `Could not clear the pattern of Oscillator: ${why}`,
      `Could not set the length of the pattern of Oscillator to 2.5 beats: ${why}`,
      `Could not turn the pattern of Oscillator on: ${why}`,
      `Could not turn the pattern of Oscillator off: ${why}`,
      `Could not make the pattern of Oscillator loop: ${why}`,
      `Could not make the pattern of Oscillator play once: ${why}`,
      `Could not transpose the pattern of Oscillator by the degrees of the scale: ${why}`,
      `Could not transpose the pattern of Oscillator by semitones: ${why}`,
    ]);
  });
});

describe("locking the piano to its scale", () => {
  test("resolves true when the piano is locked and when it is let go, which the session shows, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(tracerSnapshot.meta.piano.locked).toBe(false);
    expect(await editor.setLocked(true)).toBe(true);
    expect(client.state.snapshot?.meta.piano).toEqual({ ...tracerSnapshot.meta.piano, locked: true });
    expect(await editor.setLocked(false)).toBe(true);
    expect(client.state.snapshot?.meta.piano).toEqual(tracerSnapshot.meta.piano);
    expect(said(notices)).toEqual([]);
  });

  test("with no connection says so, in the words of what it was to do", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.setLocked(true)).toBe(false);
    expect(await editor.setLocked(false)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not lock the piano to its scale: Not connected to the server.",
      "Could not let the piano play every note: Not connected to the server.",
    ]);
  });
});

describe("showing what was added", () => {
  test("is asked for each node the page added, in the order they were, once the session has it", async () => {
    const { editor, shown } = await open();
    const lfo = await editor.addFreeModulator("lfo", oscillator.id);
    const effect = await editor.addEffect(oscillator.id, "builtin:reverb");
    const lane = await editor.addInstrument("builtin:osc");
    expect(shown).toEqual([lfo, effect, lane]);
    expect(shown).toEqual([3, 4, 5]);
  });

  test("is asked for the effect that stands by itself, and not for the lane that holds it, which has no card on the canvas", async () => {
    const { editor, shown } = await open();
    const effect = await editor.addFreeEffect("builtin:reverb");
    expect(shown).toEqual([effect]);
    expect(shown).toEqual([4]);
  });

  test("is not asked for what was refused, nor for what was not added but changed or taken out, nor for a cable", async () => {
    const { editor, shown } = await open();
    expect(await editor.addEffect(filter.id, "builtin:reverb")).toBeNull();
    expect(await editor.addInstrument("builtin:reverb")).toBeNull();
    expect(await editor.replaceInstrument(oscillator.id, "builtin:osc")).toBe(true);
    expect(await editor.addCable(lfo.id, oscillator.id, paramPort(oscillator, "detune"))).toBe(true);
    expect(await editor.removeNode(lfo.id)).toBe(true);
    expect(shown).toEqual([]);
  });

  test("is not needed: an editor with nobody to show things to adds as it did", async () => {
    const { server } = await open();
    const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
    clients.push(client);
    client.start();
    await until(() => client.state.status === "open", "the session");
    const editor = new Editor(client, new Notices());
    expect(await editor.addInstrument("builtin:osc")).toBe(3);
  });
});

describe("drawing a cable", () => {
  test("joins a modulator to a parameter, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addCable(lfo.id, oscillator.id, paramPort(oscillator, "detune"))).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(2);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, in the names the canvas has, and why", async () => {
    const { notices, editor } = await open();
    const cutoff = paramPort(filter, "cutoff");
    // (The LFO drives the cutoff already.)
    expect(await editor.addCable(lfo.id, filter.id, cutoff)).toBe(false);
    expect(await editor.addCable(oscillator.id, filter.id, cutoff)).toBe(false);
    expect(await editor.addCable(lfo.id, filter.id, { type: "param", index: 999 })).toBe(false);
    expect(said(notices)).toEqual([
      "Could not draw a cable from LFO to Filter: cutoff: that modulator drives that parameter already",
      "Could not draw a cable from Oscillator to Filter: cutoff: a cable starts at a modulator",
      "Could not draw a cable from LFO to Filter: the plugin has no parameter 999",
    ]);
  });

  test("names two nodes of one name by their numbers", async () => {
    const { notices, editor } = await open();
    await editor.addFreeModulator("lfo", oscillator.id);
    expect(await editor.addCable(3, filter.id, paramPort(filter, "cutoff"))).toBe(true);
    expect(await editor.addCable(3, filter.id, paramPort(filter, "cutoff"))).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable from LFO 2 to Filter: cutoff: that modulator drives that parameter already"]);
  });
});

describe("drawing a cable to a setting of another modulator", () => {
  /** The tracer with an envelope added to the oscillator's lane, whose node is 3. */
  async function withEnvelope() {
    const rig = await open();
    expect(await rig.editor.addFreeModulator("envelope", oscillator.id)).toBe(3);
    return rig;
  }

  test("joins a modulator to the rate of an LFO or a stage of an envelope, and says nothing", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    expect(await editor.addCable(lfo.id, 3, { type: "attack" })).toBe(true);
    const cables = client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation").map((e) => [e.from.node, e.to.node, e.to.port.type]);
    // (A modulator's cables are listed together, in the order of the modulators.)
    expect(cables).toEqual([
      [lfo.id, filter.id, "param"],
      [lfo.id, 3, "attack"],
      [3, lfo.id, "rate"],
    ]);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, in the names the canvas has, and why", async () => {
    const { notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    // (Each is said when it is refused: only the last few notices are kept.)
    const refused: [string, () => Promise<boolean>][] = [
      ["Could not draw a cable from ADSR to LFO: rate: that modulator drives that setting already", () => editor.addCable(3, lfo.id, { type: "rate" })],
      ["Could not draw a cable from LFO to ADSR: rate: that modulator has no rate", () => editor.addCable(lfo.id, 3, { type: "rate" })],
      ["Could not draw a cable from ADSR to ADSR: decay: a modulator cannot drive its own settings", () => editor.addCable(3, 3, { type: "decay" })],
      ["Could not draw a cable from LFO to Oscillator: rate: a cable ends at the settings of a modulator, and that is not one", () => editor.addCable(lfo.id, oscillator.id, { type: "rate" })],
      ["Could not draw a cable from LFO to that node: rate: there is no node 99", () => editor.addCable(lfo.id, 99, { type: "rate" })],
      ["Could not draw a cable from LFO to ADSR: a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth", () => editor.addCable(lfo.id, 3, { type: "modOut" })],
    ];
    for (const [words, draw] of refused) {
      expect(await draw()).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("names two modulators of one name by their numbers", async () => {
    const { notices, editor } = await open();
    expect(await editor.addFreeModulator("lfo", oscillator.id)).toBe(3);
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable from LFO 2 to LFO 1: rate: that modulator drives that setting already"]);
  });

  test("is told of, when it cannot be taken out, by the setting it ends at", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "rate" })).toBe(true);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable ADSR → LFO: rate: Not connected to the server."]);
  });
});

describe("drawing a cable to a host-side value", () => {
  test("joins a modulator to the volume of a lane, the mix of an effect and the volume and pan of a group, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const group = await editor.addGroup([oscillator.id]);
    expect(group).not.toBeNull();
    expect(await editor.addCable(lfo.id, oscillator.id, { type: "volume" })).toBe(true);
    expect(await editor.addCable(lfo.id, filter.id, { type: "mix" })).toBe(true);
    expect(await editor.addCable(lfo.id, group ?? -1, { type: "volume" })).toBe(true);
    expect(await editor.addCable(lfo.id, group ?? -1, { type: "pan" })).toBe(true);
    const cables = client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation" && e.from.node === lfo.id).map((e) => `${String(e.to.node)}:${e.to.port.type}`);
    expect(cables).toEqual([`${String(filter.id)}:param`, `${String(oscillator.id)}:volume`, `${String(filter.id)}:mix`, `${String(group)}:volume`, `${String(group)}:pan`]);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, in the names the canvas has, and why: the node has no such value, or the modulator drives it already", async () => {
    const { notices, editor } = await open();
    expect(await editor.addCable(lfo.id, filter.id, { type: "mix" })).toBe(true);
    // (Each is said when it is refused: only the last few notices are kept.)
    const refused: [string, () => Promise<boolean>][] = [
      ["Could not draw a cable from LFO to Filter: mix: that modulator drives that mix already", () => editor.addCable(lfo.id, filter.id, { type: "mix" })],
      ["Could not draw a cable from LFO to Filter: volume: that node has no volume", () => editor.addCable(lfo.id, filter.id, { type: "volume" })],
      ["Could not draw a cable from LFO to Oscillator: mix: that node has no mix", () => editor.addCable(lfo.id, oscillator.id, { type: "mix" })],
      ["Could not draw a cable from LFO to Oscillator: pan: that node has no pan", () => editor.addCable(lfo.id, oscillator.id, { type: "pan" })],
      ["Could not draw a cable from LFO to that node: mix: there is no node 99", () => editor.addCable(lfo.id, 99, { type: "mix" })],
    ];
    for (const [words, draw] of refused) {
      expect(await draw()).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("is told of, when it cannot be taken out, by the value it ends at: the lane's volume is its output volume", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addCable(lfo.id, oscillator.id, { type: "volume" })).toBe(true);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable LFO → Oscillator: output volume: Not connected to the server."]);
  });
});

describe("drawing a cable to the depth of a cable", () => {
  /** The tracer with an envelope added to the oscillator's lane, whose node is 3; the LFO's cable to the filter's cutoff is the cable 0. */
  async function withEnvelope() {
    const rig = await open();
    expect(await rig.editor.addFreeModulator("envelope", oscillator.id)).toBe(3);
    return rig;
  }

  test("joins a modulator to how deep a cable of another goes, and says nothing", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "depth", edge: 0 })).toBe(true);
    const ends = client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation").map((e) => [e.from.node, e.to.node, e.to.port]);
    expect(ends).toEqual([
      [lfo.id, filter.id, paramPort(filter, "cutoff")],
      [3, lfo.id, { type: "depth", edge: 0 }],
    ]);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says which cable, with the end told by the cable whose depth it is, and why", async () => {
    const { notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "depth", edge: 0 })).toBe(true);
    const refused: [string, () => Promise<boolean>][] = [
      ["Could not draw a cable from ADSR to depth of LFO to Filter: cutoff: that modulator drives that setting already", () => editor.addCable(3, lfo.id, { type: "depth", edge: 0 })],
      ["Could not draw a cable from LFO to depth of LFO to Filter: cutoff: a modulator cannot drive its own settings", () => editor.addCable(lfo.id, lfo.id, { type: "depth", edge: 0 })],
      ["Could not draw a cable from ADSR to LFO: depth: there is no cable 99", () => editor.addCable(3, lfo.id, { type: "depth", edge: 99 })],
    ];
    for (const [words, draw] of refused) {
      expect(await draw()).toBe(false);
      expect(said(notices).at(-1)).toBe(words);
    }
  });

  test("is told of, when it cannot be taken out, by the cable whose depth it ends at", async () => {
    const { client, notices, editor } = await withEnvelope();
    expect(await editor.addCable(3, lfo.id, { type: "depth", edge: 0 })).toBe(true);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable ADSR → depth of LFO → Filter: cutoff: Not connected to the server."]);
  });
});

describe("taking a node out", () => {
  test("takes a modulator with its cables, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeNode(lfo.id)).toBe(true);
    expect(client.state.snapshot?.graph.nodes.map((n) => n.name)).toEqual(["Oscillator", "Filter", "Master"]);
    expect(said(notices)).toEqual([]);
  });

  test("takes an effect with the cables that drive it, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeNode(filter.id)).toBe(true);
    expect(client.state.snapshot?.graph.nodes.map((n) => n.name)).toEqual(["Oscillator", "LFO", "Master"]);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("that cannot be taken out (the last lane of a session, a node that is not there) says what and why", async () => {
    const { notices, editor } = await open();
    expect(await editor.removeNode(oscillator.id)).toBe(false);
    expect(await editor.removeNode(999)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not remove Oscillator: a session keeps its last lane",
      "Could not remove that node: there is no node 999",
    ]);
  });
});

describe("taking a cable out", () => {
  test("takes it out, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.removeCable(0)).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toEqual([]);
    expect(said(notices)).toEqual([]);
  });

  test("that is not there says so, and one that is names what it joins", async () => {
    const { server, notices, editor } = await open();
    expect(await editor.removeCable(99)).toBe(false);
    expect(await editor.removeCable(0)).toBe(true);
    expect(await editor.removeCable(0)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out that cable: there is no cable 99", "Could not take out that cable: there is no cable 0"]);
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation")).toEqual([]);
  });

  test("when the connection is gone names the cable it could not take out", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.removeCable(0)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable LFO → Filter: cutoff: Not connected to the server."]);
  });
});

describe("drawing an audio cable", () => {
  const audioCables = (client: Client): Edge[] => (client.state.snapshot?.graph.edges ?? []).filter((e) => e.kind === "cable");

  test("asks for one from the output of a node to the input of another, at the level a new cable has when none is given, and says nothing", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addAudioCable(oscillator.id, MASTER)).toBe(true);
    // (The tracer's cable of modulation has the first id, and an audio cable has the next: they are one series.)
    expect(audioCables(client)).toEqual([{ kind: "cable", id: 1, gain: 1, from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } } }]);
    expect(said(notices)).toEqual([]);
  });

  test("asks for the level it is given, which the server holds to the levels there can be", async () => {
    const { client, editor } = await open();
    expect(await editor.addAudioCable(oscillator.id, filter.id, 0.25)).toBe(true);
    expect(await editor.addAudioCable(filter.id, MASTER, 9)).toBe(true);
    expect(audioCables(client).map((e) => (e.kind === "cable" ? [e.id, e.gain] : []))).toEqual([
      [1, 0.25],
      [2, 2],
    ]);
  });

  test("says what could not be done, in the names of the canvas and the controller's words, when it is refused, and changes nothing", async () => {
    const { client, notices, editor } = await open();
    // (The board keeps the last few notices only: each is read when it is said.)
    const refused = async (call: Promise<boolean>): Promise<string | undefined> => {
      expect(await call).toBe(false);
      return said(notices).at(-1);
    };
    expect(await refused(editor.addAudioCable(oscillator.id, oscillator.id))).toBe(
      "Could not draw an audio cable from Oscillator to Oscillator: a lane has no input of audio: a cable ends at an effect, a group or the mix",
    );
    expect(await refused(editor.addAudioCable(filter.id, filter.id))).toBe("Could not draw an audio cable from Filter to Filter: that cable would make the audio go round: it would feed what it comes from");
    expect(await refused(editor.addAudioCable(oscillator.id, 99))).toBe("Could not draw an audio cable from Oscillator to that node: there is no node 99");
    expect(await refused(editor.addAudioCable(MASTER, filter.id))).toBe("Could not draw an audio cable from Master to Filter: the mix has no output of audio");
    expect(await refused(editor.addAudioCable(lfo.id, filter.id))).toBe("Could not draw an audio cable from LFO to Filter: a modulator has no output of audio");
    expect(audioCables(client)).toEqual([]);
  });

  test("does not send a level that is not a number, which the wire would write as none and draw at the level a new cable has: it says so", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addAudioCable(oscillator.id, filter.id, Number.NaN)).toBe(false);
    expect(await editor.addAudioCable(oscillator.id, filter.id, Number.POSITIVE_INFINITY)).toBe(false);
    expect(said(notices)).toEqual([
      "Could not draw an audio cable from Oscillator to Filter: NaN is not a value for the level of a cable",
      "Could not draw an audio cable from Oscillator to Filter: Infinity is not a value for the level of a cable",
    ]);
    expect(audioCables(client)).toEqual([]);
  });

  test("is refused when there is a cable between the two already, and the one that is there stays", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addAudioCable(oscillator.id, MASTER, 0.5)).toBe(true);
    expect(await editor.addAudioCable(oscillator.id, MASTER, 0.75)).toBe(false);
    expect(said(notices)).toEqual(["Could not draw an audio cable from Oscillator to Master: there is a cable between those two already"]);
    expect(audioCables(client)).toHaveLength(1);
  });

  test("says what could not be done when the connection is gone", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addAudioCable(oscillator.id, MASTER)).toBe(false);
    expect(said(notices)).toEqual(["Could not draw an audio cable from Oscillator to Master: Not connected to the server."]);
  });

  test("asks for the second input of an effect that has one as a cable of its own beside the one at its first, and says which input a refusal is about", async () => {
    const { client, notices, editor } = await open();
    const math = await editor.addFreeEffect("builtin:math");
    expect(math).not.toBeNull();
    if (math === null) return;
    expect(await editor.addAudioCable(oscillator.id, math, null, true)).toBe(true);
    expect(await editor.addAudioCable(oscillator.id, math)).toBe(true);
    expect(audioCables(client).map((e) => [e.kind === "cable" ? e.id : -1, e.to.node, e.to.port.type])).toEqual([
      [1, math, "audioInSide"],
      [2, math, "audioIn"],
    ]);
    expect(said(notices)).toEqual([]);
    // (Each input has the cable from the oscillator once: the words say which.)
    expect(await editor.addAudioCable(oscillator.id, math, null, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not draw an audio cable from Oscillator to Math: B: there is a cable between those two already");
    expect(await editor.addAudioCable(oscillator.id, math)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not draw an audio cable from Oscillator to Math: A: there is a cable between those two already");
    // An effect that has one input has no second.
    expect(await editor.addAudioCable(oscillator.id, filter.id, null, true)).toBe(false);
    expect(said(notices).at(-1)).toBe("Could not draw an audio cable from Oscillator to Filter: that effect has no second input");
    expect(audioCables(client)).toHaveLength(2);
  });
});

describe("taking an audio cable out", () => {
  test("takes it out, and says nothing; the cable of modulation that the session has stays", async () => {
    const { client, notices, editor } = await open();
    await editor.addAudioCable(oscillator.id, MASTER);
    expect(await editor.removeCable(1)).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "cable")).toEqual([]);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(1);
    expect(said(notices)).toEqual([]);
  });

  test("names the audio cable it could not take out, by what it joins, when the connection is gone", async () => {
    const { client, notices, editor } = await open();
    await editor.addAudioCable(oscillator.id, MASTER);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable Oscillator → Master: Not connected to the server."]);
  });
});

describe("adding a source of notes", () => {
  const sourcesOf = (client: Client): Node[] => (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "source");

  test("asks for the keys, resolves with the node it made, which the canvas is asked to show, and says nothing", async () => {
    const { client, notices, shown, editor } = await open();
    const id = await editor.addSource({ type: "keys" });
    // (The tracer has nodes 0 to 2: the next is 3.)
    expect(id).toBe(3);
    expect(sourcesOf(client)).toMatchObject([{ id: 3, parent: null, name: "Keys", kind: "source", source: { type: "keys" } }]);
    expect(shown).toEqual([3]);
    expect(said(notices)).toEqual([]);
  });

  test("asks for a device by the name it is given, which the server tidies, and the node is called by it", async () => {
    const { client, editor } = await open();
    const id = await editor.addSource({ type: "device", name: "  Seaboard RISE " });
    expect(sourcesOf(client)).toMatchObject([{ id, name: "Seaboard RISE", kind: "source", source: { type: "device", name: "Seaboard RISE" } }]);
  });

  test("resolves with the last of the sources there are, which is its own when another front-end added one just before", async () => {
    const { server, shown, editor } = await open();
    server.apply({ type: "addSource", source: { type: "device", name: "Other" } }); // another front-end's, before this one is answered
    const id = await editor.addSource({ type: "keys" });
    expect(server.snapshot.graph.nodes.find((n) => n.id === id)).toMatchObject({ name: "Keys" });
    expect(shown).toEqual([id]);
  });

  test("finds its own source when another front-end added a node of another kind at the same time", async () => {
    const { server, shown, editor } = await open();
    server.apply({ type: "addGroup", members: [] }); // another front-end's, before this one is answered
    const id = await editor.addSource({ type: "keys" });
    expect(server.snapshot.graph.nodes.find((n) => n.id === id)).toMatchObject({ kind: "source", name: "Keys" });
    expect(shown).toEqual([id]);
  });

  test("resolves with null when it is refused, though another front-end added a source just before: that one is not its own, and is not shown", async () => {
    const { server, shown, editor } = await open();
    expect(await editor.addSource({ type: "keys" })).not.toBeNull();
    shown.length = 0;
    server.apply({ type: "addSource", source: { type: "device", name: "Other" } }); // another front-end's, before this one is answered
    expect(await editor.addSource({ type: "keys" })).toBeNull();
    expect(shown).toEqual([]);
  });

  test("says what could not be done, in the controller's words, when it is refused: the keys twice, a device twice, a name that is not one", async () => {
    const { client, notices, editor } = await open();
    expect(await editor.addSource({ type: "keys" })).not.toBeNull();
    expect(await editor.addSource({ type: "device", name: "Seaboard" })).not.toBeNull();
    expect(await editor.addSource({ type: "keys" })).toBeNull();
    expect(await editor.addSource({ type: "device", name: " Seaboard " })).toBeNull();
    expect(await editor.addSource({ type: "device", name: "   " })).toBeNull();
    expect(said(notices)).toEqual([
      "Could not add the keys as a source of notes: the keys are a source already",
      'Could not add "Seaboard" as a source of notes: Seaboard is a source already',
      'Could not add "" as a source of notes: a device\'s name is blank: a device is called by its name, or as much of it as tells it',
    ]);
    expect(sourcesOf(client)).toHaveLength(2);
  });

  test("names a device by what the server makes of the name: without the white space round it that Unicode has, a next-line character too, which the page's own trim leaves", async () => {
    const { notices, editor } = await open();
    expect(await editor.addSource({ type: "device", name: "\u0085Seaboard\u0085" })).not.toBeNull();
    expect(await editor.addSource({ type: "device", name: "\u0085Seaboard\u0085" })).toBeNull();
    expect(said(notices)).toEqual(['Could not add "Seaboard" as a source of notes: Seaboard is a source already']);
  });

  test("names a device that has a long name as a notice does, cut after a few characters", async () => {
    const { notices, editor } = await open();
    const name = "Launchkey Mini MK4 37 MIDI Out and a long tail";
    await editor.addSource({ type: "device", name });
    expect(await editor.addSource({ type: "device", name })).toBeNull();
    expect(said(notices)).toEqual([`Could not add "Launchkey Mini MK4 37 MI…" as a source of notes: ${name} is a source already`]);
  });

  test("says so when the connection is gone", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.addSource({ type: "keys" })).toBeNull();
    expect(await editor.addSource({ type: "device", name: "Seaboard" })).toBeNull();
    expect(said(notices)).toEqual(["Could not add the keys as a source of notes: Not connected to the server.", 'Could not add "Seaboard" as a source of notes: Not connected to the server.']);
  });
});

describe("drawing a cable of notes", () => {
  const notesCables = (client: Client): Edge[] => (client.state.snapshot?.graph.edges ?? []).filter((e) => e.kind === "events");

  test("asks for one from the output of events of a source to the input of events of a lane, and says nothing", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    expect(await editor.addNotesCable(keys, oscillator.id)).toBe(true);
    // (The tracer's cable of modulation has the first id, and a cable of notes the next: the cables of every kind are one series.)
    expect(notesCables(client)).toEqual([{ kind: "events", id: 1, from: { node: keys, port: { type: "eventsOut" } }, to: { node: oscillator.id, port: { type: "eventsIn" } } }]);
    expect(said(notices)).toEqual([]);
  });

  test("says what could not be done, in the names of the canvas and the controller's words, when it is refused, and changes nothing", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    const refused = async (call: Promise<boolean>): Promise<string | undefined> => {
      expect(await call).toBe(false);
      return said(notices).at(-1);
    };
    expect(await refused(editor.addNotesCable(oscillator.id, oscillator.id))).toBe("Could not draw a cable of notes from Oscillator to Oscillator: only a source of notes has an output of events");
    expect(await refused(editor.addNotesCable(keys, filter.id))).toBe("Could not draw a cable of notes from Keys to Filter: a cable of notes ends at a lane: it is the lanes that have an input of events");
    expect(await refused(editor.addNotesCable(keys, MASTER))).toBe("Could not draw a cable of notes from Keys to Master: the mix has no input of events");
    expect(await refused(editor.addNotesCable(MASTER, oscillator.id))).toBe("Could not draw a cable of notes from Master to Oscillator: the mix has no output of events");
    expect(await refused(editor.addNotesCable(keys, 99))).toBe("Could not draw a cable of notes from Keys to that node: there is no node 99");
    expect(await refused(editor.addNotesCable(99, oscillator.id))).toBe("Could not draw a cable of notes from that node to Oscillator: there is no node 99");
    expect(notesCables(client)).toEqual([]);
  });

  test("is refused when there is a cable between the two already, and the one that is there stays", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    expect(await editor.addNotesCable(keys, oscillator.id)).toBe(true);
    expect(await editor.addNotesCable(keys, oscillator.id)).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable of notes from Keys to Oscillator: there is a cable between those two already"]);
    expect(notesCables(client)).toHaveLength(1);
  });

  test("says what could not be done when the connection is gone", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    client.stop();
    expect(await editor.addNotesCable(keys, oscillator.id)).toBe(false);
    expect(said(notices)).toEqual(["Could not draw a cable of notes from Keys to Oscillator: Not connected to the server."]);
  });
});

describe("taking a cable of notes, and a source, out", () => {
  const notesCables = (client: Client): Edge[] => (client.state.snapshot?.graph.edges ?? []).filter((e) => e.kind === "events");

  test("takes the cable out, and says nothing; the source and the cable of modulation stay", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    await editor.addNotesCable(keys, oscillator.id);
    expect(await editor.removeCable(1)).toBe(true);
    expect(notesCables(client)).toEqual([]);
    expect(client.state.snapshot?.graph.nodes.some((n) => n.id === keys)).toBe(true);
    expect(client.state.snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(1);
    expect(said(notices)).toEqual([]);
  });

  test("names the cable of notes it could not take out, by what it joins, when the connection is gone", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    await editor.addNotesCable(keys, oscillator.id);
    client.stop();
    expect(await editor.removeCable(1)).toBe(false);
    expect(said(notices)).toEqual(["Could not take out the cable Keys → Oscillator: Not connected to the server."]);
  });

  test("takes the source out with its cables, by the name the canvas gives it", async () => {
    const { client, notices, editor } = await open();
    const keys = await editor.addSource({ type: "keys" });
    if (keys === null) throw new Error("the keys were added");
    await editor.addNotesCable(keys, oscillator.id);
    expect(await editor.removeNode(keys)).toBe(true);
    expect(client.state.snapshot?.graph.nodes.some((n) => n.id === keys)).toBe(false);
    expect(notesCables(client)).toEqual([]);
    expect(said(notices)).toEqual([]);
    expect(await editor.removeNode(keys)).toBe(false);
    expect(said(notices)).toEqual(["Could not remove that node: there is no node 3"]);
  });
});

describe("saving", () => {
  const noFile = (): MockOptions => ({ snapshot: { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, file: null } } });

  test("writes the session, which has nothing unsaved after it, and says nothing", async () => {
    const { client, notices, editor } = await open();
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.5 });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(await editor.save()).toBe(true);
    expect(client.state.snapshot?.meta.dirty).toBe(false);
    expect(said(notices)).toEqual([]);
  });

  test("that is refused says so in a notice, and the session is as unsaved as it was", async () => {
    const { client, notices, editor } = await open(noFile());
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.5 });
    expect(await editor.save()).toBe(false);
    expect(said(notices)).toEqual(["Could not save the session: No session path — cannot save"]);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
  });

  test("when the connection is gone says so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.save()).toBe(false);
    expect(said(notices)).toEqual(["Could not save the session: Not connected to the server."]);
  });
});

describe("undoing and redoing", () => {
  const volume = (client: Client): number | undefined => {
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
    return node?.kind === "instrument" ? node.volume : undefined;
  };

  test("put back what a change set and set it again, and say nothing", async () => {
    const { client, notices, editor } = await open();
    const before = volume(client);
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.25 });
    expect(await editor.undo()).toBe(true);
    expect(volume(client)).toBe(before);
    expect(await editor.redo()).toBe(true);
    expect(volume(client)).toBe(0.25);
    expect(said(notices)).toEqual([]);
  });

  test("with nothing to undo or redo are refused, and say so in a notice", async () => {
    const { notices, editor } = await open();
    expect(await editor.undo()).toBe(false);
    expect(await editor.redo()).toBe(false);
    expect(said(notices)).toEqual(["Could not undo: there is nothing to undo", "Could not redo: there is nothing to redo"]);
  });

  test("when the connection is gone say so", async () => {
    const { client, notices, editor } = await open();
    client.stop();
    expect(await editor.undo()).toBe(false);
    expect(await editor.redo()).toBe(false);
    expect(said(notices)).toEqual(["Could not undo: Not connected to the server.", "Could not redo: Not connected to the server."]);
  });
});

describe("opening another session (step 115: a session file that a person picked)", () => {
  const FILE = "[[instrument]]\nplugin = \"builtin:osc\"\n";

  test("sends the text as one intent and resolves true when the program takes it: no event is told, so the session is as it was (it is about to be another) and the history has no step", async () => {
    const { server, client, notices, editor } = await open({ openSession: true });
    const before = client.state.snapshot;
    expect(await editor.openSession(FILE, "pluck.toml")).toBe(true);
    expect(said(notices)).toEqual([]);
    const sent = server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []));
    expect(sent).toEqual([{ type: "openSession", text: FILE }]);
    expect(client.state.snapshot?.graph).toEqual(before?.graph);
    expect(client.state.snapshot?.rev, "no event told, so no revision seen").toBe(before?.rev);
    expect(client.state.history).toEqual({ undo: null, redo: null });
  });

  test("is refused, in the controller's words, by a program that cannot open another session: the page says so with the name of the file in quotes, and no revision is used", async () => {
    const { client, notices, editor } = await open();
    const rev = client.state.snapshot?.rev;
    expect(await editor.openSession(FILE, "pluck.toml")).toBe(false);
    expect(said(notices)).toEqual(['Could not open "pluck.toml": this program cannot open another session while it runs: start it with the file']);
    expect(client.state.snapshot?.rev).toBe(rev);
  });

  test("says that a file is too big at once, and does not send it: the most is the half of what the server reads, in bytes of the text and not in characters", async () => {
    const { server, notices, editor } = await open({ openSession: true });
    const most = "x".repeat(MAX_SESSION_FILE_BYTES);
    expect(await editor.openSession(most + "x", "big.toml")).toBe(false);
    expect(said(notices)).toEqual(['Could not open "big.toml": it is more than 4 MiB, which is more than a session that is opened here can be']);
    expect(server.received.some((message) => message.type === "request")).toBe(false);
    // (Exactly the most goes; and a character of three bytes is three, so a text of that many characters does not.)
    expect(await editor.openSession(most, "just.toml")).toBe(true);
    expect(await editor.openSession("€".repeat(MAX_SESSION_FILE_BYTES / 3 + 1), "euros.toml")).toBe(false);
  });

  test("reads the file that a person picked and sends what is in it; one that cannot be read is said among the notices, with why, and nothing is sent", async () => {
    const { server, notices, editor } = await open({ openSession: true });
    expect(await editor.openSessionFile({ name: "a.toml", text: () => Promise.resolve(FILE) })).toBe(true);
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "openSession", text: FILE }]);
    const sent = server.received.length;
    expect(await editor.openSessionFile({ name: "b.toml", text: () => Promise.reject(new Error("the disk went")) })).toBe(false);
    expect(said(notices)).toEqual(['Could not open "b.toml": it could not be read (the disk went)']);
    expect(server.received.length, "nothing more was sent").toBe(sent);
  });

  test("a file that a program that cannot open a session is given is refused with the file's name in the notice, as the text is", async () => {
    const { notices, editor } = await open();
    expect(await editor.openSessionFile({ name: "kick.toml", text: () => Promise.resolve(FILE) })).toBe(false);
    expect(said(notices)).toEqual(['Could not open "kick.toml": this program cannot open another session while it runs: start it with the file']);
  });

  test("the notice quotes a name that is long cut short, as the others do", async () => {
    const { notices, editor } = await open();
    await editor.openSession(FILE, "a-session-file-with-a-very-long-name.toml");
    expect(said(notices)[0]).toMatch(/^Could not open "a-session-file-with-a-ve…": /);
  });
});

describe("opening a sound of a library (step 116)", () => {
  const entry = (id: string, openable = true) => ({ id, name: id.replace(/\.toml$/, ""), detail: openable ? "osc" : "expected an equals sign", openable, modified: null });
  const library = { id: "sessions", label: "Saved sessions", place: "/home/me/.config/tang/sessions" };
  const options = { openSession: true, libraries: [{ ...library, entries: [entry("pad.toml"), entry("broken.toml", false)] }] };

  test("names the sound and its library to the server, which reads it: no text goes from the page, and the session is as it was", async () => {
    const { server, client, notices, editor } = await open(options);
    const before = client.state.snapshot;
    expect(await editor.openSound(library, entry("pad.toml"))).toBe(true);
    expect(said(notices)).toEqual([]);
    expect(server.received.at(-1)).toEqual({ type: "openSound", reqId: expect.any(Number) as number, library: "sessions", id: "pad.toml" });
    expect(server.received.some((message) => message.type === "request")).toBe(false);
    expect(client.state.snapshot?.graph).toEqual(before?.graph);
    expect(client.state.history).toEqual({ undo: null, redo: null });
  });

  test("is refused with the server's reason when the sound is not a session, and the notice names the sound and the library in quotes", async () => {
    const { notices, editor } = await open(options);
    expect(await editor.openSound(library, entry("broken.toml", false))).toBe(false);
    expect(said(notices)).toEqual(['Could not open "broken" from "Saved sessions": that is not a session: expected an equals sign']);
  });

  test("is refused when the sound is gone, and by a program that cannot open another session", async () => {
    const { notices, editor } = await open(options);
    expect(await editor.openSound(library, entry("gone.toml"))).toBe(false);
    expect(said(notices)[0]).toBe('Could not open "gone" from "Saved sessions": there is no sound "gone.toml" in this library any more');
    const cannot = await open({ libraries: options.libraries });
    expect(await cannot.editor.openSound(library, entry("pad.toml"))).toBe(false);
    expect(said(cannot.notices)[0]).toMatch(/^Could not open "pad" from "Saved sessions": this program cannot open another session while it runs/);
  });

  test("the notice quotes a name that is long cut short, as the others do, and says why when the connection is gone", async () => {
    const { client, notices, editor } = await open(options);
    client.stop();
    await editor.openSound({ ...library, label: "A library with a very long name indeed" }, entry("a-sound-with-a-very-long-name.toml"));
    expect(said(notices)[0]).toBe('Could not open "a-sound-with-a-very-long…" from "A library with a very lo…": Not connected to the server.');
  });
});
