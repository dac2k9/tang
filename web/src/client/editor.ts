// What the user does to the structure of the session, as the page asks it: add a modulator to a
// lane or a group, draw a cable from a modulator to a parameter, take either out. Each is an intent
// (docs/web-ui.md §5); the answer is the session's, which arrives as a snapshot, and what this adds
// to a plain request is what a person needs: the node that was made (the intent does not name it:
// a client finds it in the session it is then given), and, when the server refuses, a notice that
// says what could not be done and why, in the names the canvas shows.
import type { EdgeId, Endpoint, Feed, Graph, Intent, ModulatorKind, NodeId, NodeUi, NoteRange, PatternField, Plugin, Port, Route, SourceInfo } from "../protocol";
import type { Client } from "./connection";
import { isModulation } from "./cables";
import { trimmed } from "./inputs";
import { MODULATOR_LABELS } from "./modulatorKinds";
import { moduleNameOf } from "./modules";
import { cableWords, displayNames, endLabel } from "./names";
import type { Names } from "./names";
import { rangeLabel } from "./notes";
import type { Notices } from "./notices";
import { pinnedWith } from "./pinning";
import { samePort } from "./reach";
import type { Reveals } from "./reveals";

/** How many characters of a name a notice quotes: a name can be long, and a notice is read at a glance. */
const NOTICE_NAME = 24;

/** `text` as a notice quotes it: in quotes, and cut after `NOTICE_NAME` characters (counted as the server counts them: a character is not always one unit of a string). */
function quoted(text: string): string {
  const characters = Array.from(text);
  return JSON.stringify(characters.length > NOTICE_NAME ? `${characters.slice(0, NOTICE_NAME).join("")}…` : text);
}

/**
 * The most that a session file may be for the page to send it (`Editor.openSession`), in bytes of its text: the server reads a message of up to 8 MiB from a client that it let in (`MAX_OPEN_MESSAGE_BYTES`), and the text goes as a
 * JSON string, in which a quote and a line's end take two bytes: half of that is what is certain to go through, and is more than the files that people write (some kilobytes; the state of a plugin that is kept in a session makes it some hundreds).
 */
export const MAX_SESSION_FILE_BYTES = 4 * 1024 * 1024;

/**
 * How deep the cable of a control that learns starts, as the TUI's `l` gives it: the whole range, since a controller that is moved offsets the
 * parameter by as much as its knob goes. A new LFO or envelope has the controller's own depth, half (`NEW_CABLE_DEPTH`), as the TUI's `m` gives it.
 */
const LEARN_DEPTH = 1;

/** What a person was doing when an intent of each kind was made, for a notice that says it could not be done. */
const DOING: Record<Intent["type"], string> = {
  setParam: "set a parameter",
  setGain: "set a level",
  setLimiter: "put the limiter in the sound or take it out",
  loadPreset: "load a preset",
  openEditor: "open a plugin's own window",
  openSession: "open a session",
  setModulatorField: "change a modulator's setting",
  setSteps: "change the steps of a step sequencer",
  setEdgeDepth: "change the depth of a cable",
  removeEdge: "take out a cable",
  addFreeModulator: "add a modulator",
  addModEdge: "draw a cable",
  modulate: "modulate a parameter",
  addCable: "draw an audio cable",
  addSource: "add a source of notes",
  setCableGain: "change the level of an audio cable",
  addEffect: "add an effect",
  addFreeEffect: "add an effect of its own",
  addInstrument: "add an instrument",
  replaceInstrument: "change the instrument of a lane",
  clearInstrument: "take an instrument out of its lane",
  setRange: "set the keys of a lane",
  setMute: "mute or unmute a lane or a group",
  setSolo: "solo or unsolo a lane or a group",
  setRoute: "cut or give back the way out of a lane or a group",
  setModulatorEar: "choose whose notes a modulator hears",
  setFeed: "cut or give back the link into an effect",
  setScale: "set the scale of the piano",
  setLocked: "lock or unlock the piano",
  setBpm: "set the tempo",
  setRecording: "record a pattern",
  clearPattern: "clear a pattern",
  setPatternField: "change a pattern's setting",
  setNodeUi: "move a node",
  setPinned: "choose the knobs of a node",
  addGroup: "add a group",
  setLaneGroup: "put a lane in a group",
  renameGroup: "name a group",
  renameMacro: "name a macro",
  exposeMacro: "show or hide the knob of a macro on its box",
  removeNode: "remove a node",
  collapseIntoModule: "collapse the chosen nodes into a module",
  ungroupModule: "ungroup a module",
  moveEffect: "move an effect along its chain",
  save: "save the session",
  saveAs: "save the session under another name",
  saveModule: "save a chain of effects as a module",
  saveSelectionAsModule: "save the chosen nodes as a module",
  undo: "undo",
  redo: "redo",
};

const whyNot = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/** The notice for an intent that did not get through, when nothing more is known of what it was about (a knob turned: `Drive`). */
export function refusal(intent: Intent, error: unknown): string {
  return `Could not ${DOING[intent.type]}: ${whyNot(error)}`;
}

export class Editor {
  readonly #client: Client;
  readonly #notices: Notices;
  readonly #reveals: Reveals | undefined;

  /** `reveals`, if there is a canvas to show things on, is told which node the page added, for it to show. */
  constructor(client: Client, notices: Notices, reveals?: Reveals) {
    this.#client = client;
    this.#notices = notices;
    this.#reveals = reveals;
  }

  /**
   * Add a modulator of `kind` (every modulator is the session's own since step 96: it is in no lane and no group): a cable from it can end at a parameter of any plugin, and at the settings of the other modulators. It hears the notes
   * of the lane or the group `hears` (the "+" of a lane and of a group asks it so), or every note for none (the bar's "+ Modulator"). Resolves with the id of the node it made, which the canvas is asked to show,
   * or with null if it was refused (the page says why). The node is the last modulator the session did not have before: two clients adding at once may be given each other's, which is as good.
   * With `place` (a menu on the canvas, where the pointer was: the canvas's pixels) the node is put there, which is a move of its own in the history, and the canvas is not moved to show it.
   */
  async addFreeModulator(kind: ModulatorKind, hears: NodeId | null = null, place?: NodeUi): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    const what = hears === null ? `add ${MODULATOR_LABELS[kind]} of its own` : `add ${MODULATOR_LABELS[kind]} that hears ${this.#name(hears)}`;
    if (!(await this.#attempt(what, { type: "addFreeModulator", kind, hears }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "modulator" && !before.has(node.id));
    return this.#placed(added?.at(-1)?.id, place);
  }

  /**
   * Add a source of notes to the session: the keys (the computer's keys and the page's piano), or a device, called by the name of the MIDI input it stands for (or as much of it as tells it:
   * the server tidies the name). Every lane that no cable of notes ends at hears it. Resolves with the id of the node it made, which the canvas is asked to show, or with null if it was refused
   * (the page says why). The node is the last source there is (a source goes after the others): two clients adding at once may be given each other's, which is as good.
   */
  async addSource(source: SourceInfo, place?: NodeUi): Promise<NodeId | null> {
    const what = source.type === "keys" ? "add the keys as a source of notes" : `add ${quoted(trimmed(source.name))} as a source of notes`;
    if (!(await this.#attempt(what, { type: "addSource", source }))) return null;
    return this.#placed(this.#graph?.nodes.findLast((node) => node.kind === "source")?.id, place);
  }

  /**
   * Put the plugin `plugin` (the `id` of an entry of the catalog the server told) at the end of the effect chain of the lane
   * or group `parent`. Resolves with the id of the node it made, or with null if it was refused (the page says why). As for
   * `addFreeModulator`: the node is the last effect of `parent` the session did not have before.
   */
  async addEffect(parent: NodeId, plugin: string): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    if (!(await this.#attempt(`add ${this.#pluginName(plugin)} to ${this.#name(parent)}`, { type: "addEffect", parent, plugin }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "effect" && node.parent === parent && !before.has(node.id));
    return this.#shown(added?.at(-1)?.id);
  }

  /**
   * Add the plugin `plugin` (the `id` of an entry of the catalog the server told) as an effect that stands by itself: a lane of its own at the end of the session that holds it and has no instrument, so that
   * what the effect is fed is what cables bring. Resolves with the id of the effect it made (the lane is the effect's holder, and the canvas draws no card for it), which the canvas is asked to show, or with null
   * if it was refused (the page says why). As for `addFreeModulator`: the node is the last effect the session did not have before. (A module of the library is added by `addModule`.)
   */
  addFreeEffect(plugin: string, place?: NodeUi): Promise<NodeId | null> {
    return this.#addFree(plugin, `add ${this.#pluginName(plugin)} as an effect of its own`, place);
  }

  /**
   * Add the module `module` of the library (the `id` of an entry of the catalog the server told, `module:Room`) with everything that is in it (steps 104 to 108 and 111): the controller's `AddFreeEffect` of the module's
   * id, one step that Undo takes out. The canvas draws it as one box: it resolves with the id of the module's node, which the canvas is asked to show or which is put where `place` says, and not the last effect that is
   * in it, or with null if it was refused (the page says why: the file of the module is gone, or has a plugin that is not installed).
   */
  addModule(module: string, place?: NodeUi): Promise<NodeId | null> {
    return this.#addFree(module, `add the module ${moduleNameOf(module) ?? module}`, place);
  }

  /** What `addFreeEffect` and `addModule` do: the intent is the controller's one, and the node that is placed and shown is the module's if one was made, and the last effect if not. */
  async #addFree(plugin: string, what: string, place: NodeUi | undefined): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    if (!(await this.#attempt(what, { type: "addFreeEffect", plugin }))) return null;
    const added = this.#graph?.nodes.filter((node) => (node.kind === "effect" || node.kind === "module") && !before.has(node.id));
    return this.#placed((added?.findLast((node) => node.kind === "module") ?? added?.at(-1))?.id, place);
  }

  /**
   * Add a lane at the end of the session with the instrument `plugin` (the `id` of an entry of the catalog the server told) in it.
   * Resolves with the id of the lane it made (its instrument's node), or with null if it was refused (the page says why). As for
   * `addFreeModulator`: the node is the last lane the session did not have before.
   */
  async addInstrument(plugin: string, place?: NodeUi): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    if (!(await this.#attempt(`add ${this.#pluginName(plugin)} as an instrument`, { type: "addInstrument", plugin }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "instrument" && !before.has(node.id));
    return this.#placed(added?.at(-1)?.id, place);
  }

  /**
   * Add a group, a submix bus, at the end of the session, and put the lanes `members` (each the node of a lane) in it at once, whatever group
   * they were in; none makes an empty one. Resolves with the id of the group it made, or with null if it was refused (the page says why). As for
   * `addFreeModulator`: the node is the last group the session did not have before.
   */
  async addGroup(members: readonly NodeId[] = []): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    const what = members.length === 0 ? DOING.addGroup : `put ${members.map((member) => this.#name(member)).join(", ")} in a new group`;
    if (!(await this.#attempt(what, { type: "addGroup", members: [...members] }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "group" && !before.has(node.id));
    return this.#shown(added?.at(-1)?.id);
  }

  /**
   * Put the lane `node` in the group `group`, or, for null, take it out of its group. The group it leaves lets go of it: its cables to the
   * lane go, and what they drove has the value that was set. A group the lane is put in is shown to the canvas, since the layout moves it
   * to where its members are. True if it was done (the page says why if it was not).
   */
  async setLaneGroup(node: NodeId, group: NodeId | null): Promise<boolean> {
    const what = group === null ? `take ${this.#name(node)} out of its group` : `put ${this.#name(node)} in ${this.#name(group)}`;
    const done = await this.#attempt(what, { type: "setLaneGroup", node, group });
    if (done && group !== null) this.#shown(group);
    return done;
  }

  /**
   * Call the group `node` `name`: the spaces round it are not part of it, and a name of nothing gives the group its default name back ("Group" and
   * its number). True if it was done (the page says why if it was not).
   */
  renameGroup(node: NodeId, name: string): Promise<boolean> {
    return this.#attempt(`give ${this.#name(node)} the name ${quoted(name)}`, { type: "renameGroup", node, name });
  }

  /**
   * Call the macro `node` `name` (step 110): the spaces round it are not part of it, and a name of nothing gives the macro its default name back (`Macro`). The knob that a box shows for it says the name. One step of the
   * history. True if it was done (the page says why if it was not).
   */
  renameMacro(node: NodeId, name: string): Promise<boolean> {
    return this.#attempt(`give ${this.#name(node)} the name ${quoted(name)}`, { type: "renameMacro", node, name });
  }

  /**
   * Have the macro `node` show its knob on the box of the module that it is in, or not (step 110): it does to begin with. Nothing of the sound changes. One step of the history. True if it was done (the page says why if it was not).
   */
  exposeMacro(node: NodeId, expose: boolean): Promise<boolean> {
    return this.#attempt(`${expose ? "show" : "hide"} the knob of ${this.#name(node)} on its box`, { type: "exposeMacro", node, expose });
  }

  /**
   * Put the instrument `plugin` in the lane `node` in place of the one it has (an empty lane is filled). The lane keeps its
   * node, its effects, its modulators, its range and its volume; the cables that drove the old instrument's parameters go with it.
   * True if it was done (the page says why if it was not).
   */
  replaceInstrument(node: NodeId, plugin: string): Promise<boolean> {
    return this.#attempt(`change the instrument of ${this.#name(node)} to ${this.#pluginName(plugin)}`, { type: "replaceInstrument", node, plugin });
  }

  /**
   * Take the instrument out of the lane `node`: the lane stays, empty, with its effects, its modulators, its range, its volume and the rest, and the cables that drove the
   * instrument's parameters go. True if it was done (the page says why if it was not).
   */
  clearInstrument(node: NodeId): Promise<boolean> {
    return this.#attempt(`take ${this.#name(node)} out of its lane`, { type: "clearInstrument", node });
  }

  /**
   * Load the preset `preset` (its `id`) into the plugin of the instrument or effect `node` (a lane's: a bus effect has no presets to
   * load): the plugin's parameters take its values. True if it was loaded (the page says why if it was not, by the preset's name).
   */
  loadPreset(node: NodeId, preset: string): Promise<boolean> {
    return this.#attempt(`load ${this.#presetName(node, preset)} into ${this.#name(node)}`, { type: "loadPreset", node, preset });
  }

  /**
   * Open the window of the plugin of the instrument or effect `node` (step 112): the face that the plugin draws for itself, in a window on the machine that runs Tang (not in the browser), which takes the keyboard from
   * that machine's terminal; one that is open is brought to the front. What is done in it comes back as changes of the plugin's parameters, and the page shows them. True if it was opened (the page says why if it was
   * not: the plugin has none, the machine has no screen, the scan of the installed plugins is still going).
   */
  openEditor(node: NodeId): Promise<boolean> {
    return this.#attempt(`open the window of ${this.#name(node)}`, { type: "openEditor", node });
  }

  /**
   * Open the session that a person picked, in place of this one (step 115: "load a sound from within the UI, i.e. load a toml"): `text` is the text of the session file, `name` what the file is called (for the notice). Only a program
   * that can make its engine again takes it (the hello says `openSession`: a plugin in a host); it is not undone, and the page that asked is served again by the engine that is made, at another address, which a plugin's window
   * follows. A file too big to be sent is said at once and not sent. True if it was taken (the page says why if it was not: it is not a session, the program cannot).
   */
  openSession(text: string, name: string): Promise<boolean> {
    const what = `open ${quoted(name)}`;
    if (new TextEncoder().encode(text).length > MAX_SESSION_FILE_BYTES) {
      this.#notices.say(`Could not ${what}: it is more than ${String(MAX_SESSION_FILE_BYTES / (1024 * 1024))} MiB, which is more than a session that is opened here can be`);
      return Promise.resolve(false);
    }
    return this.#attempt(what, { type: "openSession", text });
  }

  /** The session in a file that a person picked (`openSession`): the file is read here, and a file that cannot be read is said among the notices. True if the session was taken. */
  async openSessionFile(file: { readonly name: string; text(): Promise<string> }): Promise<boolean> {
    let text: string;
    try {
      text = await file.text();
    } catch (error) {
      this.#notices.say(`Could not open ${quoted(file.name)}: it could not be read (${whyNot(error)})`);
      return false;
    }
    return this.openSession(text, file.name);
  }

  /**
   * Set the keys the lane `node` takes: `range`, both ends included, or null for every note. True if they were set (the page says
   * why if they were not, with the keys written as the TUI writes them: `C2-C6`).
   */
  setRange(node: NodeId, range: NoteRange | null): Promise<boolean> {
    return this.#attempt(`set the keys of ${this.#name(node)} to ${rangeLabel(range)}`, { type: "setRange", node, range });
  }

  /** Mute or unmute the lane or group `node`: it is never heard while it is muted, whatever is soloed. True if it was done (the page says why if it was not). */
  setMute(node: NodeId, muted: boolean): Promise<boolean> {
    return this.#attempt(`${muted ? "mute" : "unmute"} ${this.#name(node)}`, { type: "setMute", node, muted });
  }

  /**
   * Put the limiter on the output mix in the sound (`on`) or take it out: it holds the mix under its ceiling and does not let it clip. True if it was done (the page says why if it was not).
   */
  setLimiter(on: boolean): Promise<boolean> {
    return this.#attempt(on ? "put the limiter in the sound" : "take the limiter out of the sound", { type: "setLimiter", on });
  }

  /** Solo or unsolo the lane or group `node`: while anything is soloed only what is soloed is heard. True if it was done (the page says why if it was not). */
  setSolo(node: NodeId, solo: boolean): Promise<boolean> {
    return this.#attempt(`${solo ? "solo" : "unsolo"} ${this.#name(node)}`, { type: "setSolo", node, solo });
  }

  /**
   * Cut (`none`) or give back (`out`) the way out of the lane or group `node`: where its end goes, to the bus of its group or the mix, or nowhere but its audio cables. True if it was done (the
   * page says why if it was not: a way out that is given back where the audio would go round, by the cables there are).
   */
  setRoute(node: NodeId, route: Route): Promise<boolean> {
    return this.#attempt(route === "none" ? `cut the way out of ${this.#name(node)}` : `give ${this.#name(node)} its way out`, { type: "setRoute", node, route });
  }

  /**
   * Make the modulator `node`, which is in no lane and no group, hear the notes of the lane or group `ear`, or every note (`null`): an envelope follows the pattern that a lane plays, a control that follows the
   * number of a note follows the lane's, an LFO or a step sequencer that starts again at a note starts again at the lane's. True if it was done (the page says why if it was not).
   */
  setModulatorEar(node: NodeId, ear: NodeId | null): Promise<boolean> {
    return this.#attempt(ear === null ? `make ${this.#name(node)} hear every note` : `make ${this.#name(node)} hear ${this.#name(ear)}`, { type: "setModulatorEar", node, ear });
  }

  /**
   * Cut (`none`) or give back (`chain`) the link into the effect `node` from the stage before it in its chain (the lane's instrument, the effect before it, or the sum of the bus): an effect whose link is cut
   * is fed by its audio cables and by nothing else. True if it was done (the page says why if it was not: a link that is given back where the audio would go round, by the cables there are).
   */
  setFeed(node: NodeId, feed: Feed): Promise<boolean> {
    return this.#attempt(feed === "none" ? `cut the input of ${this.#name(node)}` : `give ${this.#name(node)} its input back`, { type: "setFeed", node, feed });
  }

  /**
   * Set the scale of the piano: `scale` is a root and a kind as the session file says them (`C major`, `F# dorian`). True if it
   * was set (the page says why if it was not).
   */
  setScale(scale: string): Promise<boolean> {
    return this.#attempt(`set the scale of the piano to ${scale}`, { type: "setScale", scale });
  }

  /** Lock the piano to its scale, so that a note outside it is not played, or let every note through. True if it was done. */
  setLocked(locked: boolean): Promise<boolean> {
    return this.#attempt(locked ? "lock the piano to its scale" : "let the piano play every note", { type: "setLocked", locked });
  }

  /**
   * Set the tempo that patterns are recorded at, in beats a minute. The server holds it to the tempos there can be (`BPM_RANGE`) and saves it with the
   * session. True if it was set (the page says why if it was not).
   */
  setBpm(bpm: number): Promise<boolean> {
    return this.#attempt(`set the tempo to ${String(bpm)}`, { type: "setBpm", bpm });
  }

  /**
   * Start recording a pattern on the lane `node`, or stop the recording that is going on there. A recording is counted in (four beats of the metronome) and then taken for the
   * length of the pattern, at the tempo of the session, from the notes that are played; the pattern it makes is the lane's, and the session tells it when it is done. A lane that has
   * no pattern is given an empty one at once, with the settings a new pattern has. True if it was done (the page says why if it was not).
   */
  setRecording(node: NodeId, recording: boolean): Promise<boolean> {
    const what = recording ? `start recording a pattern on ${this.#name(node)}` : `stop recording the pattern of ${this.#name(node)}`;
    return this.#attempt(what, { type: "setRecording", node, recording });
  }

  /** Take the pattern of the lane `node` out, and end a recording that is going on there. True if it was done (the page says why if it was not: a lane that has none). */
  clearPattern(node: NodeId): Promise<boolean> {
    return this.#attempt(`clear the pattern of ${this.#name(node)}`, { type: "clearPattern", node });
  }

  /**
   * Set a setting of the pattern of the lane `node`: its length in beats (`PATTERN_FIELD_RANGES`: held to the lengths there can be), and, as 0 or 1, whether a held key plays it
   * (`enabled`), whether it goes round while the key is held (`looping`) and whether it is transposed by the degrees of the piano's scale and not by semitones (`transpose`). A lane
   * that has no pattern is given an empty one with it, so that the length of its first recording can be chosen. True if it was done (the page says why if it was not).
   */
  setPatternField(node: NodeId, field: PatternField, value: number): Promise<boolean> {
    const pattern = `the pattern of ${this.#name(node)}`;
    const what: Record<PatternField, string> = {
      length: `set the length of ${pattern} to ${String(value)} beats`,
      enabled: `turn ${pattern} ${value === 0 ? "off" : "on"}`,
      looping: value === 0 ? `make ${pattern} play once` : `make ${pattern} loop`,
      transpose: `transpose ${pattern} ${value === 0 ? "by semitones" : "by the degrees of the scale"}`,
    };
    return this.#attempt(what[field], { type: "setPatternField", node, field, value });
  }

  /**
   * Say where the node `node` is drawn, `ui` in the pixels of the canvas, or null to give it back to the layout (the master is
   * a node too). The server keeps it to a tenth of a pixel and within the limits of the canvas, and saves it with the session.
   * True if it was kept (the page says why if it was not).
   */
  moveNode(node: NodeId, ui: NodeUi | null): Promise<boolean> {
    const what = ui === null ? `give ${this.#name(node)} back to the layout` : `move ${this.#name(node)}`;
    return this.#attempt(what, { type: "setNodeUi", node, ui });
  }

  /**
   * Choose which parameters of the plugin of the node `node` its node shows as knobs: `params` are the plugin's own indices (`Param.index`),
   * in the order they are to be shown; none gives the node back to the page's own choice (the first few). It is kept with the session.
   * True if it was done (the page says why if it was not).
   */
  setPinned(node: NodeId, params: readonly number[]): Promise<boolean> {
    return this.#attempt(`choose the knobs of ${this.#name(node)}`, { type: "setPinned", node, params: [...params] });
  }

  /**
   * Show the parameter `index` of the plugin of `node` as a knob on its node (`pinned`), or stop showing it. The node's other knobs are as
   * they were: with nothing pinned it shows the first few, and pinning one more keeps them (`pinnedWith`). True if it is so now, which it
   * may be without asking the server; the page says why if it could not be done.
   */
  async pinParam(node: NodeId, index: number, pinned: boolean): Promise<boolean> {
    const plugin = this.#plugin(node);
    const what = `${pinned ? "pin" : "unpin"} ${this.#paramName(plugin, index)} ${pinned ? "to" : "from"} ${this.#name(node)}`;
    // (A node that has no plugin, or is not there, is for the server to refuse, in its words.)
    if (!plugin) return this.#attempt(what, { type: "setPinned", node, params: [index] });
    const result = pinnedWith(plugin, index, pinned);
    if ("unchanged" in result) return true;
    if ("refused" in result) {
      this.#notices.say(`Could not ${what}: ${result.refused}`);
      return false;
    }
    return this.#attempt(what, { type: "setPinned", node, params: result.list });
  }

  /**
   * Give every node that has a place back to the layout (the master too), one after the other. True if they all went; the page says why
   * if one did not, and goes on with the rest. With no node placed there is nothing to ask.
   */
  async resetLayout(): Promise<boolean> {
    const placed = this.#graph?.nodes.filter((node) => node.ui !== undefined) ?? [];
    let all = true;
    for (const node of placed) {
      if (!(await this.moveNode(node.id, null))) all = false;
    }
    return all;
  }

  /**
   * Draw a cable from the modulator `modulator` to `to`, a port of `node`: a parameter of the plugin in it (by the plugin's own
   * index), or a setting of the modulator it is (its rate, a stage of its envelope), or the depth of one of its cables (`node` is
   * the modulator that the cable starts at). True if it was drawn.
   */
  addCable(modulator: NodeId, node: NodeId, to: Port): Promise<boolean> {
    const words = this.#words({ from: { node: modulator, port: { type: "modOut" } }, to: { node, port: to } });
    return this.#attempt(`draw a cable from ${words.replaceAll(" → ", " to ")}`, { type: "addModEdge", modulator, to: { node, port: to } });
  }

  /**
   * Draw an audio cable from the output of the node `from` (a lane's instrument, an effect or a group) to the input of the node `to` (an effect, the bus of a group or the master), or to the
   * second input of an effect that has one when `side` is set (a Math's B), at the level `gain` (held to the levels a cable can have), or at the level a new cable has when there is none. The
   * audio of `from` goes there as well as where it went. True if it was drawn (the page says why if it was not: a lane has no input, an effect has no second input, a cable between the two is
   * there already, the audio would go round).
   */
  addAudioCable(from: NodeId, to: NodeId, gain: number | null = null, side = false): Promise<boolean> {
    const ends = { from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: side ? "audioInSide" : "audioIn" } } } as const;
    const what = `draw an audio cable from ${this.#words(ends).replaceAll(" → ", " to ")}`;
    // (A number that is not a number cannot be sent: it is written as none, which is a cable at the level a new one has, and not the one that was asked for.)
    if (gain !== null && !Number.isFinite(gain)) {
      this.#notices.say(`Could not ${what}: ${String(gain)} is not a value for the level of a cable`);
      return Promise.resolve(false);
    }
    return this.#attempt(what, { type: "addCable", from: ends.from, to: ends.to, gain });
  }

  /**
   * Draw a cable of notes from the output of events of the source `from` to the input of events of the lane `to` (its instrument's node): from then on the lane hears the sources that have a cable
   * to it and no other (a lane that has none hears every source). True if it was drawn (the page says why if it was not: a node that is not a source or not a lane, a cable between the two
   * is there already).
   */
  addNotesCable(from: NodeId, to: NodeId): Promise<boolean> {
    const ends = { from: { node: from, port: { type: "eventsOut" } }, to: { node: to, port: { type: "eventsIn" } } } as const;
    return this.#attempt(`draw a cable of notes from ${this.#words(ends).replaceAll(" → ", " to ")}`, { type: "addCable", from: ends.from, to: ends.to, gain: null });
  }

  /**
   * Make a modulator of `kind` for what the port `to` of the node `node` is, and draw a cable from it to the port, as one change that is one step
   * of the history (`Modulate`): a parameter of the plugin in it (by the plugin's own index), or a setting of a modulator (its rate, a stage of its
   * envelope), or the depth of one of its cables. The modulator is the session's own (step 94), and hears the lane of a lane's instrument or an
   * effect of its chain, the group of a bus effect, and what the modulator hears for a setting of one that is the session's own, where it is a sibling that drives it. A
   * control that learns is armed, and its cable starts at the whole range (`LEARN_DEPTH`). Resolves with the id of the modulator it made, which the
   * canvas is asked to show, or with null if it was refused (the page says why). The node is the one new to the session that has the cable to the
   * port: two clients that modulate one parameter at once may be given each other's, which is as good.
   */
  async modulate(node: NodeId, to: Port, kind: ModulatorKind): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((n) => n.id));
    const what = `modulate ${endLabel(this.#graph ?? { nodes: [], edges: [] }, this.#names, { node, port: to })} with ${MODULATOR_LABELS[kind]}`;
    const intent: Intent = { type: "modulate", to: { node, port: to }, kind, depth: kind === "midiLearn" ? LEARN_DEPTH : null };
    if (!(await this.#attempt(what, intent))) return null;
    const made = this.#graph?.edges.findLast((e) => isModulation(e) && !before.has(e.from.node) && e.to.node === node && samePort(e.to.port, to));
    return this.#shown(made?.kind === "modulation" ? made.from.node : undefined);
  }

  /** Take the modulator or effect `node` out, with the cables that drive it (and, for a modulator, those it drives). True if it went. */
  removeNode(node: NodeId): Promise<boolean> {
    return this.#attempt(`remove ${this.#name(node)}`, { type: "removeNode", node });
  }

  /**
   * Collapse the nodes `nodes` (what is chosen on the canvas: a lane's instrument, an effect, a modulator) into one module where they are (step 109): the lane is the unit, so that a lane that has any of its plugins chosen is in the
   * module whole, with the lanes in the order of the session and then the modulators. The canvas draws it as one box that opens (`collapse` in `boxes.ts`), the sound is what it was, and it is one step of the history that Undo takes away
   * (the nodes stay). `name` is what the box is called (a name of nothing is `Module`), and `place`, if there is one, where it is drawn: in the same step. Resolves with the id of the box, or with null if it was refused
   * (the page says why: a node that a module does not hold, one that is in a module already, nothing chosen, a name that is not one). The canvas is not moved to show it: it is where what was chosen was.
   */
  async collapseIntoModule(nodes: readonly NodeId[], name: string, place?: NodeUi): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    const what = `collapse ${nodes.map((node) => this.#name(node)).join(", ")} into a module`;
    if (!(await this.#attempt(what, { type: "collapseIntoModule", nodes: [...nodes], name, ui: place ?? null }))) return null;
    return this.#graph?.nodes.findLast((node) => node.kind === "module" && !before.has(node.id))?.id ?? null;
  }

  /** Take the box of the module `node` away and keep what is in it (step 109): the lanes and the modulators stay in the session as they are. One step of the history, which Undo puts back. True if it was done (the page says why if it was not). */
  ungroupModule(node: NodeId): Promise<boolean> {
    return this.#attempt(`ungroup ${this.#name(node)}`, { type: "ungroupModule", node });
  }

  /**
   * Move the effect `node` (a lane's or a bus's) to place `index` of its chain, 0 being the first, the nearest the instrument or the
   * group: the effects between its place and the new one move one place the other way, and the audio goes through them in the new
   * order. The cables that drive them stay on the plugins they drove. True if it was moved (the page says why if it was not, with the place
   * counted from 1, as a person counts).
   */
  moveEffect(node: NodeId, index: number): Promise<boolean> {
    return this.#attempt(`move ${this.#name(node)} to place ${String(index + 1)} of its chain`, { type: "moveEffect", node, index });
  }

  /** Take the cable `edge` out. True if it went. */
  removeCable(edge: EdgeId): Promise<boolean> {
    return this.#attempt(`take out ${this.#cable(edge)}`, { type: "removeEdge", edge });
  }

  /** Write the session to its own file. True if it was written (the page says why if it was not). */
  save(): Promise<boolean> {
    return this.#attempt(DOING.save, { type: "save" });
  }

  /**
   * Write the session to a file called `name`, beside the one it has, and have that file be its own from then on (what the bar shows, and what Save writes):
   * a file name and not a path, to which `.toml` is added, as the program checks it. True if it was written (the page says why if it was not: a name that has a
   * folder in it, a file that is there already, a file that cannot be written).
   */
  saveAs(name: string): Promise<boolean> {
    return this.#attempt(`save the session as ${quoted(name.trim())}`, { type: "saveAs", name });
  }

  /**
   * Save the chain of effects of the lane `node` is of (the lane, or an effect of its chain) as a module of the library under `name`: a file name and not a path, to which `.toml` is added, as the program checks it; a
   * module that has the name is written over. The catalog that the server tells has the module from then on, and the bar's "+ Module" adds it. True if it was saved, and the page says so; if it was not it says why (a name that is
   * no name, a lane that has no effect, a program that keeps no library). The session is not changed by it: nothing is unsaved because of it, and there is nothing to undo.
   */
  async saveModule(node: NodeId, name: string): Promise<boolean> {
    const done = await this.#attempt(`save the chain of ${this.#name(node)} as the module ${quoted(name.trim())}`, { type: "saveModule", node, name });
    if (done) this.#notices.say(`Saved the module ${quoted(name.trim())}: + Module adds it`);
    return done;
  }

  /**
   * Save the plugins in `nodes` as a module of the library under `name` (step 105: what is chosen on the canvas, the instruments and the effects of any lanes): each lane that has one of them is a lane of the module, with the instrument
   * if it was chosen and the effects that were, each as it is now, and the audio cables that are between them. What joins them to what is not chosen is left out, as are the cables that go in and out. The server says what a name may be,
   * and a module that has the name is written over. The catalog that the server tells has the module from then on, and the bar's "+ Module" adds it. True if it was saved, and the page says so; if it was not it says why (a name that is no
   * name, a node that a module has none of, nothing chosen, a program that keeps no library). The session is not changed by it: nothing is unsaved because of it, and there is nothing to undo.
   */
  async saveSelectionAsModule(nodes: readonly NodeId[], name: string): Promise<boolean> {
    const what = `save ${nodes.map((node) => this.#name(node)).join(", ")} as the module ${quoted(name.trim())}`;
    const done = await this.#attempt(what, { type: "saveSelectionAsModule", nodes: [...nodes], name });
    if (done) this.#notices.say(`Saved the module ${quoted(name.trim())}: + Module adds it`);
    return done;
  }

  /** Put back what the last change that can be undone changed (what the server says it is: `ClientState.history`). True if it was done (the page says why if it was not). */
  undo(): Promise<boolean> {
    return this.#attempt(DOING.undo, { type: "undo" });
  }

  /** Do again what was undone last. True if it was done (the page says why if it was not). */
  redo(): Promise<boolean> {
    return this.#attempt(DOING.redo, { type: "redo" });
  }

  /**
   * The node the page added, put where `place` says if there is a place (a menu on the canvas adds it where the pointer was: it is where the person is looking, and the canvas is not asked to show it) and shown
   * otherwise, or if the place was refused (the page says why). The node, or null if the page did not find it.
   */
  async #placed(node: NodeId | undefined, place: NodeUi | undefined): Promise<NodeId | null> {
    if (node === undefined) return null;
    if (place !== undefined && (await this.moveNode(node, place))) return node;
    return this.#shown(node);
  }

  /** The node the page added, if it found it, which the canvas is asked to show (it is in the session by now); the node, or null. */
  #shown(node: NodeId | undefined): NodeId | null {
    if (node === undefined) return null;
    this.#reveals?.show(node);
    return node;
  }

  get #graph(): Graph | undefined {
    return this.#client.state.snapshot?.graph;
  }

  /** What a plugin is called, by the catalog the server told, or by its id if the catalog is not known (yet) or does not have it. */
  #pluginName(id: string): string {
    return this.#client.state.catalog?.entries.find((entry) => entry.id === id)?.name ?? id;
  }

  /** What a plugin's preset is called, by the plugin's own list, or by its id if the node or the preset is not known. */
  #presetName(node: NodeId, id: string): string {
    const found = this.#graph?.nodes.find((n) => n.id === node);
    const plugin = found?.kind === "instrument" || found?.kind === "effect" ? found.plugin : null;
    return plugin?.presets.find((preset) => preset.id === id)?.name ?? id;
  }

  /** The plugin of an instrument or an effect, if the session has the node. */
  #plugin(node: NodeId): Plugin | null {
    const found = this.#graph?.nodes.find((n) => n.id === node);
    return found?.kind === "instrument" || found?.kind === "effect" ? found.plugin : null;
  }

  /** What a parameter of a plugin is called, or "parameter 3" if the plugin is not known or has none of that index. */
  #paramName(plugin: Plugin | null, index: number): string {
    return plugin?.params.find((param) => param.index === index)?.name ?? `parameter ${String(index)}`;
  }

  #name(node: NodeId): string {
    const graph = this.#graph;
    return (graph && displayNames(graph).get(node)) ?? "that node";
  }

  /** What the nodes are called in what the page says of a cable: as on the canvas, and "that node" for one the session has not (or while it is not known). */
  get #names(): Names {
    const graph = this.#graph;
    const names = graph ? displayNames(graph) : undefined;
    return { get: (node) => names?.get(node) ?? "that node" };
  }

  /** A cable, made or not, in words: `LFO → Filter: cutoff` (`cableWords`). */
  #words(cable: { from: Endpoint; to: Endpoint; id?: EdgeId }): string {
    return cableWords(this.#graph ?? { nodes: [], edges: [] }, this.#names, cable);
  }

  /** A cable in words, as the canvas labels it (a cable of modulation, an audio cable or a cable of notes: their ids are one series); "that cable" if it is not in the session. */
  #cable(edge: EdgeId): string {
    const found = this.#graph?.edges.find((e) => (e.kind === "modulation" || e.kind === "cable" || e.kind === "events") && e.id === edge);
    return found ? `the cable ${this.#words(found)}` : "that cable";
  }

  /** Do the intent; if it is refused, or the connection fails, say so as a notice and return false. */
  async #attempt(what: string, intent: Intent): Promise<boolean> {
    try {
      await this.#client.request(intent);
      return true;
    } catch (error) {
      this.#notices.say(`Could not ${what}: ${whyNot(error)}`);
      return false;
    }
  }
}
