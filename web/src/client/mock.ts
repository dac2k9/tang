// The mock engine: a stand-in for the server that lives in the page (or in a test) and
// speaks the same protocol, so that the interface can be built, shown in Storybook and
// tested without audio hardware or a running `tang serve` (docs/web-ui.md §9.2). It
// serves a snapshot, applies intents the way the controller does (`src/app/apply.rs`:
// clamp, round an enum, one revision per change), tells every subscribed connection what
// changed, and, for the connections that ask, makes telemetry up: the LFOs run, the
// parameters they drive follow them, and the master's meter rises while a note is held.
import { BPM_RANGE, CABLE_GAIN_RANGE, DEPTH_RANGE, GAIN_RANGES, KEY_CENTRE, KEY_SPAN, MAX_STEPS, MODULATOR_FIELD_RANGES, MODULATOR_FIELD_SCALES, MODULATOR_WHOLE_FIELDS, NEW_CABLE_GAIN, NEW_STEPS, NODE_UI_LIMIT, NODE_UI_PARTS, PATTERN_COUNT_IN_BEATS, PATTERN_DEFAULT_LENGTH, PATTERN_FIELD_RANGES, PINNED_LIMIT, PROTOCOL, SCOPE_FRAME_LENGTH, STEP_RANGE } from "../protocol";
import type {
  ClientMessage,
  CatalogEntry,
  Edge,
  EdgeId,
  Endpoint,
  Event,
  Feed,
  Gain,
  Graph,
  Intent,
  LayoutEntry,
  ModulatorField,
  ModulatorKind,
  ModulatorSource,
  Node,
  NodeId,
  NodeUi,
  NoteRange,
  PatternInfo,
  Plugin,
  Port,
  Route,
  ServerMessage,
  Snapshot,
  SourceInfo,
  StampedEvent,
  TelemetryChannel,
  TelemetryLayout,
  Waveform,
  WireError,
} from "../protocol";
import type { Transport, TransportEvents, TransportFactory } from "./connection";
import { MASTER, builtinCatalog, builtinEffects, builtinInstruments, builtinPresets, tracerSnapshot } from "./fixtures";
import { sessionFileName } from "./filenames";
import { audibleOf, fedOf, heardOf } from "./audible";
import { cablesThatGo, isAudioCable, isLink, isModulation, isNotesCable } from "./cables";
import { Counters } from "./counters";
import { History, addedStep, laneOrGroupWords, planOf, removedStep, takesOut } from "./history";
import type { HistoryState, Made, Plan } from "./history";
import { noise } from "./noise";
import { deviceName, eventCableEnds, withHears } from "./noteWiring";
import { MODULATOR_LABELS, kindOf } from "./modulatorKinds";
import { DEFAULT_MODULE_NAME, cleanModuleName, moduleNameOf, moduleOf, moduleOfNodes, pluginCount, tidyName, withModules } from "./modules";
import type { Module, ModuleEffect, ModuleInstrument, ModuleOutput, ModuleTarget } from "./modules";
import { noteName } from "./notes";
import { fieldsOf, gainPorts, isGainPort, samePort } from "./reach";
import { applyEvent, withValues } from "./reducers";
import { parseScale } from "./scales";
import { stepsOutput, stepsPlace } from "./steps";
import { rateAt } from "./tempo";
import { fractionOf, isLog, valueAt } from "./travel";
import { cableEnds, cableIsBetween, cableLoops, effectLoops, feedLoops, laneLoops, linkOrCable, linksOf, routeLoops } from "./wiring";

export interface MockOptions {
  /** The session it serves; the tracer by default. */
  snapshot?: Snapshot;
  /** The plugins it says can be added, and whether it is still looking for more; the built-ins, found, by default. */
  catalog?: { entries: readonly CatalogEntry[]; scanning: boolean };
  /** A token the server wants first, as one that listens beyond loopback does. */
  token?: string;
  /** What `hello` says the protocol is, for testing a client against a server that is not its own. */
  protocol?: number;
  /** Milliseconds between the telemetry frames it makes (33 by default, like the real one). */
  telemetryPeriod?: number;
  /** The time the made-up telemetry follows, in seconds; the clock of the page by default. */
  clock?: () => number;
  /** The files that are in the directory of the session's file besides its own, which a save as does not write over (none by default). */
  files?: readonly string[];
  /** The MIDI inputs it says are open, by name, which a source of notes can be made of (none by default: the mock has no MIDI). */
  inputs?: readonly string[];
  /** The modules that its library has, by name: chains of effects that a person saved, which the catalog lists and an effect that stands by itself can be made of (none by default). */
  modules?: Readonly<Record<string, Module>>;
}

/**
 * What an intent comes to: the event it makes; or, if it changed the structure of the session, the session
 * as it is then, which the clients are given whole (the controller leaves that to its one projection of the
 * graph, `Applied::restructured`); or why it cannot be done.
 */
type Outcome = { event: Event } | { restructured: Snapshot } | { error: WireError };

const clamp = (value: number, low: number, high: number): number => Math.min(Math.max(value, low), high);

/**
 * Whether the controller can hold a number: its numbers are 32 bits, and one beyond them is infinity there (JSON
 * has no infinity to send; a number a person types, or a script sends, can be 1e39).
 */
const holds = (value: number): boolean => Number.isFinite(Math.fround(value));

/** A number rounded the way the controller does, in 32 bits: 0.49999999 is 0.5 there, which rounds up. */
const roundIn32Bits = (value: number): number => Math.round(Math.fround(value));

/** A number the controller cannot hold, as it says it: Rust writes the infinite 32-bit numbers `inf` and `-inf`. */
function shown(value: number): string {
  const held = Math.fround(value);
  if (Number.isNaN(held) || Number.isFinite(held)) return String(value);
  return held > 0 ? "inf" : "-inf";
}

/** What the controller calls a gain or a setting when it says what is wrong with one: the name of its variant, `Volume`, `GroupPan`, `Attack`. */
const variant = (name: string): string => name.charAt(0).toUpperCase() + name.slice(1);

function pluginOf(node: Node): Plugin | null {
  return node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
}

/** What the controller says to a module that is put where a plugin goes (`load_from_catalog`): it is added as an effect that stands by itself. */
const A_MODULE_IS_NOT_A_PLUGIN = "a module is added as an effect that stands by itself, in a lane of its own: it is not a plugin that goes in a chain";

function invalid(message: string): Outcome {
  return { error: { code: "invalid", message } };
}

function noNode(id: NodeId): Outcome {
  return { error: { code: "notFound", message: `there is no node ${id}` } };
}

/** A gain of the output mix, its volume or the limiter's ceiling (`set_master_gain` of the controller): the number is judged first, and then the gain, and the value is held to the range. */
function masterGainOutcome(gain: Gain, value: number): Outcome {
  if (!holds(value)) return invalid(`${shown(value)} is not a value for ${variant(gain)}`);
  if (gain !== "masterVolume" && gain !== "limiterCeiling") return invalid(`${variant(gain)} does not apply to that node`);
  const [low, high] = GAIN_RANGES[gain];
  return { event: { type: "gainSet", node: MASTER, gain, value: clamp(value, low, high) } };
}

/** The settings that a modulator of each kind has (`ModSourceSlot::setting`). */
const FIELDS_OF: Record<ModulatorSource["type"], readonly ModulatorField[]> = {
  lfo: ["rate", "waveform", "retrigger", "fade", "sync"],
  envelope: ["attack", "decay", "sustain", "release"],
  midiLearn: [],
  macro: ["value"],
  steps: ["rate", "glide", "length", "retrigger", "sync"],
};

/** A setting of a modulator: clamped to its range, rounded if it is a whole number (an index, a count, a switch), and only if the modulator has it. */
function fieldOutcome(node: Node, field: ModulatorField, value: number): Outcome {
  if (node.kind !== "modulator") return invalid("that node is not a modulator");
  if (!holds(value)) return invalid(`${shown(value)} is not a value for ${variant(field)}`);
  const [low, high] = MODULATOR_FIELD_RANGES[field];
  const clamped = clamp(value, low, high);
  if (!FIELDS_OF[node.source.type].includes(field)) return invalid(`${variant(field)} does not apply to that modulator`);
  return { event: { type: "modulatorFieldSet", node: node.id, field, value: MODULATOR_WHOLE_FIELDS.includes(field) ? roundIn32Bits(clamped) : clamped } };
}

/** All the steps of a step sequencer, each held to its range: the controller's `set_steps`, in its order of looking (a node that is a modulator, how many steps, whether they are numbers, whether it is a step sequencer). */
function stepsOutcome(node: Node, values: readonly number[]): Outcome {
  if (node.kind !== "modulator") return invalid("that node is not a modulator");
  if (values.length !== MAX_STEPS) return invalid(`a step sequencer has ${MAX_STEPS} steps, and ${values.length} were given`);
  const bad = values.find((value) => !holds(value));
  if (bad !== undefined) return invalid(`${shown(bad)} is not a value for a step`);
  if (node.source.type !== "steps") return invalid("that modulator is not a step sequencer");
  return { event: { type: "stepsSet", node: node.id, values: values.map((value) => clamp(value, STEP_RANGE[0], STEP_RANGE[1])) } };
}

/** The depth of a cable: clamped, and only of a cable that is there. */
function depthOutcome(snapshot: Snapshot, edge: number, depth: number): Outcome {
  if (snapshot.graph.edges.some((e) => isAudioCable(e) && e.id === edge)) return invalid("an audio cable has a level, and no depth");
  if (!snapshot.graph.edges.some((e) => e.kind === "modulation" && e.id === edge)) {
    return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  }
  if (!holds(depth)) return invalid(`${shown(depth)} is not a value for the depth of a cable`);
  return { event: { type: "edgeDepthSet", edge, depth: clamp(depth, DEPTH_RANGE[0], DEPTH_RANGE[1]) } };
}

/**
 * The level of an audio cable: held to what the controller holds it to, and only of an audio cable that is there (a cable of modulation has a depth, in words that say so). As `set_cable_gain`
 * of the controller, in its order: the cable, the number, the clamp.
 */
function cableGainOutcome(snapshot: Snapshot, edge: EdgeId, gain: number): Outcome {
  if (!snapshot.graph.edges.some((e) => isAudioCable(e) && e.id === edge)) {
    if (snapshot.graph.edges.some((e) => isModulation(e) && e.id === edge)) return invalid("a modulation cable has a depth, and no level");
    return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  }
  if (!holds(gain)) return invalid(`${shown(gain)} is not a value for the level of a cable`);
  return { event: { type: "cableGainSet", edge, gain: clamp(gain, CABLE_GAIN_RANGE[0], CABLE_GAIN_RANGE[1]) } };
}

/**
 * `edges` with the audio cable `added` among them, where the graph lists the cables that have been drawn between two places: after the links of the audio path (which the nodes have by
 * themselves) and the links of the lanes that hear every source, and before the cables of notes and of modulation, in the order of their ids.
 */
function withAudioCable(edges: readonly Edge[], added: Edge & { kind: "cable" }): Edge[] {
  const after = edges.findIndex((e) => isNotesCable(e) || isModulation(e));
  const end = after < 0 ? edges.length : after;
  let at = edges.findIndex(isAudioCable);
  if (at < 0) at = end;
  while (at < end && (edges[at] as Edge & { kind: "cable" }).id < added.id) at += 1;
  return [...edges.slice(0, at), added, ...edges.slice(at)];
}

/**
 * An audio cable drawn from the output of one place to the input of another, as the controller does it (`add_cable_intent`, `Controller::add_audio_cable`): what the ends are is read from
 * the ports (`cableEnds`), and then the level (a number the controller can hold; none is `NEW_CABLE_GAIN`), a cable between the two already, and a loop are refused, in that order, before an id is
 * used. The level is held to what a cable can have.
 */
function addCableOutcome(snapshot: Snapshot, from: Endpoint, to: Endpoint, gain: number | null, counters: Counters): Outcome {
  const ends = cableEnds(snapshot.graph, from, to);
  if ("error" in ends) return ends;
  const level = gain ?? NEW_CABLE_GAIN;
  if (!holds(level)) return invalid(`${shown(level)} is not a value for the level of a cable`);
  if (snapshot.graph.edges.some((e) => isAudioCable(e) && cableIsBetween(e, ends))) return invalid("there is a cable between those two already");
  if (cableLoops(snapshot.graph, ends)) return invalid("that cable would make the audio go round: it would feed what it comes from");
  const added: Edge & { kind: "cable" } = {
    from: { node: from.node, port: { type: "audioOut" } },
    // (The second input of an effect is another end than its first: a cable to each is a cable.)
    to: { node: to.node, port: { type: ends.to.kind === "effectSide" ? "audioInSide" : "audioIn" } },
    kind: "cable",
    id: counters.edge(),
    gain: clamp(level, CABLE_GAIN_RANGE[0], CABLE_GAIN_RANGE[1]),
  };
  return { restructured: { ...snapshot, graph: { ...snapshot.graph, edges: withAudioCable(snapshot.graph.edges, added) } } };
}

/**
 * `edges` with the cable of notes `added` among them, where the graph lists the cables of notes: after the audio cables that have been drawn and before the cables of modulation, in the order of
 * their ids (the cables of notes and the audio cables have ids of one series, and are listed each in its own place). A cable that is drawn has the highest id there is, so it comes last of its kind,
 * and so does one that redo draws again: whatever was drawn after it has been undone first.
 */
function withNotesCable(edges: readonly Edge[], added: Edge & { kind: "events" }): Edge[] {
  const modulation = edges.findIndex(isModulation);
  const at = modulation < 0 ? edges.length : modulation;
  return [...edges.slice(0, at), added, ...edges.slice(at)];
}

/**
 * A cable of notes drawn from the output of events of a source to the input of events of a lane, as the controller does it (`add_cable_intent`, `Controller::add_event_cable`): what the ends are is
 * read from the ports (`eventCableEnds`), and then a level (a cable of notes has none: it carries notes, which are heard or not) and a cable between the two already are refused, in that order, before
 * an id is used. The lane then hears the sources that its cables start at and no others, which the links of the graph say (`withHears`).
 */
function addNotesCableOutcome(snapshot: Snapshot, from: Endpoint, to: Endpoint, gain: number | null, counters: Counters): Outcome {
  const ends = eventCableEnds(snapshot.graph, from, to);
  if ("error" in ends) return ends;
  if (gain !== null) return invalid("a cable of notes has no level: it carries notes, which are heard or not");
  if (snapshot.graph.edges.some((e) => isNotesCable(e) && e.from.node === ends.source && e.to.node === ends.lane)) return invalid("there is a cable between those two already");
  const added: Edge & { kind: "events" } = {
    from: { node: ends.source, port: { type: "eventsOut" } },
    to: { node: ends.lane, port: { type: "eventsIn" } },
    kind: "events",
    id: counters.edge(),
  };
  return { restructured: { ...snapshot, graph: { ...snapshot.graph, edges: withNotesCable(snapshot.graph.edges, added) } } };
}

/**
 * The keys, or a device by its name, put in the session as a source of notes, after the sources there are (the graph lists them first), as the controller does it (`add_source`): a device's name is
 * the text without the spaces round it (`deviceName`), and is refused when it is blank, too long or has a control character in it; the keys, and a device of that name, are refused when they are a
 * source already; and no id is used for a refusal. It changes nothing that is heard: a lane that no cable of notes ends at hears every source, the new one too (`withHears`).
 */
function addSourceOutcome(snapshot: Snapshot, source: SourceInfo, counters: Counters): Outcome {
  let name = "Keys";
  let info: SourceInfo = { type: "keys" };
  if (source.type === "device") {
    const called = deviceName(source.name);
    if ("why" in called) return invalid(`a device's name ${called.why}`);
    name = called.name;
    info = { type: "device", name };
  }
  const { nodes } = snapshot.graph;
  const there = nodes.some((n) => n.kind === "source" && (info.type === "keys" ? n.source.type === "keys" : n.source.type === "device" && n.source.name === name));
  if (there) return invalid(info.type === "keys" ? "the keys are a source already" : `${name} is a source already`);
  const added: Node = { id: counters.node(), parent: null, name, kind: "source", source: info };
  const at = nodes.findLastIndex((n) => n.kind === "source") + 1;
  return { restructured: { ...snapshot, graph: { ...snapshot.graph, nodes: [...nodes.slice(0, at), added, ...nodes.slice(at)] } } };
}

/** A place on a node, as one key: a parameter, a setting of a modulator, the depth of one of its cables (the layout names a value once for each). */
const aimKey = (node: NodeId, port: Port): string => `${String(node)}:${port.type}${port.type === "param" ? String(port.index) : port.type === "depth" ? String(port.edge) : ""}`;

/** How deep a cable pulls when it is first drawn (`NEW_CABLE_DEPTH`). */
const NEW_CABLE_DEPTH = 0.5;

/**
 * A cable drawn from a modulator, at half depth: to a parameter of a plugin it reaches (`reachFrom`: the controller's rule,
 * read from the graph), or to a setting of another modulator it reaches, or to the depth of one of that modulator's cables
 * (`addFieldCable`).
 */
function addModEdgeOutcome(snapshot: Snapshot, modulator: NodeId, to: Edge["to"], counters: Counters): Outcome {
  const from = snapshot.graph.nodes.find((n) => n.id === modulator && n.kind !== "master");
  if (!from) return noNode(modulator);
  if (from.kind !== "modulator") return invalid("a cable starts at a modulator");
  switch (to.port.type) {
    case "param":
      return addParamCable(snapshot, from, to.node, to.port.index, counters);
    case "volume":
    case "mix":
    case "pan":
      return addGainCable(snapshot, from, to.node, to.port, counters);
    case "rate":
    case "attack":
    case "decay":
    case "sustain":
    case "release":
    case "depth":
      return addFieldCable(snapshot, from, to, counters);
    default:
      return invalid("a cable can only end at a plugin's parameter, at a volume, a mix or a pan, or at a modulator's rate, stage or depth");
  }
}

/**
 * `edges` with the cable `added` of the modulator `modulator` among them, where the graph lists a modulator's cables (the groups'
 * modulators' first, then the lanes', each modulator's cables together): after the last cable of a modulator that comes before this
 * one in the graph, this one's own included.
 */
function withCable(nodes: readonly Node[], edges: readonly Edge[], modulator: NodeId, added: Edge): Edge[] {
  const position = new Map(nodes.map((n, i) => [n.id, i]));
  const mine = position.get(modulator) ?? 0;
  const after = edges.findLastIndex((e) => isModulation(e) && (position.get(e.from.node) ?? Infinity) <= mine);
  const first = edges.findIndex(isModulation);
  const at = after >= 0 ? after + 1 : first >= 0 ? first : edges.length;
  return [...edges.slice(0, at), added, ...edges.slice(at)];
}

/**
 * `modulation` with `entry`, the value that the cable `added` is the first to drive, put where the controller lists it: its list of what cables drive goes
 * by the place of the modulator in the session's own rack, the first cable to aim at a value naming it. So a cable drawn from a modulator
 * that is not the last has its value listed before those of the modulators after it (f114: it was put at the end). (The place of a cable among its modulator's
 * decides nothing here: a cable is drawn after the others of its modulator, and a modulator cannot drive a value twice.) `cables` are those of the session
 * with `added` among them.
 */
function withEntry(nodes: readonly Node[], cables: readonly Edge[], modulation: readonly LayoutEntry[], entry: LayoutEntry, added: Edge & { kind: "modulation" }): LayoutEntry[] {
  const position = new Map(nodes.map((n, i) => [n.id, i]));
  const driving = cables.filter(isModulation);
  const rank = (cable: (typeof driving)[number]): number => position.get(cable.from.node) ?? 0;
  const earlier = (a: number, b: number): boolean => a < b;
  const mine = rank(added);
  /** Whose value this is: the first of the cables to aim at it, in the controller's order. */
  const firstToAim = (e: LayoutEntry): number | undefined =>
    driving
      .filter((c) => aimKey(c.to.node, c.to.port) === aimKey(e.node, e.port))
      .map(rank)
      .reduce<number | undefined>((first, r) => (first === undefined || earlier(r, first) ? r : first), undefined);
  const at = modulation.findIndex((e) => {
    const owner = e.kind === "effective" ? firstToAim(e) : undefined;
    return owner !== undefined && earlier(mine, owner);
  });
  return at < 0 ? [...modulation, entry] : [...modulation.slice(0, at), entry, ...modulation.slice(at)];
}

/**
 * What a modulator that puts out `output` adds, at depth `amount`, to a value that the user set to `base` in the range `min`…`max`: the engine's law (`Modulator::target_offset`). An LFO swings the
 * value around where the user set it, by depth times the range; an envelope's peak is that value, and it is taken from below it; a control that is not bound adds nothing, a CC and key pressure
 * start from nothing, and the pitch bend wheel goes both ways from its middle; a macro starts from nothing as a CC does.
 */
function offsetOf(source: Node | undefined, output: number, amount: number, base: number, min: number, max: number): number {
  const modulator = source?.kind === "modulator" ? source.source : undefined;
  if (modulator?.type === "envelope") return -(1 - output) * amount * (base - min);
  if (modulator?.type === "midiLearn") {
    if (modulator.control === null) return 0;
    if (modulator.control.type === "pitchBend") return (output - 0.5) * 2 * amount * (max - min);
    // A key goes by the number of the note, middle C being the centre and five octaves either way the whole of the depth.
    if (modulator.control.type === "key") return Math.min(Math.max((Math.round(output * 127) - KEY_CENTRE) / KEY_SPAN, -1), 1) * amount * (max - min);
  }
  return output * amount * (max - min);
}

/**
 * The session with the cable `added`, drawn from the modulator `from`, among its cables. The value it drives gets a slot in the layout of the telemetry unless
 * another cable drives it already (the layout names one slot for a value, a parameter or a setting of a modulator); the cable takes a slot either way, as the
 * controller's `add_target` has it: the one that is not named is never read.
 */
function withDrawnCable(snapshot: Snapshot, from: Node, added: Edge & { kind: "modulation" }, counters: Counters): Outcome {
  const { nodes, edges } = snapshot.graph;
  const key = aimKey(added.to.node, added.to.port);
  const aimed = edges.some((e) => isModulation(e) && aimKey(e.to.node, e.to.port) === key);
  const slot = counters.slot();
  const cables = withCable(nodes, edges, from.id, added);
  const telemetry = aimed
    ? snapshot.telemetry
    : {
        ...snapshot.telemetry,
        rev: snapshot.telemetry.rev + 1,
        modulation: withEntry(nodes, cables, snapshot.telemetry.modulation, { slot, node: added.to.node, port: added.to.port, kind: "effective" }, added),
      };
  return { restructured: { ...snapshot, graph: { nodes, edges: cables }, telemetry } };
}

/**
 * A cable to parameter `index` of the plugin in `node`. The value it drives gets a slot in the layout of the telemetry unless
 * another cable drives it already (the layout names one slot for a parameter); the cable takes a slot either way.
 */
function addParamCable(snapshot: Snapshot, from: Node, node: NodeId, index: number, counters: Counters): Outcome {
  const { nodes, edges } = snapshot.graph;
  const target = nodes.find((n) => n.id === node && n.kind !== "master");
  if (!target) return noNode(node);
  if (target.kind !== "instrument" && target.kind !== "effect") return invalid("that node has no plugin");
  if (!target.plugin) return invalid("that instrument has no plugin");
  if (!target.plugin.params.some((p) => p.index === index)) return invalid(`the plugin has no parameter ${index}`);
  const port: Port = { type: "param", index };
  const drivesIt = (e: Edge): boolean => isModulation(e) && e.from.node === from.id && e.to.node === node && samePort(e.to.port, port);
  if (edges.some(drivesIt)) return invalid("that modulator drives that parameter already");

  return withDrawnCable(snapshot, from, { from: { node: from.id, port: { type: "modOut" } }, to: { node, port }, kind: "modulation", id: counters.edge(), amount: NEW_CABLE_DEPTH }, counters);
}

/**
 * A cable to the volume, the mix or the pan that the host keeps for a node (`add_mod_edge_intent`, `gain_cable`): a lane that has an instrument has a volume, an effect a mix, a group a volume and a pan
 * (`gainPorts`), and a modulator drives each once (another may drive it as well: the two add up). The value gets a slot in the layout of the telemetry unless another cable drives it already; the cable takes a slot either way.
 */
function addGainCable(snapshot: Snapshot, from: Node, node: NodeId, port: Extract<Port, { type: "volume" | "mix" | "pan" }>, counters: Counters): Outcome {
  const { nodes, edges } = snapshot.graph;
  const target = nodes.find((n) => n.id === node && n.kind !== "master");
  if (!target) return noNode(node);
  if (!gainPorts(target).some((own) => samePort(own, port))) return invalid(`that node has no ${port.type}`);
  if (edges.some((e) => isModulation(e) && e.from.node === from.id && e.to.node === node && samePort(e.to.port, port))) return invalid(`that modulator drives that ${port.type} already`);

  return withDrawnCable(snapshot, from, { from: { node: from.id, port: { type: "modOut" } }, to: { node, port }, kind: "modulation", id: counters.edge(), amount: NEW_CABLE_DEPTH }, counters);
}

/**
 * A cable to a setting of another modulator (an LFO's rate, a stage of an envelope) or to the depth of one of its cables, as the
 * controller lets it (`add_field_cable`): the modulator is in reach of the one that is drawn from (`modulatorsInReach`), has the
 * setting (`fieldsOf`), and is not driven there already. The value that setting is set to gets a slot in the layout of the telemetry unless another
 * cable drives it already (f132, as for a parameter: `addParamCable`); the cable takes a slot either way.
 */
function addFieldCable(snapshot: Snapshot, from: Node, to: Edge["to"], counters: Counters): Outcome {
  const { nodes, edges } = snapshot.graph;
  const target = nodes.find((n) => n.id === to.node && n.kind !== "master");
  if (!target) return noNode(to.node);
  if (target.id === from.id) return invalid("a modulator cannot drive its own settings");
  if (target.kind !== "modulator") return invalid("a cable ends at the settings of a modulator, and that is not one");
  if (!fieldsOf(snapshot.graph, from.id).some((field) => field.node === to.node && samePort(field.port, to.port))) {
    if (to.port.type === "depth") return { error: { code: "notFound", message: `there is no cable ${to.port.edge}` } };
    return invalid(`that modulator has no ${to.port.type}`);
  }
  if (edges.some((e) => isModulation(e) && e.from.node === from.id && e.to.node === to.node && samePort(e.to.port, to.port))) return invalid("that modulator drives that setting already");

  return withDrawnCable(snapshot, from, { from: { node: from.id, port: { type: "modOut" } }, to, kind: "modulation", id: counters.edge(), amount: NEW_CABLE_DEPTH }, counters);
}

/**
 * The session without the cables `going` and the nodes `nodesGone`, and the audio cables that start or end at one of those. The layout of the telemetry follows: the
 * outputs of the modulators that go leave it, and so does the slot of the value a cable drove, unless another
 * cable still drives it (the layout names one slot for a parameter, whichever cables are left), and the meters of
 * a lane or a group that goes.
 */
function without(snapshot: Snapshot, going: ReadonlySet<EdgeId>, nodesGone: ReadonlySet<NodeId>): Snapshot {
  const cables = snapshot.graph.edges;
  // (The audio cables and the cables of notes at a node that goes go with it, as the controller takes them out first: `take_out_audio_cables_at`, `take_out_event_cables_at`.)
  const edges = cables.filter((e) => !(isModulation(e) && going.has(e.id)) && !((isAudioCable(e) || isNotesCable(e)) && (nodesGone.has(e.from.node) || nodesGone.has(e.to.node))));
  const aimedBefore = new Set(cables.map((e) => aimKey(e.to.node, e.to.port)));
  const aimedAfter = new Set(edges.map((e) => aimKey(e.to.node, e.to.port)));
  const leaves = (entry: LayoutEntry): boolean => {
    if (entry.kind === "modOutput" || entry.kind === "modPosition") return nodesGone.has(entry.node);
    const key = aimKey(entry.node, entry.port);
    return entry.kind === "effective" && aimedBefore.has(key) && !aimedAfter.has(key);
  };
  const modulation = snapshot.telemetry.modulation.filter((entry) => !leaves(entry));
  const meters = snapshot.telemetry.meters.filter((entry) => !nodesGone.has(entry.node));
  const changed = modulation.length !== snapshot.telemetry.modulation.length || meters.length !== snapshot.telemetry.meters.length;
  return {
    ...snapshot,
    graph: { nodes: snapshot.graph.nodes.filter((n) => !nodesGone.has(n.id)), edges },
    telemetry: changed ? { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, meters, modulation } : snapshot.telemetry,
  };
}

/** A cable taken out of the session, and with it the cables that drive the depth of one that goes (and theirs in turn). */
function removeEdgeOutcome(snapshot: Snapshot, edge: EdgeId): Outcome {
  // (An audio cable goes alone: no cable drives its level, and it has no slot of the telemetry. Nor does a cable of notes, which has no level.)
  if (snapshot.graph.edges.some((e) => (isAudioCable(e) || isNotesCable(e)) && e.id === edge)) {
    return { restructured: { ...snapshot, graph: { ...snapshot.graph, edges: snapshot.graph.edges.filter((e) => !((isAudioCable(e) || isNotesCable(e)) && e.id === edge)) } } };
  }
  if (!snapshot.graph.edges.some((e) => isModulation(e) && e.id === edge)) return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  return { restructured: without(snapshot, cablesThatGo(snapshot.graph.edges, [edge]), new Set()) };
}

/**
 * The edges of a graph, with the links of the audio path made again from its nodes (`linksOf`, which lists them as the controller's graph does: the groups' chains, then the lanes', and no link
 * into an effect whose feed is cut), where the graph lists them, before the cables and the rest.
 */
const relinked = (graph: Graph): Edge[] => [...linksOf(graph.nodes), ...graph.edges.filter((e) => !isLink(e))];

/**
 * An effect (or a bus effect) taken out of the session, as the controller does it (`remove_effect`): the cables that drive
 * its parameters go first, a lane's and a group's, with what drove the depth of any of them (what they were driving goes with
 * the plugin: nothing is given back), and the audio path closes up (`relinked`): the effect after it is fed by the stage that fed it, or by
 * nothing if its own link is cut, and the link that ends a member lane's chain goes into its group.
 */
function removeEffectOutcome(snapshot: Snapshot, effect: Node): Outcome {
  const touching = snapshot.graph.edges.filter(isModulation).filter((e) => e.to.node === effect.id).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set([effect.id]));
  return { restructured: { ...taken, graph: { ...taken.graph, edges: relinked(taken.graph) } } };
}

/**
 * An effect (a lane's or a bus's) moved to place `index` of its chain, as the controller does it (`Controller::move_effect`,
 * `move_bus_effect`): the effects between its old place and its new one move one place the other way, and the audio path goes through
 * them in the new order. The graph lists the nodes of a lane, or of a group's bus, together (the lane or the group, its effects, its
 * modulators), so the effects change places among themselves, in the places they take in the list; the links of the chain are made again
 * from the nodes in their new order (`relinked`: a member lane's chain ends in its group, a bus's and an ungrouped lane's in the master, and an
 * effect whose feed is cut has no link into it wherever it is: the cut is the effect's own). The cables and the layout of the telemetry are what
 * they were: they name the plugins by their nodes, which are the same. Refused for what is no effect, a place the chain has not, and
 * the place the effect is at.
 */
function moveEffectOutcome(snapshot: Snapshot, node: NodeId, index: number): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind !== "effect") return invalid("only an effect can be moved");
  const { nodes } = snapshot.graph;
  const chain = nodes.filter((n) => n.kind === "effect" && n.parent === found.parent);
  if (!Number.isInteger(index) || index < 0 || index >= chain.length) return invalid(`the chain has no place ${String(index)}`);
  const from = chain.indexOf(found);
  if (index === from) return invalid("that effect is at that place already");
  if (effectLoops(snapshot.graph, node, index)) return invalid("moving that effect there would make the audio go round, by the cables there are");
  const moved = [...chain];
  moved.splice(from, 1);
  moved.splice(index, 0, found);

  const inChain = new Set(chain);
  let nextNode = 0;
  const reordered = nodes.map((n) => (inChain.has(n) ? (moved[nextNode++] ?? n) : n));
  const graph: Graph = { nodes: reordered, edges: snapshot.graph.edges };
  return { restructured: { ...snapshot, graph: { nodes: reordered, edges: relinked(graph) } } };
}

/**
 * A lane taken out of the session, as the controller does it (`Controller::remove_lane`): its instrument, its effects and its
 * modulators go, with the links of its audio path, the cables of its own modulators (they reach only its own chain) and those of its
 * group that end in it, and what drove the depth of any of them. A session keeps a lane: the last one is refused. (What the cables of
 * its group were driving is gone with it: nothing is given back.)
 */
function removeLaneOutcome(snapshot: Snapshot, lane: Node): Outcome {
  const { nodes, edges } = snapshot.graph;
  if (nodes.filter((n) => n.kind === "instrument").length < 2) return invalid("a session keeps its last lane");
  const inside = new Set<NodeId>([lane.id, ...nodes.filter((n) => n.parent === lane.id).map((n) => n.id)]);
  // (The cables of the lane's own modulators end inside the lane, so the cables that end there are the lane's own and its group's.)
  const touching = edges.filter(isModulation).filter((e) => inside.has(e.to.node)).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(edges, touching), inside);
  return { restructured: { ...taken, graph: { ...taken.graph, edges: taken.graph.edges.filter((e) => !isLink(e) || !inside.has(e.from.node)) } } };
}

/**
 * A group taken out of the session, as the controller does it (`Controller::remove_group`): its bus effects and its modulators go with it,
 * and the links of its audio path, and every cable of it (and what drove the depth of any of them); its members stay, in no group, their last
 * link going into the master. The groups after it that have no name of their own are called by their places now (the graph has only the name a
 * group is called, so a name that is "Group" and its own place is taken for the default one). (What the cables were driving in the
 * members has the value that was set, which is the audio thread's to do.)
 */
function removeGroupOutcome(snapshot: Snapshot, group: Node): Outcome {
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master");
  const inside = new Set<NodeId>([group.id, ...nodes.filter((n) => n.parent === group.id && n.kind !== "instrument").map((n) => n.id)]);
  const touching = edges.filter(isModulation).filter((e) => inside.has(e.from.node) || inside.has(e.to.node)).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(edges, touching), inside);
  const groups = nodes.filter((n) => n.kind === "group");
  const place = (id: NodeId): number => groups.findIndex((n) => n.id === id);
  const renumbered = (n: Node): Node => {
    const was = place(n.id);
    return n.kind === "group" && was > place(group.id) && n.name === `Group ${was + 1}` ? { ...n, name: `Group ${was}` } : n;
  };
  const members = new Set(nodes.filter((n) => n.parent === group.id && n.kind === "instrument").map((n) => n.id));
  return {
    restructured: {
      ...taken,
      graph: {
        nodes: taken.graph.nodes.map((n) => renumbered(members.has(n.id) ? { ...n, parent: null } : n)),
        edges: taken.graph.edges.flatMap((e): Edge[] => {
          if (!isLink(e)) return [e];
          if (inside.has(e.from.node)) return [];
          return e.to.node === group.id ? [{ from: e.from, to: { node: master.id, port: { type: "audioIn" } }, kind: "audio" }] : [e];
        }),
      },
    },
  };
}

/**
 * A node taken out of the session. A modulator: its own cables go with it, and so do those of the others that drove
 * its settings (a cable that ends at the modulator), and what drove the depth of any of them. An effect: see
 * `removeEffectOutcome`; a lane, see `removeLaneOutcome`; a group, `removeGroupOutcome`. Nothing else can be taken out.
 */
function removeNodeOutcome(snapshot: Snapshot, node: NodeId): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind === "module") return removeModuleOutcome(snapshot, found);
  const outcome = removeOtherNodeOutcome(snapshot, found);
  // A lane or a modulator that goes leaves the module that it came with (`Controller::leave_module`), and a module that has none left is gone.
  return "restructured" in outcome ? { restructured: leftItsModule(outcome.restructured, node) } : outcome;
}

/** `removeNodeOutcome` for a node that is no module. */
function removeOtherNodeOutcome(snapshot: Snapshot, found: Node): Outcome {
  const node = found.id;
  if (found.kind === "effect") return removeEffectOutcome(snapshot, found);
  if (found.kind === "instrument") return removeLaneOutcome(snapshot, found);
  if (found.kind === "group") return removeGroupOutcome(snapshot, found);
  // A source of notes goes with the cables of notes that start at it, and the lanes that heard it hear what their other cables bring, or every source if they have none (`withHears`).
  if (found.kind === "source") return { restructured: without(snapshot, new Set(), new Set([found.id])) };
  // (What is left is a modulator: the master is no node of the model, as far as the controller goes.)
  if (found.kind !== "modulator") return noNode(node);
  const touching = snapshot.graph.edges.filter((e) => isModulation(e) && (e.from.node === node || e.to.node === node)).map((e) => (e as { id: EdgeId }).id);
  return { restructured: without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set([node])) };
}

/** The session with `node`, which has gone, out of the module that it came with, as `SessionModel::leave_module` does it: the module has one member less, and is gone if it has none. */
function leftItsModule(snapshot: Snapshot, node: NodeId): Snapshot {
  if (!snapshot.graph.nodes.some((n) => n.kind === "module" && n.members.includes(node))) return snapshot;
  const nodes = snapshot.graph.nodes.flatMap((n): Node[] => {
    if (n.kind !== "module" || !n.members.includes(node)) return [n];
    const members = n.members.filter((member) => member !== node);
    return members.length === 0 ? [] : [{ ...n, members }];
  });
  return { ...snapshot, graph: { ...snapshot.graph, nodes } };
}

/**
 * A module taken out, as the controller does it (`Controller::remove_module`, step 108): the record goes first, and then its modulators and its lanes, the last made first, each as the controller takes a modulator or a lane out; a session keeps a lane, so a module that has
 * every lane that there is is refused, and nothing is changed.
 */
function removeModuleOutcome(snapshot: Snapshot, module: Extract<Node, { kind: "module" }>): Outcome {
  const lanes = module.members.filter((id) => snapshot.graph.nodes.some((n) => n.id === id && n.kind === "instrument")).length;
  if (lanes >= snapshot.graph.nodes.filter((n) => n.kind === "instrument").length) return invalid("a session keeps its last lane");
  let session: Snapshot = { ...snapshot, graph: { ...snapshot.graph, nodes: snapshot.graph.nodes.filter((n) => n.id !== module.id) } };
  for (const id of [...module.members].reverse()) {
    const member = session.graph.nodes.find((n) => n.id === id);
    if (member?.kind !== "instrument" && member?.kind !== "modulator") continue;
    const taken = removeNodeOutcome(session, id);
    if ("restructured" in taken) session = taken.restructured;
  }
  return { restructured: session };
}

/** What the controller calls a lane or a modulator when it says that it is in a module already (`member_words`): "the lane of Oscillator", "an LFO". */
function memberWords(snapshot: Snapshot, node: Node): string {
  if (node.kind === "instrument") return laneOrGroupWords(snapshot.graph.nodes, node);
  if (node.kind === "modulator") return MODULATOR_LABELS[kindOf(node.source)];
  return `node ${String(node.id)}`;
}

/**
 * The nodes in `nodes` collapsed into one module where they are (`collapse_into_module_intent`, step 109), in the controller's order: every node is looked for first (the first that is not there is not found: the master is no node of the
 * model, as far as this goes), then what each is (a lane, an effect of one and a modulator are what a module holds, and an effect is its lane, which is in the module whole), then that something is chosen, that none of what it comes to is in
 * a module already, the name and the place. The record is made last, under a node of its own, so that a refusal uses no id; the lanes are in it in the order of the session and then the modulators in the order of their rack.
 */
function collapseIntoModuleOutcome(snapshot: Snapshot, nodes: readonly NodeId[], name: string, ui: NodeUi | null, counters: Counters): Outcome {
  const { graph } = snapshot;
  const chosen: Node[] = [];
  for (const id of nodes) {
    const found = graph.nodes.find((n) => n.id === id && n.kind !== "master");
    if (!found) return noNode(id);
    chosen.push(found);
  }
  const lanes = new Set<NodeId>();
  const modulators = new Set<NodeId>();
  for (const node of chosen) {
    if (node.kind === "instrument") lanes.add(node.id);
    else if (node.kind === "effect" && node.parent !== null && graph.nodes.some((n) => n.id === node.parent && n.kind === "instrument")) lanes.add(node.parent);
    else if (node.kind === "modulator") modulators.add(node.id);
    // (An effect that is not in a lane's chain is on a group's bus.)
    else if (node.kind === "group" || node.kind === "effect") return invalid("a group is not collapsed into a module: a module holds lanes and modulators");
    else if (node.kind === "source") return invalid("a source of notes is not collapsed into a module: a module holds lanes and modulators");
    else return invalid("a module cannot hold another module yet: ungroup it first");
  }
  if (nodes.length === 0) return invalid("nothing is chosen: choose the lanes, plugins and modulators to collapse into a module");
  const members = [...graph.nodes.filter((n) => n.kind === "instrument" && lanes.has(n.id)), ...graph.nodes.filter((n) => n.kind === "modulator" && modulators.has(n.id))];
  for (const member of members) {
    const module = graph.nodes.find((n) => n.kind === "module" && n.members.includes(member.id));
    if (module) return invalid(`${memberWords(snapshot, member)} is in the module ${module.name} already: ungroup it first`);
  }
  const tidied = tidyName(name);
  if ("error" in tidied) return invalid(tidied.error);
  if (ui !== null && (!holds(ui.x) || !holds(ui.y))) return invalid(`(${shown(ui.x)}, ${shown(ui.y)}) is not a place`);
  const place = ui === null ? undefined : { x: tidyCoordinate(ui.x), y: tidyCoordinate(ui.y) };
  return { restructured: withModuleRecord(snapshot, tidied.name === "" ? DEFAULT_MODULE_NAME : tidied.name, members.map((n) => n.id), counters, place) };
}

/** The box of the module `node` taken away, and what is in it kept (`ungroup_module_intent`, step 109): the record is gone, and the lanes and the modulators are not touched. */
function ungroupModuleOutcome(snapshot: Snapshot, node: NodeId): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind !== "module") return invalid("only a module can be ungrouped");
  return { restructured: { ...snapshot, graph: { ...snapshot.graph, nodes: snapshot.graph.nodes.filter((n) => n.id !== node) } } };
}

/** What a node of each kind of modulator is called (`ModSourceSlot::node_name`), and what it is to begin with. */
const NEW_MODULATORS: Record<ModulatorKind, { name: string; source: ModulatorSource }> = {
  lfo: { name: "LFO", source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 } },
  envelope: { name: "ADSR", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } },
  midiLearn: { name: "MIDI (learning…)", source: { type: "midiLearn", control: null, learning: true } },
  macro: { name: "Macro", source: { type: "macro", value: 0 } },
  // (A control that follows the notes is bound when it is made: no controller is moved for it.)
  velocity: { name: "Velocity", source: { type: "midiLearn", control: { type: "velocity" }, learning: false } },
  key: { name: "Key", source: { type: "midiLearn", control: { type: "key" }, learning: false } },
  steps: { name: "Steps", source: { type: "steps", values: [...NEW_STEPS.values], length: NEW_STEPS.length, rate: NEW_STEPS.rate, glide: NEW_STEPS.glide, retrigger: NEW_STEPS.retrigger, sync: NEW_STEPS.sync } },
};

/**
 * A modulator at the end of the session's own rack (`AddFreeModulator`; every modulator is the session's own since step 96): a node with no parent, which the graph lists after the lanes' nodes and before the master, and
 * whose output the layout of the telemetry lists after the lanes' and the groups'. It hears the lane or the group `hears` (the "+" of one), or every note. A `hears` that is not a node of the session, or is neither a lane nor a
 * group, is refused before the node is made (no id is used).
 */
function addFreeModulatorOutcome(snapshot: Snapshot, kind: ModulatorKind, hears: NodeId | null, counters: Counters): Outcome {
  if (hears !== null) {
    const heard = snapshot.graph.nodes.find((n) => n.id === hears && n.kind !== "master");
    if (!heard) return noNode(hears);
    if (heard.kind !== "instrument" && heard.kind !== "group") return invalid("only a lane or a group can be heard");
  }
  return { restructured: withModulator(snapshot, kind, counters, hears).session };
}

/**
 * The session with a modulator of `kind` at the end of the session's own rack (hearing the lane or the group `hears`, if there is one), and the node it
 * is. The session's own modulators are listed in the graph after the lanes' nodes (the sources come first and the master last), so the first of them goes before the master.
 */
function withModulator(snapshot: Snapshot, kind: ModulatorKind, counters: Counters, hears: NodeId | null = null): { session: Snapshot; node: Node } {
  const { name, source } = NEW_MODULATORS[kind];
  return withModulatorOf(snapshot, name, source, counters, hears);
}

/** `withModulator` for a modulator that is what `source` says, called `name` (a module's modulators are: `addModuleOutcome`). */
function withModulatorOf(snapshot: Snapshot, name: string, source: ModulatorSource, counters: Counters, hears: NodeId | null): { session: Snapshot; node: Node } {
  const { nodes } = snapshot.graph;
  const last = nodes.findLastIndex((n) => n.kind === "modulator");
  // (The modules that were added as one thing are listed after the modulators and before the master, as the controller lists them: `SessionModel::graph`.)
  const tail = nodes.findIndex((n) => n.kind === "module" || n.kind === "master");
  const at = last >= 0 ? last + 1 : tail >= 0 ? tail : nodes.length;
  const added: Node = { id: counters.node(), parent: null, name, kind: "modulator", source, ...(hears === null ? {} : { hears }) };
  const before = new Set(nodes.slice(0, at).filter((n) => n.kind === "modulator").map((n) => n.id));
  const outputs = snapshot.telemetry.modulation;
  const place = outputs.findLastIndex((entry) => (entry.kind === "modOutput" || entry.kind === "modPosition") && before.has(entry.node)) + 1;
  const entry: LayoutEntry = { slot: counters.slot(), node: added.id, port: { type: "modOut" }, kind: "modOutput" };
  // (A step sequencer says where it is too, in the slot after its output's, right after it.)
  const position: LayoutEntry[] = source.type === "steps" ? [{ slot: counters.slot(), node: added.id, port: { type: "modOut" }, kind: "modPosition" }] : [];
  const modulation = [...outputs.slice(0, place), entry, ...position, ...outputs.slice(place)];
  const session: Snapshot = {
    ...snapshot,
    graph: { ...snapshot.graph, nodes: [...nodes.slice(0, at), added, ...nodes.slice(at)] },
    telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, modulation },
  };
  return { session, node: added };
}

/**
 * A modulator of `kind` made for what the port `to` of a node is, with a cable from it to the port, as one change (`Controller::modulate_intent`; the TUI's `m` and
 * `l`). It is the session's own, and hears what `to` is of: the lane for the plugin of a lane (its instrument or an effect of its chain), the group for a bus effect, and for a setting
 * of a modulator what that modulator hears, where the new one is a sibling that drives it. The cable is drawn as `addModEdge` draws one (what it reaches is read
 * from the graph, so the modulator is made first), at half depth, or at `depth` clamped as a depth that is set is; if it cannot be drawn, nothing is made and no id is used.
 */
function modulateOutcome(snapshot: Snapshot, to: Edge["to"], kind: ModulatorKind, depth: number | null, counters: Counters): Outcome {
  // (In the controller's order: the number, the node, and then what the cable is refused for.)
  if (depth !== null && !holds(depth)) return invalid(`${shown(depth)} is not a value for the depth of a cable`);
  const target = snapshot.graph.nodes.find((n) => n.id === to.node && n.kind !== "master");
  if (!target) return noNode(to.node);
  // (A group has no plugin: what a cable can drive in it is its volume and its pan, and the modulator hears the group.)
  if ((target.kind === "group" && !isGainPort(to.port)) || target.kind === "source") return invalid("a modulator drives a parameter of a plugin, a volume, a mix or a pan, or a setting of a modulator, and that is none of them");
  // The new one hears the lane of a lane's instrument or of an effect of its chain, the group of a bus effect or a group, and what a modulator hears.
  if (target.kind === "effect" && target.parent === null) return invalid("that node is in no lane or group");
  const hears = target.kind === "instrument" || target.kind === "group" ? target.id : target.kind === "effect" ? target.parent : target.kind === "modulator" ? (target.hears ?? null) : null;
  const mark = counters.mark();
  const { session, node } = withModulator(snapshot, kind, counters, hears);
  const drawn = addModEdgeOutcome(session, node.id, to, counters);
  if (!("restructured" in drawn)) {
    counters.rewind(mark);
    return drawn;
  }
  if (depth === null) return drawn;
  const edges = drawn.restructured.graph.edges.map((e) => (isModulation(e) && e.from.node === node.id ? { ...e, amount: clamp(depth, DEPTH_RANGE[0], DEPTH_RANGE[1]) } : e));
  return { restructured: { ...drawn.restructured, graph: { ...drawn.restructured.graph, edges } } };
}

/**
 * An effect put at the end of the chain of a lane or a group, as the controller does it (`Controller::add_effect`): the plugin
 * has to be one the catalog names (the mock's, `catalog`), not an instrument, and one the mock can make a node of (the built-ins:
 * the controller's own nodes, as a fixture). The graph lists the nodes of a lane, or of a group's bus, together (the lane or the group, its
 * effects, its modulators), so the effect goes after the last effect it has; and the audio path goes through it (`relinked`): the link that
 * led out of the chain (to the master, or, for a lane in a group, to the group) leaves from it now, and a link comes into it from the stage that was last (it
 * is fed: `chain`).
 */
function addEffectOutcome(snapshot: Snapshot, parent: NodeId, plugin: string, counters: Counters, catalog: readonly CatalogEntry[]): Outcome {
  const owner = snapshot.graph.nodes.find((n) => n.id === parent && n.kind !== "master");
  if (!owner) return noNode(parent);
  if (owner.kind !== "instrument" && owner.kind !== "group") return invalid("an effect goes in a lane or in a group");
  if (moduleNameOf(plugin) !== null) return invalid(A_MODULE_IS_NOT_A_PLUGIN);
  const entry = catalog.find((e) => e.id === plugin);
  if (!entry) return invalid(`${plugin} is not a plugin that is installed`);
  if (entry.isInstrument) return invalid(`${entry.name} is an instrument, not an effect`);
  const made = builtinEffects[plugin];
  if (!made) return invalid(`${entry.name} could not be loaded`);

  const { nodes, edges } = snapshot.graph;
  const lastEffect = nodes.findLastIndex((n) => n.parent === parent && n.kind === "effect");
  const end = lastEffect >= 0 ? nodes[lastEffect] : owner;
  const at = nodes.indexOf(end ?? owner) + 1;
  const added: Node = { id: counters.node(), parent, name: made.name, kind: "effect", plugin: made.plugin, mix: made.mix, feed: made.feed, ...(made.side ? { side: made.side } : {}) };
  const grown: Graph = { nodes: [...nodes.slice(0, at), added, ...nodes.slice(at)], edges };
  return { restructured: { ...snapshot, graph: { nodes: grown.nodes, edges: relinked(grown) } } };
}

/**
 * Where the node of a new lane goes among the nodes of a graph: after the lanes there are, with their effects, and before the modulators and the master,
 * as the controller lists them (`SessionModel::graph`: the groups, the lanes, the modulators, the modules, the master).
 */
function endOfLanes(nodes: readonly Node[]): number {
  const at = nodes.findIndex((n) => n.kind === "master" || n.kind === "modulator" || n.kind === "module");
  return at < 0 ? nodes.length : at;
}

/**
 * An effect that stands by itself, as the controller adds one (`Controller::add_free_effect`): a lane of its own at the end of the session, with no instrument (`(empty)`, no plugin), full range and in no group,
 * and the effect as the whole of its chain, and no way out (step 88: a bare module). The plugin is looked at first, as an effect that goes in a chain is (a plugin that is refused leaves no lane and uses no id), and then the lane's node is made, and the effect's
 * (a redo gives the ids back in that order). The graph lists the lanes with their effects, and the master last, so both go before the master; the audio path is made again from the nodes (`relinked`: the
 * lane to its effect, after the links of the lanes there were, and none out of the effect); and the layout of the telemetry gets the lane's two meters, a peak and a clip count, after those of the lanes there are.
 */
function addFreeEffectOutcome(snapshot: Snapshot, plugin: string, counters: Counters, catalog: readonly CatalogEntry[], library: ReadonlyMap<string, Module>): Outcome {
  // (A module of the library is a chain of effects: `addModuleOutcome`.)
  const module = moduleNameOf(plugin);
  if (module !== null) return addModuleOutcome(snapshot, module, counters, catalog, library);
  const entry = catalog.find((e) => e.id === plugin);
  if (!entry) return invalid(`${plugin} is not a plugin that is installed`);
  if (entry.isInstrument) return invalid(`${entry.name} is an instrument, not an effect`);
  const made = builtinEffects[plugin];
  if (!made) return invalid(`${entry.name} could not be loaded`);
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master to hear a lane");

  // (A bare module: the lane's way out is cut, so that the effect sends where cables are drawn from it and nowhere before.)
  const lane: Node = { id: counters.node(), parent: null, name: "(empty)", kind: "instrument", plugin: null, range: null, transpose: 0, volume: 1, muted: false, solo: false, route: "none", pattern: null };
  const effect: Node = { id: counters.node(), parent: lane.id, name: made.name, kind: "effect", plugin: made.plugin, mix: made.mix, feed: made.feed, ...(made.side ? { side: made.side } : {}) };
  const atNode = endOfLanes(nodes);
  const grown: Graph = { nodes: [...nodes.slice(0, atNode), lane, effect, ...nodes.slice(atNode)], edges };
  const meters: LayoutEntry[] = [
    { slot: counters.slot(), node: lane.id, port: { type: "audioOut" }, kind: "peak" },
    { slot: counters.slot(), node: lane.id, port: { type: "audioOut" }, kind: "clips" },
  ];
  return {
    restructured: {
      ...snapshot,
      graph: { nodes: grown.nodes, edges: relinked(grown) },
      telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, meters: [...snapshot.telemetry.meters, ...meters] },
    },
  };
}

/** What the controller makes of a built-in effect: see `builtinEffects`. */
type BuiltEffect = (typeof builtinEffects)[string];

/**
 * A plugin of a module as it is once the module has been given it (`give_a_module_plugin_its_values`): the built-in as it is when it is new, with the preset that the module names (found by its name, and the values that it sets
 * are the plugin's own from then on, as a preset's are), the values that the module says over it (by the name of the parameter, held to its range, an enum rounded) and the knobs pinned to its node. What the module says of a
 * preset or a parameter that the plugin has not is left out, as the controller leaves it out.
 */
function moduleWorkedPlugin(built: Plugin, saved: { readonly preset: string | null; readonly params: Readonly<Record<string, number>>; readonly pinned: readonly string[] }): Plugin {
  let plugin: Plugin = built;
  const preset = saved.preset === null ? undefined : plugin.presets.find((p) => p.name === saved.preset);
  if (preset) plugin = withValues({ ...plugin, currentPreset: preset.name }, builtinPresets[plugin.id]?.[preset.id] ?? [], true) ?? plugin;
  for (const [name, value] of Object.entries(saved.params)) {
    const param = plugin.params.find((p) => p.name === name);
    if (!param) continue;
    const held = clamp(value, param.min, param.max);
    plugin = withValues(plugin, [{ param: param.index, value: param.labels === null ? held : roundIn32Bits(held) }]) ?? plugin;
  }
  const pinned: number[] = [];
  for (const name of saved.pinned) {
    const param = plugin.params.find((p) => p.name === name);
    if (param && !pinned.includes(param.index)) pinned.push(param.index);
  }
  return { ...plugin, pinned };
}

/**
 * The node that an effect of a module makes (`settings_of_a_module_effect`): the plugin as `moduleWorkedPlugin` has it, the mix (held to its range) and whether the stage before it feeds it.
 */
function moduleEffectNode(built: BuiltEffect, saved: ModuleEffect, id: NodeId, parent: NodeId): Node {
  const [low, high] = GAIN_RANGES.mix;
  const effect: Node = { id, parent, name: built.name, kind: "effect", plugin: moduleWorkedPlugin(built.plugin, saved), mix: clamp(saved.mix, low, high), feed: saved.fed ? "chain" : "none", ...(built.side ? { side: built.side } : {}) };
  return effect;
}

/**
 * The node that a lane of a module makes of its instrument (`settings_of_a_module_instrument`): the lane that has the plugin as `moduleWorkedPlugin` has it and the volume (held to its range), at full range and bare (its way
 * out is cut, as every lane that a module makes is).
 */
function moduleInstrumentNode(made: (typeof builtinInstruments)[string], saved: ModuleInstrument, id: NodeId): Node {
  const [low, high] = GAIN_RANGES.volume;
  return { id, parent: null, name: made.name, kind: "instrument", plugin: moduleWorkedPlugin(made.plugin, saved), range: null, transpose: 0, volume: clamp(saved.volume, low, high), muted: false, solo: false, route: "none", pattern: null };
}

/**
 * A module of the library added, as the controller adds one (`load_module`): a lane of its own at the end for each of the module's, bare (its way out cut, `addFreeEffectOutcome`), each with the instrument that the module says
 * it has, if it does (`moduleInstrumentNode`) and the effects of its chain in their order (`moduleEffectNode`), and then the cables that the module has between the plugins. Every plugin is looked at before anything is made, so that a
 * module that has one the catalog does not name, one of the wrong kind, or one that the mock cannot make leaves no lane and uses no id; the nodes are made lane by lane, a lane's node first and its effects' after it, and then the
 * cables (a redo gives the ids back in that order). A cable that cannot be drawn (it would make the audio go round) is left out, as the controller leaves it. The module's modulators come after the cables (`withModuleModulators`).
 */
function addModuleOutcome(snapshot: Snapshot, name: string, counters: Counters, catalog: readonly CatalogEntry[], library: ReadonlyMap<string, Module>): Outcome {
  const module = library.get(name);
  if (!module) return invalid(`there is no module called ${name} in the library`);
  const refused = (why: string): Outcome => invalid(`the module ${name} cannot be added: ${why}`);
  const loaded: { instrument: { made: (typeof builtinInstruments)[string]; saved: ModuleInstrument } | null; effects: { built: BuiltEffect; saved: ModuleEffect }[] }[] = [];
  for (const lane of module.lanes) {
    let instrument: (typeof loaded)[number]["instrument"] = null;
    if (lane.instrument) {
      const entry = catalog.find((e) => e.id === lane.instrument?.plugin);
      const made = builtinInstruments[lane.instrument.plugin];
      const why = !entry ? `${lane.instrument.plugin} is not a plugin that is installed` : !entry.isInstrument ? `${entry.name} is an effect, not an instrument` : !made ? `${entry.name} could not be loaded` : null;
      if (why !== null || !made) return refused(why ?? "it could not be loaded");
      instrument = { made, saved: lane.instrument };
    }
    const effects: { built: BuiltEffect; saved: ModuleEffect }[] = [];
    for (const saved of lane.effects) {
      const entry = catalog.find((e) => e.id === saved.plugin);
      const built = builtinEffects[saved.plugin];
      const why = !entry ? `${saved.plugin} is not a plugin that is installed` : entry.isInstrument ? `${entry.name} is an instrument, not an effect` : !built ? `${entry.name} could not be loaded` : null;
      if (why !== null || !built) return refused(why ?? "it could not be loaded");
      effects.push({ built, saved });
    }
    loaded.push({ instrument, effects });
  }
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master to hear a lane");

  // (Bare, as every effect that stands by itself is: the way out of each lane is cut. A lane's meters are made with the lane, so that the layout lists them lane by lane.)
  const made: Node[] = [];
  const meters: LayoutEntry[] = [];
  const lanes: { lane: Node; effects: Node[] }[] = [];
  for (const { instrument, effects } of loaded) {
    const lane: Node = instrument
      ? moduleInstrumentNode(instrument.made, instrument.saved, counters.node())
      : { id: counters.node(), parent: null, name: "(empty)", kind: "instrument", plugin: null, range: null, transpose: 0, volume: 1, muted: false, solo: false, route: "none", pattern: null };
    meters.push({ slot: counters.slot(), node: lane.id, port: { type: "audioOut" }, kind: "peak" }, { slot: counters.slot(), node: lane.id, port: { type: "audioOut" }, kind: "clips" });
    const fx = effects.map(({ built, saved }) => moduleEffectNode(built, saved, counters.node(), lane.id));
    made.push(lane, ...fx);
    lanes.push({ lane, effects: fx });
  }
  const atNode = endOfLanes(nodes);
  const grown: Graph = { nodes: [...nodes.slice(0, atNode), ...made, ...nodes.slice(atNode)], edges };
  let session: Snapshot = {
    ...snapshot,
    graph: { nodes: grown.nodes, edges: relinked(grown) },
    telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, meters: [...snapshot.telemetry.meters, ...meters] },
  };
  for (const cable of module.cables) {
    const start = cable.from.effect === null ? lanes[cable.from.lane]?.lane : lanes[cable.from.lane]?.effects[cable.from.effect];
    const end = lanes[cable.to.lane]?.effects[cable.to.effect];
    if (!start || !end) continue;
    const drawn = addCableOutcome(session, { node: start.id, port: { type: "audioOut" } }, { node: end.id, port: { type: cable.to.side ? "audioInSide" : "audioIn" } }, cable.gain, counters);
    if ("restructured" in drawn) session = drawn.restructured;
  }
  const withModulators = withModuleModulators(session, module, lanes, counters);
  return { restructured: withModuleRecord(withModulators.session, name, [...lanes.map(({ lane }) => lane.id), ...withModulators.made.map((n) => n.id)], counters) };
}

/**
 * The session with the record of a module that was added as one thing (`make_module`, step 108): a node of its own, made last, so that what is made before it has the ids it had, which the graph lists after the modulators and before the master
 * (`SessionModel::graph`). `members` are the lanes of the module and then its modulators, in the order they were made.
 */
function withModuleRecord(snapshot: Snapshot, name: string, members: readonly NodeId[], counters: Counters, ui?: NodeUi): Snapshot {
  const { nodes } = snapshot.graph;
  const master = nodes.findIndex((n) => n.kind === "master");
  const at = master >= 0 ? master : nodes.length;
  const record: Node = { id: counters.node(), parent: null, name, kind: "module", members: [...members], ...(ui ? { ui } : {}) };
  return { ...snapshot, graph: { ...snapshot.graph, nodes: [...nodes.slice(0, at), record, ...nodes.slice(at)] } };
}

/**
 * What a modulator is made of a module's source, as the loader makes it of a file's (`build_mod_source`): a knob is held to what a knob goes to, a fade to what a fade goes to, the steps of a sequencer to what they can be (a value, a glide
 * and a length), and a control that is bound to nothing is waiting for the next that is moved (`learning`).
 */
function heldSource(source: ModulatorSource): ModulatorSource {
  switch (source.type) {
    case "lfo":
      return { ...source, fade: clamp(source.fade, MODULATOR_FIELD_RANGES.fade[0], MODULATOR_FIELD_RANGES.fade[1]) };
    case "macro":
      return { ...source, value: clamp(source.value, MODULATOR_FIELD_RANGES.value[0], MODULATOR_FIELD_RANGES.value[1]) };
    case "midiLearn":
      return { ...source, learning: source.control === null };
    case "steps":
      return {
        ...source,
        values: source.values.map((value) => clamp(value, STEP_RANGE[0], STEP_RANGE[1])),
        length: clamp(source.length, MODULATOR_FIELD_RANGES.length[0], MODULATOR_FIELD_RANGES.length[1]),
        glide: clamp(source.glide, MODULATOR_FIELD_RANGES.glide[0], MODULATOR_FIELD_RANGES.glide[1]),
      };
    case "envelope":
      return source;
  }
}

/** What a node of a modulator that is what `source` says is called (`ModSourceSlot::node_name`): its kind, or what a control is bound to. */
function nameOfSource(source: ModulatorSource): string {
  switch (source.type) {
    case "lfo":
      return "LFO";
    case "envelope":
      return "ADSR";
    case "macro":
      return "Macro";
    case "steps":
      return "Steps";
    case "midiLearn":
      switch (source.control?.type) {
        case undefined:
          return source.learning ? "MIDI (learning…)" : "MIDI (unbound)";
        case "cc":
          return `CC ${String(source.control.number)}`;
        case "pitchBend":
          return "Pitch Bend";
        case "aftertouch":
          return "Aftertouch";
        case "velocity":
          return "Velocity";
        case "key":
          return "Key";
      }
  }
}

/**
 * The modulators of a module, made after its lanes, its plugins and its audio cables (`make_module_modulators`, step 106): all of them first, each hearing the lane of the module that it heard (the lane that was made, which is
 * `lanes[…]`) or every note, so that a cable can drive the settings of one that comes after it, and then their cables. A cable that drives the depth of another waits for it: those are made last, in sweeps over the ones that wait,
 * each as soon as the cable that it drives is there (one that drives a cable that is never made is left out). Every other cable is made in the order of the file. What cannot be made (a parameter that the plugin has not, a
 * cable that drives what it drives already, a setting that the modulator has not) is left out, with nothing used for it.
 */
function withModuleModulators(snapshot: Snapshot, module: Module, lanes: readonly { lane: Node; effects: readonly Node[] }[], counters: Counters): { session: Snapshot; made: readonly Node[] } {
  let session = snapshot;
  const made: Node[] = [];
  for (const modulator of module.modulators) {
    const source = heldSource(modulator.source);
    const ear = modulator.hears === null ? null : (lanes[modulator.hears]?.lane.id ?? null);
    const added = withModulatorOf(session, nameOfSource(source), source, counters, ear);
    session = added.session;
    made.push(added.node);
  }
  const edges: (EdgeId | null)[][] = module.modulators.map((modulator) => modulator.targets.map(() => null));
  const draw = (m: number, target: ModuleTarget, driven: EdgeId | null): EdgeId | null => {
    const from = made[m];
    if (!from) return null;
    const drawn = moduleCable(session, from, target, made, lanes, driven, counters);
    if (!drawn) return null;
    session = drawn.session;
    return drawn.edge;
  };
  let waiting: [number, number][] = [];
  module.modulators.forEach((modulator, m) => {
    modulator.targets.forEach((target, t) => {
      if (target.drives.type === "depth") {
        waiting.push([m, t]);
        return;
      }
      const edge = draw(m, target, null);
      if (edge !== null && edges[m]) edges[m][t] = edge;
    });
  });
  for (let progressed = true; progressed; ) {
    progressed = false;
    const still: [number, number][] = [];
    for (const [m, t] of waiting) {
      const target = module.modulators[m]?.targets[t];
      if (target?.drives.type !== "depth") continue;
      const driven = edges[target.drives.modulator]?.[target.drives.target] ?? null;
      if (driven === null) {
        still.push([m, t]);
        continue;
      }
      progressed = true;
      const edge = draw(m, target, driven);
      if (edge !== null && edges[m]) edges[m][t] = edge;
    }
    waiting = still;
  }
  return { session, made };
}

/**
 * The cable that `target` of a module's modulator `from` is, drawn in `session` (`module_cable`): to the parameter of a plugin of the module that has the name, to the volume or the mix of one, to a setting of another of the
 * module's modulators (`made`) or, with the id of the cable `driven` whose depth it drives, to that; at the depth that the module says (held to the range of a depth). The session with it and its id, or none if it cannot be drawn
 * (nothing is used for one that is not).
 */
function moduleCable(
  session: Snapshot,
  from: Node,
  target: ModuleTarget,
  made: readonly Node[],
  lanes: readonly { lane: Node; effects: readonly Node[] }[],
  driven: EdgeId | null,
  counters: Counters,
): { session: Snapshot; edge: EdgeId } | null {
  const plugin = (place: ModuleOutput): Node | undefined => (place.effect === null ? lanes[place.lane]?.lane : lanes[place.lane]?.effects[place.effect]);
  const { drives } = target;
  let drawn: Outcome;
  if (drives.type === "param") {
    const node = plugin(drives.plugin);
    const row = node && (node.kind === "instrument" || node.kind === "effect") ? node.plugin?.params.find((p) => p.name === drives.name) : undefined;
    if (!node || !row) return null;
    drawn = addParamCable(session, from, node.id, row.index, counters);
  } else if (drives.type === "gain") {
    const node = plugin(drives.plugin);
    if (!node) return null;
    drawn = addGainCable(session, from, node.id, { type: drives.gain }, counters);
  } else if (drives.type === "setting") {
    const node = made[drives.modulator];
    if (!node) return null;
    drawn = addFieldCable(session, from, { node: node.id, port: { type: drives.port } }, counters);
  } else {
    const node = made[drives.modulator];
    if (!node || driven === null) return null;
    drawn = addFieldCable(session, from, { node: node.id, port: { type: "depth", edge: driven } }, counters);
  }
  if (!("restructured" in drawn)) return null;
  const before = new Set(session.graph.edges.filter(isModulation).map((e) => e.id));
  const cable = drawn.restructured.graph.edges.find((e): e is Edge & { kind: "modulation"; id: EdgeId } => isModulation(e) && !before.has(e.id));
  if (!cable) return null;
  const amount = clamp(target.depth, DEPTH_RANGE[0], DEPTH_RANGE[1]);
  const edges = drawn.restructured.graph.edges.map((e) => (e === cable ? { ...e, amount } : e));
  return { session: { ...drawn.restructured, graph: { ...drawn.restructured.graph, edges } }, edge: cable.id };
}

/**
 * A chain of effects saved as a module (`save_module_intent`): the chain of the lane that `node` is, or is an effect in, under the name that `cleanModuleName` makes of `name`. The node is looked for first, then the name, then
 * the chain: a node that is not there is not found whatever the name is, and a name that is no name is refused before the chain is looked at. The event says the name the module has; what the library keeps is the server's.
 */
function saveModuleOutcome(snapshot: Snapshot, node: NodeId, name: string): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node);
  if (!found || node === MASTER) return noNode(node);
  const cleaned = cleanModuleName(name);
  if ("error" in cleaned) return invalid(cleaned.error);
  const module = moduleOf(snapshot, found);
  if (typeof module === "string") return invalid(module);
  return { event: { type: "moduleSaved", name: cleaned.name } };
}

/**
 * The plugins in `nodes` saved as a module (`save_selection_intent`, step 105): the nodes are looked for first (the first that is not there is not found), then the name, then what they make (`moduleOfNodes`).
 */
function saveSelectionOutcome(snapshot: Snapshot, nodes: readonly NodeId[], name: string): Outcome {
  const chosen: Node[] = [];
  for (const id of nodes) {
    const found = snapshot.graph.nodes.find((n) => n.id === id);
    if (!found || id === MASTER) return noNode(id);
    chosen.push(found);
  }
  const cleaned = cleanModuleName(name);
  if ("error" in cleaned) return invalid(cleaned.error);
  const module = moduleOfNodes(snapshot, chosen);
  if (typeof module === "string") return invalid(module);
  return { event: { type: "moduleSaved", name: cleaned.name } };
}

/**
 * The instrument `plugin` as the controller makes a node of it (`load_from_catalog`): the plugin has to be one the catalog names
 * (the mock's, `catalog`), an instrument and not an effect, and one the mock can make a node of (the built-ins: the controller's
 * own nodes, as a fixture); or why not.
 */
function instrumentFor(plugin: string, catalog: readonly CatalogEntry[]): { made: (typeof builtinInstruments)[string] } | { error: Outcome } {
  if (moduleNameOf(plugin) !== null) return { error: invalid(A_MODULE_IS_NOT_A_PLUGIN) };
  const entry = catalog.find((e) => e.id === plugin);
  if (!entry) return { error: invalid(`${plugin} is not a plugin that is installed`) };
  if (!entry.isInstrument) return { error: invalid(`${entry.name} is an effect, not an instrument`) };
  const made = builtinInstruments[plugin];
  if (!made) return { error: invalid(`${entry.name} could not be loaded`) };
  return { made };
}

/** Where the links of the audio path end among the edges: the graph lists them first, then the links of the lanes that hear every source, the audio cables, the cables of notes and those of modulation. */
const endOfLinks = (edges: readonly Edge[]): number => {
  const at = edges.findIndex((e) => !isLink(e));
  return at < 0 ? edges.length : at;
};

/**
 * A lane added at the end with an instrument in it, as the controller does it (`Controller::add_instrument_lane`): full range, in no
 * group. The graph lists the groups, then the lanes, then the master, so the lane goes before the master; its link to the master
 * comes after the links the others have (the rest of the edges come after all of them); and the layout of the telemetry gets its
 * two meters, a peak and a clip count, after those of the lanes there are. An instrument that starts bare (the audio input: its fixture
 * says `route: none`) is in a lane whose way out is cut and has no link to the master, so that it is heard through the cables that are drawn from it and nowhere before.
 */
function addInstrumentOutcome(snapshot: Snapshot, plugin: string, counters: Counters, catalog: readonly CatalogEntry[]): Outcome {
  const found = instrumentFor(plugin, catalog);
  if ("error" in found) return found.error;
  const { made } = found;
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master to hear a lane");

  const added: Node = { id: counters.node(), parent: null, name: made.name, kind: "instrument", plugin: made.plugin, range: made.range, transpose: made.transpose, volume: made.volume, muted: false, solo: false, route: made.route, pattern: null };
  const atNode = endOfLanes(nodes);
  const links: Edge[] = made.route === "none" ? [] : [{ from: { node: added.id, port: { type: "audioOut" } }, to: { node: master.id, port: { type: "audioIn" } }, kind: "audio" }];
  const atEdge = endOfLinks(edges);
  const meters: LayoutEntry[] = [
    { slot: counters.slot(), node: added.id, port: { type: "audioOut" }, kind: "peak" },
    { slot: counters.slot(), node: added.id, port: { type: "audioOut" }, kind: "clips" },
  ];
  return {
    restructured: {
      ...snapshot,
      graph: { nodes: [...nodes.slice(0, atNode), added, ...nodes.slice(atNode)], edges: [...edges.slice(0, atEdge), ...links, ...edges.slice(atEdge)] },
      telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, meters: [...snapshot.telemetry.meters, ...meters] },
    },
  };
}

/**
 * An instrument put in a lane in place of the one it has (or in an empty lane), as the controller does it (`Controller::put_instrument`):
 * the node stays, with its name and plugin the new instrument's and its range, transposition and volume as they were. The cables that
 * drove the old plugin's parameters go first, a lane's and a group's, with what drove the depth of any of them: they name a parameter
 * by the old plugin's own index.
 */
function replaceInstrumentOutcome(snapshot: Snapshot, lane: NodeId, plugin: string, catalog: readonly CatalogEntry[]): Outcome {
  const target = snapshot.graph.nodes.find((n) => n.id === lane && n.kind !== "master");
  if (!target) return noNode(lane);
  if (target.kind !== "instrument") return invalid("an instrument goes in a lane");
  const found = instrumentFor(plugin, catalog);
  if ("error" in found) return found.error;
  const { made } = found;

  // (Only the cables that drove the old plugin's parameters go: the lane's volume is the lane's, and the new instrument is in the lane all the same.)
  const touching = snapshot.graph.edges.filter(isModulation).filter((e) => e.to.node === lane && e.to.port.type === "param").map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set());
  const replaced: Node = { ...target, name: made.name, plugin: made.plugin };
  return { restructured: { ...taken, graph: { ...taken.graph, nodes: taken.graph.nodes.map((n) => (n.id === lane ? replaced : n)) } } };
}

/**
 * The instrument taken out of a lane, as the controller does it (`Controller::clear_instrument`): the node stays, an empty lane called so (`(empty)`, no plugin), with what else the lane has
 * (its effects, its modulators, its range, its volume and the rest). The cables that drove the old plugin's parameters go first, a lane's and a group's, with what drove the depth of any of them.
 */
function clearInstrumentOutcome(snapshot: Snapshot, lane: NodeId): Outcome {
  const target = snapshot.graph.nodes.find((n) => n.id === lane && n.kind !== "master");
  if (!target) return noNode(lane);
  if (target.kind !== "instrument") return invalid("an instrument is taken out of a lane");
  if (target.plugin === null) return invalid("the lane has no instrument to take out");
  const touching = snapshot.graph.edges.filter(isModulation).filter((e) => e.to.node === lane).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set());
  const emptied: Node = { ...target, name: "(empty)", plugin: null };
  return { restructured: { ...taken, graph: { ...taken.graph, nodes: taken.graph.nodes.map((n) => (n.id === lane ? emptied : n)) } } };
}

/**
 * A preset loaded into the plugin of `node`, as the controller does it (`load_preset`): the plugin has to be one of a lane (a bus
 * effect's cannot be loaded into: the audio thread addresses a preset by the lane's slot), and has the preset; the event says
 * the preset's name and the values the plugin has for its parameters after it. A built-in's are the controller's own (a fixture);
 * a plugin of the machine's the mock knows nothing of, so it says none.
 */
function presetOutcome(snapshot: Snapshot, node: Node, id: string): Outcome {
  if (node.kind !== "instrument" && node.kind !== "effect") return invalid("that node has no plugin");
  if (node.kind === "effect" && snapshot.graph.nodes.find((n) => n.id === node.parent)?.kind === "group") return invalid("a bus effect has no presets to load");
  if (!node.plugin) return invalid("that instrument has no plugin");
  const preset = node.plugin.presets.find((p) => p.id === id);
  if (!preset) return invalid(`the plugin has no preset ${id}`);
  const values = builtinPresets[node.plugin.id]?.[id] ?? [];
  return { event: { type: "presetLoaded", node: node.id, preset: preset.name, values } };
}

/**
 * The keys of a lane set, as the controller's `set_range` does it: only a lane has them (an empty one too), and they are
 * notes, whole numbers from 0 to 127, the low one not above the high one; none is every note.
 */
function rangeOutcome(node: Node, range: NoteRange | null): Outcome {
  if (node.kind !== "instrument") return invalid("a key range belongs to a lane");
  if (range !== null) {
    for (const note of [range.low, range.high]) {
      if (!Number.isInteger(note) || note < 0 || note > 127) return invalid(`${String(note)} is not a note: the notes are 0 to 127`);
    }
    if (range.low > range.high) return invalid(`the range goes from ${noteName(range.low)} down to ${noteName(range.high)}: its low note has to be the lower`);
  }
  return { event: { type: "rangeSet", node: node.id, range } };
}

/**
 * The piano's scale set, as the controller's `set_scale` does it: the text has to be a root and a kind of scale (the flats and
 * the long names too, in any case). The event tells the scale as it is saved, and its notes.
 */
function scaleOutcome(text: string): Outcome {
  const scale = parseScale(text);
  if (!scale) return invalid(`${JSON.stringify(text)} is not a scale: a root and a kind of scale, like C major or F# dorian`);
  return { event: { type: "scaleSet", ...scale } };
}

/** A coordinate of a place as the controller keeps it (`NodeUi::tidy`): to a tenth, half away from zero, within the limits, and never `-0`. */
function tidyCoordinate(value: number): number {
  const kept = (Math.sign(value) * Math.round(Math.abs(clamp(value, -NODE_UI_LIMIT, NODE_UI_LIMIT)) * NODE_UI_PARTS)) / NODE_UI_PARTS;
  return kept + 0;
}

/**
 * A node's place set, as the controller's `set_node_ui` does it: any node of the graph, the master too (which the controller
 * finds though it is not a node of the model), and a place that is two numbers, kept as `tidyCoordinate` says; none gives the
 * node back to the layout.
 */
function placeOutcome(snapshot: Snapshot, node: NodeId, ui: NodeUi | null): Outcome {
  if (!snapshot.graph.nodes.some((n) => n.id === node)) return noNode(node);
  if (ui === null) return { event: { type: "nodeUiSet", node, ui } };
  if (!holds(ui.x) || !holds(ui.y)) return invalid(`(${String(ui.x)}, ${String(ui.y)}) is not a place`);
  return { event: { type: "nodeUiSet", node, ui: { x: tidyCoordinate(ui.x), y: tidyCoordinate(ui.y) } } };
}

/**
 * Parameters pinned to a node's plugin, as the controller's `set_pinned` does it: a node that has a plugin (the master is no node of the
 * model, as far as the controller goes), a list that is not longer than a node may have, of parameters the plugin has, each once. The
 * controller refuses a separator too, which the page's graph does not have.
 */
function pinnedOutcome(snapshot: Snapshot, node: NodeId, params: readonly number[]): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node);
  if (!found || node === MASTER) return noNode(node);
  if (found.kind !== "instrument" && found.kind !== "effect") return invalid("that node has no plugin");
  const plugin = found.plugin;
  if (!plugin) return invalid("that instrument has no plugin");
  if (params.length > PINNED_LIMIT) return invalid(`at most ${PINNED_LIMIT} parameters can be pinned to a node`);
  for (const [i, param] of params.entries()) {
    if (plugin.params.every((p) => p.index !== param)) return invalid(`the plugin has no parameter ${param}`);
    if (params.indexOf(param) < i) return invalid(`parameter ${param} is listed twice`);
  }
  return { event: { type: "pinnedSet", node, params: [...params] } };
}

/**
 * A group named, as the controller's `rename_group` does it: only a group has a name (the master is no node of the model, as far as the
 * controller goes), taken without the spaces round it (what Rust's `trim` takes: the Unicode white space), at most `NAME_LIMIT`
 * characters (the characters that a person sees, not the halves of a pair that a string counts), and none of them a control character.
 * A name of nothing is no name: the group is called "Group" and its place among the groups, counting from one.
 */
function renameGroupOutcome(snapshot: Snapshot, node: NodeId, name: string): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node);
  if (!found || node === MASTER) return noNode(node);
  if (found.kind !== "group") return invalid("only a group has a name to change");
  const tidied = tidyName(name);
  if ("error" in tidied) return invalid(tidied.error);
  const place = snapshot.graph.nodes.filter((n) => n.kind === "group").findIndex((n) => n.id === node);
  return { event: { type: "groupRenamed", node, name: tidied.name === "" ? `Group ${place + 1}` : tidied.name } };
}

/** The lane `lane` put in the group `group` (or in none), the snapshot as it is: see `setLaneGroupOutcome`. Both are known to be what they should be. */
function lanePutIn(snapshot: Snapshot, lane: NodeId, group: NodeId | null): Snapshot {
  const found = snapshot.graph.nodes.find((n) => n.id === lane);
  const master = snapshot.graph.nodes.find((n) => n.kind === "master");
  if (!found || !master || found.parent === group) return snapshot;
  const { nodes } = snapshot.graph;
  // (The cables of the modulators that hear the group it leaves stay: a cable names the plugin it drives by its node, and the lane keeps its plugins.)
  // The lane's node is a member of the group, and the last link of its chain, the one that leaves the chain, goes into the group (or the master).
  const chain = new Set<NodeId>([lane, ...nodes.filter((n) => n.parent === lane && n.kind === "effect").map((n) => n.id)]);
  const edges = snapshot.graph.edges.map((e): Edge => {
    if (!isLink(e) || !chain.has(e.from.node) || chain.has(e.to.node)) return e;
    return { from: e.from, to: { node: group ?? master.id, port: { type: "audioIn" } }, kind: group === null ? "audio" : "member" };
  });
  return { ...snapshot, graph: { nodes: nodes.map((n) => (n.id === lane ? { ...n, parent: group } : n)), edges } };
}

/**
 * A lane put in a group, or taken out of its group (`null`), as the controller does it (`Controller::set_lane_group`): the node has to be a
 * lane and the group a group (the master is no node of the model, as far as the controller goes). No cable goes (`lanePutIn`: a cable names
 * its plugin by node), and the audio path of the lane ends in its new group, or in the master.
 */
function setLaneGroupOutcome(snapshot: Snapshot, node: NodeId, group: NodeId | null): Outcome {
  const lane = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!lane) return noNode(node);
  if (lane.kind !== "instrument") return invalid("only a lane can be put in a group");
  if (group !== null) {
    const target = snapshot.graph.nodes.find((n) => n.id === group && n.kind !== "master");
    if (!target) return noNode(group);
    if (target.kind !== "group") return invalid("that node is not a group");
  }
  if (laneLoops(snapshot.graph, node, group)) return invalid("putting that lane in that group would make the audio go round, by the cables there are");
  return { restructured: lanePutIn(snapshot, node, group) };
}

/**
 * The way out of a lane or a group cut or given back, as the controller does it (`set_route_intent`): the node has to be a lane or a group (the master is no node of the model, as far as the
 * controller goes), and a way out that is given back is refused where the audio would go round with the cables there are. The links of the audio path are then the graph's for the nodes as they
 * are (`linksOf`): a chain whose way out is cut ends at its last node, and one that is given back goes into its group or the mix again, where the graph lists it.
 */
function setRouteOutcome(snapshot: Snapshot, node: NodeId, route: Route): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind !== "instrument" && found.kind !== "group") return invalid("only a lane or a group has a way out");
  if (route === "out" && routeLoops(snapshot.graph, node, route)) {
    return invalid(`giving that ${found.kind === "instrument" ? "lane" : "group"} its way out would make the audio go round, by the cables there are`);
  }
  const nodes = snapshot.graph.nodes.map((n) => (n.id === node && (n.kind === "instrument" || n.kind === "group") ? { ...n, route } : n));
  return { restructured: { ...snapshot, graph: { nodes, edges: [...linksOf(nodes), ...snapshot.graph.edges.filter((e) => !isLink(e))] } } };
}

/**
 * Whose notes a modulator hears, as the controller does it (`set_modulator_ear_intent`): the node has to be a modulator (the master is no node of the model, as far as the controller goes), and the
 * ear a lane or a group, or none for every note. The graph says it as `hears`, and says nothing for none.
 */
function setModulatorEarOutcome(snapshot: Snapshot, node: NodeId, ear: NodeId | null): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind !== "modulator") return invalid("only a modulator has an ear");
  if (ear !== null) {
    const heard = snapshot.graph.nodes.find((n) => n.id === ear && n.kind !== "master");
    if (!heard) return noNode(ear);
    if (heard.kind !== "instrument" && heard.kind !== "group") return invalid("only a lane or a group can be heard");
  }
  return { restructured: { ...snapshot, graph: { ...snapshot.graph, nodes: snapshot.graph.nodes.map((n) => (n === found ? withEar(found, ear) : n)) } } };
}

/** The modulator `node` hearing `ear`, or every note for none: a modulator that hears every note says nothing of it (`hears` is absent on the wire). */
function withEar(node: Extract<Node, { kind: "modulator" }>, ear: NodeId | null): Node {
  const copy = { ...node };
  if (ear === null) delete copy.hears;
  else copy.hears = ear;
  return copy;
}

/**
 * The session with the ears that name a lane or a group that is gone taken out of its modulators, as the graph the controller gives has them (it says whom a modulator hears if it is a lane or a group that is there,
 * and nothing else: the audio thread hears every note for a lane that went). The same snapshot if there is none, so that nothing that compares them finds a change.
 */
function withoutLostEars(snapshot: Snapshot): Snapshot {
  const { nodes } = snapshot.graph;
  const heard = new Set(nodes.filter((n) => n.kind === "instrument" || n.kind === "group").map((n) => n.id));
  const lost = nodes.some((n) => n.kind === "modulator" && n.hears !== undefined && !heard.has(n.hears));
  if (!lost) return snapshot;
  return { ...snapshot, graph: { ...snapshot.graph, nodes: nodes.map((n) => (n.kind === "modulator" && n.hears !== undefined && !heard.has(n.hears) ? withEar(n, null) : n)) } };
}

/**
 * The link into an effect cut or given back, as the controller does it (`set_feed_intent`): the node has to be an effect of a lane's chain or of a group's bus (the master is no node of the model, as far as the
 * controller goes), and a link that is given back is refused where the audio would go round with the cables there are. The links of the audio path are then the graph's for the nodes as they are
 * (`linksOf`: no link goes into an effect that is not fed, and the chain goes on from it).
 */
function setFeedOutcome(snapshot: Snapshot, node: NodeId, feed: Feed): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind !== "effect") return invalid("only an effect has a feed");
  if (feed === "chain" && feedLoops(snapshot.graph, node, feed)) return invalid("giving that effect its input back would make the audio go round, by the cables there are");
  const nodes = snapshot.graph.nodes.map((n) => (n.id === node && n.kind === "effect" ? { ...n, feed } : n));
  return { restructured: { ...snapshot, graph: { nodes, edges: [...linksOf(nodes), ...snapshot.graph.edges.filter((e) => !isLink(e))] } } };
}

/**
 * A group added at the end, with the lanes in `members` put in it, as the controller does it (`add_group_intent`): every member is looked at
 * first, and one that is no lane refuses the whole. The graph lists the groups (each with its effects and modulators), then the lanes, then the
 * master, so the group goes before the first lane; its link to the master comes after the links of the groups there are, and its two meters in
 * the layout after theirs (the master's, the groups', the lanes').
 */
function addGroupOutcome(snapshot: Snapshot, members: readonly NodeId[], counters: Counters): Outcome {
  for (const member of members) {
    const lane = snapshot.graph.nodes.find((n) => n.id === member && n.kind !== "master");
    if (!lane) return noNode(member);
    if (lane.kind !== "instrument") return invalid("only a lane can be put in a group");
  }
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master to hear a group");

  const id = counters.node();
  const added: Node = { id, parent: null, name: `Group ${nodes.filter((n) => n.kind === "group").length + 1}`, kind: "group", volume: 1, pan: 0, muted: false, solo: false, route: "out" };
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const inLane = (node: NodeId): boolean => {
    const found = byId.get(node);
    const owner = found?.kind === "effect" && found.parent !== null ? byId.get(found.parent) : found;
    return owner?.kind === "instrument";
  };
  const firstLane = nodes.findIndex((n) => n.kind === "instrument");
  const atNode = firstLane >= 0 ? firstLane : endOfLanes(nodes);
  const firstLink = edges.findIndex((e) => isLink(e) && inLane(e.from.node));
  const atEdge = firstLink >= 0 ? firstLink : endOfLinks(edges);
  const link: Edge = { from: { node: id, port: { type: "audioOut" } }, to: { node: master.id, port: { type: "audioIn" } }, kind: "audio" };
  const meters: LayoutEntry[] = [
    { slot: counters.slot(), node: id, port: { type: "audioOut" }, kind: "peak" },
    { slot: counters.slot(), node: id, port: { type: "audioOut" }, kind: "clips" },
  ];
  const firstMeter = snapshot.telemetry.meters.findIndex((m) => byId.get(m.node)?.kind === "instrument");
  const atMeter = firstMeter >= 0 ? firstMeter : snapshot.telemetry.meters.length;
  const made: Snapshot = {
    ...snapshot,
    graph: { nodes: [...nodes.slice(0, atNode), added, ...nodes.slice(atNode)], edges: [...edges.slice(0, atEdge), link, ...edges.slice(atEdge)] },
    telemetry: {
      ...snapshot.telemetry,
      rev: snapshot.telemetry.rev + 1,
      meters: [...snapshot.telemetry.meters.slice(0, atMeter), ...meters, ...snapshot.telemetry.meters.slice(atMeter)],
    },
  };
  return { restructured: members.reduce((session, lane) => lanePutIn(session, lane, id), made) };
}

/** The tempo that is set (`set_bpm`): a number held to the tempos there can be, or the controller's words for what is not one. */
function bpmOutcome(bpm: number): Outcome {
  if (!holds(bpm)) return invalid(`${shown(bpm)} is not a tempo`);
  const [low, high] = BPM_RANGE;
  return { event: { type: "bpmSet", bpm: clamp(bpm, low, high) } };
}

/** The pattern a lane is given when it has none and something is asked of it (`PatternState::empty`): the length a new pattern has, looping, not in key, not heard, at the tempo of the session. */
const emptyPattern = (bpm: number): PatternInfo => ({ recording: false, enabled: false, looping: true, inKey: false, lengthBeats: PATTERN_DEFAULT_LENGTH, bpm, notes: 0 });

/**
 * What an intent about the pattern of a lane comes to (`set_recording`, `clear_pattern`, `set_pattern_field`): the pattern the lane has afterwards, as one event. A recording
 * that is started gives a lane with no pattern an empty one; one that is started while it is going on, or stopped while it is not, is told as it is. A setting is held to its
 * range and a switch rounded, and gives a lane with no pattern an empty one, so that the length of its first recording can be chosen. A recording that comes to an end by itself
 * is the engine's, which the mock has no notes for.
 */
function patternOutcome(snapshot: Snapshot, intent: Extract<Intent, { type: "setRecording" | "clearPattern" | "setPatternField" }>): Outcome {
  // (A number is judged before the node it is for.)
  if (intent.type === "setPatternField" && !holds(intent.value)) return invalid(`${shown(intent.value)} is not a value for ${variant(intent.field)}`);
  // (The master is a node of the graph and not of the model: the controller does not find it.)
  const node = snapshot.graph.nodes.find((n) => n.id === intent.node && n.kind !== "master");
  if (!node) return noNode(intent.node);
  if (node.kind !== "instrument") return invalid("a pattern belongs to a lane");
  const told = (pattern: PatternInfo | null): Outcome => ({ event: { type: "patternSet", node: node.id, pattern } });
  const bpm = snapshot.meta.bpm;
  switch (intent.type) {
    case "setRecording":
      if (intent.recording === (node.pattern?.recording === true)) return told(node.pattern);
      return told({ ...(node.pattern ?? emptyPattern(bpm)), recording: intent.recording });
    case "clearPattern":
      return node.pattern === null ? invalid("that lane has no pattern") : told(null);
    case "setPatternField": {
      const [low, high] = PATTERN_FIELD_RANGES[intent.field];
      const held = clamp(intent.value, low, high);
      const pattern = node.pattern ?? emptyPattern(bpm);
      switch (intent.field) {
        case "length":
          return told({ ...pattern, lengthBeats: held });
        case "enabled":
          return told({ ...pattern, enabled: roundIn32Bits(held) !== 0 });
        case "looping":
          return told({ ...pattern, looping: roundIn32Bits(held) !== 0 });
        case "transpose":
          return told({ ...pattern, inKey: roundIn32Bits(held) !== 0 });
      }
    }
  }
}

/** The session saved to its own file, or why it cannot be (the controller's words for a session with no file). */
function saveOutcome(snapshot: Snapshot): Outcome {
  if (snapshot.meta.file === null) return { error: { code: "io", message: "No session path — cannot save" } };
  return { event: { type: "saved", file: snapshot.meta.file } };
}

/**
 * The session saved as another file (`save_as_intent`): the file that the name makes (`sessionFileName`), beside the session's own, which is not written over
 * if another file of that name is there already (the session's own is: that is a save). `files` is what the directory has.
 */
function saveAsOutcome(snapshot: Snapshot, name: string, files: ReadonlySet<string>): Outcome {
  const made = sessionFileName(name);
  if ("error" in made) return invalid(made.error);
  if (made.file !== snapshot.meta.file && files.has(made.file)) return invalid(`a file called ${made.file} is there already`);
  return { event: { type: "saved", file: made.file } };
}

/**
 * Do what the controller does with an intent, to a snapshot, with the plugins its catalog names and the files its directory has. A change of structure leaves the links that the graph derives
 * from each source to each lane that has no cable of notes as the cables say (`withHears`): a lane that is added, or whose last cable goes, hears every source.
 */
function outcomeOf(snapshot: Snapshot, intent: Intent, counters: Counters, catalog: readonly CatalogEntry[], files: ReadonlySet<string>, library: ReadonlyMap<string, Module>): Outcome {
  const outcome = intentOutcome(snapshot, intent, counters, catalog, files, library);
  return "restructured" in outcome ? { restructured: { ...outcome.restructured, graph: withHears(outcome.restructured.graph) } } : outcome;
}

/** What the controller makes of an intent: see `outcomeOf`. */
function intentOutcome(snapshot: Snapshot, intent: Intent, counters: Counters, catalog: readonly CatalogEntry[], files: ReadonlySet<string>, library: ReadonlyMap<string, Module>): Outcome {
  if (intent.type === "save") return saveOutcome(snapshot);
  if (intent.type === "saveAs") return saveAsOutcome(snapshot, intent.name, files);
  if (intent.type === "saveModule") return saveModuleOutcome(snapshot, intent.node, intent.name);
  if (intent.type === "saveSelectionAsModule") return saveSelectionOutcome(snapshot, intent.nodes, intent.name);
  // (Undo and Redo are not the outcome of an intent on a snapshot: the history is the server's, `MockServer#carry`.)
  if (intent.type === "undo" || intent.type === "redo") return invalid(`${intent.type} is done by the history`);
  if (intent.type === "setScale") return scaleOutcome(intent.scale);
  if (intent.type === "setLocked") return { event: { type: "lockedSet", locked: intent.locked } };
  if (intent.type === "setBpm") return bpmOutcome(intent.bpm);
  if (intent.type === "setRecording" || intent.type === "clearPattern" || intent.type === "setPatternField") return patternOutcome(snapshot, intent);
  if (intent.type === "setNodeUi") return placeOutcome(snapshot, intent.node, intent.ui);
  if (intent.type === "setPinned") return pinnedOutcome(snapshot, intent.node, intent.params);
  if (intent.type === "renameGroup") return renameGroupOutcome(snapshot, intent.node, intent.name);
  if (intent.type === "setLaneGroup") return setLaneGroupOutcome(snapshot, intent.node, intent.group);
  if (intent.type === "setRoute") return setRouteOutcome(snapshot, intent.node, intent.route);
  if (intent.type === "setModulatorEar") return setModulatorEarOutcome(snapshot, intent.node, intent.ear);
  if (intent.type === "setFeed") return setFeedOutcome(snapshot, intent.node, intent.feed);
  if (intent.type === "addGroup") return addGroupOutcome(snapshot, intent.members, counters);
  if (intent.type === "addEffect") return addEffectOutcome(snapshot, intent.parent, intent.plugin, counters, catalog);
  if (intent.type === "addFreeEffect") return addFreeEffectOutcome(snapshot, intent.plugin, counters, catalog, library);
  if (intent.type === "addInstrument") return addInstrumentOutcome(snapshot, intent.plugin, counters, catalog);
  if (intent.type === "replaceInstrument") return replaceInstrumentOutcome(snapshot, intent.node, intent.plugin, catalog);
  if (intent.type === "clearInstrument") return clearInstrumentOutcome(snapshot, intent.node);
  if (intent.type === "setEdgeDepth") return depthOutcome(snapshot, intent.edge, intent.depth);
  if (intent.type === "removeEdge") return removeEdgeOutcome(snapshot, intent.edge);
  if (intent.type === "addFreeModulator") return addFreeModulatorOutcome(snapshot, intent.kind, intent.hears, counters);
  if (intent.type === "modulate") return modulateOutcome(snapshot, intent.to, intent.kind, intent.depth, counters);
  if (intent.type === "removeNode") return removeNodeOutcome(snapshot, intent.node);
  if (intent.type === "collapseIntoModule") return collapseIntoModuleOutcome(snapshot, intent.nodes, intent.name, intent.ui, counters);
  if (intent.type === "ungroupModule") return ungroupModuleOutcome(snapshot, intent.node);
  if (intent.type === "moveEffect") return moveEffectOutcome(snapshot, intent.node, intent.index);
  if (intent.type === "addModEdge") return addModEdgeOutcome(snapshot, intent.modulator, intent.to, counters);
  if (intent.type === "addSource") return addSourceOutcome(snapshot, intent.source, counters);
  // (A cable that has a port of events at either end is a cable of notes, and is judged as one: from a source to a lane, with no level.)
  if (intent.type === "addCable" && (intent.from.port.type === "eventsOut" || intent.to.port.type === "eventsIn")) return addNotesCableOutcome(snapshot, intent.from, intent.to, intent.gain, counters);
  if (intent.type === "addCable") return addCableOutcome(snapshot, intent.from, intent.to, intent.gain, counters);
  if (intent.type === "setCableGain") return cableGainOutcome(snapshot, intent.edge, intent.gain);
  // The limiter on the output mix, and the master's own gains: the master is the one node that the controller's `set_gain` looks for before it looks in the model.
  if (intent.type === "setLimiter") return { event: { type: "limiterSet", on: intent.on } };
  if (intent.type === "setGain" && intent.node === MASTER) return masterGainOutcome(intent.gain, intent.value);
  // (The master is a node of the graph and not of the model: the controller does not find it.)
  const node = snapshot.graph.nodes.find((n) => n.id === intent.node && n.kind !== "master");
  if (!node) return noNode(intent.node);
  switch (intent.type) {
    case "setParam": {
      // (In the controller's order: a node with no plugin, a lane with none, a plugin without the parameter, a number it cannot hold.)
      if (node.kind !== "instrument" && node.kind !== "effect") return invalid("that node has no plugin");
      if (node.plugin === null) return invalid("that instrument has no plugin");
      const param = node.plugin.params.find((p) => p.index === intent.param);
      if (!param) return invalid(`the plugin has no parameter ${intent.param}`);
      if (!holds(intent.value)) return invalid(`${shown(intent.value)} is not a value for ${param.name}`);
      const clamped = clamp(intent.value, param.min, param.max);
      const value = param.labels === null ? clamped : roundIn32Bits(clamped);
      return { event: { type: "paramSet", node: node.id, param: param.index, value } };
    }
    case "setGain": {
      // (The number is judged before the node it is for.)
      if (!holds(intent.value)) return invalid(`${shown(intent.value)} is not a value for ${variant(intent.gain)}`);
      const fits =
        (intent.gain === "volume" && node.kind === "instrument") ||
        (intent.gain === "mix" && node.kind === "effect") ||
        ((intent.gain === "groupVolume" || intent.gain === "groupPan") && node.kind === "group");
      if (!fits) return invalid(`${variant(intent.gain)} does not apply to that node`);
      const [low, high] = GAIN_RANGES[intent.gain];
      return { event: { type: "gainSet", node: node.id, gain: intent.gain, value: clamp(intent.value, low, high) } };
    }
    case "loadPreset":
      return presetOutcome(snapshot, node, intent.preset);
    case "setRange":
      return rangeOutcome(node, intent.range);
    // (A lane or a group: nothing else has a mute or a solo.)
    case "setMute":
      return node.kind === "instrument" || node.kind === "group"
        ? { event: { type: "muteSet", node: node.id, muted: intent.muted } }
        : invalid("only a lane or a group can be muted");
    case "setSolo":
      return node.kind === "instrument" || node.kind === "group"
        ? { event: { type: "soloSet", node: node.id, solo: intent.solo } }
        : invalid("only a lane or a group can be soloed");
    case "setModulatorField":
      return fieldOutcome(node, intent.field, intent.value);
    case "setSteps":
      return stepsOutcome(node, intent.values);
  }
}

/**
 * A waveform `turns` times round, from -1 to 1: the whole part of `turns` is the cycle it is in and the rest its phase. The random waveforms have a number for each cycle (`noise`, whose seed is
 * `seed`): `random` holds it for the cycle and `smooth` glides to the next cycle's, easing at both ends, as the engine's do (`LfoWaveform::eval_cycle`).
 */
function shape(waveform: Waveform, turns: number, seed: number): number {
  const cycle = Math.floor(turns);
  const p = turns - cycle;
  switch (waveform) {
    case "sine":
      return Math.sin(2 * Math.PI * p);
    case "triangle":
      return 4 * Math.abs(((p + 0.75) % 1) - 0.5) - 1;
    case "saw":
      return 2 * p - 1;
    case "square":
      return p < 0.5 ? 1 : -1;
    case "random":
      return noise(seed, cycle);
    case "smooth": {
      const from = noise(seed, cycle);
      return from + (noise(seed, cycle + 1) - from) * (p * p * (3 - 2 * p));
    }
  }
}

/** The number of a channel in the header of its frames. */
const CHANNEL_NUMBERS: Record<TelemetryChannel, number> = { meters: 0, modulation: 1, held: 2, scope: 3 };

/** The number of the channel of a probe's frames, which has the node where the others have the layout's revision (`probe_frame`). */
const PROBE_CHANNEL_NUMBER = 4;

/** How many nodes can be probed at once, whichever clients watch them: the engine's pool of rings (`telemetry::PROBES`). */
const PROBE_RINGS = 8;

/** The bytes of a telemetry frame, as the server writes them (`telemetry_frame`); a probe's (`probe_frame`) has its node where the layout's revision is. */
function frameBytes(channel: TelemetryChannel | "probe", layoutRev: number, values: readonly number[]): ArrayBuffer {
  const buffer = new ArrayBuffer(8 + 4 * values.length);
  const view = new DataView(buffer);
  view.setUint8(0, channel === "probe" ? PROBE_CHANNEL_NUMBER : CHANNEL_NUMBERS[channel]);
  view.setUint16(2, values.length, true);
  view.setUint32(4, layoutRev, true);
  values.forEach((value, i) => {
    view.setFloat32(8 + 4 * i, value, true);
  });
  return buffer;
}

/** The frequency of a MIDI note, in Hz: A4 (69) is 440. */
const hertzOf = (note: number): number => 440 * 2 ** ((note - 69) / 12);

/** How loud the mock's sound is at the most, for one note: as loud as its meter says (0.35). */
const SCOPE_LEVEL = 0.35;

/**
 * A scope frame for the notes that are held, which is what the real engine has an ear for and the mock has not: each note a sound of its own pitch (its tone and the
 * two harmonics above it, a little like a saw), the same on both sides, silence while no note is held. `seconds` is the time of the newest frame, and each frame
 * before it is a sample's time earlier, so that a note that is held is one wave from one frame to the next.
 */
function scopeValues(notes: ReadonlySet<number>, seconds: number, sampleRate: number): number[] {
  const values = new Array<number>(2 * SCOPE_FRAME_LENGTH).fill(0);
  if (notes.size === 0) return values;
  // (A tone and the harmonics, 1 + 1/2 + 1/3 as loud at the most, shared among the notes.)
  const scale = SCOPE_LEVEL / (1.83 * notes.size);
  for (let frame = 0; frame < SCOPE_FRAME_LENGTH; frame += 1) {
    const time = seconds - (SCOPE_FRAME_LENGTH - 1 - frame) / sampleRate;
    let sample = 0;
    for (const note of notes) {
      const turn = 2 * Math.PI * hertzOf(note) * time;
      sample += Math.sin(turn) + 0.5 * Math.sin(2 * turn) + 0.33 * Math.sin(3 * turn);
    }
    values[frame] = sample * scale;
    values[SCOPE_FRAME_LENGTH + frame] = sample * scale;
  }
  return values;
}

/** How fast a meter's peak falls, as the engine's does: 40 dB a second. */
const PEAK_FALL_DB_PER_SECOND = 40;

/** What the published reduction of the limiter is held to 0 under, in decibels (`REDUCTION_FLOOR` of the engine). */
const REDUCTION_FLOOR = 0.05;

/** What an intent told the clients: the events it made, stamped, and whether it changed the structure (they are given the snapshot then). */
export interface Told {
  events: StampedEvent[];
  restructured: boolean;
}

/**
 * A recording that is going on, as the engine's pattern player of a lane has it: when it began (in seconds of the server's clock), the tempo it was counted in (the engine
 * fixes the length of a beat when it begins), and how many notes have been played since.
 */
interface Recording {
  startedAt: number;
  bpm: number;
  notes: number;
}

/** How often the server looks at the recordings that are going on, in milliseconds. */
const RECORDING_WATCH = 50;

export class MockServer {
  /** Everything the clients have sent, in order. */
  readonly received: ClientMessage[] = [];
  /** A factory of transports to this server, for a client. */
  readonly transport: TransportFactory = () => new MockConnection(this);

  readonly #options: MockOptions;
  #snapshot: Snapshot;
  #catalog: { entries: readonly CatalogEntry[]; scanning: boolean };
  /** How many times the catalog has been told (it is the `rev` of the message). */
  #catalogRev = 0;
  /** The MIDI inputs that are open, as the clients are told (and were told last: a list that is the same as that is not told again). */
  #inputs: readonly string[];
  readonly #counters: Counters;
  /** The files of the directory of the session's file: what the options say, and the file the session has (it is there, or it is written when it is saved). */
  readonly #files: Set<string>;
  /** The library of modules, by name: the chains of effects that were saved (the options say which there are to begin with). */
  readonly #library: Map<string, Module>;
  readonly #connections = new Set<MockConnection>();
  /** What can be undone and redone, as the controller keeps it (`src/app/history.rs`). */
  readonly #history = new History();
  /** What the clients were last told Undo and Redo would do. */
  #historyTold: HistoryState = { undo: null, redo: null };
  /** The recordings that are going on, by the node of their lane: what the engine's pattern players hold, which the controller hears of when they are done. */
  readonly #recordings = new Map<NodeId, Recording>();
  #recordingTimer: ReturnType<typeof setInterval> | undefined;
  /** The last note that each control that follows the notes has heard, by the node of the control (what the engine's controls hold): what it puts out is made of it. */
  readonly #heard = new Map<NodeId, { note: number; velocity: number }>();
  /** When each step sequencer that a note starts again was started last, in seconds of the server's clock (one that none has started goes from 0). */
  readonly #restarted = new Map<NodeId, number>();

  constructor(options: MockOptions = {}) {
    this.#options = options;
    this.#snapshot = options.snapshot ?? tracerSnapshot;
    this.#catalog = options.catalog ?? { entries: builtinCatalog, scanning: false };
    this.#inputs = options.inputs ?? [];
    this.#counters = new Counters(this.#snapshot);
    this.#files = new Set([...(options.files ?? []), ...(this.#snapshot.meta.file === null ? [] : [this.#snapshot.meta.file])]);
    this.#library = new Map(Object.entries(options.modules ?? {}));
  }

  /** The session as the server has it now. */
  get snapshot(): Snapshot {
    return this.#snapshot;
  }

  /** How many connections are up. */
  get connections(): number {
    return this.#connections.size;
  }

  /** The notes held by every connection together, by note number. */
  get heldNotes(): ReadonlySet<number> {
    return new Set([...this.#connections].flatMap((c) => [...c.notes]));
  }

  /** What Undo and Redo would do now. */
  get history(): HistoryState {
    return this.#history.state();
  }

  /** Another front-end's intent (the TUI's, or another browser's): applied, and every subscribed client is told. What was told. */
  apply(intent: Intent): Told {
    const done = this.#carry(intent, true);
    if ("error" in done) throw new Error(done.error.message);
    // (The real server tells what can be undone at its next refresh, when a front-end applies an intent on its own.)
    this.#tellHistory();
    return done.told;
  }

  /** Change the session without telling anybody: the clients will see a revision missing at the next change. */
  applyQuietly(intent: Intent): void {
    const done = this.#carry(intent, false);
    if ("error" in done) throw new Error(done.error.message);
  }

  /** The server's idea of what the telemetry slots mean changes (a node came or went): the subscribed clients are told. */
  announceLayout(layout: TelemetryLayout): void {
    this.#snapshot = { ...this.#snapshot, telemetry: layout };
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell({ type: "layout", layout });
    }
  }

  /** The scan finds more plugins, or ends: the subscribed clients are told the catalog as it is now. */
  announceCatalog(entries: readonly CatalogEntry[], scanning: boolean): void {
    this.#catalog = { entries, scanning };
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell(this.#catalogMessage());
    }
  }

  /** A MIDI device is plugged in or pulled out: the subscribed clients are told the inputs that are open now, if that is not what they were told. */
  announceInputs(names: readonly string[]): void {
    if (names.length === this.#inputs.length && names.every((name, i) => name === this.#inputs[i])) return;
    this.#inputs = names;
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell(this.#inputsMessage());
    }
  }

  #inputsMessage(): ServerMessage {
    return { type: "inputs", names: [...this.#inputs] };
  }

  #catalogMessage(): ServerMessage {
    return { type: "catalog", rev: ++this.#catalogRev, scanning: this.#catalog.scanning, entries: this.#entries() };
  }

  /** What the catalog lists: the plugins that the scan found and the modules of the library, by name (`Catalog::entries`). */
  #entries(): CatalogEntry[] {
    return withModules(this.#catalog.entries, this.#library);
  }

  /**
   * Keep the module that `intent` saves (the chain of `intent.node`, or the plugins in `intent.nodes`), under the name that the intent gives it, as `save_module_intent` and `save_selection_intent` write its file: a module of that
   * name is written over, and the clients are told the catalog again if what it lists is not what it listed (a module saved again with as many plugins is the same entry).
   */
  #keepModule(snapshot: Snapshot, intent: Extract<Intent, { type: "saveModule" | "saveSelectionAsModule" }>, tell: boolean): void {
    const cleaned = cleanModuleName(intent.name);
    if ("error" in cleaned) return;
    let module: Module | string;
    if (intent.type === "saveModule") {
      const node = snapshot.graph.nodes.find((n) => n.id === intent.node);
      if (!node) return;
      module = moduleOf(snapshot, node);
    } else {
      // (The nodes are all there: an intent that names one that is not was refused before this.)
      const chosen = intent.nodes.flatMap((id) => snapshot.graph.nodes.filter((n) => n.id === id));
      module = moduleOfNodes(snapshot, chosen);
    }
    if (typeof module === "string") return;
    const before = this.#library.get(cleaned.name);
    this.#library.set(cleaned.name, module);
    if (!tell || (before !== undefined && pluginCount(before) === pluginCount(module))) return;
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell(this.#catalogMessage());
    }
  }

  #historyMessage(): ServerMessage {
    const { undo, redo } = this.#history.state();
    return { type: "history", undo, redo };
  }

  /** Tell the subscribed clients what Undo and Redo would do, if it is not what they were told. */
  #tellHistory(): void {
    const now = this.#history.state();
    if (now.undo === this.#historyTold.undo && now.redo === this.#historyTold.redo) return;
    this.#historyTold = now;
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell(this.#historyMessage());
    }
  }

  /** The network goes: every connection ends without a goodbye. */
  drop(): void {
    for (const connection of [...this.#connections]) connection.lose();
  }

  /** What this server says in `hello`. */
  hello(): ServerMessage {
    return {
      type: "hello",
      protocol: this.#options.protocol ?? PROTOCOL,
      rev: this.#snapshot.rev,
      features: ["notes", "telemetry"],
      needsToken: this.#options.token !== undefined,
    };
  }

  /** The token the server wants, if it wants one. */
  get token(): string | undefined {
    return this.#options.token;
  }

  get telemetryPeriod(): number {
    return this.#options.telemetryPeriod ?? 33;
  }

  /** The time the made-up telemetry follows, in seconds. */
  now(): number {
    return this.#options.clock ? this.#options.clock() : performance.now() / 1000;
  }

  add(connection: MockConnection): void {
    this.#connections.add(connection);
  }

  remove(connection: MockConnection): void {
    this.#connections.delete(connection);
  }

  /**
   * The nodes that the engine listens to, as the controller has them: those that the connections ask for (in the order of the connections, and of each one's asking) that have a signal
   * (an instrument that has a plugin, an effect, a group: not a modulator, not the master, not a node that is not there), the first of them that the engine has rings for.
   */
  rings(): NodeId[] {
    const nodes = this.#snapshot.graph.nodes;
    const audible = (id: NodeId): boolean => {
      const node = nodes.find((n) => n.id === id);
      return node !== undefined && ((node.kind === "instrument" && node.plugin !== null) || node.kind === "effect" || node.kind === "group");
    };
    const wanted: NodeId[] = [];
    for (const connection of this.#connections) {
      for (const node of connection.probes) if (!wanted.includes(node) && audible(node)) wanted.push(node);
    }
    return wanted.slice(0, PROBE_RINGS);
  }

  /** What a probe's frame of `node` is as of `seconds`: the sound of the notes that are held, as the scope's is (the mock has no ear for what each node makes of it). */
  probeFrame(node: NodeId, seconds: number): ArrayBuffer {
    return frameBytes("probe", node, scopeValues(this.heldNotes, seconds, this.#snapshot.meta.sampleRate));
  }

  /** A message from a connection: what the real server's connection and owner do with it, together. */
  receive(connection: MockConnection, text: string): void {
    const message = JSON.parse(text) as ClientMessage;
    this.received.push(message);
    // (What has run out by now is done before anything is asked or played: a client that subscribes is given the session as it is.)
    this.settle();
    if (!connection.authorized) {
      if (message.type === "auth" && message.token === this.token) connection.authorized = true;
      else connection.refuse("unauthorized", message.type === "auth" ? "the token is wrong" : "send the token first");
      return;
    }
    switch (message.type) {
      case "auth":
        break;
      case "subscribe":
        connection.subscribed = true;
        connection.tell({ type: "snapshot", snapshot: this.#snapshot });
        connection.tell(this.#catalogMessage());
        connection.tell(this.#historyMessage());
        connection.tell(this.#inputsMessage());
        break;
      case "telemetry":
        connection.wantTelemetry(message.channels);
        break;
      case "probes": {
        connection.wantProbes(message.nodes);
        // (The answer is which of them have a ring: a node that has none has its frames when another client lets go of one, without asking again.)
        const rings = this.rings();
        connection.tell({ type: "probes", nodes: connection.probes.filter((node) => rings.includes(node)) });
        break;
      }
      case "request": {
        const done = this.#carry(message.intent, true);
        if ("error" in done) {
          connection.tell({ type: "rejected", reqId: message.reqId, error: done.error });
        } else {
          // (The events are told before the answer, and so is the snapshot that tells a change of structure, and what can be undone now.)
          this.#tellHistory();
          connection.tell({ type: "ack", reqId: message.reqId, rev: done.rev });
        }
        break;
      }
      case "noteOn":
        connection.notes.add(message.note);
        this.#played(message.note);
        this.#listened(message.note, message.velocity);
        break;
      case "noteOff":
        connection.notes.delete(message.note);
        break;
      // (The server's own notes, every connection's: what the real server lets go of is what its held-notes bits say, whoever set them.)
      case "panic":
        for (const each of this.#connections) each.notes.clear();
        break;
    }
  }

  /**
   * Do an intent as the controller does, what it keeps in the history included: the revision it made and what it told, or why it could not
   * be done. Undo and Redo apply what the last step says, each intent of it a revision of its own, and are no steps themselves.
   */
  #carry(requested: Intent, tell: boolean): { rev: number; told: Told } | { error: WireError } {
    // (What has run out by now is done first: an intent is about the session as it is.)
    this.settle();
    if (requested.type === "undo" || requested.type === "redo") return this.#backAndForth(requested.type, tell);
    // (An audio cable between the two ends of a link of the audio path that a lane or a group makes by itself is that link, given back if it is cut: `Controller::link_or_cable`.)
    const asked = linkOrCable(this.#snapshot.graph, requested);
    if ("error" in asked) return asked;
    const intent = asked;
    const plan = planOf(this.#snapshot, intent, this.now(), this.#entries());
    const before = this.#snapshot;
    this.#counters.begin();
    const outcome = outcomeOf(this.#snapshot, intent, this.#counters, this.#entries(), this.#files, this.#library);
    const made = this.#counters.take();
    if ("error" in outcome) return outcome;
    const done = this.#commit(outcome, tell);
    // (A module that is saved is a file of the library, which the catalog lists from then on.)
    if (intent.type === "saveModule" || intent.type === "saveSelectionAsModule") this.#keepModule(before, intent, tell);
    // (A save as writes a file, which a save as after it does not write over.)
    if ("event" in outcome && outcome.event.type === "saved" && outcome.event.file !== null) this.#files.add(outcome.event.file);
    this.#keep(plan, made, before);
    this.#followRecordings(intent, tell);
    return done;
  }

  // -- Recordings made up --

  /**
   * Do what the engine does with a recording that an intent started or ended (the controller's part of it is in `outcomeOf`). A recording that is started is counted in from now; one that is
   * stopped in the count-in has recorded nothing, and the pattern is as it was; one that is stopped after it is done with what has been played. A start while it is going on, which the
   * controller tells as it is, begins nothing again. (A pattern that is cleared ends the recording that is going on with it: `settle` lets go of a recording that has no pattern.)
   */
  #followRecordings(intent: Intent, tell: boolean): void {
    if (intent.type !== "setRecording") return;
    const going = this.#recordings.get(intent.node);
    if (intent.recording) {
      if (going !== undefined) return;
      this.#recordings.set(intent.node, { startedAt: this.now(), bpm: this.#snapshot.meta.bpm, notes: 0 });
      this.#watchRecordings();
      return;
    }
    if (going === undefined) return;
    this.#recordings.delete(intent.node);
    if (this.now() - going.startedAt < (PATTERN_COUNT_IN_BEATS * 60) / going.bpm) return;
    this.#finished(intent.node, going);
    this.#resync(tell);
  }

  /**
   * Do what the engine would have done by now: the recordings that have run their length (the count-in and the beats of the pattern, at the tempo they were counted in) are done, and the
   * clients are given the session with the patterns they made. The server does it by a timer while a recording goes on, and before it answers anything.
   */
  settle(): void {
    const now = this.now();
    let changed = false;
    for (const [node, recording] of [...this.#recordings]) {
      const lane = this.#snapshot.graph.nodes.find((n) => n.id === node);
      // (A lane that is taken out, or whose pattern is, is let go of: there is nothing to make the pattern of.)
      if (lane?.kind !== "instrument" || lane.pattern === null) {
        this.#recordings.delete(node);
        continue;
      }
      if (now < recording.startedAt + ((PATTERN_COUNT_IN_BEATS + lane.pattern.lengthBeats) * 60) / recording.bpm) continue;
      this.#recordings.delete(node);
      this.#finished(node, recording);
      changed = true;
    }
    if (changed) this.#resync(true);
    if (this.#recordings.size === 0 && this.#recordingTimer !== undefined) {
      clearInterval(this.#recordingTimer);
      this.#recordingTimer = undefined;
    }
  }

  /** Look at the recordings every `RECORDING_WATCH` milliseconds while there are any. */
  #watchRecordings(): void {
    this.#recordingTimer ??= setInterval(() => {
      this.settle();
    }, RECORDING_WATCH);
  }

  /**
   * A note is played: each control that follows the notes (a velocity, a key) that hears it takes it, as the engine's do. A lane's hears the notes the lane takes (those in its keys), as its
   * transpose has made them, and not one that the transpose takes off the keyboard; a group's hears the notes that any of its members takes, as they are played; one that is in no lane and no group hears
   * every note, whatever the lanes' keys are (`Events::all`). A note-on with no velocity is a note-off.
   * (The mock has no remap and no patterns, which the engine's controls hear the notes after.)
   */
  #listened(note: number, velocity: number): void {
    if (!(velocity > 0)) return;
    const { nodes } = this.#snapshot.graph;
    const takes = (lane: Node): boolean => lane.kind === "instrument" && (lane.range === null || (note >= lane.range.low && note <= lane.range.high));
    for (const control of nodes) {
      if (control.kind !== "modulator") continue;
      // (A step sequencer that a note starts again is started by the notes that its lane takes, or that any member of its group does, as an envelope is.)
      const follows = control.source.type === "midiLearn" && (control.source.control?.type === "velocity" || control.source.control?.type === "key");
      // (An LFO hears every note too: its wave starts again at one if it retriggers, and its fade, if it has one, starts at one whether it does or not.)
      const restarts = (control.source.type === "steps" && control.source.retrigger) || control.source.type === "lfo";
      if (!follows && !restarts) continue;
      // (A modulator hears the lane or the group it has an ear for, and every note without one: the graph says none for an ear that is gone.)
      const owner = nodes.find((n) => n.id === control.hears);
      const remember = (heard: number): void => {
        if (follows) this.#heard.set(control.id, { note: heard, velocity });
        else this.#restarted.set(control.id, this.now());
      };
      if (owner?.kind === "instrument") {
        const heard = note + owner.transpose;
        if (takes(owner) && heard >= 0 && heard <= 127) remember(heard);
      } else if (owner?.kind === "group") {
        if (nodes.some((n) => n.kind === "instrument" && n.parent === owner.id && takes(n))) remember(note);
      } else {
        remember(note);
      }
    }
  }

  /** A note is played: every recording that is going on takes it, if it is in the keys of its lane (a note of the count-in is recorded too, at the start of the pattern). */
  #played(note: number): void {
    for (const [node, recording] of this.#recordings) {
      const lane = this.#snapshot.graph.nodes.find((n) => n.id === node);
      const range = lane?.kind === "instrument" ? lane.range : null;
      if (range === null || (note >= range.low && note <= range.high)) recording.notes += 1;
    }
  }

  /**
   * A recording is done: the lane has the pattern it made, as the controller has it when it has folded in the engine's report (`finish_recording`): the notes it has, heard if it has any, at the
   * tempo the session has now, and the settings and the length the lane's pattern has. The session is changed and the clients are not told yet (`resync`).
   */
  #finished(node: NodeId, recording: Recording): void {
    const lane = this.#snapshot.graph.nodes.find((n) => n.id === node);
    // (A recording is let go of when its pattern is taken out or its lane is: there is nothing to make it of otherwise.)
    if (lane?.kind !== "instrument" || lane.pattern === null) return;
    const pattern: PatternInfo = { ...lane.pattern, recording: false, enabled: recording.notes > 0, notes: recording.notes, bpm: this.#snapshot.meta.bpm };
    const nodes = this.#snapshot.graph.nodes.map((n) => (n === lane ? { ...lane, pattern } : n));
    this.#snapshot = { ...this.#snapshot, graph: { ...this.#snapshot.graph, nodes } };
  }

  /** Give the clients the session as it is, at a revision of its own (what the controller's `resync` does for what the engine reports): it is a change that is not saved. */
  #resync(tell: boolean): void {
    this.#snapshot = { ...this.#snapshot, rev: this.#snapshot.rev + 1, meta: { ...this.#snapshot.meta, dirty: true } };
    if (!tell) return;
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell({ type: "snapshot", snapshot: this.#snapshot });
    }
  }

  /** The intents of the last step that can be undone, or of the last that was undone, carried out one after the other. */
  #backAndForth(which: "undo" | "redo", tell: boolean): { rev: number; told: Told } | { error: WireError } {
    const step = which === "undo" ? this.#history.takeUndo() : this.#history.takeRedo();
    if (!step) return { error: { code: "invalid", message: `there is nothing to ${which}` } };
    let rev = this.#snapshot.rev;
    const told: Told = { events: [], restructured: false };
    // What was taken out is put back as it was: the nodes and the cables of the session, and the layout of its telemetry, are what they were before.
    // (Everything done after was undone first, so that nothing else has changed them.)
    if (which === "undo" && step.taken) {
      const { graph, telemetry } = step.taken;
      const done = this.#commit({ restructured: { ...this.#snapshot, graph, telemetry } }, tell);
      this.#history.keptUndone(step);
      return done;
    }
    // What they make is made under the ids it was made under the first time (what is not made of them is not kept for what is made next).
    this.#counters.reuse(which === "undo" ? step.undoMade : step.redoMade);
    const was = this.#snapshot;
    try {
      // (A step whose intent cannot be done, a node that is not there any more, is dropped, and what was done of it stays.)
      for (const intent of which === "undo" ? step.undo : step.redo) {
        const outcome = outcomeOf(this.#snapshot, intent, this.#counters, this.#entries(), this.#files, this.#library);
        if ("error" in outcome) return outcome;
        const done = this.#commit(outcome, tell);
        rev = done.rev;
        told.events.push(...done.told.events);
        told.restructured ||= done.told.restructured;
      }
    } finally {
      this.#counters.dropReused();
    }
    // What a removal that is done again takes is what the next undo puts back (the step's way back is replaced each time, as the controller's is): the session as it was just before,
    // which is not what it was the first time when what was added has been added again, with slots of the telemetry of its own.
    if (which === "redo" && step.taken) step.taken = { graph: was.graph, telemetry: was.telemetry };
    if (which === "undo") this.#history.keptUndone(step);
    else this.#history.keptRedone(step);
    return { rev, told };
  }

  /** An intent has been carried out, and made `made`: its step is kept, or the history is forgotten. */
  #keep(plan: Plan, made: Made, before: Snapshot): void {
    if (plan === "barrier") {
      this.#history.clear();
    } else if (plan !== "nothing") {
      if ("step" in plan) {
        this.#history.record(plan.step);
        return;
      }
      if ("removed" in plan) {
        const { label, intent, at } = plan.removed;
        this.#history.record(removedStep(label, intent, { graph: before.graph, telemetry: before.telemetry }, at));
        return;
      }
      const { label, intent, at } = plan.added;
      const undo = takesOut(this.#snapshot, intent, made);
      // (It made something that cannot be named, or what it made is not there: nothing puts it back.)
      if (undo) this.#history.record(addedStep(label, undo, intent, made, at));
      else this.#history.clear();
    }
  }

  /**
   * The next revision: apply what the intent did to the model and, if `tell`, tell the subscribed clients. The
   * revision of the intent. (A change of structure is that, and the snapshot that tells it is the revision after,
   * as `Controller::apply` has it.)
   */
  #commit(outcome: Exclude<Outcome, { error: WireError }>, tell: boolean): { rev: number; told: Told } {
    const rev = this.#snapshot.rev + 1;
    if ("restructured" in outcome) {
      this.#snapshot = withoutLostEars({ ...outcome.restructured, rev: rev + 1, meta: { ...outcome.restructured.meta, dirty: true } });
      if (tell) {
        for (const connection of this.#connections) {
          if (connection.subscribed) connection.tell({ type: "snapshot", snapshot: this.#snapshot });
        }
      }
      return { rev, told: { events: [], restructured: true } };
    }
    const stamped: StampedEvent = { ...outcome.event, rev };
    const applied = applyEvent(this.#snapshot, stamped);
    if (!applied.ok) throw new Error(applied.reason);
    this.#snapshot = applied.snapshot;
    if (tell) {
      for (const connection of this.#connections) {
        if (connection.subscribed) connection.tell({ type: "event", event: stamped });
      }
    }
    return { rev, told: { events: [stamped], restructured: false } };
  }

  // -- Telemetry made up --

  /** What a modulator puts out at `seconds`, from -1 to 1 for an LFO and from 0 to 1 for the rest. */
  #modulatorOutput(node: Node, seconds: number): number {
    if (node.kind !== "modulator") return 0;
    const { source } = node;
    switch (source.type) {
      case "lfo": {
        // (The seed of the numbers is the node's id: the engine's is the order in which modulators came into it, which the page does not know. The wave goes from the last note that started
        // it again, if it retriggers, or from the start of the clock; what `#listened` has noted is of the notes the LFO heard, and the fade is the part of it that has gone by since the last.)
        const noted = this.#restarted.get(node.id);
        // (It goes at the tempo's rate if it follows the tempo, as the engine's does: its own is kept for when it is free. The mock's wave is the clock's times the rate, so that a rate that
        // changes, or a tempo, moves it to where that rate has it from the start, where the engine's goes on from where it was.)
        const wave = shape(source.waveform, rateAt(source.rate, source.sync, this.#snapshot.meta.bpm) * Math.max(seconds - (source.retrigger ? noted ?? 0 : 0), 0), node.id);
        return source.fade > 0 && noted !== undefined ? wave * Math.min(Math.max(seconds - noted, 0) / source.fade, 1) : wave;
      }
      case "envelope":
        return this.heldNotes.size > 0 ? source.sustain : 0;
      // (The mock has no MIDI: a control rests, as the engine's does with nothing played or turned: nothing, the pitch bend wheel in the middle and a key at middle C. What follows the notes goes by the last
      // that it heard.)
      case "midiLearn": {
        const heard = this.#heard.get(node.id);
        switch (source.control?.type) {
          case "pitchBend":
            return 0.5;
          case "velocity":
            return heard ? heard.velocity / 127 : 0;
          case "key":
            return heard ? heard.note / 127 : KEY_CENTRE / 127;
          default:
            return 0;
        }
      }
      // (A macro puts out the number it is set to, and nothing moves it but a person.)
      case "macro":
        return source.value;
      // (A step sequencer goes through its steps at its rate from the last note that started it again, or from the start of the clock: the engine's goes by the buffer, which this does not.
      // What `#listened` has noted is of the notes that started it, which only a step sequencer that retriggers is.)
      case "steps":
        return stepsOutput(source.values, source.length, Math.max(seconds - (this.#restarted.get(node.id) ?? 0), 0) * rateAt(source.rate, source.sync, this.#snapshot.meta.bpm), source.glide);
    }
  }

  /** Where a step sequencer is at `seconds`: the step as the whole part and how far into it as the fraction, from the last note that started it again, or the start of the clock (`#modulatorOutput`). */
  #stepsPosition(node: Node | undefined, seconds: number): number {
    if (node?.kind !== "modulator" || node.source.type !== "steps") return 0;
    const { source } = node;
    return stepsPlace(source.length, Math.max(seconds - (this.#restarted.get(node.id) ?? 0), 0) * rateAt(source.rate, source.sync, this.#snapshot.meta.bpm), source.values.length);
  }

  /** The values of a channel's list in the layout, as of `seconds`; `peaks` is the meters' state, kept from frame to frame. */
  frame(channel: TelemetryChannel, seconds: number, peaks: Map<number, number>, elapsed: number): ArrayBuffer {
    // (The held notes follow no layout: a value for each of the 128 notes, 1 for one that any connection holds.)
    if (channel === "held") {
      const held = this.heldNotes;
      return frameBytes(channel, 0, Array.from({ length: 128 }, (_, note) => (held.has(note) ? 1 : 0)));
    }
    const { telemetry } = this.#snapshot;
    const nodes = this.#snapshot.graph.nodes;
    // What can be heard: a lane that has an instrument and reaches the master by the mute and solo of the session (`audibleOf`), which is what the master's meter and its scope have
    // an ear for. The mock has no ear for anything else: a note that is held is a level in every meter and a sound in every scope, where it can be heard.
    const audible = audibleOf(this.#snapshot.graph);
    const heard = heardOf(this.#snapshot.graph);
    const fed = fedOf(this.#snapshot.graph);
    const sounding = nodes.some((n) => n.kind === "instrument" && n.plugin !== null && audible.get(n.id) === true);
    // (The scope follows none either: the frames of the left side and then of the right, as many of each as the real server's.)
    if (channel === "scope") return frameBytes(channel, 0, scopeValues(sounding ? this.heldNotes : new Set<number>(), seconds, this.#snapshot.meta.sampleRate));
    const outputOf = (id: NodeId): number => {
      const node = nodes.find((n) => n.id === id);
      return node ? this.#modulatorOutput(node, seconds) : 0;
    };
    const fall = 10 ** ((-PEAK_FALL_DB_PER_SECOND * elapsed) / 20);
    const playing = this.heldNotes.size > 0 ? 0.35 : 0;
    // What a meter has an ear for, as the engine's are: a lane's after its own mute and solo (and what a cable sends it), a group's after the bus is let go of and with something to carry (a lane of
    // it that is heard and whose way out is not cut, or a cable that is heard from where it starts), and the master's after everything.
    // What the output mix is: the mix turned by the master's volume, and held under the ceiling while the limiter is in the sound (`Master::run_node`). A meter at the master has an ear for what comes out.
    const master = nodes.find((n) => n.kind === "master");
    const beforeTheLimiter = (sounding ? playing : 0) * (master?.kind === "master" ? master.volume : 1);
    const ceiling = master?.kind === "master" && master.limiter ? 10 ** (master.ceiling / 20) : Number.POSITIVE_INFINITY;
    const levelOf = (id: NodeId): number => {
      const node = nodes.find((n) => n.id === id);
      if (node?.kind === "instrument") return heard.get(id) === true ? playing : 0;
      if (node?.kind === "group") {
        const carried = fed.has(id) || nodes.some((n) => n.kind === "instrument" && n.parent === id && n.route === "out" && heard.get(n.id) === true);
        return heard.get(id) === true && carried ? playing : 0;
      }
      return Math.min(beforeTheLimiter, ceiling);
    };
    const value = (entry: LayoutEntry): number => {
      switch (entry.kind) {
        case "peak": {
          const peak = Math.max(levelOf(entry.node), (peaks.get(entry.slot) ?? 0) * fall);
          peaks.set(entry.slot, peak);
          return peak;
        }
        case "clips":
          return 0;
        // How far the limiter brings the mix down, in decibels, falling as the engine's does (40 a second); nothing while it is out of the sound.
        case "reduction": {
          const now = beforeTheLimiter > ceiling ? 20 * Math.log10(beforeTheLimiter / ceiling) : 0;
          const reduction = Math.max(now, (peaks.get(entry.slot) ?? 0) - PEAK_FALL_DB_PER_SECOND * elapsed);
          peaks.set(entry.slot, reduction);
          return reduction < REDUCTION_FLOOR ? 0 : reduction;
        }
        case "modOutput":
          return outputOf(entry.node);
        case "modPosition":
          return this.#stepsPosition(nodes.find((n) => n.id === entry.node), seconds);
        case "effective":
          return this.#effective(entry, outputOf);
      }
    };
    return frameBytes(channel, telemetry.rev, telemetry[channel].map(value));
  }

  /**
   * The value a setting of a modulator, or the depth of one of its cables, is set to: what the user set, and what the last cable aimed at it makes of it (the
   * engine's writes to a setting are last-wins, `apply_cross_mod`), by the law of the modulator that drives it (a rise to the user's value for an envelope, an
   * offset around it for the others) and clamped to the range of the setting. A setting that is turned on a log scale (a rate, an attack, a decay, a release) is
   * moved a fraction of the knob's travel, in ratios: the same law over the places on the knob, 0 to 1 (`Modulator::modulated_value`).
   */
  #settingEffective(entry: LayoutEntry, outputOf: (id: NodeId) => number): number {
    const { nodes, edges } = this.#snapshot.graph;
    const node = nodes.find((n) => n.id === entry.node);
    if (node?.kind !== "modulator") return 0;
    const port = entry.port;
    let base: number;
    let range: readonly [number, number];
    if (port.type === "depth") {
      const cable = edges.find((e) => isModulation(e) && e.id === port.edge);
      if (!cable || !isModulation(cable)) return 0;
      [base, range] = [cable.amount, DEPTH_RANGE];
    } else if (port.type === "rate" && (node.source.type === "lfo" || node.source.type === "steps")) {
      [base, range] = [node.source.rate, MODULATOR_FIELD_RANGES.rate];
    } else if ((port.type === "attack" || port.type === "decay" || port.type === "sustain" || port.type === "release") && node.source.type === "envelope") {
      [base, range] = [node.source[port.type], MODULATOR_FIELD_RANGES[port.type]];
    } else {
      return 0;
    }
    const [min, max] = range;
    const scale = port.type === "rate" || port.type === "attack" || port.type === "decay" || port.type === "release" ? MODULATOR_FIELD_SCALES[port.type] : "linear";
    // (On the knob the user's value is at a place, and the law moves it from there between 0 and 1.)
    const onKnob = isLog(scale, min) && max > min;
    const [from, low, high] = onKnob ? [fractionOf(base, min, max, "log"), 0, 1] : [base, min, max];
    let value = from;
    for (const edge of edges) {
      if (!isModulation(edge) || edge.to.node !== entry.node || !samePort(edge.to.port, port)) continue;
      const source = nodes.find((n) => n.id === edge.from.node);
      value = from + offsetOf(source, outputOf(edge.from.node), edge.amount, from, low, high);
    }
    return onKnob ? valueAt(value, min, max, "log") : clamp(value, min, max);
  }

  /**
   * The volume, the mix or the pan that the host keeps for a node, as the cables aimed at it make it: what the user set, and the offsets of the cables added up, each from the value the user set, and
   * held to the value's range, as `apply_gain_drives` does it.
   */
  #gainEffective(entry: LayoutEntry, port: Extract<Port, { type: "volume" | "mix" | "pan" }>, outputOf: (id: NodeId) => number): number {
    const { nodes, edges } = this.#snapshot.graph;
    const node = nodes.find((n) => n.id === entry.node);
    const found: { base: number; range: readonly [number, number] } | undefined =
      node?.kind === "instrument" && port.type === "volume"
        ? { base: node.volume, range: GAIN_RANGES.volume }
        : node?.kind === "effect" && port.type === "mix"
          ? { base: node.mix, range: GAIN_RANGES.mix }
          : node?.kind === "group" && port.type === "volume"
            ? { base: node.volume, range: GAIN_RANGES.groupVolume }
            : node?.kind === "group" && port.type === "pan"
              ? { base: node.pan, range: GAIN_RANGES.groupPan }
              : undefined;
    if (!found) return 0;
    const { base, range } = found;
    let offset = 0;
    for (const edge of edges) {
      if (!isModulation(edge) || edge.to.node !== entry.node || !samePort(edge.to.port, port)) continue;
      offset += offsetOf(nodes.find((n) => n.id === edge.from.node), outputOf(edge.from.node), edge.amount, base, range[0], range[1]);
    }
    return clamp(base + offset, range[0], range[1]);
  }

  /**
   * The value a parameter is set to: what the user set, and the offsets that the cables aimed at it make of it, each from the value the user set (an LFO or a control swings it around
   * that value, an envelope's peak is that value) and added, as the engine does (`apply_free_modulators`), and clamped.
   */
  #effective(entry: LayoutEntry, outputOf: (id: NodeId) => number): number {
    const { nodes, edges } = this.#snapshot.graph;
    const node = nodes.find((n) => n.id === entry.node);
    if (isGainPort(entry.port)) return this.#gainEffective(entry, entry.port, outputOf);
    if (entry.port.type !== "param") return this.#settingEffective(entry, outputOf);
    const index = entry.port.index;
    const param = node ? pluginOf(node)?.params.find((p) => p.index === index) : undefined;
    if (!param) return 0;
    const drivers = edges
      .filter(isModulation)
      .filter((edge) => edge.to.node === entry.node && edge.to.port.type === "param" && edge.to.port.index === index)
      .map((edge) => ({ edge, source: nodes.find((n) => n.id === edge.from.node) }));
    let offset = 0;
    for (const { edge, source } of drivers) {
      offset += offsetOf(source, outputOf(edge.from.node), edge.amount, param.value, param.min, param.max);
    }
    return clamp(param.value + offset, param.min, param.max);
  }
}

/** One client's connection to a `MockServer`; what the client holds is its transport. */
export class MockConnection implements Transport {
  authorized: boolean;
  subscribed = false;
  readonly notes = new Set<number>();
  /** The nodes whose signal the client wants, each once, in the order it asked. */
  probes: NodeId[] = [];

  #events: TransportEvents | null = null;
  #timer: ReturnType<typeof setInterval> | undefined;
  #channels = new Set<TelemetryChannel>();
  readonly #peaks = new Map<number, number>();
  #last = 0;

  constructor(private readonly server: MockServer) {
    this.authorized = server.token === undefined;
  }

  connect(events: TransportEvents): void {
    this.#events = events;
    this.server.add(this);
    queueMicrotask(() => {
      if (this.#events !== events) return;
      events.open();
      this.tell(this.server.hello());
    });
  }

  /** The client sends a message (its text): the server gets it after a moment, in order. */
  send(text: string): void {
    queueMicrotask(() => {
      if (this.#events) this.server.receive(this, text);
    });
  }

  /** The server sends a message to the client, in order with the others. */
  tell(message: ServerMessage): void {
    const events = this.#events;
    const text = JSON.stringify(message);
    queueMicrotask(() => {
      events?.text(text);
    });
  }

  /** The client closes: nothing more is said either way, and the notes it held end. */
  close(): void {
    this.#end();
  }

  /** The connection is lost: the client is told it closed, abnormally. */
  lose(): void {
    const events = this.#events;
    this.#end();
    if (events) queueMicrotask(() => { events.close({ code: 1006, reason: "" }); });
  }

  /** The server closes the connection for breaking a rule: it says why, and closes. */
  refuse(code: WireError["code"], message: string): void {
    const events = this.#events;
    this.tell({ type: "closing", error: { code, message } });
    this.#end();
    if (events) queueMicrotask(() => { events.close({ code: 1008, reason: "closing" }); });
  }

  wantTelemetry(channels: readonly TelemetryChannel[]): void {
    this.#channels = new Set(channels);
    this.#schedule();
  }

  /** The nodes whose signal the client wants now (what it wanted before it does not). */
  wantProbes(nodes: readonly NodeId[]): void {
    this.probes = [...new Set(nodes)];
    this.#schedule();
  }

  /** Send the frames that are wanted every period, or stop if none are. */
  #schedule(): void {
    clearInterval(this.#timer);
    this.#timer = undefined;
    if (this.#channels.size === 0 && this.probes.length === 0) return;
    this.#last = this.server.now();
    this.#timer = setInterval(() => {
      const events = this.#events;
      if (!events) return;
      const now = this.server.now();
      for (const channel of this.#channels) {
        events.binary(this.server.frame(channel, now, this.#peaks, Math.max(now - this.#last, 0)));
      }
      // (A node that has a ring: the others are watched by no one as far as the engine goes, and have no frames.)
      const rings = this.probes.length > 0 ? this.server.rings() : [];
      for (const node of this.probes) if (rings.includes(node)) events.binary(this.server.probeFrame(node, now));
      this.#last = now;
    }, this.server.telemetryPeriod);
  }

  #end(): void {
    this.#events = null;
    this.server.remove(this);
    clearInterval(this.#timer);
    this.#timer = undefined;
  }
}
