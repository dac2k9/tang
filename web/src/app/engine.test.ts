import { afterEach, describe, expect, test, vi } from "vitest";

import { createConnection, engineFor, MOCK_INPUTS, MOCK_LIBRARIES, opensFor, sceneFor } from "./engine";

describe("which scene a page asks of the mock", () => {
  test("is the tracer, unless it asks for the stress scene, the two lanes or the group", () => {
    expect(sceneFor("")).toBe("tracer");
    expect(sceneFor("?engine=mock")).toBe("tracer");
    expect(sceneFor("?scene=tracer")).toBe("tracer");
    expect(sceneFor("?scene=whatever")).toBe("tracer");
    expect(sceneFor("?engine=mock&scene=stress")).toBe("stress");
    expect(sceneFor("?scene=stress&engine=mock")).toBe("stress");
    expect(sceneFor("?engine=mock&scene=lanes")).toBe("lanes");
    expect(sceneFor("?engine=mock&scene=groups")).toBe("groups");
  });

  test("gives the mock the session that was asked for", async () => {
    const at = { protocol: "http:", host: "localhost:5173", hash: "" };
    for (const [search, modulators] of [
      ["?engine=mock", 1],
      ["?engine=mock&scene=stress", 32],
      ["?engine=mock&scene=lanes", 2],
      ["?engine=mock&scene=groups", 1],
    ] as const) {
      const { client } = createConnection({ ...at, search });
      client.start();
      await vi.waitFor(() => {
        expect(client.state.status).toBe("open");
      });
      expect(client.state.snapshot?.graph.nodes.filter((node) => node.kind === "modulator")).toHaveLength(modulators);
      client.stop();
    }
  });
});

describe("what the scenes of the mock are", () => {
  test("the groups scene is the tracer with a group that nothing is in, which the page has no way to make: its lane is in none, and the group has no effect", async () => {
    const { client } = createConnection({ protocol: "http:", host: "localhost:5173", hash: "", search: "?engine=mock&scene=groups" });
    client.start();
    await vi.waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    const nodes = client.state.snapshot?.graph.nodes ?? [];
    const groups = nodes.filter((node) => node.kind === "group");
    expect(groups.map((group) => group.name)).toEqual(["Group 1"]);
    expect(nodes.filter((node) => node.kind === "instrument").map((lane) => lane.parent)).toEqual([null]);
    expect(nodes.filter((node) => node.kind === "effect" && node.parent === groups[0]?.id)).toHaveLength(0);
    // The tracer's own scene has none.
    const plain = createConnection({ protocol: "http:", host: "localhost:5173", hash: "", search: "?engine=mock" }).client;
    plain.start();
    await vi.waitFor(() => {
      expect(plain.state.status).toBe("open");
    });
    expect(plain.state.snapshot?.graph.nodes.filter((node) => node.kind === "group")).toHaveLength(0);
    client.stop();
    plain.stop();
  });

  test("sessions as the program has them once it has loaded them: no modulator is in a lane or a group, and each hears the lane it was in", async () => {
    const at = { protocol: "http:", host: "localhost:5173", hash: "" };
    for (const search of ["?engine=mock", "?engine=mock&scene=stress", "?engine=mock&scene=lanes", "?engine=mock&scene=groups"]) {
      const { client } = createConnection({ ...at, search });
      client.start();
      await vi.waitFor(() => {
        expect(client.state.status).toBe("open");
      });
      const modulators = (client.state.snapshot?.graph.nodes ?? []).flatMap((node) => (node.kind === "modulator" ? [node] : []));
      expect(modulators.length, search).toBeGreaterThan(0);
      const lanes = new Set(client.state.snapshot?.graph.nodes.filter((node) => node.kind === "instrument").map((node) => node.id));
      for (const modulator of modulators) {
        expect(modulator.parent, `${search}: ${modulator.name}`).toBeNull();
        expect(modulator.hears !== undefined && lanes.has(modulator.hears), `${search}: ${modulator.name}`).toBe(true);
      }
      client.stop();
    }
  });
});

describe("the MIDI inputs of the mock", () => {
  test("are open in every scene, so that a source of notes can be added for one: the mock has no MIDI of its own", async () => {
    const at = { protocol: "http:", host: "localhost:5173", hash: "" };
    expect(MOCK_INPUTS).toEqual(["Mock Keyboard MIDI In", "Mock Pad Controller"]);
    for (const search of ["?engine=mock", "?engine=mock&scene=stress", "?engine=mock&scene=lanes", "?engine=mock&scene=groups"]) {
      const { client } = createConnection({ ...at, search });
      client.start();
      await vi.waitFor(() => {
        expect(client.state.status).toBe("open");
      });
      expect(client.state.inputs, search).toEqual(MOCK_INPUTS);
      client.stop();
    }
  });
});

describe("which engine a page asks for", () => {
  test("is the server it came from, unless it says otherwise", () => {
    expect(engineFor("")).toBe("server");
    expect(engineFor("?foo=1")).toBe("server");
    expect(engineFor("?engine=server")).toBe("server");
    expect(engineFor("?engine=whatever")).toBe("server");
  });

  test("is the mock when the address says engine=mock", () => {
    expect(engineFor("?engine=mock")).toBe("mock");
    expect(engineFor("?foo=1&engine=mock")).toBe("mock");
  });

  test("makes a client of the kind", () => {
    const at = { protocol: "http:", host: "localhost:5173", hash: "" };
    expect(createConnection({ ...at, search: "?engine=mock" }).engine).toBe("mock");
    expect(createConnection({ ...at, search: "" }).engine).toBe("server");
    expect(createConnection({ ...at, search: "" }).client.state.status).toBe("idle");
  });
});

/** A WebSocket that does nothing but remember what it was asked to do. */
class FakeSocket {
  static readonly OPEN = 1;
  static instances: FakeSocket[] = [];
  binaryType = "blob";
  readyState = 0;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }
  send(text: string): void {
    this.sent.push(text);
  }
  close(): void {
    // (A real socket reports that it has closed a moment later; nothing here is waiting for that.)
  }
}

describe("the connection of a page", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    FakeSocket.instances = [];
  });

  test("goes to the WebSocket of the server the page came from, and gives it the token in the address", () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const { client } = createConnection({ protocol: "https:", host: "tang.example:9000", search: "", hash: "#token=0123abcd" });
    client.start();
    const socket = FakeSocket.instances[0];
    expect(socket?.url).toBe("wss://tang.example:9000/ws");
    if (!socket) return;
    socket.readyState = FakeSocket.OPEN;
    socket.onopen?.();
    socket.onmessage?.({ data: JSON.stringify({ type: "hello", protocol: 1, rev: 0, features: [], needsToken: true, libraries: [] }) });
    // (It also asks for the telemetry that the views draw: the levels, what the modulators do, and the keys that are held.)
    expect(socket.sent.map((text) => JSON.parse(text) as unknown)).toEqual([
      { type: "auth", token: "0123abcd" },
      { type: "subscribe" },
      { type: "telemetry", channels: ["meters", "modulation", "held"] },
    ]);
    client.stop();
  });

  test("does not open a socket when the engine is the mock", () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const { client } = createConnection({ protocol: "http:", host: "localhost:5173", search: "?engine=mock", hash: "" });
    client.start();
    expect(FakeSocket.instances).toHaveLength(0);
    client.stop();
  });
});

describe("the libraries of sounds of the mock (step 116)", () => {
  const at = { protocol: "http:", host: "localhost:5173", hash: "" };

  test("are asked for by an address that says `open=1`, and by nothing else", () => {
    expect(opensFor("?engine=mock&open=1")).toBe(true);
    expect(opensFor("?open=1&engine=mock")).toBe(true);
    for (const search of ["", "?engine=mock", "?engine=mock&open=0", "?engine=mock&open=yes", "?engine=mock&open="]) expect(opensFor(search), search).toBe(false);
  });

  test("are in the hello of a mock that is asked for them, with the program able to open a session, and in none that is not", async () => {
    const asked = createConnection({ ...at, search: "?engine=mock&open=1" }).client;
    const plain = createConnection({ ...at, search: "?engine=mock" }).client;
    asked.start();
    plain.start();
    await vi.waitFor(() => {
      expect(asked.state.status).toBe("open");
      expect(plain.state.status).toBe("open");
    });
    expect(asked.state.hello?.features).toContain("openSession");
    expect(asked.state.hello?.libraries.map((library) => library.id)).toEqual(MOCK_LIBRARIES.map((library) => library.id));
    expect(plain.state.hello?.features).not.toContain("openSession");
    expect(plain.state.hello?.libraries).toEqual([]);
    const listing = await asked.listSounds("folder1");
    expect(listing.entries.length, "longer than the picker is tall").toBeGreaterThan(10);
    expect(listing.entries.some((entry) => !entry.openable), "and one of them is not a session").toBe(true);
    asked.stop();
    plain.stop();
  });
});
