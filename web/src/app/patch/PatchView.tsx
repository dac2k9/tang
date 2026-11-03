// The canvas (docs/web-ui.md §9.3): the session as nodes and cables, drawn by React Flow.
// What can be done on the canvas is to turn the controls on the nodes, to add a modulator to a
// lane or a group, to take a modulator or a cable out, to draw a cable by dragging it from a
// modulator's output to a parameter's port, or an audio cable from the output of a lane, an effect or a
// group to an input of audio, or a cable of notes from a source to a lane, to turn the level of an audio cable, and to move a node by dragging it (by what is not a
// control): where it is put down goes to the server, which keeps it with the session (§4.4), and
// every browser draws the node there. The nodes that are not moved are where the layout puts them, and stay there when the session
// changes round them (a cable drawn, a node added or taken out: `toFlow` is given where it put them last) until the person asks for
// the nodes to be arranged again. (Step 105: a button turns choosing on, and what is chosen, the plugins in it, is saved as a module: see `selection.ts`. Step 107: a box drawn with Shift held chooses at any time, a click
// with Shift or Control or Command held adds a node to what is chosen, a node that is chosen is moved with the others, and a right click opens a menu of what is under the pointer: see `contextMenus.ts`.)
import { Background, Controls, Panel, ReactFlow, useKeyPress, useReactFlow, useStore } from "@xyflow/react";
import type { Connection, Edge, EdgeTypes, NodeChange, NodeTypes, OnConnectStart, ReactFlowInstance } from "@xyflow/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent } from "react";

import { takesText } from "../../client/keyboard";
import type { Graph, NodeId, NodeUi } from "../../protocol";
import { useClient, useClientSelection, useEditor, useReveals } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { canConnect, ConnectingContext, drawCable, kindOfDrag, takersOf } from "./connection";
import type { CableKind } from "./connection";
import { ContextMenuHost, useContextMenu } from "./contextMenu";
import { edgeSpec, nodeSpec, paneMenu } from "./contextMenus";
import type { CanvasActions, MenuDeps } from "./contextMenus";
import { AudioEdge, HearsEdge, ModulationEdge, NotesEdge, SendEdge } from "./edges";
import { InspectorContext, paramsButtonId } from "./inspector";
import type { Inspector } from "./inspector";
import { ModuleNode } from "./ModuleBox";
import { GroupNode, MasterNode, ModulatorNode, PluginNode, SourceNode } from "./nodes";
import { OpenModulesContext } from "./openModules";
import type { OpenModules } from "./openModules";
import { ParamPanel } from "./ParamPanel";
import { dialogPlace, ModuleNameDialog, SaveSelectionButton } from "./SaveModule";
import type { DialogPlace } from "./SaveModule";
import { modulePluginsOf } from "./selection";
import { ScopesContext } from "./scopes";
import type { Scopes } from "./scopes";
import { StructureContext } from "./structure";
import { sameGraphStructure } from "./toFlow";
import type { Flow, PatchEdge, PatchNode } from "./toFlow";
import { useLayout } from "./useLayout";
import { useStuckDragGuard } from "./stuckDrag";
import { useMovableNodes } from "./useMovableNodes";
import { rectOf, viewToShow } from "./view";
import type { View } from "./view";
import "@xyflow/react/dist/style.css";
import "./patch.css";

// (Outside the component, so that they are the same objects every time.)
const NODE_TYPES: NodeTypes = {
  instrument: PluginNode,
  effect: PluginNode,
  modulator: ModulatorNode,
  group: GroupNode,
  master: MasterNode,
  source: SourceNode,
  module: ModuleNode,
};

/** The keys that add a node to the ones that are chosen when it is clicked (step 105): Shift, which a person who has a box to draw round nodes expects it of, and the keys that React Flow has for it, Control and Command. A node is chosen by a click only while one is held (or choosing is on). */
const MULTI_SELECTION_KEYS = ["Shift", "Meta", "Control"];

const EDGE_TYPES: EdgeTypes = { audio: AudioEdge, modulation: ModulationEdge, send: SendEdge, notes: NotesEdge, hears: HearsEdge };

/**
 * The cable that is being dragged: the ports that take it, for them to say so, and whether it is audio, notes or modulation, which the line of the cable is drawn by (in the stylesheet, by the
 * `data-connecting` of the canvas: a line of modulation is magenta and dashed, one of audio is amber and plain and one of notes is violet and plain, as the cables are once they are drawn).
 */
interface Dragged {
  takers: ReadonlySet<string>;
  kind: CableKind;
}

/** How long a node the page added is waited for: the layout that has it comes with the snapshot, which is as good as at once. */
const SHOW_WITHIN_MS = 3000;

/** How long the canvas takes to move to a node. */
const MOVE_MS = 250;

/** The room that the canvas leaves round the nodes when it fits them in the view (a share of the view). */
const FIT_PADDING = 0.12;

/** How far (in pixels) the pointer has to go with a node before it is a drag: a click on a node's header is not a move. */
const DRAG_THRESHOLD = 3;

/**
 * Moves the canvas to a node the page has just added, if it is not all in the view (a lane goes below the others, an effect
 * after the last of its row): the page asks for it through `Reveals` once the session has the node, and the layout may come a
 * moment later. It is only for what the page itself added: a node somebody else adds is not worth the canvas moving under a
 * person who is turning a knob. Draws nothing; it is in the canvas for the instance of it (`useReactFlow`).
 */
function ShowWhatWasAdded({ graph, flow }: { graph: Graph | null; flow: Flow }) {
  const reveals = useReveals();
  const canvas = useReactFlow();
  const width = useStore((state) => state.width);
  const height = useStore((state) => state.height);
  const minZoom = useStore((state) => state.minZoom);
  const latest = useRef({ graph, flow });
  const wanted = useRef<{ node: NodeId; until: number } | null>(null);
  // Where the canvas is on its way to, until it has had the time to get there: where it looks meanwhile is a point on the way, and a
  // node that is all in the view there may be out of the view the canvas ends in (a node that moved while it was going).
  const going = useRef<{ view: View; until: number } | null>(null);

  const show = useCallback(() => {
    const want = wanted.current;
    if (!want) return;
    const now = Date.now();
    if (now > want.until) {
      wanted.current = null;
      return;
    }
    const { graph: current, flow: laidOut } = latest.current;
    const rect = current ? rectOf(current, laidOut, want.node) : null;
    // (Not laid out yet: when the next layout is.)
    if (!rect) return;
    wanted.current = null;
    const moving = going.current;
    const looking = moving && now < moving.until ? moving.view : canvas.getViewport();
    const next = viewToShow(rect, looking, { width, height }, minZoom);
    if (!next) return;
    going.current = { view: next, until: now + MOVE_MS };
    void canvas.setViewport(next, { duration: MOVE_MS });
  }, [canvas, width, height, minZoom]);

  useEffect(() => {
    latest.current = { graph, flow };
    show();
  }, [graph, flow, show]);

  useEffect(
    () =>
      reveals.subscribe((node) => {
        wanted.current = { node, until: Date.now() + SHOW_WITHIN_MS };
        show();
      }),
    [reveals, show],
  );
  return null;
}

/**
 * Moves the canvas to show every node when the nodes have been arranged again (a layout puts them where they were not, and the strips are taller than what was in the view), once
 * the nodes are where they will be: not while the places that were given are being given back. `requests` counts the times the nodes were asked to be arranged. Draws nothing; it is in
 * the canvas for the instance of it (`useReactFlow`).
 */
function FitWhenArranged({ requests, arranging }: { requests: number; arranging: boolean }) {
  const canvas = useReactFlow();
  const shown = useRef(0);
  useEffect(() => {
    if (arranging || requests === shown.current) return;
    shown.current = requests;
    void canvas.fitView({ padding: FIT_PADDING, maxZoom: 1, duration: MOVE_MS });
  }, [canvas, requests, arranging]);
  return null;
}

function PatchCanvas() {
  // The canvas is laid out again when something other than a value changed.
  const graph = useClientSelection((state) => state.snapshot?.graph ?? null, sameGraphStructure);
  // The nodes that show a scope of what comes out of them: the viewer's own, which the layout makes room for (a node that cannot show one is not drawn with it, and one that is not there is nothing).
  const [shown, setShown] = useState<ReadonlySet<NodeId>>(() => new Set());
  // The node whose scope has just been opened: the canvas moves to show it once the layout has made room for the scope, if the scope is out of the view.
  const opened = useRef<NodeId | null>(null);
  const scopes = useMemo<Scopes>(
    () => ({
      open: shown,
      toggle: (node) => {
        if (!shown.has(node)) opened.current = node;
        setShown((now) => {
          const next = new Set(now);
          if (!next.delete(node)) next.add(node);
          return next;
        });
      },
    }),
    [shown],
  );
  // The modules that are open, which are their nodes on the canvas (a module that is not is one box): the viewer's own, like the scopes, and what the layout is made for.
  const [openModules, setOpenModules] = useState<ReadonlySet<NodeId>>(() => new Set());
  // What the canvas moves to once the layout has what a module was opened or closed to: the first of its lanes that is on the canvas, or the box.
  const toShow = useRef<{ opened: NodeId } | { closed: NodeId } | null>(null);
  const modules = useMemo<OpenModules>(
    () => ({
      open: openModules,
      toggle: (node) => {
        toShow.current = openModules.has(node) ? { closed: node } : { opened: node };
        setOpenModules((now) => {
          const next = new Set(now);
          if (!next.delete(node)) next.add(node);
          return next;
        });
      },
    }),
    [openModules],
  );
  const { flow, arranging, optimized, arrangeAgain } = useLayout(graph, shown, openModules);
  const reveals = useReveals();
  // (After the commit that has the nodes of the module, or its box, on the canvas: the canvas moves to show them.)
  useEffect(() => {
    const want = toShow.current;
    if (want === null) return;
    toShow.current = null;
    if ("closed" in want) {
      reveals.show(want.closed);
      return;
    }
    const first = flow.nodes.find((node) => node.data.module?.node === want.opened);
    if (first) reveals.show(first.data.node);
  }, [flow, reveals]);
  // (After the commit that has the node as tall as it is with its scope: the canvas's own effects, which keep the layout it moves by, run before this one, which is the parent's.)
  useEffect(() => {
    const node = opened.current;
    if (node === null) return;
    opened.current = null;
    reveals.show(node);
  }, [flow, reveals]);
  // How many times the nodes have been asked to be arranged again, for the canvas to be moved to show them.
  const [arrangements, setArrangements] = useState(0);
  const editor = useEditor();
  const client = useClient();
  const openMenu = useContextMenu();
  // (The button has nothing to do when the nodes are where the arrangement that makes the cables short put them and the session has not changed since: `optimized`, which a session as it was opened, one that has changed and one in which a person has placed a node are not.)
  const arrange = useCallback(() => {
    arrangeAgain();
    setArrangements((count) => count + 1);
    // (If one is refused the page says so: `Editor` puts the reason among the notices.)
    void editor.resetLayout();
  }, [arrangeAgain, editor]);
  // The nodes as they are on the canvas: the layout's, but where a person is dragging one it is where the pointer has taken it.
  const moveNode = useCallback((node: NodeId, ui: NodeUi) => editor.moveNode(node, ui), [editor]);
  const { nodes, onNodesChange, onNodeDragStart, onNodeDragStop } = useMovableNodes(flow.nodes, moveNode);
  // Choosing nodes to make a module of (steps 105 and 107). A node is chosen by a box drawn on the canvas while Shift is held (or whenever, while the button has choosing on), and by a click while Shift, Control or Command is held (or
  // while choosing is on): a plain click chooses nothing, since a click on a knob is a click on the node it is in, so that the canvas can be chosen from only while one of those is held (`elementsSelectable`: React Flow marks a node
  // chosen as soon as it is clicked, before what it tells the page can be looked at). A node that is chosen is moved with the others (whether or not choosing is on: a node is draggable by its own word, `toFlow`'s `draggable`, which is what `nodesDraggable` gives way to); the cables are none of what is chosen.
  const [choosing, setChoosing] = useState(false);
  const adding = useKeyPress(MULTI_SELECTION_KEYS);
  // Whether a box is being drawn: it goes on being a box when Shift is let go of before the button is.
  const [boxing, setBoxing] = useState(false);
  const chosen = useMemo(() => nodes.filter((node) => node.selected === true).map((node) => node.data.node), [nodes]);
  const held = useMemo(() => modulePluginsOf(graph, chosen), [graph, chosen]);
  const forgetChosen = useCallback(() => {
    const changes = nodes.filter((node) => node.selected === true).map((node): NodeChange<PatchNode> => ({ id: node.id, type: "select", selected: false }));
    if (changes.length > 0) onNodesChange(changes);
  }, [nodes, onNodesChange]);
  const chooseAll = useCallback(() => {
    const changes = nodes.filter((node) => node.selected !== true).map((node): NodeChange<PatchNode> => ({ id: node.id, type: "select", selected: true }));
    if (changes.length > 0) onNodesChange(changes);
  }, [nodes, onNodesChange]);
  const toggleChoosing = useCallback(() => {
    if (choosing) forgetChosen();
    setChoosing(!choosing);
  }, [choosing, forgetChosen]);
  // (A box is done when the button is let go of, whatever happened to it: the next click is a click. Not at once: React Flow ends the box with the same event, and must find that choosing is still on to do it.)
  useEffect(() => {
    const done = (): void => {
      window.setTimeout(() => {
        setBoxing(false);
      }, 0);
    };
    window.addEventListener("pointerup", done, true);
    window.addEventListener("pointercancel", done, true);
    return () => {
      window.removeEventListener("pointerup", done, true);
      window.removeEventListener("pointercancel", done, true);
    };
  }, []);
  // (Escape leaves it, and gives up what is chosen, unless something that is open took the key: a menu closes with it.)
  const anyChosen = chosen.length > 0;
  useEffect(() => {
    if (!choosing && !anyChosen) return;
    const leave = (event: KeyboardEvent): void => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      forgetChosen();
      setChoosing(false);
    };
    window.addEventListener("keydown", leave);
    return () => {
      window.removeEventListener("keydown", leave);
    };
  }, [choosing, anyChosen, forgetChosen]);
  // (A cable is not chosen, and a click on one is as it was: it is none of what a module is made of.)
  const edges = useMemo(() => flow.edges.map((edge) => ({ ...edge, selectable: false })), [flow.edges]);
  // (A card that stays in hand when the release of the mouse was lost is let go of.)
  useStuckDragGuard();
  // The ports that take the cable being dragged, for them to say so.
  const [connecting, setConnecting] = useState<Dragged | null>(null);
  // The plugin whose parameters are in the panel, if the panel is open.
  const [inspected, setInspected] = useState<NodeId | null>(null);
  const inspector = useMemo<Inspector>(
    () => ({
      inspected,
      toggle: (node) => {
        setInspected((now) => (now === node ? null : node));
      },
    }),
    [inspected],
  );
  const closePanel = useCallback(
    (focusBack: boolean) => {
      setInspected(null);
      if (focusBack && inspected !== null) focusWhenPossible(paramsButtonId(inspected));
    },
    [inspected],
  );

  // The menus (step 107). The canvas that React Flow made, to turn a place on the screen into a place on the canvas and to show every node; where the pointer was when the menu opened, for the field that a menu opens to be there.
  const canvasRef = useRef<Pick<ReactFlowInstance<PatchNode>, "screenToFlowPosition" | "fitView"> | null>(null);
  const menuAt = useRef({ x: 0, y: 0 });
  const [naming, setNaming] = useState<{ label: string; at: DialogPlace; save: (name: string) => Promise<boolean> } | null>(null);
  const closeNaming = useCallback(() => {
    setNaming(null);
  }, []);
  const canvasActions = useMemo<CanvasActions>(
    () => ({
      inspecting: (node) => inspected === node,
      inspect: (node) => {
        inspector.toggle(node);
      },
      scoped: (node) => shown.has(node),
      scope: (node) => {
        scopes.toggle(node);
      },
      choosing,
      toggleChoosing,
      chosen,
      held,
      chooseAll,
      chooseNone: forgetChosen,
      nameModule: (label, save) => {
        setNaming({ label, at: dialogPlace(menuAt.current.x, menuAt.current.y), save });
      },
      arrange,
      canArrange: !(flow.arranged && optimized),
      fit: () => {
        void canvasRef.current?.fitView({ padding: FIT_PADDING, maxZoom: 1, duration: MOVE_MS });
      },
      toggleModule: modules.toggle,
    }),
    [inspected, inspector, shown, scopes, choosing, toggleChoosing, chosen, held, chooseAll, forgetChosen, arrange, flow.arranged, optimized, modules],
  );
  /** What a menu opened by `event` is made from: the session as it is now, and the place on the canvas that the pointer was at. */
  const depsAt = useCallback(
    (event: { clientX: number; clientY: number }): MenuDeps => {
      menuAt.current = { x: event.clientX, y: event.clientY };
      const at = canvasRef.current?.screenToFlowPosition({ x: event.clientX, y: event.clientY });
      return { state: client.state, editor, canvas: canvasActions, ...(at ? { at } : {}) };
    },
    [client, editor, canvasActions],
  );
  const onPaneContextMenu = useCallback(
    (event: ReactMouseEvent | MouseEvent) => {
      openMenu(event, { label: "the canvas", items: paneMenu(depsAt(event)) });
    },
    [openMenu, depsAt],
  );
  const onNodeContextMenu = useCallback(
    (event: ReactMouseEvent, node: PatchNode) => {
      // (A field has its own menu, which cuts and pastes.)
      if (takesText(event.target)) return;
      openMenu(event, nodeSpec(depsAt(event), node));
    },
    [openMenu, depsAt],
  );
  const onEdgeContextMenu = useCallback(
    (event: ReactMouseEvent, edge: PatchEdge) => {
      openMenu(event, edgeSpec(depsAt(event), edge));
    },
    [openMenu, depsAt],
  );

  const onConnectStart = useCallback<OnConnectStart>(
    (_event, start) => {
      const takers = takersOf(graph, flow.nodes, start);
      setConnecting(takers ? { takers, kind: kindOfDrag(start.handleId) } : null);
    },
    [graph, flow.nodes],
  );
  const onConnectEnd = useCallback(() => {
    setConnecting(null);
  }, []);
  const isValidConnection = useCallback((connection: Connection | Edge) => canConnect(graph, flow.nodes, connection), [graph, flow.nodes]);
  const onConnect = useCallback(
    (connection: Connection) => {
      // (If it is refused the page says so: `Editor` puts the reason among the notices.)
      drawCable(editor, connection);
    },
    [editor],
  );

  return (
    <div className="tang-patch" data-testid="patch" data-connecting={connecting?.kind}>
      <InspectorContext.Provider value={inspector}>
        <ScopesContext.Provider value={scopes}>
          <OpenModulesContext.Provider value={modules}>
            <div className="tang-patch__canvas">
              <StructureContext.Provider value={graph}>
                <ConnectingContext.Provider value={connecting?.takers ?? null}>
                  <ReactFlow
                    nodes={nodes}
                    edges={edges}
                    onInit={(instance) => {
                      canvasRef.current = instance;
                    }}
                    onNodesChange={onNodesChange}
                    onSelectionStart={() => {
                      setBoxing(true);
                    }}
                    onPaneClick={forgetChosen}
                    onPaneContextMenu={onPaneContextMenu}
                    onNodeContextMenu={onNodeContextMenu}
                    onEdgeContextMenu={onEdgeContextMenu}
                    onNodeDragStart={onNodeDragStart}
                    onNodeDragStop={onNodeDragStop}
                    nodeDragThreshold={DRAG_THRESHOLD}
                    nodeTypes={NODE_TYPES}
                    edgeTypes={EDGE_TYPES}
                    colorMode="dark"
                    fitView
                    fitViewOptions={{ padding: FIT_PADDING, maxZoom: 1 }}
                    minZoom={0.2}
                    maxZoom={1.5}
                    connectionRadius={40}
                    isValidConnection={isValidConnection}
                    onConnectStart={onConnectStart}
                    onConnectEnd={onConnectEnd}
                    onConnect={onConnect}
                    edgesFocusable={false}
                    elementsSelectable={choosing || adding || boxing}
                    selectionOnDrag={choosing}
                    selectNodesOnDrag={false}
                    multiSelectionKeyCode={MULTI_SELECTION_KEYS}
                    panOnDrag={choosing ? [1] : true}
                    deleteKeyCode={null}
                  >
                    <Background gap={24} />
                    <Controls showInteractive={false} />
                    <Panel position="top-right" className="tang-patch__tools">
                      {choosing || anyChosen ? (
                        <>
                          <span className="tang-patch__chosen" role="status">
                            {chosen.length === 0
                              ? "Drag a box round the nodes to keep"
                              : `${String(held.nodes.length)} chosen${held.left > 0 ? `, ${String(held.left)} that a module cannot hold left out` : ""}${held.nodes.length > 0 && held.plugins === 0 ? ", and a module needs an effect or an instrument" : ""}`}
                          </span>
                          <SaveSelectionButton nodes={held.nodes} plugins={held.plugins} />
                        </>
                      ) : null}
                      <button
                        type="button"
                        className="tang-patch__arrange"
                        title="Choose nodes, with a box drawn round them or a click, to save them as a module with the cables that are between them (a box drawn with Shift held chooses at any time, and a click with Shift held adds a node)"
                        aria-label="Choose nodes to save as a module"
                        aria-pressed={choosing}
                        onClick={toggleChoosing}
                      >
                        Select
                      </button>
                      <button
                        type="button"
                        className="tang-patch__arrange"
                        title="Put every node where the cables between them are short, and give back the places that were moved"
                        aria-label="Arrange the nodes again"
                        disabled={flow.arranged && optimized}
                        onClick={arrange}
                      >
                        Arrange
                      </button>
                    </Panel>
                    <ShowWhatWasAdded graph={graph} flow={flow} />
                    <FitWhenArranged requests={arrangements} arranging={arranging} />
                  </ReactFlow>
                </ConnectingContext.Provider>
              </StructureContext.Provider>
            </div>
          </OpenModulesContext.Provider>
        </ScopesContext.Provider>
        {inspected !== null ? <ParamPanel key={inspected} node={inspected} onClose={closePanel} /> : null}
        {naming ? <ModuleNameDialog label={naming.label} at={naming.at} save={naming.save} onClose={closeNaming} /> : null}
      </InspectorContext.Provider>
    </div>
  );
}

/** The canvas, with the menus that a right click opens on it. */
export function PatchView() {
  return (
    <ContextMenuHost>
      <PatchCanvas />
    </ContextMenuHost>
  );
}
