import { useMemo, useState } from "react";

import { ConnectionStatus } from "../ui/ConnectionStatus";
import { NoticeBoard } from "../ui/NoticeBoard";
import { AddFreeEffectMenu } from "./AddFreeEffect";
import { AddFreeModulatorMenu } from "./AddFreeModulator";
import { AddInstrumentMenu } from "./AddInstrument";
import { AddModuleMenu } from "./AddModule";
import { AddSourceMenu } from "./AddSource";
import { BusyLight } from "./BusyLight";
import { useClientState, useNotices } from "./ClientProvider";
import type { EngineKind } from "./engine";
import { ErrorBoundary } from "./ErrorBoundary";
import { PatchView } from "./patch/PatchView";
import { Piano } from "./piano/Piano";
import { SessionBar } from "./SessionBar";
import { useKeyboardPiano } from "./useKeyboardPiano";
import "./App.css";

/** The session as JSON, for whoever wants to see what the server sent. Made only while it is shown: it is a lot to write on every change. */
function SessionJson() {
  const snapshot = useClientState((state) => state.snapshot);
  const raw = useMemo(() => (snapshot ? JSON.stringify(snapshot, null, 2) : null), [snapshot]);
  return raw ? <pre className="tang-session">{raw}</pre> : null;
}

function RawSession() {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="tang-raw"
      open={open}
      onToggle={(event) => {
        setOpen(event.currentTarget.open);
      }}
    >
      <summary>Session as JSON</summary>
      {open ? <SessionJson /> : null}
    </details>
  );
}

/** What the keys of the computer do: they play, from this octave. */
const KEYS_HINT = "The keys Z S X D C V G B H N J M , L . ; / and Q 2 W 3 E R 5 T 6 Y 7 U I 9 O 0 P play notes; [ and ] change the octave";

/** The whole page: the state of the connection, the session as a patch, the piano, and the session as the server sent it. */
export function App({ engine }: { engine: EngineKind }) {
  const status = useClientState((state) => state.status);
  const problem = useClientState((state) => state.problem);
  const notices = useNotices();
  const octave = useKeyboardPiano();

  return (
    <div className="tang-app">
      <header className="tang-app__bar">
        <h1 className="tang-app__title">Tang</h1>
        <ConnectionStatus status={status} />
        <SessionBar />
        <AddInstrumentMenu />
        <AddFreeEffectMenu />
        <AddFreeModulatorMenu />
        <AddSourceMenu />
        <AddModuleMenu />
        <span className="tang-app__keys" title={KEYS_HINT}>
          Keys: octave {octave}
        </span>
        <span className="tang-app__engine">{engine === "mock" ? "mock engine" : "server"}</span>
      </header>
      <main className="tang-app__main">
        <BusyLight />
        {problem ? (
          <p className="tang-app__problem" role="alert">
            {problem}
          </p>
        ) : null}
        <NoticeBoard notices={notices} />
        <div className="tang-app__patch">
          <ErrorBoundary what="the patch">
            <PatchView />
          </ErrorBoundary>
        </div>
        <ErrorBoundary what="the piano">
          <Piano octave={octave} />
        </ErrorBoundary>
        <ErrorBoundary what="the session as JSON">
          <RawSession />
        </ErrorBoundary>
      </main>
    </div>
  );
}
