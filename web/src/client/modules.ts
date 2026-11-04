// Modules, for the page's mock engine (steps 104 to 106): what the controller does with `Intent::SaveModule`, `Intent::SaveSelectionAsModule` and with an `AddFreeEffect` of `module:<name>`
// (`src/app/module.rs`, `apply.rs`), with a library that is kept in memory in place of a directory of files. A module is lanes of plugins, the audio cables between them and the modulators that were chosen with them: each
// lane has an instrument or none and its effects in order, each plugin as a session file says one (the plugin, the preset by its name, the values that are not the plugin's own, the mix of an effect and the volume of an
// instrument, the knobs pinned to its node, and whether the stage before an effect feeds it); each modulator has what it is and how it is set, the lane of the module whose notes it hears (or every note) and its cables to the
// module's plugins and to the settings of its other modulators. `fixtures/modules.json` and `fixtures/moduleGraphs.json`, which the controller writes, pin what is made of one to the controller's.
import { NAME_LIMIT } from "../protocol";
import type { CatalogEntry, ModulatorSource, Node, NodeId, Plugin, Snapshot } from "../protocol";
import { isModulation } from "./cables";
import { sessionFileName } from "./filenames";

/** What the catalog's id of a module begins with: `module:Space` is the module called `Space`. */
export const MODULE_PREFIX = "module:";

/** The id that the catalog gives the module called `name`. */
export const moduleId = (name: string): string => `${MODULE_PREFIX}${name}`;

/** The name of the module that the catalog's `id` says, or null for the id of a plugin. */
export const moduleNameOf = (id: string): string | null => (id.startsWith(MODULE_PREFIX) ? id.slice(MODULE_PREFIX.length) : null);

/** An effect of a module, as its file says it. */
export interface ModuleEffect {
  readonly plugin: string;
  /** The name of the preset it has, if any. */
  readonly preset: string | null;
  /** The values that are not the plugin's own once its preset is loaded, by the name of the parameter. */
  readonly params: Readonly<Record<string, number>>;
  readonly mix: number;
  /** The names of the parameters pinned to its node, in order. */
  readonly pinned: readonly string[];
  /** Whether the stage before it feeds it (`feed = "chain"`). */
  readonly fed: boolean;
}

/** The instrument of a lane of a module, as its file says it: what an effect has but for the mix, and the lane's volume. */
export interface ModuleInstrument {
  readonly plugin: string;
  readonly preset: string | null;
  readonly params: Readonly<Record<string, number>>;
  /** The lane's volume (what the host keeps for an instrument). */
  readonly volume: number;
  readonly pinned: readonly string[];
}

/** A lane of a module: an instrument, if it has one, and the effects of its chain in order. */
export interface ModuleLane {
  readonly instrument: ModuleInstrument | null;
  readonly effects: readonly ModuleEffect[];
}

/** Where an audio cable of a module starts: the instrument of a lane (`effect` is null), or an effect of its chain. */
export interface ModuleOutput {
  readonly lane: number;
  readonly effect: number | null;
}

/** Where an audio cable of a module ends: an effect of a lane, or its second input. */
export interface ModuleInput {
  readonly lane: number;
  readonly effect: number;
  readonly side: boolean;
}

/** An audio cable of a module, from the output of one plugin to the input of an effect, at a level. */
export interface ModuleCable {
  readonly from: ModuleOutput;
  readonly to: ModuleInput;
  readonly gain: number;
}

/** A setting of a modulator that the cable of another can drive: an LFO's rate or a stage of an envelope (the ports of the graph that are one). */
export type ModuleSetting = "rate" | "attack" | "decay" | "sustain" | "release";

/**
 * What a cable of a module's modulator drives (`ModuleDrives`): a parameter of a plugin of the module by its name, the volume of a lane's instrument or the mix of an effect, a setting of another of the module's
 * modulators, or the depth of the cable that another of them has at a place among its cables (those that the module keeps). A plugin is named by its place in the module, a modulator by its place among the module's.
 */
export type ModuleDrives =
  | { readonly type: "param"; readonly plugin: ModuleOutput; readonly name: string }
  | { readonly type: "gain"; readonly plugin: ModuleOutput; readonly gain: "volume" | "mix" }
  | { readonly type: "setting"; readonly modulator: number; readonly port: ModuleSetting }
  | { readonly type: "depth"; readonly modulator: number; readonly target: number };

/** A cable of a module's modulator: what it drives, and how far. */
export interface ModuleTarget {
  readonly drives: ModuleDrives;
  readonly depth: number;
}

/** A modulator of a module (step 106): what it is and how it is set, the lane of the module that it hears (none is every note) and its cables, in order. */
export interface ModuleModulator {
  readonly source: ModulatorSource;
  readonly hears: number | null;
  readonly targets: readonly ModuleTarget[];
}

/** A module as the library keeps it. */
export interface Module {
  readonly lanes: readonly ModuleLane[];
  readonly cables: readonly ModuleCable[];
  readonly modulators: readonly ModuleModulator[];
}

/** How many plugins a module is made of, the instruments and the effects (`Module::plugin_count`): its modulators are not counted. */
export const pluginCount = (module: Module): number => module.lanes.reduce((count, lane) => count + (lane.instrument ? 1 : 0) + lane.effects.length, 0);

/**
 * The name a module is saved under, as the controller makes it (`module::clean_name`): a plain file name (`sessionFileName`, which `filenames.json` pins to the controller's) without the `.toml`
 * that its file has, and not one that ends in it twice, which the library would list under another name; or the words for what is wrong with it.
 */
export function cleanModuleName(given: string): { name: string } | { error: string } {
  const made = sessionFileName(given);
  if ("error" in made) return made;
  const stem = made.file.endsWith(".toml") ? made.file.slice(0, -".toml".length) : made.file;
  if (stem.endsWith(".toml")) return { error: "the name of a module does not end in .toml: that is the extension of its file" };
  return { name: stem };
}

/** What the box of a module that is collapsed is called when it is given a name of nothing (`DEFAULT_MODULE_NAME` of `session.rs`). */
export const DEFAULT_MODULE_NAME = "Module";

/**
 * A name as the controller tidies the name of a group and of a box (step 109): the spaces round it go, as Rust's `trim` takes them (the Unicode white space), and it is at most `NAME_LIMIT` characters and has no control
 * character; or the words for what is wrong with it. A name of nothing is the empty name, which a group is given its own for and a box is called `Module`.
 */
export function tidyName(given: string): { name: string } | { error: string } {
  const name = given.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
  if (Array.from(name).length > NAME_LIMIT) return { error: `a name is at most ${String(NAME_LIMIT)} characters` };
  if (/\p{Cc}/u.test(name)) return { error: "a name has no control characters" };
  return { name };
}

/** What a box is called that is given `given` for a name: the name tidied, or `Module` for one that is nothing; a name that is refused is called so too, for the words of a step that is not kept. */
export function boxTitle(given: string): string {
  const tidied = tidyName(given);
  return "name" in tidied && tidied.name !== "" ? tidied.name : DEFAULT_MODULE_NAME;
}

/** The entry that the catalog has for the module called `name` (`Catalog::entries`): no instrument, as many plugins as parameters, and no presets. */
export const moduleEntry = (name: string, module: Module): CatalogEntry => ({ id: moduleId(name), name, format: "Module", isInstrument: false, params: pluginCount(module), presets: 0 });

/**
 * The plugins and the modules of a library as the catalog lists them to a client (`Catalog::entries`): by name without regard to case, a plugin before a module that has the same name. The plugins
 * are as they came.
 */
export function withModules(plugins: readonly CatalogEntry[], library: ReadonlyMap<string, Module>): CatalogEntry[] {
  const lower = (entry: CatalogEntry): string => entry.name.toLowerCase();
  return [...plugins, ...[...library].map(([name, module]) => moduleEntry(name, module))].sort((a, b) => (lower(a) < lower(b) ? -1 : lower(a) > lower(b) ? 1 : 0));
}

/** How far a value may be from the plugin's own for the file to keep it (`f32::EPSILON`, in `overrides` of `src/app/save.rs`). */
const SAME = 1.1920929e-7;

/** The nodes of the chain of effects of the lane `node` is, or is an effect in (`module::chain_of`), or why it is none, in the controller's words: a group's bus, some other node, a lane that has no effect. */
export function chainOf(snapshot: Snapshot, node: Node): Node[] | string {
  const { nodes } = snapshot.graph;
  let lane: Node | undefined;
  if (node.kind === "instrument") lane = node;
  else if (node.kind === "effect") lane = nodes.find((n) => n.id === node.parent);
  else if (node.kind === "group") return "a module is the chain of a lane: a group's bus is not saved";
  if (lane?.kind !== "instrument") return lane?.kind === "group" ? "a module is the chain of a lane: a group's bus is not saved" : "a module is made of the chain of a lane";
  const chain = nodes.filter((n) => n.kind === "effect" && n.parent === lane.id);
  return chain.length === 0 ? "a lane that has no effect has no chain to save" : chain;
}

/** The module that the chain of the lane of `node` is (`module::to_text`): `chainOf` and then `moduleOfNodes`. The reason, in the controller's words, if there is none. */
export function moduleOf(snapshot: Snapshot, node: Node): Module | string {
  const chain = chainOf(snapshot, node);
  return typeof chain === "string" ? chain : moduleOfNodes(snapshot, chain);
}

/**
 * The module that the plugins and the modulators in `chosen` are (`module::to_text_of`, steps 105 and 106): the instrument of a lane is the lane's node, an effect is its own. Each lane that has one of them is a lane of the module,
 * with the instrument if it was chosen and the effects that were, in the order of their chain, each as it is now; and the audio cables that have both ends among them, in the order of their ids. What joins them to what is not chosen
 * is the session's: the cables that go in and out, the way out of a lane, the link from a stage that was not chosen (the effect after it is not fed by the chain: `fed` is false, but for the first of a lane that has no instrument,
 * which has nothing before it either way). The modulators that are chosen are the module's too, in the order of the session's, with the cables that they have to what is chosen (`modulatorsOfModule`). The reason, in the
 * controller's words, for a group, the bus of one, a source, a lane that has no instrument, a modulator with no plugin to go with, or nothing at all.
 */
export function moduleOfNodes(snapshot: Snapshot, chosen: readonly Node[]): Module | string {
  const { nodes, edges } = snapshot.graph;
  const picked = new Map<number, { instrument: boolean; effects: Set<number> }>();
  const modulators = new Set<NodeId>();
  const lanePicked = (lane: number): { instrument: boolean; effects: Set<number> } => {
    let found = picked.get(lane);
    if (!found) {
      found = { instrument: false, effects: new Set() };
      picked.set(lane, found);
    }
    return found;
  };
  for (const node of chosen) {
    if (node.kind === "instrument") {
      if (node.plugin === null) return "a lane that has no instrument has none to save: choose its effects";
      lanePicked(node.id).instrument = true;
    } else if (node.kind === "effect") {
      const owner = nodes.find((n) => n.id === node.parent);
      if (owner?.kind === "group") return "a module is made of effects and instruments: a group's bus is not saved";
      if (owner?.kind !== "instrument") return "a module is made of effects and instruments: that effect is in no lane";
      lanePicked(owner.id).effects.add(node.id);
    } else if (node.kind === "group") {
      return "a module is made of effects and instruments: a group's bus is not saved";
    } else if (node.kind === "modulator") {
      modulators.add(node.id);
    } else if (node.kind === "source") {
      return "a source of notes is not part of a module: a module is made of effects and instruments, and the modulators that go with them";
    } else {
      return `there is no node ${String(node.id)}`;
    }
  }
  if (chosen.length === 0) return "nothing is chosen: a module is made of the effects and instruments that are";
  if (picked.size === 0) return "a modulator alone is not a module: choose the effects or instruments that it goes with";
  // (The lanes of the module are the session's, in its order; the effects of each, in the order of its chain.)
  const lanes: ModuleLane[] = [];
  const laneAt = new Map<number, number>();
  const instrumentAt = new Map<NodeId, number>();
  const effectAt = new Map<number, { lane: number; effect: number }>();
  for (const lane of nodes) {
    if (lane.kind !== "instrument") continue;
    const here = picked.get(lane.id);
    if (!here) continue;
    const chain = nodes.filter((n): n is Extract<Node, { kind: "effect" }> => n.kind === "effect" && n.parent === lane.id);
    const effects: ModuleEffect[] = [];
    let before: number | null = null;
    chain.forEach((effect, index) => {
      if (!here.effects.has(effect.id)) return;
      const plugin = effect.plugin;
      // (The stage before it in the module's chain is the one before it in the session's only if that was chosen too; the first of a lane that has no instrument has nothing before it either way, which the cut below leaves alone.)
      const sameStage = before === null ? index === 0 : before + 1 === index;
      const cut = !sameStage && (before !== null || here.instrument);
      effectAt.set(effect.id, { lane: lanes.length, effect: effects.length });
      effects.push({
        plugin: plugin.id,
        preset: plugin.currentPreset,
        params: valuesOver(plugin),
        mix: effect.mix,
        pinned: pinnedNames(plugin),
        fed: !cut && effect.feed === "chain",
      });
      before = index;
    });
    laneAt.set(lane.id, lanes.length);
    if (here.instrument && lane.plugin) instrumentAt.set(lane.id, lanes.length);
    const plugin = lane.plugin;
    lanes.push({ instrument: here.instrument && plugin ? { plugin: plugin.id, preset: plugin.currentPreset, params: valuesOver(plugin), volume: lane.volume, pinned: pinnedNames(plugin) } : null, effects });
  }
  // (The audio cables that have both ends among what is chosen, in the order of their ids; one that starts at an instrument that was not chosen, at a group, or ends at a group's bus or the mix is the session's.)
  const cables = edges.flatMap((edge): ModuleCable[] => {
    if (edge.kind !== "cable") return [];
    const start = effectAt.get(edge.from.node);
    let from: ModuleOutput | null = start ? { lane: start.lane, effect: start.effect } : null;
    const lane = laneAt.get(edge.from.node);
    if (!from && lane !== undefined && lanes[lane]?.instrument) from = { lane, effect: null };
    const end = effectAt.get(edge.to.node);
    if (!from || !end) return [];
    return [{ from, to: { lane: end.lane, effect: end.effect, side: edge.to.port.type === "audioInSide" }, gain: edge.gain }];
  });
  return { lanes, cables, modulators: modulatorsOfModule(snapshot, modulators, laneAt, instrumentAt, effectAt) };
}

/**
 * The modulators that are chosen as a module has them (`modulators_of_text`, step 106), in the order of the session's: what each is and how it is set, the lane of the module that it hears if the lane that it heard is one
 * of the module's (else every note), and its cables, in the order that they were drawn in. A cable stays if what it drives is in the module: a plugin that is chosen (a lane's instrument, an effect), the volume of a lane's
 * instrument or the mix of an effect that is chosen, a modulator that is chosen, or a cable of such a modulator that stays; one that drives the depth of a cable goes when that cable does, and the cables are looked at until no
 * more go. What a module says of a cable is the place that its end has in the module: a plugin's, a modulator's among the module's, and a cable's among those of its modulator that stay.
 */
function modulatorsOfModule(
  snapshot: Snapshot,
  chosen: ReadonlySet<NodeId>,
  laneAt: ReadonlyMap<NodeId, number>,
  instrumentAt: ReadonlyMap<NodeId, number>,
  effectAt: ReadonlyMap<NodeId, { lane: number; effect: number }>,
): ModuleModulator[] {
  const { nodes, edges } = snapshot.graph;
  const rack = nodes.filter((node): node is Extract<Node, { kind: "modulator" }> => node.kind === "modulator" && chosen.has(node.id));
  const placeOf = new Map(rack.map((node, place) => [node.id, place]));
  const cablesOf = (modulator: NodeId): (typeof edges)[number][] => edges.filter((edge) => isModulation(edge) && edge.from.node === modulator);
  const pluginPlace = (node: NodeId): ModuleOutput | null => {
    const effect = effectAt.get(node);
    if (effect) return { lane: effect.lane, effect: effect.effect };
    const lane = instrumentAt.get(node);
    return lane === undefined ? null : { lane, effect: null };
  };
  // (For each cable of each of them: whether it stays, and, for one that drives the depth of another, which cable that is: the place of its modulator among these and its own among that modulator's.)
  const stays: boolean[][] = [];
  const depthOf: ({ place: number; cable: number } | null)[][] = [];
  for (const modulator of rack) {
    const row = cablesOf(modulator.id).map((edge): { keep: boolean; of: { place: number; cable: number } | null } => {
      const port = edge.to.port;
      switch (port.type) {
        case "param":
          return { keep: pluginPlace(edge.to.node) !== null, of: null };
        case "volume":
          return { keep: instrumentAt.has(edge.to.node), of: null };
        case "mix":
          return { keep: effectAt.has(edge.to.node), of: null };
        case "rate":
        case "attack":
        case "decay":
        case "sustain":
        case "release":
          return { keep: placeOf.has(edge.to.node), of: null };
        case "depth": {
          const driven = edges.find((other) => isModulation(other) && other.id === port.edge);
          const owner = driven ? placeOf.get(driven.from.node) : undefined;
          if (!driven || owner === undefined) return { keep: false, of: null };
          return { keep: true, of: { place: owner, cable: cablesOf(driven.from.node).indexOf(driven) } };
        }
        default:
          return { keep: false, of: null };
      }
    });
    stays.push(row.map((cable) => cable.keep));
    depthOf.push(row.map((cable) => cable.of));
  }
  for (let changed = true; changed; ) {
    changed = false;
    depthOf.forEach((row, place) => {
      row.forEach((of, cable) => {
        if (of && stays[place]?.[cable] && !stays[of.place]?.[of.cable]) {
          stays[place][cable] = false;
          changed = true;
        }
      });
    });
  }
  // (The place that each cable that stays has among those of its modulator that stay: what the file names a cable by.)
  const rank = stays.map((row) => {
    let kept = 0;
    return row.map((keep) => (keep ? kept++ : kept));
  });
  return rack.map((modulator, place): ModuleModulator => {
    const targets: ModuleTarget[] = [];
    cablesOf(modulator.id).forEach((edge, cable) => {
      if (!stays[place]?.[cable] || edge.kind !== "modulation") return;
      const port = edge.to.port;
      const target = nodes.find((node) => node.id === edge.to.node);
      let drives: ModuleDrives | null = null;
      if (port.type === "param") {
        const plugin = pluginPlace(edge.to.node);
        const name = target && (target.kind === "instrument" || target.kind === "effect") ? target.plugin?.params.find((p) => p.index === port.index)?.name : undefined;
        if (plugin && name !== undefined) drives = { type: "param", plugin, name };
      } else if (port.type === "volume" || port.type === "mix") {
        const plugin = pluginPlace(edge.to.node);
        if (plugin) drives = { type: "gain", plugin, gain: port.type };
      } else if (port.type === "depth") {
        const of = depthOf[place]?.[cable];
        const placed = of ? rank[of.place]?.[of.cable] : undefined;
        if (of && placed !== undefined) drives = { type: "depth", modulator: of.place, target: placed };
      } else if (port.type === "rate" || port.type === "attack" || port.type === "decay" || port.type === "sustain" || port.type === "release") {
        const sibling = placeOf.get(edge.to.node);
        if (sibling !== undefined) drives = { type: "setting", modulator: sibling, port: port.type };
      }
      if (drives) targets.push({ drives, depth: edge.amount });
    });
    // (It hears a lane of the module if the lane that it heard is one: the lane's place is the module's. A group is none.)
    const hears = modulator.hears === undefined ? null : (laneAt.get(modulator.hears) ?? null);
    return { source: modulator.source, hears, targets };
  });
}

/** The values of a plugin that are not its own once its preset is loaded, by the name of the parameter (`overrides` of `src/app/save.rs`). */
function valuesOver(plugin: Plugin): Record<string, number> {
  return Object.fromEntries(plugin.params.filter((p) => Math.abs(p.value - p.default) > SAME).map((p) => [p.name, p.value]));
}

/** The names of the parameters pinned to a plugin's node, in order (`pinned_names`). */
function pinnedNames(plugin: Plugin): string[] {
  return plugin.pinned.flatMap((index) => plugin.params.filter((p) => p.index === index).map((p) => p.name));
}
