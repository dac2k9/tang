// The connection to the server, as the rest of the app sees it: a store of what the
// client knows (the connection's state, the session) and the calls it makes. It speaks
// the protocol of docs/web-ui.md §7: hello, the token if one is wanted, subscribe, a
// snapshot, and then events that are applied one by one. Nothing here depends on React
// or on the browser: the socket is a `Transport`, so the same client talks to a real
// server (`websocket.ts`) or to the mock engine (`mock.ts`).
import { createStore } from "zustand/vanilla";
import type { StoreApi } from "zustand/vanilla";

import { PROTOCOL } from "../protocol";
import type { CatalogEntry, ClientMessage, ErrorCode, Intent, NodeId, ServerMessage, Snapshot, SoundEntry, SoundLibrary, TelemetryChannel, WireError } from "../protocol";
import { NO_HISTORY } from "./history";
import type { HistoryState } from "./history";
import { applyEvent } from "./reducers";
import { decodeFrame, TelemetryBuffers } from "./telemetry";

/** What a transport tells the client. */
export interface TransportEvents {
  open: () => void;
  text: (data: string) => void;
  binary: (data: ArrayBuffer) => void;
  close: (info: CloseInfo) => void;
}

export interface CloseInfo {
  code: number;
  reason: string;
}

/** One connection to a server, used once: a WebSocket, or the mock engine's stand-in. */
export interface Transport {
  connect: (events: TransportEvents) => void;
  send: (text: string) => void;
  close: () => void;
}

export type TransportFactory = () => Transport;

/**
 * Where the client is.
 * - `idle`: not started.
 * - `connecting`: the first attempt is under way.
 * - `syncing`: the server has said hello and the client has asked for the session (a snapshot is awaited).
 * - `open`: the client has the session and hears of every change.
 * - `reconnecting`: the connection is gone (the session shown, if any, is the last known) and another attempt is coming or under way.
 * - `stopped`: not going on: stopped by the caller, or refused (see `problem`).
 */
export type Status = "idle" | "connecting" | "syncing" | "open" | "reconnecting" | "stopped";

export interface Hello {
  protocol: number;
  rev: number;
  features: readonly string[];
  needsToken: boolean;
  /** The libraries of sounds that the server has, which `listSounds` lists and `openSound` opens a sound of, in the order to show them (step 116); none for a program that cannot open another session. */
  libraries: readonly SoundLibrary[];
}

/** What a library has, as the server last said (`Client.listSounds`). */
export interface SoundListing {
  /** Newest first. */
  entries: readonly SoundEntry[];
  /** The library has more than it lists. */
  truncated: boolean;
}

/** The plugins that can be added to the session, as far as the server has found them. */
export interface Catalog {
  entries: readonly CatalogEntry[];
  /** The server is still looking, and will say again when it has more. */
  scanning: boolean;
}

export interface ClientState {
  status: Status;
  /** Why the client is not going on, in words for a person; null while it is, or is trying to. */
  problem: string | null;
  hello: Hello | null;
  snapshot: Snapshot | null;
  /** What the server can add, as it last said (the one of the connection before, while this one is made good); null until it has. It is no part of the session: telling it changes nothing in `snapshot`. */
  catalog: Catalog | null;
  /**
   * The MIDI inputs the server says are open, by name and in the order it opened them, as it last said: what a source of notes can be made of (`addSource`). None until it says (and where it
   * has no MIDI). It is no part of the session either (a session names devices that are not plugged in), and it is the server's own: the one of the connection before stays while this one is
   * made good, and the server tells it again.
   */
  inputs: readonly string[];
  /**
   * What Undo and Redo would do, in the words of a button (null for what they would not): the server's history, as it last said. It is no part of the
   * session either, and it is the server's own: gone with the connection, and told again by the server the connection is made good to.
   */
  history: HistoryState;
  /** How many times the connection has been lost and made again. */
  reconnects: number;
  /**
   * The nodes whose signal the server says it sends, as it last said (`watchProbe`): those that have a signal and a ring (the engine has a few, shared by every client). A node
   * that is left out has none now, and its frames come if one comes free. Gone with the connection.
   */
  probed: readonly NodeId[];
}

/** A request that was not carried out: the server rejected it, or the connection went before it answered. */
export class RequestError extends Error {
  constructor(
    message: string,
    /** The server's code for it, if it was the server that refused. */
    readonly code: ErrorCode | null,
  ) {
    super(message);
    this.name = "RequestError";
  }
}

export interface ClientOptions {
  transport: TransportFactory;
  /** What a server that wants a token is given. */
  token?: string | undefined;
  /** The telemetry channels to ask for on every connection. */
  telemetry?: readonly TelemetryChannel[];
  /** Milliseconds to wait before the `attempt`th try to connect again (1 for the first). */
  reconnectDelay?: (attempt: number) => number;
  /** Milliseconds a request may wait for its answer before the client says it is slow (`Client.slow`); `SLOW_AFTER` if not given. */
  slowAfter?: number;
}

/** A quarter of a second, doubling up to five seconds. */
const defaultDelay = (attempt: number): number => Math.min(250 * 2 ** (attempt - 1), 5000);

/** A request that has waited this long (milliseconds) for its answer is slow: a plugin that is slow to load, a file that is slow to write. */
export const SLOW_AFTER = 400;

/** What the server says before it closes a connection, when it is not worth trying again. */
const FINAL: readonly ErrorCode[] = ["unauthorized", "badRequest"];

/** What a server's answer to a request is: the revision of the session that an intent or a sound that was opened brought about, or the sounds that a library has. */
type Answer = { type: "ack"; rev: number } | ({ type: "sounds" } & SoundListing);

interface Pending {
  resolve: (answer: Answer) => void;
  reject: (error: RequestError) => void;
  /** Says that it is slow, if it has not been answered by then. */
  timer: ReturnType<typeof setTimeout> | undefined;
  /** Whether it has been told to be slow (and is in the count). */
  slow: boolean;
}

/** The revision that an answer carries, if it is an acknowledgement. */
const acknowledged = (answer: Answer): number | undefined => (answer.type === "ack" ? answer.rev : undefined);

/** The listing that an answer carries, if it is one. */
const listed = (answer: Answer): SoundListing | undefined => (answer.type === "sounds" ? { entries: answer.entries, truncated: answer.truncated } : undefined);

export class Client {
  readonly store: StoreApi<ClientState> = createStore<ClientState>(() => ({
    status: "idle",
    problem: null,
    hello: null,
    snapshot: null,
    catalog: null,
    inputs: [],
    history: NO_HISTORY,
    reconnects: 0,
    probed: [],
  }));
  /** The latest telemetry, for a loop that draws it. */
  readonly telemetry = new TelemetryBuffers();
  /**
   * How many requests have waited longer than `slowAfter` for their answer, for a view that tells the person that the server is busy. It is a store of its
   * own and not part of the state: that is told to every control at every change, and a request goes at every frame of a drag, where this changes only
   * for the few that are slow.
   */
  readonly slow: StoreApi<{ count: number }> = createStore<{ count: number }>(() => ({ count: 0 }));

  readonly #options: ClientOptions;
  #channels: TelemetryChannel[];
  /** The channels that something watches, and how many things do (`watchTelemetry`). */
  readonly #watched = new Map<TelemetryChannel, number>();
  /** The nodes whose signal something watches, and how many things do, in the order they were first asked for (`watchProbe`). */
  readonly #probes = new Map<NodeId, number>();
  #transport: Transport | null = null;
  readonly #pending = new Map<number, Pending>();
  #nextRequest = 1;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #attempt = 0;
  #running = false;
  /** What the server said it was closing the connection for. */
  #closing: WireError | null = null;

  constructor(options: ClientOptions) {
    this.#options = options;
    this.#channels = [...(options.telemetry ?? [])];
  }

  get state(): ClientState {
    return this.store.getState();
  }

  /** Connect, and go on connecting when the connection is lost, until `stop`. */
  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#attempt = 0;
    this.#set({ status: "connecting", problem: null });
    this.#connect();
  }

  /** Disconnect, and stay so. What the client knew stays in the store. */
  stop(): void {
    this.#running = false;
    clearTimeout(this.#timer);
    this.#timer = undefined;
    const transport = this.#transport;
    this.#transport = null;
    transport?.close();
    this.#rejectPending("The client was stopped.");
    this.#set({ status: "stopped" });
  }

  /**
   * Ask the server to apply an intent. Resolves with the revision of the session it
   * brought about once the server has answered, by which time the change is in the
   * store (the event comes before the answer); rejects with a `RequestError` if the
   * server refused it or the connection went first.
   */
  request(intent: Intent): Promise<number> {
    return this.#ask((reqId) => ({ type: "request", reqId, intent }), acknowledged);
  }

  /**
   * Ask what the library `library` (one that the hello lists) has (step 116): resolves with its sounds, newest first, once the server has answered (a library that is far away takes its time); rejects with a `RequestError` if the
   * server could not list it (a folder that is gone, a server that does not answer) or the connection went first. It is no change of the session: it is not an intent.
   */
  listSounds(library: string): Promise<SoundListing> {
    return this.#ask((reqId) => ({ type: "listSounds", reqId, library }), listed);
  }

  /**
   * Open the sound `id` of `library` in place of the session (step 116): the server reads it (or fetches it) and goes on as `request` does with an `openSession` intent. Resolves with the revision that it used, rejects with a
   * `RequestError` if the server refused it (the sound is gone, is not a session, the program cannot open another session) or the connection went first.
   */
  openSound(library: string, id: string): Promise<number> {
    return this.#ask((reqId) => ({ type: "openSound", reqId, library, id }), acknowledged);
  }

  /** Send the message that `build` makes of a request id, and wait for the answer to it, which `take` makes the result of (or `undefined` if it is not one that the message gets). */
  #ask<T>(build: (reqId: number) => ClientMessage, take: (answer: Answer) => T | undefined): Promise<T> {
    if (!this.#live) return Promise.reject(new RequestError("Not connected to the server.", null));
    const reqId = this.#nextRequest;
    this.#nextRequest = (this.#nextRequest + 1) >>> 0 || 1;
    return new Promise<T>((resolve, reject) => {
      const answered = (answer: Answer): void => {
        const result = take(answer);
        if (result === undefined) reject(new RequestError("The server answered with something this page did not ask for.", null));
        else resolve(result);
      };
      const pending: Pending = { resolve: answered, reject, timer: undefined, slow: false };
      pending.timer = setTimeout(() => {
        pending.slow = true;
        this.#slowBy(1);
      }, this.#options.slowAfter ?? SLOW_AFTER);
      this.#pending.set(reqId, pending);
      this.#send(build(reqId));
    });
  }

  /** Ask for these telemetry channels (and no others but those that something watches), now and on every connection after. */
  setTelemetry(channels: readonly TelemetryChannel[]): void {
    this.#channels = [...channels];
    this.#sendChannels();
  }

  /**
   * Ask for `channel` as well, until the function that is returned is called: for a channel that costs something to send and that only a
   * picture on the screen uses (the scope). Several can watch one channel, and it is asked for until the last of them lets go; letting go twice
   * is letting go once. A new list of channels (`setTelemetry`) does not say what is watched.
   */
  watchTelemetry(channel: TelemetryChannel): () => void {
    this.#watched.set(channel, (this.#watched.get(channel) ?? 0) + 1);
    this.#sendChannels();
    let watching = true;
    return () => {
      if (!watching) return;
      watching = false;
      const left = (this.#watched.get(channel) ?? 1) - 1;
      if (left > 0) this.#watched.set(channel, left);
      else this.#watched.delete(channel);
      this.#sendChannels();
    };
  }

  /**
   * Ask for the signal at the output of `node` (an instrument, an effect, a group, a bus effect), as frames of its own (`telemetry.probe`), until the function that is returned is
   * called: for a picture of it on the screen. Several can watch a node, and it is asked for until the last of them lets go; letting go twice is letting go once. What the server says
   * it watches is in the state (`probed`).
   */
  watchProbe(node: NodeId): () => void {
    const count = this.#probes.get(node) ?? 0;
    this.#probes.set(node, count + 1);
    if (count === 0) this.#sendProbes();
    let watching = true;
    return () => {
      if (!watching) return;
      watching = false;
      const left = (this.#probes.get(node) ?? 1) - 1;
      if (left > 0) {
        this.#probes.set(node, left);
      } else {
        this.#probes.delete(node);
        this.#sendProbes();
      }
    };
  }

  #sendProbes(): void {
    if (this.#live) this.#send({ type: "probes", nodes: [...this.#probes.keys()] });
  }

  /** The channels that are asked for: those of the client and those that something watches. */
  get #wanted(): TelemetryChannel[] {
    return [...new Set([...this.#channels, ...this.#watched.keys()])];
  }

  #sendChannels(): void {
    if (this.#live) this.#send({ type: "telemetry", channels: this.#wanted });
  }

  noteOn(note: number, velocity = 100): void {
    if (this.#live) this.#send({ type: "noteOn", note, velocity });
  }

  noteOff(note: number): void {
    if (this.#live) this.#send({ type: "noteOff", note });
  }

  /** Let go of everything that sounds, whoever plays it: the server ends every note it holds, with the pedal and all notes off and all sound off on every channel. */
  panic(): void {
    if (this.#live) this.#send({ type: "panic" });
  }

  /** The server has said hello and the connection is up. */
  get #live(): boolean {
    const { status } = this.store.getState();
    return this.#transport !== null && (status === "syncing" || status === "open");
  }

  #set(partial: Partial<ClientState>): void {
    this.store.setState(partial);
  }

  #send(message: ClientMessage): void {
    this.#transport?.send(JSON.stringify(message));
  }

  #connect(): void {
    const transport = this.#options.transport();
    this.#transport = transport;
    this.#closing = null;
    // What an abandoned transport still says is of no interest.
    const current = (): boolean => this.#transport === transport;
    transport.connect({
      open: () => {
        // The server speaks first.
      },
      text: (data) => {
        if (current()) this.#onText(data);
      },
      binary: (data) => {
        if (current()) this.#onFrame(data);
      },
      close: () => {
        if (current()) this.#onClose();
      },
    });
  }

  #onText(data: string): void {
    let message: ServerMessage;
    try {
      message = JSON.parse(data) as ServerMessage;
    } catch {
      this.#giveUp("The server sent something that is not a message. Reload the page.");
      return;
    }
    switch (message.type) {
      case "hello":
        this.#onHello(message);
        break;
      case "snapshot":
        this.telemetry.setLayout(message.snapshot.telemetry);
        this.#attempt = 0;
        this.#set({ snapshot: message.snapshot, status: "open", problem: null });
        break;
      case "event": {
        const { snapshot } = this.store.getState();
        if (!snapshot) break;
        const applied = applyEvent(snapshot, message.event);
        if (applied.ok) {
          if (applied.snapshot !== snapshot) this.#set({ snapshot: applied.snapshot });
        } else {
          // Something was missed: start again from a snapshot (the events go on meanwhile).
          this.#set({ status: "syncing" });
          this.#send({ type: "subscribe" });
        }
        break;
      }
      case "layout": {
        this.telemetry.setLayout(message.layout);
        const { snapshot } = this.store.getState();
        if (snapshot) this.#set({ snapshot: { ...snapshot, telemetry: message.layout } });
        break;
      }
      case "catalog":
        // The whole of it each time: what the scan has found so far replaces what it had found.
        this.#set({ catalog: { entries: message.entries, scanning: message.scanning } });
        break;
      case "inputs": {
        // The whole of it each time, and nothing is told to the page that is the same.
        const { inputs } = this.store.getState();
        if (inputs.length !== message.names.length || inputs.some((name, i) => name !== message.names[i])) this.#set({ inputs: message.names });
        break;
      }
      case "probes": {
        // The whole of it each time, and nothing is told to the page that is the same.
        const { probed } = this.store.getState();
        if (probed.length !== message.nodes.length || probed.some((node, i) => node !== message.nodes[i])) this.#set({ probed: message.nodes });
        break;
      }
      case "history": {
        // The whole of it each time, and nothing is told to the page that is the same.
        const { history } = this.store.getState();
        if (history.undo !== message.undo || history.redo !== message.redo) this.#set({ history: { undo: message.undo, redo: message.redo } });
        break;
      }
      case "ack":
        this.#settle(message.reqId, (pending) => {
          pending.resolve({ type: "ack", rev: message.rev });
        });
        break;
      case "sounds":
        this.#settle(message.reqId, (pending) => {
          pending.resolve({ type: "sounds", entries: message.entries, truncated: message.truncated });
        });
        break;
      case "rejected":
        this.#settle(message.reqId, (pending) => {
          pending.reject(new RequestError(message.error.message, message.error.code));
        });
        break;
      case "closing":
        this.#closing = message.error;
        break;
    }
  }

  #onHello(hello: Hello): void {
    this.#set({ hello: { protocol: hello.protocol, rev: hello.rev, features: hello.features, needsToken: hello.needsToken, libraries: hello.libraries } });
    if (hello.protocol !== PROTOCOL) {
      this.#giveUp(
        `This page speaks protocol ${PROTOCOL} and the server speaks ${hello.protocol}: the page and the program are not from the same build.`,
      );
      return;
    }
    if (hello.needsToken) {
      const { token } = this.#options;
      if (!token) {
        this.#giveUp("This server wants a token: open the address Tang printed when it started, which has it after the #.");
        return;
      }
      this.#send({ type: "auth", token });
    }
    this.#send({ type: "subscribe" });
    if (this.#wanted.length > 0) this.#send({ type: "telemetry", channels: this.#wanted });
    if (this.#probes.size > 0) this.#send({ type: "probes", nodes: [...this.#probes.keys()] });
    this.#set({ status: "syncing" });
  }

  #onFrame(data: ArrayBuffer): void {
    const frame = decodeFrame(data);
    if (frame) this.telemetry.accept(frame);
  }

  #onClose(): void {
    this.#transport = null;
    // (The notes that were held, as far as this page knew, are held no more as far as it knows, and the sound that it drew is not one that it hears.)
    this.telemetry.forgetHeld();
    this.telemetry.forgetScope();
    this.telemetry.forgetProbes();
    if (this.store.getState().probed.length > 0) this.#set({ probed: [] });
    this.#rejectPending("The connection was lost before the server answered.");
    const closing = this.#closing;
    this.#closing = null;
    if (!this.#running) return;
    if (closing && FINAL.includes(closing.code)) {
      this.#giveUp(closing.message);
      return;
    }
    this.#attempt += 1;
    const delay = (this.#options.reconnectDelay ?? defaultDelay)(this.#attempt);
    // (The history is the server's, and this one may have lost it: it is told again once the session is.)
    this.#set({ status: "reconnecting", history: NO_HISTORY, reconnects: this.store.getState().reconnects + 1 });
    this.#timer = setTimeout(() => {
      this.#timer = undefined;
      if (this.#running) this.#connect();
    }, delay);
  }

  /** Stop for good, because trying again would not help: say why. */
  #giveUp(problem: string): void {
    this.#running = false;
    const transport = this.#transport;
    this.#transport = null;
    transport?.close();
    this.#rejectPending(problem);
    this.#set({ status: "stopped", problem });
  }

  #settle(reqId: number, how: (pending: Pending) => void): void {
    const pending = this.#pending.get(reqId);
    if (!pending) return;
    this.#pending.delete(reqId);
    this.#forget(pending);
    how(pending);
  }

  #rejectPending(why: string): void {
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const p of pending) {
      this.#forget(p);
      p.reject(new RequestError(why, null));
    }
  }

  /** A request is over, answered or not: it is not going to be slow, or it is not slow any more. */
  #forget(pending: Pending): void {
    clearTimeout(pending.timer);
    if (pending.slow) this.#slowBy(-1);
  }

  #slowBy(change: number): void {
    this.slow.setState({ count: this.slow.getState().count + change });
  }
}
