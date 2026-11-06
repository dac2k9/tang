// Which server the page talks to: the one it came from, or the mock engine that runs in
// the page (`?engine=mock`), which needs nothing but the page itself.
import { Client } from "../client/connection";
import { tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { MockLibrary, MockOptions } from "../client/mock";
import { stressSnapshot } from "../client/stress";
import { serverUrl, tokenFromHash, webSocketTransport } from "../client/websocket";

export type EngineKind = "server" | "mock";

/** The MIDI inputs that the mock engine says are open, for a source of notes to be added for (the real program says which inputs it has opened). */
export const MOCK_INPUTS: readonly string[] = ["Mock Keyboard MIDI In", "Mock Pad Controller"];

/**
 * The libraries of sounds that the mock engine lists when the page is asked to say that its program can open another session (`?open=1`, step 116), for the picker to be seen and tried without a plugin: a folder of saved sessions, with names that
 * say nothing as the ones that Tang makes do, and a folder of examples that is longer than the picker is tall, one of which is not a session.
 */
export const MOCK_LIBRARIES: readonly MockLibrary[] = [
  {
    id: "sessions",
    label: "Saved sessions",
    place: "/Users/someone/.config/tang/sessions",
    entries: [
      { id: "session-19eb859829d.toml", name: "session-19eb859829d", detail: "osc · 2 effects", openable: true, modified: Date.UTC(2026, 6, 7, 11, 10) / 1000 },
      { id: "session-19eb8473abc.toml", name: "session-19eb8473abc", detail: "osc", openable: true, modified: Date.UTC(2026, 5, 11, 22, 10) / 1000 },
      { id: "session-q.toml", name: "session-q", detail: "osc, Pianoteq 9 · 1 effect · 1 modulator", openable: true, modified: Date.UTC(2026, 1, 27, 9, 0) / 1000 },
    ],
  },
  {
    id: "folder1",
    label: "Examples",
    place: "/Users/someone/tang/examples",
    entries: [
      ...["supersaw", "sub-bass", "wobble", "pluck1", "bell-2", "bell-3", "adventure-acid", "king-hippo-rave", "boss-fight", "all-your-base", "hunted", "lost-bird"].map((name, at) => ({
        id: `${name}.toml`,
        name,
        detail: at % 3 === 0 ? "osc · 1 effect" : "osc, helm and 2 more · 3 effects · 2 modulators",
        openable: true,
        modified: Date.UTC(2026, 9, 28 - at, 12) / 1000,
      })),
      { id: "notes.toml", name: "notes", detail: "not a session: expected an equals sign", openable: false, modified: Date.UTC(2026, 0, 3, 12) / 1000 },
    ],
  },
];

/** Whether an address asks the mock engine to say that its program can open another session, with libraries of sounds to open one from (`?open=1`). */
export function opensFor(search: string): boolean {
  return new URLSearchParams(search).get("open") === "1";
}

/**
 * The session the mock engine serves: the tracer, the big one that the performance test runs on, two lanes with an
 * LFO each (where a cable can be drawn out of reach, which the tracer's one lane has no room for), or the tracer with a
 * group, empty (the page has no way to make a group, so that the tests of what the page does with one start from a session that has it).
 */
export type SceneKind = "tracer" | "stress" | "lanes" | "groups";

/** The engine a page address asks for: `?engine=mock` for the mock, the server otherwise. */
export function engineFor(search: string): EngineKind {
  return new URLSearchParams(search).get("engine") === "mock" ? "mock" : "server";
}

/** The scene an address asks of the mock engine: `?scene=stress` for the big one, `?scene=lanes` for the two lanes, `?scene=groups` for the tracer with a group, the tracer otherwise. */
export function sceneFor(search: string): SceneKind {
  const scene = new URLSearchParams(search).get("scene");
  return scene === "stress" || scene === "lanes" || scene === "groups" ? scene : "tracer";
}

/** The snapshot of a scene, as the program has the session once it has loaded it: every modulator is the session's own, and hears the lane or the group it was made for. */
function snapshotOf(scene: SceneKind): MockOptions {
  switch (scene) {
    case "stress":
      return { snapshot: stressSnapshot() };
    case "lanes":
      return { snapshot: stressSnapshot({ lanes: 2, lfosPerLane: 1, targetsPerLfo: 1 }) };
    case "tracer":
      return { snapshot: tracerSnapshot };
    case "groups": {
      // (The tracer, and a group that nothing is in: made the way the mock makes everything, by the intent that the controller's answer is pinned to.)
      const server = new MockServer({ snapshot: tracerSnapshot });
      server.applyQuietly({ type: "addGroup", members: [] });
      return { snapshot: server.snapshot };
    }
  }
}

export interface Connection {
  client: Client;
  engine: EngineKind;
}

/** A client for the page at `location`, not yet started. */
export function createConnection(location: Pick<Location, "protocol" | "host" | "search" | "hash">): Connection {
  const engine = engineFor(location.search);
  const opens: MockOptions = opensFor(location.search) ? { openSession: true, libraries: MOCK_LIBRARIES } : {};
  const mock = (): MockServer => new MockServer({ ...snapshotOf(sceneFor(location.search)), inputs: [...MOCK_INPUTS], ...opens });
  const transport = engine === "mock" ? mock().transport : webSocketTransport(serverUrl(location));
  // The views draw the levels, what the modulators do and the keys that are held, which the server only makes for those who ask.
  return { client: new Client({ transport, token: tokenFromHash(location.hash), telemetry: ["meters", "modulation", "held"] }), engine };
}
