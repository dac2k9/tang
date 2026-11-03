// The layout of the canvas, kept from one change of the session to the next (docs/web-ui.md §9.3): `toFlow` is given where it put the nodes last,
// so that a cable that is drawn, or a node that is added or taken out, does not rearrange the canvas under a person's hands, and it is made from
// nothing, every node where the layout has it, when the person asks for the nodes to be arranged again. That layout is the arrangement that makes the cables short (`arrange.ts`, step 98),
// which is worked out once, when the last of the nodes that a person had placed has been given back (or at once, if none was placed), and not every time the session changes.
import { useCallback, useState } from "react";

import type { Graph, NodeId } from "../../protocol";
import { toFlow } from "./toFlow";
import type { Flow } from "./toFlow";

/** What the canvas draws before the session has come. */
const NO_FLOW: Flow = { nodes: [], edges: [], places: new Map(), arranged: true };

/** No module is open: every module is a box. */
const NONE_OPEN: ReadonlySet<NodeId> = new Set();

interface Laid {
  /** The session that `flow` is the layout of. */
  graph: Graph | null;
  /** The nodes that showed a scope when it was laid out (the viewer's own: they are taller). */
  scopes: ReadonlySet<NodeId>;
  /** The modules that were open when it was laid out (the viewer's own: an open module is its nodes, and a closed one is a box). */
  open: ReadonlySet<NodeId>;
  flow: Flow;
  /**
   * The nodes that a person placed are being given back to the layout, one answer of the server after the other: until the last has been, the layout is made from nothing
   * each time (a node that the layout had to move clear of one that was placed would stay where it was moved to, and the nodes would not be arranged when the last is given back).
   */
  arranging: boolean;
  /**
   * `flow` is the arrangement that makes the cables short, as it was made, and the session has not changed since (a session that is opened is laid out in rows, and a change of its structure leaves the nodes where they
   * were); or there is no session to arrange.
   */
  optimized: boolean;
}

/** Whether a person has placed any node of the session. */
const anyPlaced = (graph: Graph | null): boolean => graph?.nodes.some((node) => node.ui !== undefined) ?? false;

export interface Layout {
  flow: Flow;
  /** The nodes that a person had placed are being given back to the layout: what is laid out now is not what they will be when the last is (`Laid.arranging`). */
  arranging: boolean;
  /** The nodes are where the arrangement that makes the cables short put them and the session has not changed since, or there is no session (`Laid.optimized`): there is nothing for the button to do. */
  optimized: boolean;
  /** Lay the nodes out from nothing: every node that is not placed by a person goes where the layout puts it. */
  arrangeAgain: () => void;
}

/**
 * The layout of `graph` (which is the same object while the session's structure is the same: what is laid out is not the values), with room for the scopes that the nodes in `scopes`
 * show (the same object while they are the same nodes), and with the modules in `open` as their nodes and the others as boxes (step 108; the same object while they are the same modules).
 */
export function useLayout(graph: Graph | null, scopes: ReadonlySet<NodeId>, open: ReadonlySet<NodeId> = NONE_OPEN): Layout {
  const [laid, setLaid] = useState<Laid>({ graph: null, scopes, open, flow: NO_FLOW, arranging: false, optimized: true });
  let current = laid;
  // (A layout that is made when the session changes, or a scope is shown or taken away, or a module is opened or closed, in the render that sees it: there is no frame that draws the new session with the old layout.)
  if (laid.graph !== graph || laid.scopes !== scopes || laid.open !== open) {
    // (While the places that were given are being given back the nodes are laid out in rows, one answer of the server after the other, and arranged when the last has been: `arrange` is for that one.)
    const arrange = laid.arranging && !anyPlaced(graph);
    const flow = graph ? toFlow(graph, laid.arranging ? undefined : laid.flow.places, scopes, { arrange, open }) : NO_FLOW;
    current = { graph, scopes, open, flow, arranging: laid.arranging && anyPlaced(graph), optimized: graph === null || arrange };
    setLaid(current);
  }
  const arrangeAgain = useCallback(() => {
    setLaid((now) => {
      const placed = anyPlaced(now.graph);
      // (With nodes placed, the rows' layout is what is shown until the last of them has been given back, and it is arranged then.)
      return { graph: now.graph, scopes: now.scopes, open: now.open, flow: now.graph ? toFlow(now.graph, undefined, now.scopes, { arrange: !placed, open: now.open }) : NO_FLOW, arranging: placed, optimized: !placed };
    });
  }, []);
  return { flow: current.flow, arranging: current.arranging, optimized: current.optimized, arrangeAgain };
}
