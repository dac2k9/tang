// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import type { MockOptions } from "../client/mock";
import { NoticeBoard } from "../ui/NoticeBoard";
import { useNotices } from "./ClientProvider";
import { SessionBar } from "./SessionBar";
import { rig } from "./testing";

afterEach(cleanup);

// "Open…" in the bar of the session (step 115): a button for a program that can open another session while it runs (the hello says `openSession`), which asks for a file with the browser's chooser, reads it here and sends its text. What
// the server makes of a text is `apply.rs`'s and the editor's; what is looked at here is the page: when the button is there, what it asks the chooser for, what it sends and what it does while the answer is awaited.

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

const bar = (options: MockOptions = {}) =>
  rig(
    <>
      <SessionBar />
      <Board />
    </>,
    options,
  );

const button = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: "Open…" });
const chooser = (): HTMLInputElement => screen.getByLabelText<HTMLInputElement>("Session file");

/** A file as the chooser gives one: a name and text that can be read (jsdom's own `File` has no `text`). */
const picked = (name: string, text: string): File => Object.assign(new File([text], name, { type: "text/plain" }), { text: () => Promise.resolve(text) });

/** The texts that the server was asked to open, in order. */
const openedBy = (made: { requests: () => { intent: unknown }[] }): unknown[] =>
  made.requests().flatMap(({ intent }) => (typeof intent === "object" && intent !== null && "type" in intent && intent.type === "openSession" && "text" in intent ? [intent.text] : []));

describe("the Open of the bar of the session", () => {
  test("is not there for a program that cannot open another session: the hello does not say that it can", async () => {
    await bar();
    expect(screen.queryByRole("button", { name: "Open…" })).toBeNull();
    expect(screen.queryByLabelText("Session file")).toBeNull();
  });

  test("is a button, with a chooser of toml files that is not shown, for a program that can: the button is what opens the chooser", async () => {
    await bar({ openSession: true });
    expect(button().disabled).toBe(false);
    expect(chooser().type).toBe("file");
    expect(chooser().hidden).toBe(true);
    expect(chooser().accept).toBe(".toml,text/plain");
    let clicked = 0;
    chooser().addEventListener("click", (event) => {
      clicked += 1;
      event.preventDefault();
    });
    fireEvent.click(button());
    expect(clicked, "the button opens the chooser").toBe(1);
  });

  test("sends the text of the file that is picked, once, and the chooser is emptied for the same file to be picked again", async () => {
    const made = await bar({ openSession: true });
    const text = '[[instrument]]\nplugin = "builtin:osc"\n';
    // (What the page does to the chooser's value is watched: jsdom keeps a file list that the test put there out of what it calls the value.)
    const assigned: string[] = [];
    Object.defineProperty(chooser(), "value", { configurable: true, get: () => "", set: (value: string) => assigned.push(value) });
    fireEvent.change(chooser(), { target: { files: [picked("pluck.toml", text)] } });
    await waitFor(() => {
      expect(openedBy(made)).toEqual([text]);
    });
    expect(assigned, "emptied, once, before anything else is done with the file").toEqual([""]);
    // (A second pick of the same file is a second request: the chooser says nothing if it is told what it has.)
    fireEvent.change(chooser(), { target: { files: [picked("pluck.toml", text)] } });
    await waitFor(() => {
      expect(openedBy(made)).toEqual([text, text]);
    });
  });

  test("sends nothing when the chooser is left with no file", async () => {
    const made = await bar({ openSession: true });
    fireEvent.change(chooser(), { target: { files: [] } });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(openedBy(made)).toEqual([]);
    expect(button().disabled).toBe(false);
  });

  test("is not pressed again while a file is being opened, and is again when it has been", async () => {
    const made = await bar({ openSession: true });
    let release: (text: string) => void = () => undefined;
    const slow = Object.assign(new File([""], "slow.toml"), { text: () => new Promise<string>((resolve) => (release = resolve)) });
    fireEvent.change(chooser(), { target: { files: [slow] } });
    await waitFor(() => {
      expect(button().disabled, "while the file is read").toBe(true);
    });
    release('[[instrument]]\nplugin = "builtin:osc"\n');
    await waitFor(() => {
      expect(button().disabled).toBe(false);
    });
    expect(openedBy(made)).toHaveLength(1);
  });

  test("says among the notices why a file could not be read, and the button is there again", async () => {
    const made = await bar({ openSession: true });
    const broken = Object.assign(new File([""], "gone.toml"), { text: () => Promise.reject(new Error("the disk went")) });
    fireEvent.change(chooser(), { target: { files: [broken] } });
    expect(await screen.findByText('Could not open "gone.toml": it could not be read (the disk went)')).toBeTruthy();
    expect(openedBy(made)).toEqual([]);
    expect(button().disabled).toBe(false);
  });
});
