// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Client } from "../client/connection";
import { MockServer } from "../client/mock";
import type { MockLibrary, MockOptions } from "../client/mock";
import { MAX_SESSION_FILE_BYTES } from "../client/editor";
import type { SoundEntry } from "../protocol";
import { ClientProvider } from "./ClientProvider";
import { SessionBar } from "./SessionBar";
import { holdable, rig } from "./testing";

afterEach(cleanup);

// The picker of sounds (step 116), opened by the Open of the bar of the session in a program that has libraries of sounds: what it lists and how, what a search and the keys do, what opening does when it is taken and when it is refused.
// What the server makes of a listing and of a sound is `src/web/desk.rs`'s and the mock's; here the page.

const day = (year: number, month: number, date: number): number => new Date(year, month - 1, date, 12).getTime() / 1000;
const entry = (name: string, detail = "osc", openable = true, modified: number | null = null): SoundEntry => ({ id: `${name}.toml`, name, detail, openable, modified });

const sessions = (): MockLibrary => ({
  id: "sessions",
  label: "Saved sessions",
  place: "/home/me/.config/tang/sessions",
  entries: [entry("lush pad", "Helm · 2 effects", true, day(2026, 7, 7)), entry("kick", "osc", true, day(2026, 6, 11)), entry("broken", "not a session: expected an equals sign", false, day(2026, 2, 28))],
});
const examples = (): MockLibrary => ({ id: "folder1", label: "Examples", place: "/src/examples", entries: [entry("buo", "osc · 1 effect"), entry("bu1", "osc and 1 more")] });

const bar = (options: MockOptions = {}) => rig(<SessionBar />, { openSession: true, libraries: [sessions(), examples()], ...options });

const openButton = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: "Open…" });
/** Press the Open of the bar as a person does: the button has the focus (a click that is dispatched at it does not give it), which the picker gives back when it goes. */
const pressOpen = (): void => {
  openButton().focus();
  fireEvent.click(openButton());
};
const picker = (): HTMLElement => screen.getByRole("dialog", { name: "Open a sound" });
const search = (): HTMLInputElement => within(picker()).getByRole<HTMLInputElement>("searchbox", { name: "Search sounds" });
const submit = (): HTMLButtonElement => within(picker()).getByRole<HTMLButtonElement>("button", { name: "Open" });
const names = (): string[] => within(picker()).queryAllByRole("option").map((option) => option.querySelector(".tang-picker__name")?.textContent ?? "");
const option = (name: string): HTMLElement => {
  const found = within(picker()).queryAllByRole("option").find((each) => each.querySelector(".tang-picker__name")?.textContent === name);
  if (!found) throw new Error(`no sound called ${name}`);
  return found;
};

/** The pickers is open, with what the libraries listed shown. */
async function opened(options: MockOptions = {}) {
  const made = await bar(options);
  pressOpen();
  await screen.findByRole("dialog", { name: "Open a sound" });
  await waitFor(() => {
    expect(within(picker()).queryAllByText("Loading…")).toEqual([]);
  });
  return made;
}

/** The sounds that the server was asked to open, as the library and the id. */
const asked = (made: { server: MockServer }): string[] => made.server.received.flatMap((message) => (message.type === "openSound" ? [`${message.library}/${message.id}`] : []));
const listed = (made: { server: MockServer }): string[] => made.server.received.flatMap((message) => (message.type === "listSounds" ? [message.library] : []));

describe("the Open of the bar of the session", () => {
  test("opens the picker, and not the browser's chooser, when the hello lists libraries of sounds", async () => {
    await bar();
    let clicked = 0;
    screen.getByLabelText("Session file").addEventListener("click", (event) => {
      clicked += 1;
      event.preventDefault();
    });
    expect(screen.queryByRole("dialog")).toBeNull();
    pressOpen();
    expect(await screen.findByRole("dialog", { name: "Open a sound" })).toBeTruthy();
    expect(clicked).toBe(0);
  });

  test("asks for the chooser as before when the hello lists none", async () => {
    await bar({ libraries: [] });
    let clicked = 0;
    screen.getByLabelText("Session file").addEventListener("click", (event) => {
      clicked += 1;
      event.preventDefault();
    });
    pressOpen();
    expect(clicked).toBe(1);
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("the picker", () => {
  test("lists each library under its label, with where it is, and each sound with its name, what is in it and the day it was changed", async () => {
    await opened();
    expect(within(picker()).getByRole("heading", { name: /Saved sessions/ }).textContent).toContain("/home/me/.config/tang/sessions");
    expect(within(picker()).getByRole("heading", { name: /Examples/ }).getAttribute("id")).not.toBeNull();
    expect(names()).toEqual(["lush pad", "kick", "broken", "buo", "bu1"]);
    const pad = option("lush pad");
    expect(pad.textContent).toBe("lush padHelm · 2 effects2026-07-07");
    expect(within(screen.getByRole("listbox", { name: "Saved sessions" })).getAllByRole("option")).toHaveLength(3);
    expect(within(screen.getByRole("listbox", { name: "Examples" })).getAllByRole("option")).toHaveLength(2);
  });

  test("has the focus in its search field when it opens, so that what is typed is a search", async () => {
    await opened();
    expect(document.activeElement).toBe(search());
  });

  test("asks each library for what it has when it opens, and asks again the next time", async () => {
    const made = await opened();
    expect(listed(made)).toEqual(["sessions", "folder1"]);
    fireEvent.keyDown(search(), { key: "Escape" });
    pressOpen();
    await screen.findByRole("dialog", { name: "Open a sound" });
    expect(listed(made)).toEqual(["sessions", "folder1", "sessions", "folder1"]);
  });

  test("marks the first sound that can be opened, and a sound that cannot be is shown as it is and cannot be marked", async () => {
    await opened();
    expect(option("lush pad").getAttribute("aria-selected")).toBe("true");
    expect(option("kick").getAttribute("aria-selected")).toBe("false");
    const broken = option("broken");
    expect(broken.getAttribute("aria-disabled")).toBe("true");
    expect(option("kick").getAttribute("aria-disabled")).toBe("false");
    fireEvent.click(broken);
    expect(option("lush pad").getAttribute("aria-selected"), "a click on it marks nothing").toBe("true");
    expect(broken.getAttribute("aria-selected")).toBe("false");
    expect(search().getAttribute("aria-activedescendant")).toBe(option("lush pad").id);
  });

  test("says Loading… for a library that has not answered, and shows what the others have, which do not wait for it", async () => {
    const server = new MockServer({ telemetryPeriod: 5, openSession: true, libraries: [sessions(), examples()] });
    const connection = holdable(server);
    const client = new Client({ transport: connection.transport, reconnectDelay: () => 60_000 });
    render(
      <ClientProvider client={client}>
        <SessionBar />
      </ClientProvider>,
    );
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    connection.hold();
    pressOpen();
    await screen.findByRole("dialog", { name: "Open a sound" });
    expect(within(picker()).getAllByRole("status").map((note) => note.textContent)).toEqual(["Loading…", "Loading…"]);
    expect(submit().disabled, "nothing to open yet").toBe(true);
    connection.answer();
    await waitFor(() => {
      expect(names()).toEqual(["lush pad", "kick", "broken"]);
    });
    expect(within(picker()).getAllByRole("status").map((note) => note.textContent), "the other is still awaited").toEqual(["Loading…"]);
    expect(submit().disabled).toBe(false);
    connection.answer();
    await waitFor(() => {
      expect(names()).toEqual(["lush pad", "kick", "broken", "buo", "bu1"]);
    });
    expect(within(picker()).queryAllByRole("status")).toEqual([]);
  });

  test("says why a library could not be listed, in its own place, with a button that asks again, and the others are as they were", async () => {
    const gone: MockLibrary = { id: "folder1", label: "Examples", place: "/src/examples", fails: "/src/examples: it does not exist" };
    const made = await opened({ libraries: [sessions(), gone] });
    expect(names()).toEqual(["lush pad", "kick", "broken"]);
    expect(within(picker()).getByText(/\/src\/examples: it does not exist/)).toBeTruthy();
    // (The folder is made, and the person asks again.)
    delete gone.fails;
    gone.entries = [entry("buo")];
    fireEvent.click(within(picker()).getByRole("button", { name: "Try again" }));
    await waitFor(() => {
      expect(names()).toEqual(["lush pad", "kick", "broken", "buo"]);
    });
    expect(listed(made)).toEqual(["sessions", "folder1", "folder1"]);
    expect(within(picker()).queryByRole("button", { name: "Try again" })).toBeNull();
  });

  test("shows Loading… in a library that is asked again, until the answer comes", async () => {
    const gone: MockLibrary = { id: "folder1", label: "Examples", place: "/src/examples", fails: "/src/examples: it does not exist" };
    const server = new MockServer({ telemetryPeriod: 5, openSession: true, libraries: [gone] });
    const connection = holdable(server);
    const client = new Client({ transport: connection.transport, reconnectDelay: () => 60_000 });
    render(
      <ClientProvider client={client}>
        <SessionBar />
      </ClientProvider>,
    );
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    pressOpen();
    await within(await screen.findByRole("dialog", { name: "Open a sound" })).findByText(/it does not exist/);
    delete gone.fails;
    gone.entries = [entry("buo")];
    connection.hold();
    fireEvent.click(within(picker()).getByRole("button", { name: "Try again" }));
    await waitFor(() => {
      expect(within(picker()).getByRole("status").textContent).toBe("Loading…");
    });
    expect(within(picker()).queryByText(/it does not exist/), "the old answer is gone while the new one is awaited").toBeNull();
    connection.answer();
    await waitFor(() => {
      expect(names()).toEqual(["buo"]);
    });
  });

  test("says so for a library that has no sounds, and for one that has more than it lists", async () => {
    await opened({ libraries: [{ id: "sessions", label: "Saved sessions", place: "/s", entries: [] }, { ...examples(), truncated: true }] });
    expect(within(picker()).getByText("No sounds here yet.")).toBeTruthy();
    expect(within(picker()).getByText("This library has more sounds than are listed.")).toBeTruthy();
    expect(names()).toEqual(["buo", "bu1"]);
  });

  test("finds sounds by a word of the name or of what is said of them, in every library, and says where nothing matches", async () => {
    await opened();
    fireEvent.change(search(), { target: { value: "helm" } });
    expect(names()).toEqual(["lush pad"]);
    expect(within(picker()).getByText("No sound here matches “helm”.")).toBeTruthy();
    fireEvent.change(search(), { target: { value: " BU " } });
    expect(names()).toEqual(["buo", "bu1"]);
    fireEvent.change(search(), { target: { value: "nothing like it" } });
    expect(names()).toEqual([]);
    expect(within(picker()).getAllByText("No sound here matches “nothing like it”.")).toHaveLength(2);
    expect(submit().disabled).toBe(true);
    fireEvent.change(search(), { target: { value: "" } });
    expect(names()).toEqual(["lush pad", "kick", "broken", "buo", "bu1"]);
  });

  test("marks the first sound that a search finds, and Enter opens it", async () => {
    const made = await opened();
    fireEvent.change(search(), { target: { value: "bu" } });
    expect(option("buo").getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(search(), { key: "Enter" });
    await waitFor(() => {
      expect(asked(made)).toEqual(["folder1/buo.toml"]);
    });
  });

  test("moves the mark with the arrows over the sounds that can be opened, from library to library, and Enter opens the marked one", async () => {
    const made = await opened();
    fireEvent.keyDown(search(), { key: "ArrowDown" });
    expect(option("kick").getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(search(), { key: "ArrowDown" });
    expect(option("buo").getAttribute("aria-selected"), "the one that cannot be opened is passed over").toBe("true");
    fireEvent.keyDown(search(), { key: "ArrowUp" });
    expect(option("kick").getAttribute("aria-selected")).toBe("true");
    for (const key of ["ArrowUp", "ArrowUp", "ArrowUp"]) fireEvent.keyDown(search(), { key });
    expect(option("lush pad").getAttribute("aria-selected"), "and it stops at the top").toBe("true");
    for (let at = 0; at < 6; at += 1) fireEvent.keyDown(search(), { key: "ArrowDown" });
    expect(option("bu1").getAttribute("aria-selected"), "and at the bottom").toBe("true");
    fireEvent.keyDown(search(), { key: "Enter" });
    await waitFor(() => {
      expect(asked(made)).toEqual(["folder1/bu1.toml"]);
    });
  });

  test("opens nothing for a sound that cannot be opened, whatever is done to it, and the keys of the field are the field's own: an arrow and Enter do not move its caret or send a form", async () => {
    const made = await opened();
    fireEvent.doubleClick(option("broken"));
    fireEvent.click(option("broken"));
    fireEvent.doubleClick(option("broken"));
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(asked(made)).toEqual([]);
    expect(fireEvent.keyDown(search(), { key: "ArrowDown" }), "an arrow's default is prevented").toBe(false);
    expect(fireEvent.keyDown(search(), { key: "ArrowUp" })).toBe(false);
    expect(fireEvent.keyDown(search(), { key: "a" }), "a letter is the field's").toBe(true);
    expect(fireEvent.keyDown(search(), { key: "Enter" }), "Enter's default is prevented").toBe(false);
  });

  test("marks the sound that is clicked, opens it with the Open button, and opens one that is clicked twice at once", async () => {
    const made = await opened();
    fireEvent.click(option("kick"));
    expect(option("kick").getAttribute("aria-selected")).toBe("true");
    fireEvent.click(submit());
    await waitFor(() => {
      expect(asked(made)).toEqual(["sessions/kick.toml"]);
    });
    pressOpen();
    await screen.findByRole("dialog", { name: "Open a sound" });
    await waitFor(() => {
      expect(names()).toHaveLength(5);
    });
    fireEvent.doubleClick(option("bu1"));
    await waitFor(() => {
      expect(asked(made)).toEqual(["sessions/kick.toml", "folder1/bu1.toml"]);
    });
  });

  test("goes when the sound is taken, and gives the focus back to the button that opened it", async () => {
    await opened();
    fireEvent.keyDown(search(), { key: "Enter" });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(document.activeElement).toBe(openButton());
  });

  test("stays when the sound is refused, and says why in the notices of the box, and the buttons are there again for another", async () => {
    const library = sessions();
    const made = await opened({ libraries: [library] });
    // (The file is gone from the folder since it was listed.)
    library.entries = [entry("kick")];
    fireEvent.keyDown(search(), { key: "Enter" });
    expect(await within(picker()).findByText('Could not open "lush pad" from "Saved sessions": there is no sound "lush pad.toml" in this library any more')).toBeTruthy();
    expect(screen.queryByRole("dialog")).not.toBeNull();
    expect(submit().disabled).toBe(false);
    expect(asked(made)).toEqual(["sessions/lush pad.toml"]);
    fireEvent.click(option("lush pad"));
    fireEvent.click(submit());
    await waitFor(() => {
      expect(asked(made)).toHaveLength(2);
    });
  });

  test("asks for one sound at a time while the answer is awaited, and its buttons wait too", async () => {
    const server = new MockServer({ telemetryPeriod: 5, openSession: true, libraries: [sessions()] });
    const connection = holdable(server);
    const client = new Client({ transport: connection.transport, reconnectDelay: () => 60_000 });
    render(
      <ClientProvider client={client}>
        <SessionBar />
      </ClientProvider>,
    );
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    pressOpen();
    await waitFor(() => {
      expect(names()).toHaveLength(3);
    });
    connection.hold();
    fireEvent.keyDown(search(), { key: "Enter" });
    await waitFor(() => {
      expect(submit().disabled).toBe(true);
    });
    fireEvent.keyDown(search(), { key: "Enter" });
    fireEvent.doubleClick(option("kick"));
    expect(within(picker()).getByRole<HTMLButtonElement>("button", { name: "From this computer…" }).disabled).toBe(true);
    // (The mock takes what is sent a moment after it is sent: what was asked twice more is there by now, if it was.)
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(server.received.filter((message) => message.type === "openSound")).toHaveLength(1);
    connection.answer();
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
  });

  test("is left by Cancel and by Escape, which send nothing", async () => {
    const made = await opened();
    fireEvent.click(within(picker()).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(openButton());
    pressOpen();
    await screen.findByRole("dialog", { name: "Open a sound" });
    fireEvent.keyDown(search(), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(asked(made)).toEqual([]);
  });

  test("says what opening does, and says it more strongly when the session has changes that are not saved", async () => {
    const made = await opened();
    expect(within(picker()).getByText("Opening a sound replaces this session: what is here is not kept.")).toBeTruthy();
    fireEvent.keyDown(search(), { key: "Escape" });
    // (A change: the tempo.)
    await made.client.request({ type: "setBpm", bpm: 99 });
    await waitFor(() => {
      expect(made.client.state.snapshot?.meta.dirty).toBe(true);
    });
    pressOpen();
    await screen.findByRole("dialog", { name: "Open a sound" });
    expect(within(picker()).getByText("This session has changes that are not saved: opening a sound replaces them.")).toBeTruthy();
  });
});

describe("the picker's own chooser, for a file of this computer", () => {
  const chooser = (): HTMLInputElement => within(picker()).getByLabelText<HTMLInputElement>("Choose a session file", { selector: "input" });
  const file = (name: string, text: string, read: () => Promise<string> = () => Promise.resolve(text)): File => Object.assign(new File([text], name, { type: "text/plain" }), { text: read });
  const TEXT = '[[instrument]]\nplugin = "builtin:osc"\n';

  test("is a button among the others that asks the browser for a toml file, and is not shown", async () => {
    await opened();
    expect(chooser().type).toBe("file");
    expect(chooser().hidden).toBe(true);
    expect(chooser().accept).toBe(".toml,text/plain");
    let clicked = 0;
    chooser().addEventListener("click", (event) => {
      clicked += 1;
      event.preventDefault();
    });
    fireEvent.click(within(picker()).getByRole("button", { name: "From this computer…" }));
    expect(clicked).toBe(1);
  });

  test("sends the text of the file that is picked, and the picker goes when the session is taken", async () => {
    const made = await opened();
    fireEvent.change(chooser(), { target: { files: [file("pluck.toml", TEXT)] } });
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(made.requests().map(({ intent }) => intent)).toEqual([{ type: "openSession", text: TEXT }]);
  });

  test("says in the box why a file could not be taken, and stays, with the buttons there again", async () => {
    const made = await opened();
    fireEvent.change(chooser(), { target: { files: [file("gone.toml", "", () => Promise.reject(new Error("the disk went")))] } });
    expect(await within(picker()).findByText('Could not open "gone.toml": it could not be read (the disk went)')).toBeTruthy();
    expect(within(picker()).getByRole<HTMLButtonElement>("button", { name: "From this computer…" }).disabled).toBe(false);
    fireEvent.change(chooser(), { target: { files: [file("big.toml", "x".repeat(MAX_SESSION_FILE_BYTES + 1))] } });
    expect(await within(picker()).findByText(/Could not open "big.toml": it is more than 4 MiB/)).toBeTruthy();
    expect(made.requests()).toEqual([]);
  });

  test("waits while the file is read: the buttons are not pressed again, and the picker goes when the session has been taken", async () => {
    await opened();
    let release: (text: string) => void = () => undefined;
    const slow = file("slow.toml", "", () => new Promise<string>((resolve) => (release = resolve)));
    fireEvent.change(chooser(), { target: { files: [slow] } });
    await waitFor(() => {
      expect(within(picker()).getByRole<HTMLButtonElement>("button", { name: "From this computer…" }).disabled, "while the file is read").toBe(true);
    });
    expect(submit().disabled).toBe(true);
    release(TEXT);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
  });

  test("sends nothing when the chooser is left with no file, and is emptied for the same file to be picked again", async () => {
    const made = await opened();
    const assigned: string[] = [];
    Object.defineProperty(chooser(), "value", { configurable: true, get: () => "", set: (value: string) => assigned.push(value) });
    fireEvent.change(chooser(), { target: { files: [] } });
    expect(made.requests()).toEqual([]);
    fireEvent.change(chooser(), { target: { files: [file("pluck.toml", TEXT)] } });
    expect(assigned, "emptied each time, before the file is looked at").toEqual(["", ""]);
  });
});
