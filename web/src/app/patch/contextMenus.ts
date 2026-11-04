// What the right-click menus of the canvas offer (step 107): for the canvas itself, for a node (whatever kind of node it is), for a cable and for the nodes that are chosen. Each is a list of items that do their own work
// (`ContextItem.run`), made from the session as it is when the menu is opened (the menus are looked up when a person asks for them, not each time anything changes), by what the buttons of the page already say: the menus of
// a node's header, the bar's "+" menus, the keys that Undo and Redo. A choice asks the editor, which says among the notices why if the server refuses it, and the things that are the canvas's own (the panel of the parameters, a
// node's scope, what is chosen, the arrangement) are asked of `CanvasActions`. So there is one place that says what a node can be asked to do, and the menu and the buttons agree.
import { effectItems, instrumentItems, moduleItems } from "../../client/catalog";
import type { ClientState } from "../../client/connection";
import type { Editor } from "../../client/editor";
import { EVERY_NOTE, earItems } from "../../client/hears";
import { noteFromItems, noteToItems, sourceChosen, sourceItems } from "../../client/noteItems";
import { keptWords, removalWords } from "../../client/removal";
import { chainLengthOf, chainPlaceOf, flagsOf, limiterOf, paramCountOf, presetsOf, removalOf } from "../../client/selectors";
import { sendItems, sendTarget } from "../../client/sends";
import type { NodeId, NodeUi } from "../../protocol";
import type { ContextItem } from "../../ui/ContextMenu";
import type { MenuItem } from "../../ui/MenuButton";
import { MODULATOR_KINDS } from "./nodes";
import { boxDetail } from "./boxes";
import type { MenuSpec } from "./contextMenu";
import { COLLAPSE_NAME } from "./SaveModule";
import type { NameOptions } from "./SaveModule";
import { collapseWhyNot, collapseWords } from "./selection";
import type { Collapsible, Held } from "./selection";
import type { PatchEdge, PatchNode, PatchNodeData } from "./toFlow";

/** What the menus ask of the session: the methods of the editor that they use. */
export type MenuEditor = Pick<
  Editor,
  | "addInstrument"
  | "addFreeEffect"
  | "addModule"
  | "addFreeModulator"
  | "addSource"
  | "addEffect"
  | "replaceInstrument"
  | "clearInstrument"
  | "loadPreset"
  | "addAudioCable"
  | "addNotesCable"
  | "setMute"
  | "setSolo"
  | "setModulatorEar"
  | "setLimiter"
  | "moveEffect"
  | "removeNode"
  | "collapseIntoModule"
  | "ungroupModule"
  | "removeCable"
  | "setRoute"
  | "setFeed"
  | "saveModule"
  | "saveSelectionAsModule"
  | "undo"
  | "redo"
>;

/** What the menus ask of the canvas, which is not the session's: the panel of the parameters, a node's scope, what is chosen, the arrangement and the view. */
export interface CanvasActions {
  /** Whether the panel of the parameters of the node is open. */
  inspecting: (node: NodeId) => boolean;
  /** Opens the panel of the parameters of the node, or shuts it if it is open. */
  inspect: (node: NodeId) => void;
  /** Whether the node shows a scope of what comes out of it. */
  scoped: (node: NodeId) => boolean;
  /** Shows the scope of the node, or takes it away. */
  scope: (node: NodeId) => void;
  /** Whether choosing is on (the button of the canvas's bar). */
  choosing: boolean;
  toggleChoosing: () => void;
  /** The nodes that are chosen, and what a module can hold of them. */
  chosen: readonly NodeId[];
  held: Held;
  /** What collapsing the chosen nodes into one module takes (step 109), and where the box is drawn. */
  collapsible: Collapsible;
  collapseAt?: NodeUi | undefined;
  chooseAll: () => void;
  chooseNone: () => void;
  /** Opens the field for the name of a module, which asks `save` for the name that was typed (the field starts with `options`'s name and calls its button by its action). */
  nameModule: (label: string, save: (name: string) => Promise<boolean>, options?: NameOptions) => void;
  /** Arranges the nodes again; false when they are arranged already. */
  arrange: () => void;
  canArrange: boolean;
  /** Moves the canvas to show every node. */
  fit: () => void;
  /** Opens the module that is a box, or closes the module that is open (step 108). */
  toggleModule: (module: NodeId) => void;
}

/** What a menu is made from. */
export interface MenuDeps {
  /** The session as it is now. */
  state: ClientState;
  editor: MenuEditor;
  canvas: CanvasActions;
  /** Where the pointer was, in the pixels of the canvas: a node that a menu adds is put there. */
  at?: NodeUi;
}

/** The label of the field for the name that what is chosen is saved under: the button of the canvas's bar has it too (the browser specs and the screen readers find the field by it). */
export const SAVE_CHOSEN = "Save the chosen nodes as a module";

/** The label of the field for the name that what is chosen is collapsed under (step 109): as `SAVE_CHOSEN` is, the bar's button has it too. */
export const COLLAPSE_CHOSEN = "Collapse the chosen nodes into a module";

/** A menu of `items`, or an item that is shut, saying why, when there are none to choose from. */
const submenu = (id: string, label: string, items: readonly ContextItem[], none: string): ContextItem => (items.length === 0 ? { id, label, items, disabled: true, title: none } : { id, label, items });

/** The items of a menu (`MenuItem`, which the buttons of the page have lists of) as items that do `choose` with the id of the one that is chosen. */
function choosing<Id extends string>(items: readonly MenuItem<Id>[], choose: (id: Id) => void): ContextItem[] {
  return items.map((item) => ({
    ...item,
    run: () => {
      choose(item.id);
    },
  }));
}

/** The item of a switch: a check while it is on. */
const toggle = (id: string, label: string, on: boolean, flip: () => void, title?: string): ContextItem => ({ id, label, checked: on, title, run: flip });

/** An item that takes something away. */
const removing = (id: string, label: string, run: () => void): ContextItem => ({ id, label, danger: true, rule: true, run });

/** The plugins that can be an instrument, as a menu that puts one in the lane `node` (or a lane of its own: none). */
function instrumentMenu(deps: MenuDeps, label: string, choose: (plugin: string) => void): ContextItem {
  const entries = deps.state.catalog?.entries ?? [];
  return submenu("instrument", label, choosing(instrumentItems(entries), choose), "No instrument is installed");
}

/** The mute and the solo of a lane or a group, as switches. */
function muteSolo(deps: MenuDeps, lane: NodeId): ContextItem[] {
  const flags = flagsOf(deps.state, lane);
  if (!flags) return [];
  const { editor } = deps;
  return [
    toggle("mute", "Mute", flags.muted, () => {
      void editor.setMute(lane, !flags.muted);
    }),
    toggle("solo", "Solo", flags.solo, () => {
      void editor.setSolo(lane, !flags.solo);
    }),
  ];
}

/** The menu that adds a modulator that hears the lane or group `lane` (the "+" of its header). */
function modulatorMenu(deps: MenuDeps, lane: NodeId): ContextItem {
  const { editor } = deps;
  return {
    id: "modulator",
    label: "Add a modulator",
    items: choosing(MODULATOR_KINDS, (kind) => {
      void editor.addFreeModulator(kind, lane);
    }),
  };
}

/** The menu that puts an effect at the end of the chain of the lane or group `parent` (the "fx" of its header). */
function effectMenu(deps: MenuDeps, parent: NodeId): ContextItem {
  const { editor, state } = deps;
  return submenu(
    "effect",
    "Add an effect",
    choosing(effectItems(state.catalog?.entries ?? []), (plugin) => {
      void editor.addEffect(parent, plugin);
    }),
    "No effect is installed",
  );
}

/** The menu that draws an audio cable from the output of `node` to a place that takes it (the "⇢" of its header). */
function sendMenu(deps: MenuDeps, node: NodeId): ContextItem {
  const graph = deps.state.snapshot?.graph;
  const { editor } = deps;
  return submenu(
    "send",
    "Send audio to",
    graph
      ? choosing(sendItems(graph, node), (to) => {
          const end = sendTarget(to);
          void editor.addAudioCable(node, end.node, null, end.side);
        })
      : [],
    "There is no place that takes a cable from it",
  );
}

/** The presets of the plugin of `node`, as a menu that loads one, the one loaded last marked (the "≡" of its header); nothing for a plugin that has none. */
function presetMenu(deps: MenuDeps, node: NodeId): ContextItem[] {
  const found = presetsOf(deps.state, node);
  if (!found || found.presets.length === 0) return [];
  const { editor } = deps;
  const items = found.presets.map((preset): MenuItem => ({ id: preset.id, label: preset.name, current: preset.name === found.current }));
  return [
    {
      id: "presets",
      label: "Load a preset",
      items: choosing(items, (preset) => {
        void editor.loadPreset(node, preset);
      }),
    },
  ];
}

/** The switches of a plugin's node that are the canvas's own: its panel of parameters and its scope. */
function viewItems(deps: MenuDeps, node: NodeId, scoped: boolean): ContextItem[] {
  const { canvas, state } = deps;
  return [
    ...(paramCountOf(state, node) > 0
      ? [
          toggle("parameters", "Parameters…", canvas.inspecting(node), () => {
            canvas.inspect(node);
          }),
        ]
      : []),
    ...(scoped
      ? [
          toggle("scope", "Scope", canvas.scoped(node), () => {
            canvas.scope(node);
          }),
        ]
      : []),
  ];
}

/** The item that saves the chain of effects of the lane `lane` as a module, if it has one: the field for the name that it opens says whose chain it is, by the name that the canvas calls the lane. */
function chainItem(deps: MenuDeps, lane: NodeId): ContextItem[] {
  const removal = removalOf(deps.state, lane);
  if (!removal || chainLengthOf(deps.state, lane) === 0) return [];
  const { canvas, editor } = deps;
  return [
    {
      id: "save-chain",
      label: "Save the chain as a module…",
      run: () => {
        canvas.nameModule(`Save the chain of ${removal.name} as a module`, (name) => editor.saveModule(lane, name));
      },
    },
  ];
}

/** The two items that take a lane out: its instrument alone, and the whole lane, in the words that say what goes and what stays. */
function laneRemoval(deps: MenuDeps, lane: NodeId): ContextItem[] {
  const removal = removalOf(deps.state, lane);
  if (!removal) return [];
  const { editor } = deps;
  const what = removal.kind === "lane" ? `the lane of ${removal.name}` : removal.name;
  const items: ContextItem[] = [];
  if (removal.instrument) {
    items.push(
      removing("remove-instrument", `Remove ${removal.name} only${keptWords(removal)}`, () => {
        void editor.clearInstrument(lane);
      }),
    );
  }
  if (!removal.last) {
    items.push(
      removing("remove", `Remove ${what} for good${removalWords(removal)}`, () => {
        void editor.removeNode(lane);
      }),
    );
  }
  // (One rule above the first of them.)
  return items.map((item, index) => (index === 0 ? item : { ...item, rule: false }));
}

/** The menu of a lane, with its instrument or without: what the buttons of its card are, and more. */
function laneMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const { editor, state } = deps;
  const graph = state.snapshot?.graph;
  const empty = data.format === null;
  const from = graph && data.notes ? noteFromItems(graph, node) : [];
  return [
    instrumentMenu(deps, empty ? "Choose an instrument" : "Change the instrument", (plugin) => {
      void editor.replaceInstrument(node, plugin);
    }),
    ...presetMenu(deps, node),
    ...viewItems(deps, node, !empty),
    { ...effectMenu(deps, node), rule: true },
    modulatorMenu(deps, node),
    ...(empty ? [] : [sendMenu(deps, node)]),
    ...(data.notes
      ? [
          submenu(
            "notes",
            "Take notes from",
            choosing(from, (source) => {
              void editor.addNotesCable(Number(source), node);
            }),
            "No source of notes is left to take notes from",
          ),
        ]
      : []),
    ...muteSolo(deps, node).map((item, index) => (index === 0 ? { ...item, rule: true } : item)),
    ...chainItem(deps, node),
    ...laneRemoval(deps, node),
  ];
}

/** The menu of an effect: where it is in its chain, its presets, its panel, its scope, what it sends, and, if it stands for its lane, the lane's own. */
function effectNodeMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const { editor, state } = deps;
  const place = chainPlaceOf(state, node);
  const held = data.heldIn;
  const own: ContextItem[] = held?.first === true ? [modulatorMenu(deps, held.lane), ...muteSolo(deps, held.lane), ...chainItem(deps, held.lane)] : [];
  return [
    ...(place
      ? [
          {
            id: "earlier",
            label: "Move earlier in the chain",
            disabled: place.index === 0,
            run: () => {
              void editor.moveEffect(node, place.index - 1);
            },
          },
          {
            id: "later",
            label: "Move later in the chain",
            disabled: place.index >= place.count - 1,
            run: () => {
              void editor.moveEffect(node, place.index + 1);
            },
          },
        ]
      : []),
    ...presetMenu(deps, node),
    ...viewItems(deps, node, true),
    { ...sendMenu(deps, node), rule: true },
    ...own.map((item, index) => (index === 0 ? { ...item, rule: true } : item)),
    removing("remove", `Remove ${data.name}`, () => {
      void editor.removeNode(held?.alone === true ? held.lane : node);
    }),
  ];
}

/** The menu of a modulator: whose notes it hears, and taking it out. */
function modulatorNodeMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const graph = deps.state.snapshot?.graph;
  const { editor } = deps;
  const ears = graph ? earItems(graph, node) : [];
  return [
    ...(ears.length > 0
      ? [
          {
            id: "hears",
            label: "Whose notes it hears",
            items: choosing(ears, (id) => {
              void editor.setModulatorEar(node, id === EVERY_NOTE ? null : Number(id));
            }),
          },
        ]
      : []),
    removing("remove", `Remove ${data.name}`, () => {
      void editor.removeNode(node);
    }),
  ];
}

/** The menu of a group: what its header has. */
function groupMenu(deps: MenuDeps, node: NodeId): ContextItem[] {
  return [
    ...viewItems(deps, node, true),
    { ...effectMenu(deps, node), rule: true },
    modulatorMenu(deps, node),
    sendMenu(deps, node),
    ...muteSolo(deps, node).map((item, index) => (index === 0 ? { ...item, rule: true } : item)),
    ...laneRemoval(deps, node),
  ];
}

/** The menu of a box, a module that is closed: opening it, which puts what is in it on the canvas, and taking it out with all of it. */
function moduleMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const { editor, canvas } = deps;
  return [
    {
      id: "open-module",
      label: `Open ${data.name}`,
      run: () => {
        canvas.toggleModule(node);
      },
    },
    {
      id: "ungroup-module",
      label: `Ungroup ${data.name}${data.box ? `: keep the ${boxDetail(data.box)}, take the box away` : ""}`,
      run: () => {
        void editor.ungroupModule(node);
      },
    },
    removing("remove", `Remove ${data.name} for good${data.box ? `: the ${boxDetail(data.box)} in it go with it` : ""}`, () => {
      void editor.removeNode(node);
    }),
  ];
}

/** The menu of a source of notes: sending its notes to a lane, and taking it out. */
function sourceMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const graph = deps.state.snapshot?.graph;
  const { editor } = deps;
  return [
    submenu(
      "notes",
      "Send notes to",
      graph
        ? choosing(noteToItems(graph, node), (lane) => {
            void editor.addNotesCable(node, Number(lane));
          })
        : [],
      "Every lane has a cable of notes from it",
    ),
    removing("remove", `Remove ${data.name}`, () => {
      void editor.removeNode(node);
    }),
  ];
}

/** The menu of what is chosen: a module of it, and none of it chosen. */
export function selectionMenu(deps: MenuDeps): ContextItem[] {
  const { canvas, editor } = deps;
  const { held, collapsible } = canvas;
  const nothingToCollapse = collapseWhyNot(collapsible, canvas.chosen.length);
  return [
    {
      id: "save-chosen",
      label: `Save the ${String(held.nodes.length)} chosen as a module…`,
      disabled: held.plugins === 0,
      title: held.plugins === 0 ? "A module needs an effect or an instrument" : undefined,
      run: () => {
        canvas.nameModule(SAVE_CHOSEN, (name) => editor.saveSelectionAsModule(held.nodes, name));
      },
    },
    {
      id: "collapse-chosen",
      label: `Collapse the ${nothingToCollapse === null ? collapseWords(collapsible) : "chosen nodes"} into a module…`,
      disabled: nothingToCollapse !== null,
      title: nothingToCollapse ?? undefined,
      run: () => {
        canvas.nameModule(COLLAPSE_CHOSEN, (name) => editor.collapseIntoModule(collapsible.nodes, name, canvas.collapseAt).then((box) => box !== null), COLLAPSE_NAME);
      },
    },
    {
      id: "choose-none",
      label: "Choose none",
      run: () => {
        canvas.chooseNone();
      },
    },
  ];
}

/** The menu of the canvas, where nothing is: adding to the session, going back and forward, what is chosen, and the view. */
export function paneMenu(deps: MenuDeps): ContextItem[] {
  const { state, editor, canvas, at } = deps;
  const entries = state.catalog?.entries ?? [];
  const graph = state.snapshot?.graph;
  const sources = graph ? sourceItems(graph, state.inputs) : [];
  const { undo, redo } = state.history;
  return [
    submenu(
      "add-instrument",
      "Add an instrument",
      choosing(instrumentItems(entries), (plugin) => {
        void editor.addInstrument(plugin, at);
      }),
      "No instrument is installed",
    ),
    submenu(
      "add-effect",
      "Add an effect",
      choosing(effectItems(entries), (plugin) => {
        void editor.addFreeEffect(plugin, at);
      }),
      "No effect is installed",
    ),
    {
      id: "add-modulator",
      label: "Add a modulator",
      items: choosing(MODULATOR_KINDS, (kind) => {
        void editor.addFreeModulator(kind, null, at);
      }),
    },
    submenu(
      "add-source",
      "Add a source of notes",
      sources.map((item) => ({
        ...item,
        run: () => {
          void editor.addSource(sourceChosen(item.id), at);
        },
      })),
      "The keys and every input that is open are sources already",
    ),
    submenu(
      "add-module",
      "Add a module",
      // (A module is one box on the canvas, and goes where the pointer was as anything that is added does.)
      choosing(moduleItems(entries), (module) => {
        void editor.addModule(module, at);
      }),
      "No module is saved: choose nodes and save them as a module",
    ),
    {
      id: "undo",
      label: undo === null ? "Undo" : `Undo ${undo}`,
      rule: true,
      disabled: undo === null,
      title: undo === null ? "Nothing to undo" : undefined,
      run: () => {
        void editor.undo();
      },
    },
    {
      id: "redo",
      label: redo === null ? "Redo" : `Redo ${redo}`,
      disabled: redo === null,
      title: redo === null ? "Nothing to redo" : undefined,
      run: () => {
        void editor.redo();
      },
    },
    ...(canvas.chosen.length > 0 ? selectionMenu(deps).map((item, index) => (index === 0 ? { ...item, rule: true } : item)) : []),
    {
      id: "choose-all",
      label: "Choose every node",
      rule: canvas.chosen.length === 0,
      run: () => {
        canvas.chooseAll();
      },
    },
    toggle(
      "choosing",
      "Choose nodes with a box",
      canvas.choosing,
      () => {
        canvas.toggleChoosing();
      },
      "A drag draws a box round the nodes to choose (with Shift held, it always does)",
    ),
    {
      id: "arrange",
      label: "Arrange the nodes",
      rule: true,
      disabled: !canvas.canArrange,
      title: canvas.canArrange ? undefined : "They are arranged",
      run: () => {
        canvas.arrange();
      },
    },
    {
      id: "fit",
      label: "Show every node",
      run: () => {
        canvas.fit();
      },
    },
  ];
}

/** The menu of the node `node` (`data` is what the canvas knows of it): what its header's buttons offer and what it is not room for there. Nothing for a node that is not there. */
export function nodeMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const items = ownMenu(deps, node, data);
  const tag = data.module;
  if (tag === null) return items;
  // (A lane of a module that is open can close it, first of all, with a rule after.)
  return [
    {
      id: "close-module",
      label: `Close ${tag.name}`,
      run: () => {
        deps.canvas.toggleModule(tag.node);
      },
    },
    {
      id: "ungroup-module",
      label: `Ungroup ${tag.name}`,
      run: () => {
        void deps.editor.ungroupModule(tag.node);
      },
    },
    ...items.map((item, index) => (index === 0 ? { ...item, rule: true } : item)),
  ];
}

function ownMenu(deps: MenuDeps, node: NodeId, data: PatchNodeData): ContextItem[] {
  const found = deps.state.snapshot?.graph.nodes.find((candidate) => candidate.id === node);
  switch (found?.kind) {
    case "instrument":
      return laneMenu(deps, node, data);
    case "effect":
      return effectNodeMenu(deps, node, data);
    case "modulator":
      return modulatorNodeMenu(deps, node, data);
    case "group":
      return groupMenu(deps, node);
    case "source":
      return sourceMenu(deps, node, data);
    case "module":
      return moduleMenu(deps, node, data);
    case "master": {
      const { editor } = deps;
      const on = limiterOf(deps.state) === true;
      return [
        toggle("limiter", "Limiter", on, () => {
          void editor.setLimiter(!on);
        }),
      ];
    }
    default:
      return [];
  }
}

/** The menu of a cable: taking it out, as the button at its middle does (a link of the audio path is cut). Nothing for what has no button: the links by which a lane hears every source. */
export function edgeMenu(deps: MenuDeps, edge: PatchEdge): ContextItem[] {
  const { editor } = deps;
  const label = edge.data?.label ?? "a cable";
  const cable = edge.data?.cable;
  const owner = edge.data?.wayOut;
  const fed = edge.data?.feedOf;
  if (cable !== undefined) {
    return [
      removing("remove", `Remove cable ${label}`, () => {
        void editor.removeCable(cable);
      }),
    ];
  }
  if (owner !== undefined) {
    return [
      removing("remove", `Remove cable ${label}`, () => {
        void editor.setRoute(owner, "none");
      }),
    ];
  }
  if (fed !== undefined) {
    return [
      removing("remove", `Remove cable ${label}`, () => {
        void editor.setFeed(fed, "none");
      }),
    ];
  }
  return [];
}

/** The menu that a right click on a cable opens: its own, named for the cable, or the canvas's where there is nothing to do to it (the links by which a lane hears every source). */
export function edgeSpec(deps: MenuDeps, edge: PatchEdge): MenuSpec {
  const items = edgeMenu(deps, edge);
  return items.length > 0 ? { label: edge.data?.label ?? "a cable", items } : { label: "the canvas", items: paneMenu(deps) };
}

/** The menu that a right click on a node opens: the menu of what is chosen, for a node that is one of several that are, and the node's own otherwise (a node that is not chosen, or the only one that is). */
export function nodeSpec(deps: MenuDeps, node: PatchNode): MenuSpec {
  const chosen = deps.canvas.chosen.length;
  if (node.selected === true && chosen > 1) return { label: `the ${String(chosen)} chosen nodes`, items: selectionMenu(deps) };
  return { label: node.data.name, items: nodeMenu(deps, node.data.node, node.data) };
}
