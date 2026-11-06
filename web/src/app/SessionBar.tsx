// The session as a whole, in the bar at the top of the page: the file it is saved to, whether
// there is anything in it that is not saved, the buttons that undo and redo (`UndoRedo`), its tempo (`Tempo`), the button
// (and the keys: Control+S, or Command+S on a Mac) that saves it, and the one that saves it under another
// name (`SaveAs`), and, for a program that can open another session while it runs, the one that opens a file the person picks (`OpenSession`). The file is the server's own: a page names a file, and the server says where it goes.
import { useCallback, useEffect, useRef, useState } from "react";

import { useClient, useClientState, useEditor } from "./ClientProvider";
import { OpenSession } from "./OpenSession";
import { SaveAs } from "./SaveAs";
import "./SessionBar.css";
import { Tempo } from "./Tempo";
import { UndoRedo } from "./UndoRedo";

/** Whether `event` is the browser's own "save this page" chord, which here saves the session: Control or Command with S, and nothing else. */
const isSaveChord = (event: KeyboardEvent): boolean => event.key.toLowerCase() === "s" && (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey;

export function SessionBar() {
  const client = useClient();
  const editor = useEditor();
  const meta = useClientState((state) => state.snapshot?.meta);
  const [saving, setSaving] = useState(false);
  const busy = useRef(false);

  // Asks only for what there is to save, and for one save at a time. (The state is read when the key is pressed, not when it was drawn.)
  const save = useCallback((): void => {
    if (busy.current || client.state.snapshot?.meta.dirty !== true) return;
    busy.current = true;
    setSaving(true);
    void editor.save().finally(() => {
      busy.current = false;
      setSaving(false);
    });
  }, [client, editor]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (!isSaveChord(event)) return;
      // (The browser's own would write the page to a file.)
      event.preventDefault();
      save();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [save]);

  if (!meta) return null;
  return (
    <div className="tang-session-bar">
      <span className="tang-session-bar__file" title="The file the session is saved to">
        {meta.file ?? "no file"}
      </span>
      <span className={meta.dirty ? "tang-session-bar__state tang-session-bar__state--unsaved" : "tang-session-bar__state"} aria-live="polite">
        {meta.dirty ? "Unsaved changes" : "Saved"}
      </span>
      <UndoRedo />
      <Tempo />
      <button type="button" className="tang-session-bar__button" disabled={!meta.dirty || saving} title="Save the session to its file (Control+S, or Command+S)" onClick={save}>
        Save
      </button>
      <SaveAs />
      <OpenSession />
    </div>
  );
}
