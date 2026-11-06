// "Open…", in the bar of the session (step 115: "load a sound from within the UI, i.e. load a toml"): a button that asks for a session file with the browser's own chooser (the web view of a plugin's window has one too), reads it
// here, and sends its text (`Editor.openSession`), for a program that can open another session while it runs: the hello says `openSession` (a plugin in a host; a program that is started with its file has no such button). The server
// says what is wrong with a file that is not a session, among the notices. It is not undone: the session that is open goes when the program has made its engine again with the new one, and the page is served by that.
import { useRef, useState } from "react";
import type { ChangeEvent } from "react";

import { canOpenSession } from "../client/selectors";
import { useClientState, useEditor } from "./ClientProvider";

export function OpenSession() {
  const editor = useEditor();
  const can = useClientState(canOpenSession);
  const [opening, setOpening] = useState(false);
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

  return (
    <>
      <button
        type="button"
        className="tang-session-bar__button"
        disabled={opening}
        title="Open a session file (.toml) in place of this session: what is here is not kept"
        onClick={() => {
          chooser.current?.click();
        }}
      >
        Open…
      </button>
      <input ref={chooser} type="file" accept=".toml,text/plain" hidden aria-label="Session file" onChange={chosen} />
    </>
  );
}
