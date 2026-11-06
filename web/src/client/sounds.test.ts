import { describe, expect, test } from "vitest";

import type { SoundEntry } from "../protocol";
import { activeOf, dateOf, matches, matching, movedFrom, rowsOf, sameRow } from "./sounds";
import type { Listing, Row } from "./sounds";

// What the sound picker works out of the listings (step 116): a search, the rows in the order they are shown, the sound that Enter opens, and where an arrow goes.

const entry = (name: string, detail = "osc", openable = true): SoundEntry => ({ id: `${name}.toml`, name, detail, openable, modified: null });
const row = (library: string, name: string, openable = true): Row => ({ library, entry: entry(name, "osc", openable) });

describe("a search", () => {
  const pad = entry("lush pad", "Helm · 2 effects");

  test("asks for every sound when it has no words, whatever the spaces", () => {
    expect(matches(pad, "")).toBe(true);
    expect(matches(pad, "   ")).toBe(true);
  });

  test("finds a word in the name or in what is said of the sound, in any case", () => {
    expect(matches(pad, "LUSH")).toBe(true);
    expect(matches(pad, "helm")).toBe(true);
    expect(matches(pad, "effects")).toBe(true);
    expect(matches(pad, "lead")).toBe(false);
  });

  test("needs every one of its words, in any order, and a word may be a part of one", () => {
    expect(matches(pad, "pad helm")).toBe(true);
    expect(matches(pad, "helm lush")).toBe(true);
    expect(matches(pad, "pa he")).toBe(true);
    expect(matches(pad, "pad piano")).toBe(false);
  });

  test("keeps the order of the sounds it finds", () => {
    const all = [entry("b pad"), entry("a lead"), entry("c pad")];
    expect(matching(all, "pad").map((found) => found.name)).toEqual(["b pad", "c pad"]);
    expect(matching(all, "")).toEqual(all);
  });
});

describe("the rows of the picker", () => {
  const listings: Record<string, Listing | undefined> = {
    sessions: { status: "ready", entries: [entry("new"), entry("old")], truncated: false },
    examples: { status: "ready", entries: [entry("buo"), entry("bu1"), entry("broken", "not a session", false)], truncated: false },
    far: { status: "loading" },
    gone: { status: "failed", message: "it is gone" },
  };
  const libraries = [{ id: "sessions" }, { id: "far" }, { id: "gone" }, { id: "examples" }, { id: "unknown" }];

  test("are the sounds of the libraries that have been listed, library after library in the order they are given", () => {
    expect(rowsOf(libraries, listings, "").map((each) => `${each.library}/${each.entry.name}`)).toEqual(["sessions/new", "sessions/old", "examples/buo", "examples/bu1", "examples/broken"]);
  });

  test("are those that a search asks for", () => {
    expect(rowsOf(libraries, listings, "bu").map((each) => each.entry.name)).toEqual(["buo", "bu1"]);
    expect(rowsOf(libraries, listings, "nothing like it")).toEqual([]);
  });

  test("are none for a library that is loading, has failed or is not known", () => {
    expect(rowsOf([{ id: "far" }, { id: "gone" }, { id: "unknown" }], listings, "")).toEqual([]);
  });

  test("tell one sound from another of the same name in another library", () => {
    expect(sameRow(row("a", "x"), row("a", "x"))).toBe(true);
    expect(sameRow(row("a", "x"), row("b", "x"))).toBe(false);
    expect(sameRow(row("a", "x"), row("a", "y"))).toBe(false);
    expect(sameRow(null, null)).toBe(false);
    expect(sameRow(row("a", "x"), null)).toBe(false);
  });
});

describe("the sound that Enter opens", () => {
  const rows = [row("a", "broken", false), row("a", "first"), row("a", "second"), row("b", "third")];

  test("is the one that was chosen, while it is shown and can be opened", () => {
    expect(activeOf(rows, row("b", "third"))).toEqual(row("b", "third"));
  });

  test("is the first that can be opened when none was chosen, or the one that was is not shown any more, or cannot be opened", () => {
    expect(activeOf(rows, null)).toEqual(row("a", "first"));
    expect(activeOf(rows, row("c", "gone"))).toEqual(row("a", "first"));
    expect(activeOf(rows, row("a", "broken", false))).toEqual(row("a", "first"));
  });

  test("is none when nothing shown can be opened", () => {
    expect(activeOf([], null)).toBeNull();
    expect(activeOf([row("a", "broken", false)], null)).toBeNull();
  });
});

describe("an arrow", () => {
  const rows = [row("a", "broken", false), row("a", "first"), row("a", "also broken", false), row("a", "second"), row("b", "third")];

  test("goes to the next sound that can be opened, over those that cannot, and stops at the ends", () => {
    expect(movedFrom(rows, row("a", "first"), 1)).toEqual(row("a", "second"));
    expect(movedFrom(rows, row("a", "second"), 1)).toEqual(row("b", "third"));
    expect(movedFrom(rows, row("b", "third"), 1)).toEqual(row("b", "third"));
    expect(movedFrom(rows, row("b", "third"), -1)).toEqual(row("a", "second"));
    expect(movedFrom(rows, row("a", "first"), -1)).toEqual(row("a", "first"));
  });

  test("goes to the first that can be opened from no sound, or from one that is not shown", () => {
    expect(movedFrom(rows, null, 1)).toEqual(row("a", "first"));
    expect(movedFrom(rows, null, -1)).toEqual(row("a", "first"));
    expect(movedFrom(rows, row("z", "gone"), 1)).toEqual(row("a", "first"));
  });

  test("goes nowhere when no sound can be opened", () => {
    expect(movedFrom([], null, 1)).toBeNull();
    expect(movedFrom([row("a", "broken", false)], null, 1)).toBeNull();
  });
});

describe("the date of a sound", () => {
  test("is the day it was last changed, in the person's own time zone, with two digits for the month and the day", () => {
    const local = new Date(2026, 6, 7, 11, 10, 0);
    expect(dateOf(local.getTime() / 1000)).toBe("2026-07-07");
    expect(dateOf(new Date(2026, 0, 2, 0, 5, 0).getTime() / 1000)).toBe("2026-01-02");
    expect(dateOf(new Date(2026, 11, 31, 23, 59, 59).getTime() / 1000)).toBe("2026-12-31");
  });

  test("is nothing for a sound whose library does not know when it was changed", () => {
    expect(dateOf(null)).toBe("");
  });
});
