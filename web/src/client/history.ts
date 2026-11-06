// The history of what a person turned, and the way back (docs/web-ui.md §5 and D35): the mock engine's, which has to be the controller's
// (`src/app/history.rs`), so that what is built on the mock is built on what the program does; the fixture `history.json` pins the two together.
//
// Every change that can be undone leaves a step: the intents that put it back and the intents that do it again, in the words a button has for
// it. A change that goes on (a knob that is dragged is a change at every frame) is one step: a change of what the last step was of, at most a
// second after the last of it, is merged into it, and the step goes back to what the first change found.
//
// What is added is taken out by undo and put back by redo under the ids it had (`Made`): every step that names the node or the cable goes on naming it.
import { HISTORY_MAX_STEPS, HISTORY_SAME_STEP, WAVEFORMS } from "../protocol";
import type { CatalogEntry, EdgeId, Endpoint, Gain, Graph, Intent, ModulatorField, Node, NodeId, Snapshot, TelemetryLayout } from "../protocol";
import { cablesThatGo, drivesRoundToItself, isAudioCable, isModulation, isNotesCable } from "./cables";
import { trimmed } from "./inputs";
import { macroExposed, macroName } from "./macros";
import { MODULATOR_LABELS, kindOf } from "./modulatorKinds";
import { boxTitle, moduleNameOf, tidyName } from "./modules";

/** What a change is of, for a change that goes on to be told from the rest: a string that says which parameter, gain, setting or cable. */
export type Key = string;

/** What the buttons need to say: the words of what Undo and Redo would do, or null for what they would not. */
export interface HistoryState {
  undo: string | null;
  redo: string | null;
}

/** What the buttons say when there is nothing to undo or redo (and what they say before the server has told). */
export const NO_HISTORY: HistoryState = { undo: null, redo: null };

/** The ids that what was done made, each in the order it was handed out: the nodes and the cables. */
export interface Made {
  nodes: NodeId[];
  edges: EdgeId[];
}

/** Nothing made. (A new one each time: what is made is written to.) */
export const noneMade = (): Made => ({ nodes: [], edges: [] });

/** What a removal took with it, for it to be put back: the nodes and the cables of the session and the layout of its telemetry as they were before it. */
export interface Taken {
  graph: Graph;
  telemetry: TelemetryLayout;
}

/** One thing that was done, and the way back. */
export interface Step {
  /** What it was, for a button: "set cutoff". */
  label: string;
  /** The intents that put it back, in the order they are applied. */
  undo: Intent[];
  /** The intents that do it again, in the order they are applied. */
  redo: Intent[];
  /** What it was of, if it is a change that goes on. */
  key: Key | null;
  /** When it was last changed, in seconds. */
  last: number;
  /** The ids that `undo` makes, in the order it makes them (none, for most). */
  undoMade: Made;
  /** The ids that `redo` makes, in the order it makes them: what was added is added again under the ids it had. */
  redoMade: Made;
  /** For a step that took something out: the session as it was, which undo puts back (a removal is undone by being put back, and not by intents). */
  taken: Taken | null;
}

/** A step of one change: `undo` puts back what it found, `redo` is the change. */
export function stepOf(label: string, undo: Intent, redo: Intent, key: Key | null, at: number): Step {
  return { label, undo: [undo], redo: [redo], key, last: at, undoMade: noneMade(), redoMade: noneMade(), taken: null };
}

/** A step of an add: `redo` is the intent, which made `made` the first time, and `undo` takes it out (the intents that do, in the order they are applied). */
export function addedStep(label: string, undo: readonly Intent[], redo: Intent, made: Made, at: number): Step {
  return { label, undo: [...undo], redo: [redo], key: null, last: at, undoMade: noneMade(), redoMade: made, taken: null };
}

/** A step of a removal: `redo` is the intent that took something out, and `taken` is the session as it was, which undo puts back. */
export function removedStep(label: string, redo: Intent, taken: Taken, at: number): Step {
  return { label, undo: [], redo: [redo], key: null, last: at, undoMade: noneMade(), redoMade: noneMade(), taken };
}

/** The steps that can be undone, the last on top, and the steps that were undone and can be done again. */
export class History {
  readonly #undo: Step[] = [];
  readonly #redo: Step[] = [];

  /**
   * Something was done. What could have been redone cannot be any more. If it is another change of what the last step was of, soon after
   * it, the two are one step: it puts back what the first found and does what the last did.
   */
  record(step: Step): void {
    this.#redo.length = 0;
    const top = this.#undo.at(-1);
    if (top && step.key !== null && top.key === step.key && step.last - top.last <= HISTORY_SAME_STEP) {
      top.redo = step.redo;
      top.last = step.last;
      return;
    }
    this.#undo.push(step);
    if (this.#undo.length > HISTORY_MAX_STEPS) this.#undo.shift();
  }

  /** The step to undo, taken off (it is gone if it cannot be put back; `keptUndone` keeps it to be redone if it was). */
  takeUndo(): Step | undefined {
    return this.#undo.pop();
  }

  /** The step that was undone, to be redone. */
  keptUndone(step: Step): void {
    this.#redo.push(step);
  }

  /** The step to redo, taken off. */
  takeRedo(): Step | undefined {
    return this.#redo.pop();
  }

  /** The step that was redone, to be undone again: as it was, and not merged into the one before it. */
  keptRedone(step: Step): void {
    this.#undo.push(step);
  }

  /** Forget everything: something was done that cannot be undone, and what came before cannot be undone past it. True if there was something to forget. */
  clear(): boolean {
    const had = this.#undo.length > 0 || this.#redo.length > 0;
    this.#undo.length = 0;
    this.#redo.length = 0;
    return had;
  }

  /** What Undo and Redo would do. */
  state(): HistoryState {
    return { undo: this.#undo.at(-1)?.label ?? null, redo: this.#redo.at(-1)?.label ?? null };
  }

  /** How many steps can be undone (for tests). */
  get undoable(): number {
    return this.#undo.length;
  }

  /** How many steps can be redone (for tests). */
  get redoable(): number {
    return this.#redo.length;
  }
}

/** An add that is carried out, whose step is made once it is known what it made. */
export interface Added {
  label: string;
  intent: Intent;
  /** When it was done, in seconds. */
  at: number;
}

/** A removal that is carried out: its step holds the session as it was before it, which is known only when it is carried out. */
export interface Removed {
  label: string;
  /** The intent that took something out, which redo does again. */
  intent: Intent;
  /** When it was done, in seconds. */
  at: number;
}

/**
 * What a change is to the history: a step; or an add, whose step is made when it is carried out; or a removal, whose step holds what was taken out; or
 * something that cannot be undone yet (and what came before cannot be undone past it); or no change.
 */
export type Plan = { step: Step } | { added: Added } | { removed: Removed } | "barrier" | "nothing";

/**
 * The intents that take out what `added`, an add that was carried out, made: the one node, or the one cable (an effect that stands by itself: its lane), or the lanes and the modulators of a module, the last that was made first, as they are in
 * `snapshot`; or null if it made something that cannot be named, or what it made is not there. As `takes_out` of the controller.
 */
export function takesOut(snapshot: Snapshot, added: Intent, made: Made): Intent[] | null {
  // A module that was collapsed made its record and nothing else: undo takes the record away, and the lanes and the modulators in it stay.
  if (added.type === "collapseIntoModule") {
    const [node, ...more] = made.nodes;
    return node !== undefined && more.length === 0 && snapshot.graph.nodes.some((n) => n.id === node && n.kind === "module") ? [{ type: "ungroupModule", node }] : null;
  }
  // A module made a lane for each of its own, and the plugins in them, and the cables between the plugins, and then its modulators with their cables: each lane and each modulator is taken out, with all that is in it and the cables
  // at it, the last made first (the modulators, which come last, go first).
  if (added.type === "addFreeEffect" && moduleNameOf(added.plugin) !== null) {
    const taken = made.nodes.filter((id) => snapshot.graph.nodes.some((n) => n.id === id && (n.kind === "instrument" || n.kind === "modulator")));
    return taken.length === 0 ? null : taken.reverse().map((node): Intent => ({ type: "removeNode", node }));
  }
  const one = takesOutOne(snapshot, added, made);
  return one === null ? null : [one];
}

/** The intent that takes out what `added`, an add that was carried out, made when it made one thing (a node, a cable, or a lane with its effect). */
function takesOutOne(snapshot: Snapshot, added: Intent, made: Made): Intent | null {
  // An audio cable and a cable of notes are one cable and no node.
  if (added.type === "addCable") {
    const [edge, ...more] = made.edges;
    if (edge === undefined || more.length > 0 || made.nodes.length > 0) return null;
    return snapshot.graph.edges.some((e) => (isAudioCable(e) || isNotesCable(e)) && e.id === edge) ? { type: "removeEdge", edge } : null;
  }
  if (added.type === "addModEdge") {
    const [edge, ...more] = made.edges;
    if (edge === undefined || more.length > 0 || made.nodes.length > 0) return null;
    return snapshot.graph.edges.some((e) => e.kind === "modulation" && e.id === edge) ? { type: "removeEdge", edge } : null;
  }
  // An effect that stands by itself makes the lane that holds it, and then the effect, and no cable: it is the lane that is taken out, with the effect in it.
  if (added.type === "addFreeEffect") {
    const [lane, effect, ...rest] = made.nodes;
    if (lane === undefined || effect === undefined || rest.length > 0 || made.edges.length > 0) return null;
    return snapshot.graph.nodes.some((n) => n.id === lane && n.kind === "instrument") ? { type: "removeNode", node: lane } : null;
  }
  const [node, ...more] = made.nodes;
  if (node === undefined || more.length > 0) return null;
  // A modulate makes a modulator and a cable, which goes with the modulator: it is the node that is taken out. Nothing else makes a cable besides a node.
  if (made.edges.length !== (added.type === "modulate" ? 1 : 0)) return null;
  return snapshot.graph.nodes.some((n) => n.id === node) ? { type: "removeNode", node } : null;
}

/** What a modulate calls what it drives (as the controller does): a parameter by its name, a setting of a modulator or the depth of a cable by the name of the port; none for what is neither. */
function drivenName(snapshot: Snapshot, to: Endpoint): string | undefined {
  const { port } = to;
  switch (port.type) {
    case "param": {
      const node = snapshot.graph.nodes.find((n) => n.id === to.node);
      const plugin = node && (node.kind === "instrument" || node.kind === "effect") ? node.plugin : null;
      return plugin?.params.find((p) => p.index === port.index)?.name;
    }
    case "rate":
    case "attack":
    case "decay":
    case "sustain":
    case "release":
    case "depth":
    case "volume":
    case "mix":
    case "pan":
      return port.type;
    case "modOut":
    case "eventsIn":
    case "eventsOut":
    case "audioIn":
    case "audioInSide":
    case "audioOut":
      return undefined;
  }
}

const GAIN_LABELS: Record<Gain, string> = {
  volume: "set volume",
  mix: "set mix",
  groupVolume: "set the group's volume",
  groupPan: "set pan",
  masterVolume: "set the master volume",
  limiterCeiling: "set the limiter's ceiling",
};

/** The host-side gain `gain` of `node`, if it has it. */
function gainOf(node: Node, gain: Gain): number | undefined {
  if (gain === "volume" && node.kind === "instrument") return node.volume;
  if (gain === "mix" && node.kind === "effect") return node.mix;
  if (gain === "groupVolume" && node.kind === "group") return node.volume;
  if (gain === "groupPan" && node.kind === "group") return node.pan;
  if (gain === "masterVolume" && node.kind === "master") return node.volume;
  if (gain === "limiterCeiling" && node.kind === "master") return node.ceiling;
  return undefined;
}

/** The value of a setting of a modulator, if its kind has it. */
function fieldOf(node: Node, field: ModulatorField): number | undefined {
  if (node.kind !== "modulator") return undefined;
  const { source } = node;
  if (source.type === "lfo") {
    if (field === "rate") return source.rate;
    if (field === "waveform") return WAVEFORMS.indexOf(source.waveform);
    if (field === "retrigger") return source.retrigger ? 1 : 0;
    if (field === "fade") return source.fade;
    if (field === "sync") return source.sync;
    return undefined;
  }
  if (source.type === "envelope") {
    if (field === "attack") return source.attack;
    if (field === "decay") return source.decay;
    if (field === "sustain") return source.sustain;
    if (field === "release") return source.release;
  }
  if (source.type === "macro" && field === "value") return source.value;
  if (source.type === "steps") {
    if (field === "rate" || field === "glide" || field === "length") return source[field];
    if (field === "retrigger") return source.retrigger ? 1 : 0;
    if (field === "sync") return source.sync;
  }
  return undefined;
}

/**
 * What a lane or a group is called in the words of a step: "the lane of Oscillator", the group's name, or, for a lane that has no instrument, what it holds: the name of its one effect, "a chain of effects" for more
 * and "an empty lane" for none. As `lane_words` of the controller.
 */
export function laneOrGroupWords(nodes: readonly Node[], node: Extract<Node, { kind: "instrument" | "group" }>): string {
  if (node.kind === "group") return node.name;
  if (node.plugin) return `the lane of ${node.name}`;
  const [only, ...more] = nodes.filter((n) => n.kind === "effect" && n.parent === node.id);
  if (only === undefined) return "an empty lane";
  return more.length === 0 ? only.name : "a chain of effects";
}

/**
 * What `intent` is to the history, read from the session before it is applied: the step that puts back what it will change (so it is the value it
 * will overwrite that is read), or that it cannot be undone, or that it is no change. As `plan_history` of the controller.
 */
export function planOf(snapshot: Snapshot, intent: Intent, now: number, catalog: readonly CatalogEntry[]): Plan {
  const nodeOf = (id: number): Node | undefined => snapshot.graph.nodes.find((n) => n.id === id);
  const step = (label: string, undo: Intent, key: Key | null): Plan => ({ step: stepOf(label, undo, intent, key, now) });
  const added = (label: string): Plan => ({ added: { label, intent, at: now } });
  const catalogName = (plugin: string): string | undefined => catalog.find((entry) => entry.id === plugin)?.name;
  switch (intent.type) {
    // (What is done to a pattern is no step: it is not put back by undo, and does not forget what was done before it. Nor is a module that is saved: it is a file of the library, and no part of the session.)
    case "setRecording":
    case "clearPattern":
    case "setPatternField":
    case "save":
    case "saveAs":
    case "saveModule":
    case "saveSelectionAsModule":
    case "openEditor":
    case "openSession":
    case "undo":
    case "redo":
      return "nothing";
    // A source of notes that is added is an add (a refusal leaves no step: the step is made once it is carried out), in the words of the button that adds it: the keys, or the device by its name without
    // the spaces round it.
    case "addSource":
      return added(intent.source.type === "keys" ? "add Keys" : `add ${trimmed(intent.source.name)}`);
    // A cable that is drawn is an add too, and its level (an audio cable's: a cable of notes has none) a step that a drag makes one of, put back to the level it found. The ports tell which cable it is.
    case "addCable":
      return added(intent.from.port.type === "eventsOut" || intent.to.port.type === "eventsIn" ? "draw a cable of notes" : "draw an audio cable");
    case "setCableGain": {
      const cable = snapshot.graph.edges.find((e) => isAudioCable(e) && e.id === intent.edge);
      if (cable?.kind !== "cable") return "nothing";
      return step("set the level of an audio cable", { type: "setCableGain", edge: intent.edge, gain: cable.gain }, `level:${String(intent.edge)}`);
    }
    case "setParam": {
      const node = nodeOf(intent.node);
      const plugin = node && (node.kind === "instrument" || node.kind === "effect") ? node.plugin : null;
      const row = plugin?.params.find((p) => p.index === intent.param);
      if (!row) return "nothing";
      return step(`set ${row.name}`, { type: "setParam", node: intent.node, param: intent.param, value: row.value }, `param:${String(intent.node)}:${String(intent.param)}`);
    }
    case "setGain": {
      const node = nodeOf(intent.node);
      const old = node ? gainOf(node, intent.gain) : undefined;
      if (old === undefined) return "nothing";
      return step(GAIN_LABELS[intent.gain], { type: "setGain", node: intent.node, gain: intent.gain, value: old }, `gain:${String(intent.node)}:${intent.gain}`);
    }
    // (The limiter on the output mix: what is as it is asked to be changes nothing, and no step is kept for it.)
    case "setLimiter": {
      const master = snapshot.graph.nodes.find((n) => n.kind === "master");
      if (master?.kind !== "master" || master.limiter === intent.on) return "nothing";
      return step(`turn the limiter ${intent.on ? "on" : "off"}`, { type: "setLimiter", on: master.limiter }, null);
    }
    case "setModulatorField": {
      const node = nodeOf(intent.node);
      const old = node ? fieldOf(node, intent.field) : undefined;
      if (old === undefined) return "nothing";
      return step(`set ${intent.field}`, { type: "setModulatorField", node: intent.node, field: intent.field, value: old }, `field:${String(intent.node)}:${intent.field}`);
    }
    // All the steps of a step sequencer are one thing to the history: a stroke across the bars is one step, which puts back what the stroke found.
    case "setSteps": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "modulator" || node.source.type !== "steps") return "nothing";
      return step("set steps", { type: "setSteps", node: intent.node, values: node.source.values }, `steps:${String(intent.node)}`);
    }
    case "setEdgeDepth": {
      const edge = snapshot.graph.edges.find((e) => e.kind === "modulation" && e.id === intent.edge);
      if (edge?.kind !== "modulation") return "nothing";
      return step("set the depth of a cable", { type: "setEdgeDepth", edge: intent.edge, depth: edge.amount }, `depth:${String(intent.edge)}`);
    }
    case "setRange": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "instrument") return "nothing";
      return step("set the keys", { type: "setRange", node: intent.node, range: node.range }, null);
    }
    // (A node that is neither a lane nor a group is refused, and what is as it is asked to be changes nothing: no step is kept for either.)
    case "setMute": {
      const node = nodeOf(intent.node);
      if ((node?.kind !== "instrument" && node?.kind !== "group") || node.muted === intent.muted) return "nothing";
      return step(`${intent.muted ? "mute" : "unmute"} ${laneOrGroupWords(snapshot.graph.nodes, node)}`, { type: "setMute", node: intent.node, muted: node.muted }, null);
    }
    case "setSolo": {
      const node = nodeOf(intent.node);
      if ((node?.kind !== "instrument" && node?.kind !== "group") || node.solo === intent.solo) return "nothing";
      return step(`${intent.solo ? "solo" : "unsolo"} ${laneOrGroupWords(snapshot.graph.nodes, node)}`, { type: "setSolo", node: intent.node, solo: node.solo }, null);
    }
    // (As a mute is: what is neither a lane nor a group is refused, and a way out that is as it is asked to be changes nothing.)
    case "setRoute": {
      const node = nodeOf(intent.node);
      if ((node?.kind !== "instrument" && node?.kind !== "group") || node.route === intent.route) return "nothing";
      const words = laneOrGroupWords(snapshot.graph.nodes, node);
      return step(intent.route === "none" ? `cut the way out of ${words}` : `give ${words} its way out`, { type: "setRoute", node: intent.node, route: node.route }, null);
    }
    // (As a way out is: what is no modulator of the session's own is refused, an ear that is neither a lane nor a group is refused too, and what is as it is asked to be changes nothing: no step for any.)
    case "setModulatorEar": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "modulator" || node.parent !== null || (node.hears ?? null) === intent.ear) return "nothing";
      const heard = intent.ear === null ? null : nodeOf(intent.ear);
      if (heard !== null && heard?.kind !== "instrument" && heard?.kind !== "group") return "nothing";
      const kind = MODULATOR_LABELS[kindOf(node.source)];
      return step(heard === null ? `make ${kind} hear every note` : `make ${kind} hear ${laneOrGroupWords(snapshot.graph.nodes, heard)}`, { type: "setModulatorEar", node: intent.node, ear: node.hears ?? null }, null);
    }
    // (As a way out is: what is no effect is refused, and a feed that is as it is asked to be changes nothing.)
    case "setFeed": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "effect" || node.feed === intent.feed) return "nothing";
      return step(intent.feed === "none" ? `cut the input of ${node.name}` : `give ${node.name} its input back`, { type: "setFeed", node: intent.node, feed: node.feed }, null);
    }
    case "setNodeUi": {
      const node = nodeOf(intent.node);
      if (!node) return "nothing";
      return step("move a node", { type: "setNodeUi", node: intent.node, ui: node.ui ?? null }, null);
    }
    case "setPinned": {
      const node = nodeOf(intent.node);
      const plugin = node && (node.kind === "instrument" || node.kind === "effect") ? node.plugin : null;
      if (!plugin) return "nothing";
      return step("choose the knobs", { type: "setPinned", node: intent.node, params: [...plugin.pinned] }, null);
    }
    case "renameGroup": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "group") return "nothing";
      // (The graph has the name a group is called by, and not whether it was ever given one: one that is called by its place has none.)
      const place = snapshot.graph.nodes.filter((n) => n.kind === "group").findIndex((n) => n.id === node.id);
      const old = node.name === `Group ${String(place + 1)}` ? "" : node.name;
      return step("rename a group", { type: "renameGroup", node: intent.node, name: old }, null);
    }
    // (As a group's name is: what is no macro is refused, a name that is refused is no step, and a name that the macro has changes nothing: no step is kept for any. The graph says the name that a macro was given and nothing for none.)
    case "renameMacro": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "modulator" || node.source.type !== "macro") return "nothing";
      const tidied = tidyName(intent.name);
      const old = macroName(node.source);
      if ("error" in tidied || tidied.name === old) return "nothing";
      return step("rename a macro", { type: "renameMacro", node: intent.node, name: old }, null);
    }
    // (As a mute is: what is no macro is refused, and a flag that is as it is asked to be changes nothing: no step is kept for either.)
    case "exposeMacro": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "modulator" || node.source.type !== "macro" || macroExposed(node.source) === intent.expose) return "nothing";
      return step(intent.expose ? "show a macro on its box" : "hide a macro from its box", { type: "exposeMacro", node: intent.node, expose: !intent.expose }, null);
    }
    case "setScale":
      return step("set the scale", { type: "setScale", scale: snapshot.meta.piano.scale }, null);
    case "setLocked":
      return step(intent.locked ? "lock the piano to its scale" : "let every note of the piano through", { type: "setLocked", locked: snapshot.meta.piano.locked }, null);
    case "setBpm":
      return step("set the tempo", { type: "setBpm", bpm: snapshot.meta.bpm }, "tempo");
    case "moveEffect": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "effect") return "nothing";
      const chain = snapshot.graph.nodes.filter((n) => n.kind === "effect" && n.parent === node.parent);
      return step("move an effect", { type: "moveEffect", node: intent.node, index: chain.findIndex((n) => n.id === node.id) }, null);
    }
    case "addEffect": {
      // (The catalog names the plugin, or the intent is refused.)
      const name = catalogName(intent.plugin);
      return name === undefined ? "nothing" : added(`add ${name}`);
    }
    case "addInstrument": {
      const name = catalogName(intent.plugin);
      return name === undefined ? "nothing" : added(`add ${name} as an instrument`);
    }
    // A module that is collapsed is an add, in the words of what was done by the name the box has: undo takes the record away and leaves what was in it (step 109).
    case "collapseIntoModule":
      return added(`collapse into ${boxTitle(intent.name)}`);
    // The box is taken away and what is in it stays: the record alone goes, and comes back as it was; a node that is no module is refused.
    case "ungroupModule": {
      const node = nodeOf(intent.node);
      return node?.kind === "module" ? { removed: { label: `ungroup ${node.name}`, intent, at: now } } : "nothing";
    }
    // (As an effect that goes in a chain: the words of the button that adds one. Undo takes the lane that holds it out with it.)
    case "addFreeEffect": {
      const name = catalogName(intent.plugin);
      return name === undefined ? "nothing" : added(`add ${name}`);
    }
    // (Every modulator is the session's own: it is added in the words of the button that adds one, and undo takes it out, with its cables.)
    case "addFreeModulator":
      return added(`add ${MODULATOR_LABELS[intent.kind]}`);
    case "addModEdge":
      return added("draw a cable");
    case "modulate": {
      // A modulator and its cable, in the words of the TUI's `m` and `l`. A target that cannot be named is refused with the rest, and no step is kept for it.
      const name = drivenName(snapshot, intent.to);
      return name === undefined ? "nothing" : added(`${intent.kind === "midiLearn" ? "learn" : "modulate"} ${name}`);
    }
    case "addGroup": {
      // (A member that is no lane is refused with the rest, so it need not be looked at.)
      const leavesAGroup = intent.members.some((id) => {
        const member = nodeOf(id);
        return member?.kind === "instrument" && member.parent !== null;
      });
      // A lane that leaves the group it is in goes back to none when the new group is taken out, and not to that one: a step does not bring it back.
      return leavesAGroup ? "barrier" : added("add a group");
    }
    case "setLaneGroup": {
      // (A node that is no lane, and a group that is no group, are refused: no step is kept for them.)
      const lane = nodeOf(intent.node);
      if (lane?.kind !== "instrument") return "nothing";
      if (lane.parent === intent.group) return "nothing";
      return step(intent.group === null ? "take a lane out of its group" : "put a lane in a group", { type: "setLaneGroup", node: intent.node, group: lane.parent }, null);
    }
    case "removeEdge": {
      // An audio cable and a cable of notes go alone, and come back as they were.
      if (snapshot.graph.edges.some((e) => isAudioCable(e) && e.id === intent.edge)) return { removed: { label: "remove an audio cable", intent, at: now } };
      if (snapshot.graph.edges.some((e) => isNotesCable(e) && e.id === intent.edge)) return { removed: { label: "remove a cable of notes", intent, at: now } };
      if (!snapshot.graph.edges.some((e) => isModulation(e) && e.id === intent.edge)) return "nothing";
      // Cables that drive each other's depths are not put back (`RemovedCable::still_driven` of the controller).
      if (drivesRoundToItself(snapshot.graph.edges, cablesThatGo(snapshot.graph.edges, [intent.edge]))) return "barrier";
      return { removed: { label: "remove a cable", intent, at: now } };
    }
    case "removeNode": {
      const node = nodeOf(intent.node);
      // An effect goes with the cables that drive it, and with those that drive the depth of any of them, and comes back with them (the controller's
      // `Inverse::Effect`). None of them is on a cycle of cables that drive each other's depths: a cable drives one thing, and these are a tree of drivers.
      if (node?.kind === "effect") return { removed: { label: `remove ${node.name}`, intent, at: now } };
      // A lane goes with its plugins and its pattern, after the cables that drive its plugins and those that drive the depth of any of them, and comes back with them, at its place among
      // the lanes and in its group (the controller's `Inverse::Lane`). The modulators that heard it stay, hearing every note. None of those cables is on a cycle of cables that drive each
      // other's depths: a cable drives one thing, so the ones that drive their depths are a tree of drivers. (A lane with no instrument is "an empty lane".)
      if (node?.kind === "instrument") return { removed: { label: `remove ${laneOrGroupWords(snapshot.graph.nodes, node)}`, intent, at: now } };
      // A source of notes goes with the cables of notes that start at it, and comes back with them, at its place among the sources (the controller's `Inverse::Source`), in the words of the button that
      // adds one: the keys, or the device by its name (what the node is called).
      if (node?.kind === "source") return { removed: { label: `remove ${node.name}`, intent, at: now } };
      // A module goes with its modulators and its lanes, whole, and comes back with them as one (the controller's `Inverse::Module`: its record, as it was), in the words of the button that adds one: its name. Cables that drive each
      // other's depths are not put back, as for a modulator that goes alone.
      if (node?.kind === "module") {
        const { edges } = snapshot.graph;
        const members = new Set(node.members);
        const touching = edges.flatMap((e) => (isModulation(e) && (members.has(e.from.node) || members.has(e.to.node)) ? [e.id] : []));
        if (drivesRoundToItself(edges, cablesThatGo(edges, touching))) return "barrier";
        return { removed: { label: `remove ${node.name}`, intent, at: now } };
      }
      // A group goes with its bus effects and the cables that drive them, and its members are in none; it comes back with them, at its place among the groups and with its members put in
      // it again (the controller's `Inverse::Group`). The modulators that heard it stay, hearing every note.
      if (node?.kind === "group") return { removed: { label: `remove ${laneOrGroupWords(snapshot.graph.nodes, node)}`, intent, at: now } };
      if (node?.kind !== "modulator") return node ? "barrier" : "nothing";
      // The cables that go with it: its own, those that drive its settings and those that drive the depth of any of them (`removeNodeOutcome`).
      const { edges } = snapshot.graph;
      const touching = edges.flatMap((e) => (isModulation(e) && (e.from.node === node.id || e.to.node === node.id) ? [e.id] : []));
      // Cables that drive each other's depths are not put back, as for a cable (`RemovedCable::still_driven` of the controller).
      if (drivesRoundToItself(edges, cablesThatGo(edges, touching))) return "barrier";
      return { removed: { label: `remove ${MODULATOR_LABELS[kindOf(node.source)]}`, intent, at: now } };
    }
    case "replaceInstrument": {
      // The instrument of a lane goes with the cables that drive it, and those that drive the depth of any of them (a tree of drivers: none is on a cycle, as for an effect), and comes
      // back with them (the controller's `Inverse::Instrument`). A step in the words of the button that puts one in, by the name the catalog has for it: a lane that has none is filled.
      // (A node that is no lane, or a plugin that the catalog does not name, is refused.)
      const lane = nodeOf(intent.node);
      const name = catalogName(intent.plugin);
      if (lane?.kind !== "instrument" || name === undefined) return "nothing";
      return { removed: { label: lane.plugin === null ? `put ${name} in a lane` : `change the instrument to ${name}`, intent, at: now } };
    }
    case "clearInstrument": {
      // The instrument of a lane goes with the cables that drive it, and those that drive the depth of any of them (a tree of drivers: none is on a cycle, as for an effect), and comes back with
      // them (the controller's `Inverse::Instrument`). A step in the words of what was done, by the instrument's name: the lane stays. (A node that is no lane, and a lane that has no instrument,
      // are refused.)
      const lane = nodeOf(intent.node);
      if (lane?.kind !== "instrument" || lane.plugin === null) return "nothing";
      return { removed: { label: `take ${lane.name} out of its lane`, intent, at: now } };
    }
    case "loadPreset": {
      // A preset that is loaded overwrites the values of the plugin and the preset it had: undo gives them back (the controller's `Inverse::Preset`). A step in the words of the
      // preset, by the name the plugin has for the id. A node that has no such plugin, a bus effect (which has no presets to load) and an id the plugin lacks are refused.
      const node = nodeOf(intent.node);
      if (node?.kind !== "instrument" && node?.kind !== "effect") return "nothing";
      if (node.kind === "effect" && node.parent !== null && nodeOf(node.parent)?.kind === "group") return "nothing";
      const preset = node.plugin?.presets.find((p) => p.id === intent.preset);
      return preset ? { removed: { label: `load preset ${preset.name}`, intent, at: now } } : "nothing";
    }
  }
}
