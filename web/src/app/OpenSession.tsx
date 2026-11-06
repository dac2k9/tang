// "Open…", in the bar of the session (step 115: "load a sound from within the UI, i.e. load a toml"): a button for a program that can open another session while it runs: the hello says `openSession` (a plugin in a host; a program that is
// started with its file has no such button). With no library of sounds to list (the hello lists none) it asks for a session file with the browser's own chooser (the web view of a plugin's window has one too), reads it here, and sends its text
// (`Editor.openSessionFile`); with some (step 116) it opens the picker (`SoundPicker`), which lists them, with the chooser among its own. The server says what is wrong with a file that is not a session, among the notices. It is not undone: the
// session that is open goes when the program has made its engine again with the new one, and the page is served by that.
import { useRef, useState } from "react";
import type { ChangeEvent } from "react";

import { canOpenSession, soundLibraries } from "../client/selectors";
import { useClientState, useEditor } from "./ClientProvider";
import { SoundPicker } from "./SoundPicker";

export function OpenSession() {
  const editor = useEditor();
  const can = useClientState(canOpenSession);
  const libraries = useClientState(soundLibraries);
  const [opening, setOpening] = useState(false);
  const [picking, setPicking] = useState(false);
  const chooser = useRef<HTMLInputElement>(null);
  if (!can) return null;

  const chosen = (event: ChangeEvent<HTMLInputElement>): void => {
    const input = event.currentTarget;
    const file = input.files?.[0];
    // (Emptied, so that the same file can be chosen again: a session that was changed since is not the one it was.)
    input.value = "";
    if (!file) return;
    setOpening(true);
    void editor.openSessionFile(file).finally(() => {
      setOpening(false);
    });
  };

  const listed = libraries.length > 0;
  return (
    <>
      <button
        type="button"
        className="tang-session-bar__button"
        disabled={opening}
        title={
          listed
            ? "Open a sound: a session file from the libraries that Tang lists, or from this computer. What is here is not kept"
            : "Open a session file (.toml) in place of this session: what is here is not kept"
        }
        onClick={() => {
          if (listed) setPicking(true);
          else chooser.current?.click();
        }}
      >
        Open…
      </button>
      <input ref={chooser} type="file" accept=".toml,text/plain" hidden aria-label="Session file" onChange={chosen} />
      {picking ? (
        <SoundPicker
          onClose={() => {
            setPicking(false);
          }}
        />
      ) : null}
    </>
  );
}
