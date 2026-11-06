// The picker of sounds (step 116: "a more advanced loader/picker", for a server of sounds one day): a dialog that lists what Tang can open a session from, a library at a time, and opens the one that a person picks in place of the session.
// A library is a place that the server has (a folder of session files on the machine that runs Tang; later, a server that people upload to): the hello lists them, and each is asked for what it has when the dialog opens, all at
// once, so that one that is slow to answer holds up none of the others. "From this computer" is the page's own: the browser's chooser. A search finds a word in a name or in what is said of a sound; Enter opens the first sound
// that it finds, and the arrows move to another. Opening is not undone, and the dialog says so.
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { ChangeEvent, KeyboardEvent } from "react";

import { soundLibraries } from "../client/selectors";
import { activeOf, dateOf, movedFrom, rowsOf, sameRow } from "../client/sounds";
import type { Listing, Row } from "../client/sounds";
import { Dialog } from "../ui/Dialog";
import { NoticeBoard } from "../ui/NoticeBoard";
import { useClient, useClientState, useEditor, useNotices } from "./ClientProvider";
import "./SoundPicker.css";

const whyNot = (error: unknown): string => (error instanceof Error ? error.message : String(error));

export function SoundPicker({ onClose }: { onClose: () => void }) {
  const client = useClient();
  const editor = useEditor();
  const notices = useNotices();
  const libraries = useClientState(soundLibraries);
  const unsaved = useClientState((state) => state.snapshot?.meta.dirty === true);
  const [listings, setListings] = useState<Record<string, Listing>>(() => Object.fromEntries(libraries.map((library) => [library.id, { status: "loading" } as const])));
  const [query, setQuery] = useState("");
  const [chosen, setChosen] = useState<Row | null>(null);
  const [opening, setOpening] = useState(false);
  const chooser = useRef<HTMLInputElement>(null);
  const alive = useRef(true);
  const listId = useId();

  // Asks a library what it has, and puts the answer (or why there is none) with the others.
  const ask = useCallback(
    (library: string): void => {
      const tell = (listing: Listing): void => {
        if (alive.current) setListings((now) => ({ ...now, [library]: listing }));
      };
      client.listSounds(library).then(
        (found) => {
          tell({ status: "ready", entries: found.entries, truncated: found.truncated });
        },
        (error: unknown) => {
          tell({ status: "failed", message: whyNot(error) });
        },
      );
    },
    [client],
  );

  useEffect(() => {
    alive.current = true;
    for (const library of libraries) ask(library.id);
    return () => {
      alive.current = false;
    };
  }, [libraries, ask]);

  const retry = (library: string): void => {
    setListings((now) => ({ ...now, [library]: { status: "loading" } }));
    ask(library);
  };

  const rows = useMemo(() => rowsOf(libraries, listings, query), [libraries, listings, query]);
  const active = activeOf(rows, chosen);
  const rowId = (at: number): string => `${listId}-${String(at)}`;
  const activeAt = active === null ? -1 : rows.findIndex((row) => sameRow(row, active));

  // (The sound that an arrow went to is brought into view.)
  useEffect(() => {
    if (activeAt >= 0) document.getElementById(`${listId}-${String(activeAt)}`)?.scrollIntoView({ block: "nearest" });
  }, [activeAt, listId]);

  const open = (row: Row | null): void => {
    const library = libraries.find((each) => each.id === row?.library);
    if (row === null || library === undefined || !row.entry.openable || opening) return;
    setOpening(true);
    // (A sound that is refused is said in the notices below, with why, and the dialog stays for another.)
    void editor.openSound(library, row.entry).then((done) => {
      setOpening(false);
      if (done) onClose();
    });
  };

  const picked = (event: ChangeEvent<HTMLInputElement>): void => {
    const input = event.currentTarget;
    const file = input.files?.[0];
    // (Emptied, so that the same file can be chosen again: a session that was changed since is not the one it was.)
    input.value = "";
    if (!file) return;
    setOpening(true);
    void editor.openSessionFile(file).then((done) => {
      setOpening(false);
      if (done) onClose();
    });
  };

  const onSearchKey = (event: KeyboardEvent<HTMLInputElement>): void => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setChosen(movedFrom(rows, active, event.key === "ArrowDown" ? 1 : -1));
    } else if (event.key === "Enter") {
      event.preventDefault();
      open(active);
    }
  };

  return (
    <Dialog title="Open a sound" onClose={onClose} className="tang-picker">
      <div className="tang-picker__top">
        <input
          type="search"
          className="tang-picker__search"
          aria-label="Search sounds"
          placeholder="Search sounds"
          aria-controls={listId}
          aria-activedescendant={activeAt >= 0 ? rowId(activeAt) : undefined}
          autoComplete="off"
          spellCheck={false}
          data-autofocus
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
          }}
          onKeyDown={onSearchKey}
        />
        <button
          type="button"
          className="tang-session-bar__button"
          disabled={opening}
          title="Open a session file (.toml) from this computer"
          onClick={() => {
            chooser.current?.click();
          }}
        >
          From this computer…
        </button>
        <input ref={chooser} type="file" accept=".toml,text/plain" hidden aria-label="Choose a session file" onChange={picked} />
      </div>
      <div className="tang-picker__libraries" id={listId}>
        {libraries.map((library) => {
          const listing = listings[library.id];
          const mine = rows.filter((row) => row.library === library.id);
          const headingId = `${listId}-library-${library.id}`;
          return (
            <section key={library.id} className="tang-picker__library" aria-labelledby={headingId}>
              <h3 id={headingId} className="tang-picker__heading">
                {library.label}
                <span className="tang-picker__place" title={library.place}>
                  {library.place}
                </span>
              </h3>
              {listing === undefined || listing.status === "loading" ? (
                <p className="tang-picker__note" role="status">
                  Loading…
                </p>
              ) : listing.status === "failed" ? (
                <p className="tang-picker__note tang-picker__note--problem">
                  {listing.message}
                  <button
                    type="button"
                    className="tang-picker__retry"
                    onClick={() => {
                      retry(library.id);
                    }}
                  >
                    Try again
                  </button>
                </p>
              ) : listing.entries.length === 0 ? (
                <p className="tang-picker__note">No sounds here yet.</p>
              ) : mine.length === 0 ? (
                <p className="tang-picker__note">No sound here matches “{query.trim()}”.</p>
              ) : (
                <ul className="tang-picker__rows" role="listbox" aria-label={library.label}>
                  {mine.map((row) => {
                    const at = rows.findIndex((each) => sameRow(each, row));
                    const selected = sameRow(row, active);
                    return (
                      <li
                        key={row.entry.id}
                        id={rowId(at)}
                        role="option"
                        aria-selected={selected}
                        aria-disabled={!row.entry.openable}
                        className={["tang-picker__row", selected ? "tang-picker__row--active" : "", row.entry.openable ? "" : "tang-picker__row--unavailable"].filter(Boolean).join(" ")}
                        onClick={() => {
                          if (row.entry.openable) setChosen(row);
                        }}
                        onDoubleClick={() => {
                          open(row);
                        }}
                      >
                        <span className="tang-picker__name">{row.entry.name}</span>
                        <span className="tang-picker__detail">{row.entry.detail}</span>
                        <span className="tang-picker__date">{dateOf(row.entry.modified)}</span>
                      </li>
                    );
                  })}
                </ul>
              )}
              {listing?.status === "ready" && listing.truncated ? <p className="tang-picker__note">This library has more sounds than are listed.</p> : null}
            </section>
          );
        })}
      </div>
      <NoticeBoard notices={notices} />
      <p className="tang-picker__warning">{unsaved ? "This session has changes that are not saved: opening a sound replaces them." : "Opening a sound replaces this session: what is here is not kept."}</p>
      <div className="tang-picker__buttons">
        <button type="button" className="tang-session-bar__button" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="tang-session-bar__button tang-picker__open"
          disabled={active === null || opening}
          onClick={() => {
            open(active);
          }}
        >
          Open
        </button>
      </div>
    </Dialog>
  );
}
