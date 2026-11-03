// The tag over a lane of a module that is open (docs/web-ui.md §9.3, step 108): the nodes of an open module are on the canvas as any are, and this is what says which they are, and closes the module again.
import { useContext } from "react";

import { closeModuleId, OpenModulesContext } from "./openModules";
import type { ModuleTag } from "./toFlow";

/**
 * The tag over a lane of a module that is open (or over the first effect of a lane that has no card): the module's name, which closes it again, so that it is drawn as one box. It is over the top edge of
 * the card, at its left, where the level of a lane is under it.
 */
export function ModuleTagButton({ tag }: { tag: ModuleTag }) {
  const modules = useContext(OpenModulesContext);
  return (
    <button
      type="button"
      id={closeModuleId(tag.node)}
      className="tang-node__module-tag nodrag nopan"
      aria-label={`Close ${tag.name}`}
      title={`${tag.name} is open: close it to draw it as one box`}
      aria-expanded
      onClick={() => {
        modules.toggle(tag.node);
      }}
    >
      ▾ {tag.name}
    </button>
  );
}
