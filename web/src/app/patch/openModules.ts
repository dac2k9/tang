// Which modules are open (docs/web-ui.md §9.3, step 108): a module that is added is one box, and its nodes are on the canvas only while it is open. That is the viewer's own choice, like which node shows its
// scope or whose parameters are in the panel, and not part of the session: the canvas holds it (it lays the nodes out for it) and hands the nodes this to open a module or close it.
import { createContext } from "react";

import type { NodeId } from "../../protocol";

export interface OpenModules {
  /** The modules that are open. */
  open: ReadonlySet<NodeId>;
  /** Open the module `node` if it is a box, and close it if it is open. */
  toggle: (node: NodeId) => void;
}

/** What a node asks the canvas for. (Where there is no canvas to hold them, the buttons of a box and the tags of a lane do nothing.) */
export const OpenModulesContext = createContext<OpenModules>({ open: new Set(), toggle: () => undefined });

/** The `id` of the button of the box `node` that opens it. */
export const openModuleId = (node: NodeId): string => `open-module-${String(node)}`;

/** The `id` of the tag over a lane of the module `node`, which is open, that closes it. */
export const closeModuleId = (node: NodeId): string => `close-module-${String(node)}`;
