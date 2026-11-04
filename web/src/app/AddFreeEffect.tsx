// The button in the bar of the page that adds an effect that stands by itself: a menu of what is installed and can be an effect, under the format of each. (The modules that a person saved are not in it since step 111:
// they have a button of their own, `AddModule.tsx`.) The effect goes in a lane of its own at the
// end of the session that has no instrument and no card on the canvas (it is a module like any other: the audio that it is fed is what cables bring to it, and where it goes is a link or a cable
// from it); what the canvas calls it, and where it is, is the canvas's business.
import { useMemo } from "react";

import { effectItems } from "../client/catalog";
import { MenuButton } from "../ui/MenuButton";
import { useClientState, useEditor } from "./ClientProvider";
import { ADD_FREE_EFFECT_ID } from "./patch/nodes";

export function AddFreeEffectMenu() {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => effectItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id={ADD_FREE_EFFECT_ID}
      label="Add an effect"
      className="tang-app__add-instrument"
      items={items}
      onChoose={(plugin) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addFreeEffect(plugin);
      }}
    >
      + Effect
    </MenuButton>
  );
}
