// What the sound picker (step 116) works out of what the libraries list: which sounds a search asks for, the rows in the order they are shown, the one that Enter opens and where an arrow goes. No
// view and no connection here: the picker keeps what the server told it of each library (`Listing`), and these say what it makes of it.
import type { SoundEntry } from "../protocol";

/** What the picker knows of a library: its listing is being asked for, is in, or could not be had. */
export type Listing =
  | { status: "loading" }
  | { status: "ready"; entries: readonly SoundEntry[]; truncated: boolean }
  | { status: "failed"; message: string };

/** One sound in the picker: the library it is in, and what that says of it. */
export interface Row {
  library: string;
  entry: SoundEntry;
}

/** Whether `a` and `b` are the same sound of the same library. */
export const sameRow = (a: Row | null, b: Row | null): boolean => a !== null && b !== null && a.library === b.library && a.entry.id === b.entry.id;

/**
 * Whether `entry` is one that `query` asks for: every word of it is in the name or in what is said of the sound (the plugins, how many effects), whatever the case. A query of no words asks for everything.
 */
export function matches(entry: SoundEntry, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter((word) => word !== "");
  if (words.length === 0) return true;
  const text = `${entry.name} ${entry.detail}`.toLowerCase();
  return words.every((word) => text.includes(word));
}

/** The sounds of `entries` that `query` asks for, in the order they have. */
export const matching = (entries: readonly SoundEntry[], query: string): SoundEntry[] => entries.filter((entry) => matches(entry, query));

/** Every sound that is shown, library after library as they are listed, in the order of the picker: those of the listings that are in, that `query` asks for. */
export function rowsOf(libraries: readonly { id: string }[], listings: Readonly<Record<string, Listing | undefined>>, query: string): Row[] {
  return libraries.flatMap((library) => {
    const listing = listings[library.id];
    return listing?.status === "ready" ? matching(listing.entries, query).map((entry) => ({ library: library.id, entry })) : [];
  });
}

/** The sound that Enter opens: `chosen` if it is still shown and can be opened, else the first that can be (a search that is typed takes the first of what it finds). None if none can. */
export function activeOf(rows: readonly Row[], chosen: Row | null): Row | null {
  const kept = rows.find((row) => sameRow(row, chosen));
  if (kept?.entry.openable === true) return kept;
  return rows.find((row) => row.entry.openable) ?? null;
}

/** The sound that an arrow goes to from `from`: the next that can be opened in that direction (`by` is 1 for down, -1 for up), and `from` at either end. The first that can be, for no sound. */
export function movedFrom(rows: readonly Row[], from: Row | null, by: 1 | -1): Row | null {
  const openable = rows.filter((row) => row.entry.openable);
  if (openable.length === 0) return null;
  const at = openable.findIndex((row) => sameRow(row, from));
  if (at < 0) return openable[0] ?? null;
  return openable[Math.min(Math.max(at + by, 0), openable.length - 1)] ?? null;
}

/** The day that a sound was last changed, as a person reads it (`2026-07-07`), in their own time zone; nothing for a sound whose library does not know. */
export function dateOf(seconds: number | null): string {
  if (seconds === null) return "";
  const day = new Date(seconds * 1000);
  const two = (number: number): string => String(number).padStart(2, "0");
  return `${String(day.getFullYear())}-${two(day.getMonth() + 1)}-${two(day.getDate())}`;
}
