// The button in the bar of the page that adds a module (step 111; the user's "move modules to its own category as they are now under effects"): a menu of the modules that a person saved, the library
// that the server lists in its catalog (steps 104 to 106). A module goes in as everything that was saved in it, in one step, and the canvas draws it as one box (step 108). With none saved the button is shut, as the
// bar's other buttons are when there is nothing for them to add.
import { useMemo } from "react";

import { moduleItems } from "../client/catalog";
import { MenuButton } from "../ui/MenuButton";
import { useClientState, useEditor } from "./ClientProvider";

export function AddModuleMenu() {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => moduleItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id="add-module"
      label="Add a module"
      className="tang-app__add-instrument"
      items={items}
      onChoose={(module) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addModule(module);
      }}
    >
      + Module
    </MenuButton>
  );
}
