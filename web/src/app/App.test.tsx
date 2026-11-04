// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { Client } from "../client/connection";
import type { ClientOptions } from "../client/connection";
import { groupsFixture, nodeNamed, sourcesFixture, tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import type { Notices } from "../client/notices";
import { App } from "./App";
import { ClientProvider, useNotices } from "./ClientProvider";

afterEach(cleanup);

const filter = nodeNamed(tracerSnapshot, "Filter");

function page(options: MockOptions = {}, clientOptions: Partial<ClientOptions> = {}) {
  const server = new MockServer(options);
  const client = new Client({ transport: server.transport, reconnectDelay: () => 5, ...clientOptions });
  const view = render(
    <ClientProvider client={client}>
      <App engine="mock" />
    </ClientProvider>,
  );
  return { server, client, view };
}

/** The text of the session as the page shows it, once whoever is looking has asked to see it. */
const shown = (): string => document.querySelector(".tang-session")?.textContent ?? "";

/** Open the session as JSON, which the page keeps closed (and does not write while it is). */
function openJson(): void {
  const details = document.querySelector("details");
  if (!details) throw new Error("the page has no session as JSON");
  details.open = true;
  fireEvent(details, new Event("toggle"));
}

describe("the page", () => {
  test("connects, says so, and shows the session as the server sent it", async () => {
    page();
    expect(screen.getByRole("status").textContent).toBe("Connecting…");
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("Connected");
    });
    openJson();
    expect(JSON.parse(shown())).toEqual(tracerSnapshot);
    expect(shown()).toContain('\n  "graph"'); // written out for a person to read
    expect(screen.getByText("mock engine")).toBeTruthy();
  });

  test("says when it is talking to a real server", async () => {
    const server = new MockServer();
    const client = new Client({ transport: server.transport });
    render(
      <ClientProvider client={client}>
        <App engine="server" />
      </ClientProvider>,
    );
    await screen.findByText("Connected");
    expect(screen.getByText("server")).toBeTruthy();
    expect(screen.queryByText("mock engine")).toBeNull();
  });

  test("cannot be rendered without a client", () => {
    const quiet = vi.spyOn(console, "error").mockImplementation(() => undefined);
    expect(() => render(<App engine="mock" />)).toThrow(/ClientProvider/);
    quiet.mockRestore();
  });

  test("says what could not be done, where the person can read it and take it away", async () => {
    const server = new MockServer();
    const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
    let notices: Notices | undefined;
    function Probe() {
      notices = useNotices();
      return null;
    }
    render(
      <ClientProvider client={client}>
        <Probe />
        <App engine="mock" />
      </ClientProvider>,
    );
    await screen.findByText("Connected");
    expect(screen.queryByRole("button", { name: /^Dismiss/ })).toBeNull();
    act(() => {
      notices?.say("Could not add an LFO to Filter: a modulator goes in a lane or in a group");
    });
    expect(screen.getByText(/^Could not add an LFO to Filter/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^Dismiss: Could not add an LFO/ }));
    expect(screen.queryByText(/^Could not add an LFO to Filter/)).toBeNull();
  });

  test("shows a change somebody else makes", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    openJson();
    expect(shown()).not.toContain("0.123");
    server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.123 });
    await waitFor(() => {
      expect(shown()).toContain("0.123");
    });
  });

  test("goes on showing the session while the connection is lost, and says it is", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    openJson();
    server.drop();
    await screen.findByText("Connection lost, reconnecting…");
    expect(JSON.parse(shown())).toEqual(tracerSnapshot);
    await screen.findByText("Connected");
  });

  test("tells the person why when the server cannot be used", async () => {
    page({ protocol: 9 });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/protocol 1 and the server speaks 9/);
    expect(screen.getByRole("status").textContent).toBe("Disconnected");
    openJson();
    expect(shown()).toBe("");
  });

  test("shows the session as a patch: the nodes of the tracer and the cables between them", async () => {
    page();
    await screen.findByText("Connected");
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    expect(Array.from(patch.querySelectorAll(".tang-node__name")).map((name) => name.textContent)).toEqual([
      "Oscillator",
      "Filter",
      "LFO",
      "Master",
    ]);
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__edge")).toHaveLength(3);
    });
  });

  test("has a button in the bar that opens a menu of the instruments that are installed, under the format each is of, and not of the effects", async () => {
    page();
    fireEvent.click(await screen.findByRole("button", { name: "Add an instrument" }));
    const menu = screen.getByRole("menu", { name: "Add an instrument" });
    const groups = within(menu)
      .getAllByRole("group")
      .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
    // (The catalog is by name: the audio input comes before the oscillator.)
    expect(groups).toEqual([["Built-in", ["Audio Input", "Oscillator"]]]);
  });

  test("asks for a lane with the instrument that was chosen, and the lane is on the canvas when the server has made it", async () => {
    const { server } = page();
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    fireEvent.click(await screen.findByRole("button", { name: "Add an instrument" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "addInstrument", plugin: "builtin:osc" }]);
    // The two lanes are told apart by a number, and the new one is a lane of its own: it has all the buttons a lane has.
    expect(await screen.findByRole("button", { name: "Change the instrument of Oscillator 2" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add an effect to Oscillator 2" })).toBeTruthy();
    // The menu has gone, and the focus is back on the button that opened it.
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an instrument" }));
  });

  test("has no button in the bar that adds a group, and a lane has no choice of group while the session has none", async () => {
    page();
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    // (The page offers no way to make a group: a session that was loaded with one has it drawn, and the terminal can make one.)
    expect(await screen.findByRole("button", { name: "Add an instrument" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Add a group" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: /^Group of / })).toBeNull();
  });

  test("draws a group that the session has, with the buttons a group has, and a lane is put in it from its own choice: none and each group, and no new group", async () => {
    page({ snapshot: groupsFixture.snapshot });
    expect(await screen.findByRole("button", { name: "Add an effect to Lead" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Rename Lead" })).toBeTruthy();
    const choice = screen.getByRole<HTMLSelectElement>("combobox", { name: "Group of Oscillator 2" });
    expect(Array.from(choice.options).map((option) => option.textContent)).toEqual(["none", "Group 1", "Group 2", "Lead"]);
  });

  test("gives the focus to the button that adds an instrument when a group is taken out, and when a lane is", async () => {
    page({ snapshot: groupsFixture.snapshot });
    fireEvent.click(await screen.findByRole("button", { name: "Remove Lead" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove Lead for good" }));
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an instrument" }));
    });
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator 2" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove the lane of Oscillator 2 for good/ }));
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an instrument" }));
    });
  });

  test("gives the focus to the button in the bar that adds an instrument when the instrument of a lane is taken out alone and nothing else of the lane has a card: its effect stands by itself then", async () => {
    page({ snapshot: groupsFixture.snapshot });
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator 2" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator 2 only/ }));
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an instrument" }));
    });
    // (The lane has no card: its filter is on the canvas, with the mute and the solo of the lane, which are its own.)
    expect(screen.queryByRole("button", { name: "Choose an instrument for (empty)" })).toBeNull();
    expect(screen.getByRole("button", { name: "Mute Filter" })).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Remove Filter" })).toHaveLength(1);
  });

  test("gives the focus to the button that puts an instrument in when the instrument of a lane that has a card is taken out alone, and the lane stays", async () => {
    // (Lane 3 is in a group, which a card has the choice of: the lane keeps its card with no instrument, and the card is where an instrument is put in.)
    page({ snapshot: groupsFixture.snapshot });
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator 1" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator 1 only/ }));
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Choose an instrument for (empty)" }));
    });
  });

  test("puts the instrument back by Undo, in the words of what was done, and takes it out again by Redo", async () => {
    page({ snapshot: groupsFixture.snapshot });
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator 2" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator 2 only/ }));
    fireEvent.click(await screen.findByRole("button", { name: "Undo take Oscillator out of its lane" }));
    expect(await screen.findByRole("button", { name: "Change the instrument of Oscillator 2" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Mute Filter" })).toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "Redo take Oscillator out of its lane" }));
    expect(await screen.findByRole("button", { name: "Mute Filter" })).toBeTruthy();
    // (The lane has no card: the oscillators that are left are numbered without it, so that "Oscillator 2" is another lane's.)
    expect(document.getElementById("change-instrument-5")).toBeNull();
  });

  test("does not give the focus to the button that puts an instrument in when the instrument could not be taken out", async () => {
    const { server } = page({ snapshot: groupsFixture.snapshot }, { reconnectDelay: () => 60_000 });
    await screen.findByText("Connected");
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator 2" }));
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator 2 only/ }));
    expect(await screen.findByText("Could not take Oscillator 2 out of its lane: Not connected to the server.")).toBeTruthy();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(document.activeElement).not.toBe(screen.getByRole("button", { name: "Change the instrument of Oscillator 2" }));
  });

  test("does not give the focus to the button that adds an instrument when the group could not be taken out", async () => {
    const { server } = page({ snapshot: groupsFixture.snapshot }, { reconnectDelay: () => 60_000 });
    await screen.findByText("Connected");
    fireEvent.click(await screen.findByRole("button", { name: "Remove Lead" }));
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove Lead for good" }));
    expect(await screen.findByText("Could not remove Lead: Not connected to the server.")).toBeTruthy();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(document.activeElement).not.toBe(screen.getByRole("button", { name: "Add an instrument" }));
    expect(screen.getByRole("button", { name: "Remove Lead" })).toBeTruthy();
  });

  test("has nothing to open until the server has said what is installed, or when nothing that is can be an instrument", async () => {
    const effect = { id: "builtin:filter", name: "Filter", format: "Built-in", isInstrument: false, params: 4, presets: 0 };
    for (const catalog of [{ entries: [], scanning: true }, { entries: [effect], scanning: false }]) {
      page({ catalog });
      const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add an instrument" });
      expect(button.disabled).toBe(true);
      cleanup();
    }
  });

  test("says why a lane could not be added, among the notices", async () => {
    // (The connection stays down: after a few milliseconds, as in the other tests, a slow machine would find it up again at the click.)
    const { server } = page({}, { reconnectDelay: () => 60_000 });
    await screen.findByText("Connected");
    fireEvent.click(await screen.findByRole("button", { name: "Add an instrument" }));
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    // (The menu is still there: what is installed is known, and the connection is what is not.)
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
    expect(await screen.findByText("Could not add Oscillator as an instrument: Not connected to the server.")).toBeTruthy();
  });

  test("has a button in the bar that opens a menu of the sources of notes that can be added: the keys, and each MIDI input that is open, under a heading", async () => {
    page({ inputs: ["Seaboard RISE", "Launchkey Mini MK4 37 MIDI Out"] });
    fireEvent.click(await screen.findByRole("button", { name: "Add a source of notes" }));
    const menu = screen.getByRole("menu", { name: "Add a source of notes" });
    const listed = within(menu)
      .getAllByRole("menuitem")
      .map((item) => item.textContent);
    expect(listed).toEqual(["Keys", "Seaboard RISE", "Launchkey Mini MK4 37 MIDI Out"]);
    expect(within(menu).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["MIDI inputs"]);
  });

  test("asks for the source that was chosen, and the node is on the canvas when the server has made it, with its buttons; the focus is back on the button, and the keys are not offered again", async () => {
    const { server } = page({ inputs: ["Seaboard RISE"] });
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    const button = await screen.findByRole("button", { name: "Add a source of notes" });
    fireEvent.click(button);
    fireEvent.click(screen.getByRole("menuitem", { name: "Keys" }));
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "addSource", source: { type: "keys" } }]);
    expect(await screen.findByRole("button", { name: "Send notes from Keys" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove Keys" })).toBeTruthy();
    // The menu has gone, and the focus is back on the button that opened it.
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(button);
    fireEvent.click(button);
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Seaboard RISE"]);
  });

  test("asks for a device by the name of the input that was chosen, and the input is not offered again", async () => {
    const { server } = page({ inputs: ["Seaboard RISE", "IAC Driver Bus 1"] });
    fireEvent.click(await screen.findByRole("button", { name: "Add a source of notes" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Seaboard RISE" }));
    expect(await screen.findByRole("button", { name: "Send notes from Seaboard RISE" })).toBeTruthy();
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "addSource", source: { type: "device", name: "Seaboard RISE" } }]);
    fireEvent.click(screen.getByRole("button", { name: "Add a source of notes" }));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keys", "IAC Driver Bus 1"]);
  });

  test("has nothing to open when there is nothing to add: the keys are a source of the session and no input is open", async () => {
    const wired = sourcesFixture.cases.find((entry) => entry.name === "wired");
    if (!wired) throw new Error("the fixture has the session of the sources");
    page({ snapshot: wired.snapshot, inputs: [] });
    const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add a source of notes" });
    expect(button.disabled).toBe(true);
  });

  test("follows the inputs that open and close: the menu lists the ones that are open when it is opened", async () => {
    const { client, server } = page({ inputs: [] });
    const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add a source of notes" });
    act(() => {
      server.announceInputs(["Seaboard RISE"]);
    });
    await waitFor(() => {
      expect(client.state.inputs).toEqual(["Seaboard RISE"]);
    });
    fireEvent.click(button);
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keys", "Seaboard RISE"]);
  });

  test("lists the inputs that are open when one takes the place of another, as many as there were", async () => {
    const { client, server } = page({ inputs: ["Seaboard RISE"] });
    const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add a source of notes" });
    act(() => {
      server.announceInputs(["IAC Driver Bus 1"]);
    });
    await waitFor(() => {
      expect(client.state.inputs).toEqual(["IAC Driver Bus 1"]);
    });
    fireEvent.click(button);
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keys", "IAC Driver Bus 1"]);
  });

  test("says why a source could not be added, among the notices", async () => {
    const { server } = page({}, { reconnectDelay: () => 60_000 });
    await screen.findByText("Connected");
    fireEvent.click(await screen.findByRole("button", { name: "Add a source of notes" }));
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "Keys" }));
    expect(await screen.findByText("Could not add the keys as a source of notes: Not connected to the server.")).toBeTruthy();
  });

  test("does not give the focus to the button that adds a source when the source could not be taken out", async () => {
    const wired = sourcesFixture.cases.find((entry) => entry.name === "wired");
    if (!wired) throw new Error("the fixture has the session of the sources");
    // (An input is open, so that the button has something to offer and can take the focus.)
    const { server } = page({ snapshot: wired.snapshot, inputs: ["Seaboard RISE"] }, { reconnectDelay: () => 60_000 });
    await screen.findByText("Connected");
    const remove = await screen.findByRole("button", { name: "Remove Keys" });
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    fireEvent.click(remove);
    expect(await screen.findByText("Could not remove Keys: Not connected to the server.")).toBeTruthy();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(document.activeElement).not.toBe(screen.getByRole("button", { name: "Add a source of notes" }));
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Add a source of notes" }).disabled).toBe(false);
  });

  test("gives the focus to the button that adds a source when a source is taken out", async () => {
    const wired = sourcesFixture.cases.find((entry) => entry.name === "wired");
    if (!wired) throw new Error("the fixture has the session of the sources");
    page({ snapshot: wired.snapshot, inputs: [] });
    fireEvent.click(await screen.findByRole("button", { name: "Remove Keys" }));
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a source of notes" }));
    });
  });

  test("does not play a note for a key typed in the filter of a plugin's parameters, which is for text; the same key anywhere else plays", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    fireEvent.click(await screen.findByRole("button", { name: "Parameters of Oscillator" }));
    const search = await screen.findByRole("searchbox", { name: "Filter the parameters of Oscillator" });
    fireEvent.keyDown(search, { code: "KeyZ", key: "z" });
    fireEvent.keyUp(search, { code: "KeyZ", key: "z" });
    // (Z is a C, and X, which is typed on the page, a D: if the first had played, it would have been sent before the second.)
    fireEvent.keyDown(document.body, { code: "KeyX", key: "x" });
    fireEvent.keyUp(document.body, { code: "KeyX", key: "x" });
    await waitFor(() => {
      expect(server.received.filter((message) => message.type === "noteOn").map((message) => (message as { note: number }).note)).toEqual([50]);
    });
  });

  test("keeps the session as JSON closed, and writes it only while it is open", async () => {
    page();
    await screen.findByText("Connected");
    expect(document.querySelector("details")?.open).toBe(false);
    expect(document.querySelector(".tang-session")).toBeNull();
    openJson();
    expect(document.querySelector(".tang-session")).not.toBeNull();
    const details = document.querySelector("details");
    if (details) {
      details.open = false;
      fireEvent(details, new Event("toggle"));
    }
    expect(document.querySelector(".tang-session")).toBeNull();
  });

  test("does not go blank for a number the server could not write: JSON has no infinity, and says null", async () => {
    const quiet = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    const nodes = tracerSnapshot.graph.nodes.map((node) => (node.id === oscillator.id && node.kind === "instrument" ? { ...node, volume: null as unknown as number } : node));
    page({ snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } } });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/the patch/);
    // What is not the patch is there: the state of the connection, and the session as the server sent it.
    expect(screen.getByRole("status").textContent).toBe("Connected");
    openJson();
    expect(shown()).toContain('"volume": null');
    quiet.mockRestore();
  });

  test("leaves the server when it is taken off the page", async () => {
    const { server, view } = page();
    await screen.findByText("Connected");
    expect(server.connections).toBe(1);
    view.unmount();
    expect(server.connections).toBe(0);
  });
});

describe("the computer keyboard", () => {
  /** The notes the page has told the server, in order. */
  const notes = (server: MockServer): string[] =>
    server.received.flatMap((message) => {
      if (message.type === "noteOn") return [`on ${String(message.note)} ${String(message.velocity)}`];
      if (message.type === "noteOff") return [`off ${String(message.note)}`];
      return [];
    });

  /** Wait until the server has been told exactly these notes (what the page sends arrives a moment after the key). */
  const toHaveHeard = (server: MockServer, expected: string[]) =>
    waitFor(() => {
      expect(notes(server)).toEqual(expected);
    });

  /**
   * Prove that what came before a probe was not sent: the probe is a key of the piano, and messages arrive in the
   * order they were sent, so when it has come, so has everything before it that would have.
   */
  async function probe(server: MockServer, before: string[]): Promise<void> {
    fireEvent.keyDown(document.body, { code: "KeyV" });
    fireEvent.keyUp(document.body, { code: "KeyV" });
    await toHaveHeard(server, [...before, "on 53 100", "off 53"]);
  }

  test("plays a note while a key is down, and keeps that key from the browser", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    // (`fireEvent` says whether the event was left alone: false is a key the page took.)
    expect(fireEvent.keyDown(document.body, { code: "KeyZ" })).toBe(false);
    await toHaveHeard(server, ["on 48 100"]);
    expect(fireEvent.keyUp(document.body, { code: "KeyZ" })).toBe(false);
    await toHaveHeard(server, ["on 48 100", "off 48"]);
    // A key that is not on the piano is the browser's, and so is one with Control or Command held.
    expect(fireEvent.keyDown(document.body, { code: "KeyA" })).toBe(true);
    expect(fireEvent.keyDown(document.body, { code: "KeyZ", metaKey: true })).toBe(true);
    expect(fireEvent.keyDown(document.body, { code: "KeyZ", ctrlKey: true })).toBe(true);
    await probe(server, ["on 48 100", "off 48"]);
  });

  test("says which octave it plays at, and the bracket keys change it", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    expect(screen.getByText("Keys: octave 4")).toBeTruthy();
    fireEvent.keyDown(document.body, { code: "BracketRight" });
    expect(screen.getByText("Keys: octave 5")).toBeTruthy();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    await toHaveHeard(server, ["on 60 100"]);
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    expect(screen.getByText("Keys: octave 3")).toBeTruthy();
    // The note that is held ends as it began, at the octave it was played in.
    fireEvent.keyUp(document.body, { code: "KeyZ" });
    await toHaveHeard(server, ["on 60 100", "off 60"]);
  });

  test("leaves the keys to a place that takes text", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    const field = document.createElement("input");
    document.body.append(field);
    expect(fireEvent.keyDown(field, { code: "KeyZ" })).toBe(true);
    expect(fireEvent.keyDown(field, { code: "BracketRight" })).toBe(true);
    expect(screen.getByText("Keys: octave 4")).toBeTruthy();
    field.remove();
    await probe(server, []);
  });

  test("lets go of what it plays when the window loses the focus, and when the page is hidden", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    fireEvent.blur(window);
    await toHaveHeard(server, ["on 48 100", "off 48"]);
    // (The key coming up afterwards has nothing left to end.)
    fireEvent.keyUp(document.body, { code: "KeyZ" });

    fireEvent.keyDown(document.body, { code: "KeyC" });
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    fireEvent(document, new Event("visibilitychange"));
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    await toHaveHeard(server, ["on 48 100", "off 48", "on 52 100", "off 52"]);
    // A page that is shown again is not made to let go of what is held then.
    fireEvent.keyDown(document.body, { code: "KeyB" });
    fireEvent(document, new Event("visibilitychange"));
    await toHaveHeard(server, ["on 48 100", "off 48", "on 52 100", "off 52", "on 55 100"]);
  });
});

describe("the piano", () => {
  const keyOf = (note: number): HTMLElement => {
    const found = document.querySelector<HTMLElement>(`[data-note="${String(note)}"]`);
    if (!found) throw new Error(`no key for note ${String(note)}`);
    return found;
  };

  test("has its place under the patch from the start, so that the canvas never has to give way to it, and has the keys once there is a session", async () => {
    page();
    // (The canvas is fitted to the room it has when it is first drawn: the piano that came a moment after would take some of it.)
    const piano = screen.getByRole("region", { name: "Piano" });
    expect(piano.children).toHaveLength(0);
    await screen.findByRole("group", { name: "Piano keys" });
    expect(piano.children.length).toBeGreaterThan(0);
    expect(within(piano).getByRole("combobox", { name: "Scale root" })).toBeTruthy();
    expect(within(piano).getByRole("combobox", { name: "Scale kind" })).toBeTruthy();
    expect(within(piano).getByRole("button", { name: "Lock to scale" })).toBeTruthy();
    expect(within(piano).getByRole("group", { name: "Piano keys" })).toBeTruthy();
    // C major, as the tracer is: C is the root.
    expect(keyOf(60).dataset.role).toBe("root");
    // It comes after the patch, which has the room that is left.
    const patch = document.querySelector(".tang-app__patch");
    expect(patch?.compareDocumentPosition(piano)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  test("is of the same window as the keys of the computer: the bracket keys move it", async () => {
    page();
    await screen.findByRole("group", { name: "Piano keys" });
    const lowest = (): number => Math.min(...Array.from(document.querySelectorAll<HTMLElement>("[data-note]"), (key) => Number(key.dataset.note)));
    const before = lowest();
    fireEvent.keyDown(document.body, { code: "BracketRight" });
    expect(lowest()).toBe(before + 12);
  });

  test("plays a note for a pointer on a key, through the connection of the page", async () => {
    const { server } = page();
    await screen.findByRole("group", { name: "Piano keys" });
    fireEvent.pointerDown(keyOf(60), { pointerId: 1, button: 0 });
    await waitFor(() => {
      expect(server.received.filter((message) => message.type === "noteOn")).toEqual([{ type: "noteOn", note: 60, velocity: 100 }]);
    });
    fireEvent.pointerUp(window, { pointerId: 1 });
    await waitFor(() => {
      expect(server.received.filter((message) => message.type === "noteOff")).toEqual([{ type: "noteOff", note: 60 }]);
    });
  });

  test("sets the scale of the session when it is chosen, and the session says so", async () => {
    const { client } = page();
    const root = await screen.findByRole<HTMLSelectElement>("combobox", { name: "Scale root" });
    fireEvent.change(root, { target: { value: "7" } });
    await waitFor(() => {
      expect(client.state.snapshot?.meta.piano.scale).toBe("G major");
    });
    expect(keyOf(67).dataset.role).toBe("root");
    expect(client.state.snapshot?.meta.dirty).toBe(true);
  });
});

describe("the bar's button that adds a module (step 111)", () => {
  const space = { lanes: [{ instrument: null, effects: [{ plugin: "builtin:reverb", preset: null, params: {}, mix: 0.4, pinned: [], fed: true }] }], cables: [], modulators: [] };

  test("is there after the other buttons that add, and is shut while no module is saved, which no other button of the bar is", async () => {
    page();
    const buttons = (await screen.findAllByRole("button", { name: /^Add (an instrument|an effect|a modulator|a source of notes|a module)$/ })).map((button) => button.getAttribute("aria-label"));
    expect(buttons).toEqual(["Add an instrument", "Add an effect", "Add a modulator", "Add a source of notes", "Add a module"]);
    const button = screen.getByRole<HTMLButtonElement>("button", { name: "Add a module" });
    expect(button.textContent).toBe("+ Module");
    expect(button.disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Add an effect" }).disabled).toBe(false);
  });

  test("opens a menu of the modules that were saved, under no heading, and asks for the module that is chosen; the box is on the canvas when the server has made it, and the effects' menu has none", async () => {
    const { server } = page({ modules: { Space: space, "Big hall": space } });
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    const button = await screen.findByRole("button", { name: "Add a module" });
    await waitFor(() => {
      expect((button as HTMLButtonElement).disabled).toBe(false);
    });
    fireEvent.click(button);
    const menu = screen.getByRole("menu", { name: "Add a module" });
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Big hall", "Space"]);
    expect(within(menu).queryAllByRole("group")).toEqual([]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Space" }));
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "addFreeEffect", plugin: "module:Space" }]);
    expect(await screen.findByRole("button", { name: "Open Space" })).toBeTruthy();
    // The menu has gone, and the focus is back on the button that opened it.
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(button);
    // The menu of effects is the plugins', and has no module in it.
    fireEvent.click(screen.getByRole("button", { name: "Add an effect" }));
    expect(within(screen.getByRole("menu", { name: "Add an effect" })).queryByRole("menuitem", { name: "Space" })).toBeNull();
  });

  test("says why a module could not be added, in the words of what was asked, among the notices", async () => {
    const { server } = page({ modules: { Space: space } }, { reconnectDelay: () => 60_000 });
    await screen.findByText("Connected");
    const button = await screen.findByRole("button", { name: "Add a module" });
    await waitFor(() => {
      expect((button as HTMLButtonElement).disabled).toBe(false);
    });
    fireEvent.click(button);
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "Space" }));
    expect(await screen.findByText("Could not add the module Space: Not connected to the server.")).toBeTruthy();
  });
});
