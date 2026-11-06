// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { Dialog } from "./Dialog";

afterEach(cleanup);

// The dialog (step 116): a box over the page with the focus in it, that Escape and a press on the backdrop close, that Tab does not leave, and whose keys are not the page's.

function mount(onClose = vi.fn(), content = <input aria-label="Search" data-autofocus />) {
  const view = render(
    <div>
      <button type="button">opener</button>
      <Dialog title="Open a sound" onClose={onClose}>
        {content}
        <button type="button">Cancel</button>
        <button type="button">Open</button>
      </Dialog>
    </div>,
  );
  return { view, onClose };
}

const box = (): HTMLElement => screen.getByRole("dialog");
/** What is behind the box: the backdrop that a press on closes the dialog. */
const backdrop = (): HTMLElement => {
  const found = box().parentElement;
  if (!found) throw new Error("the box has no backdrop");
  return found;
};

describe("a dialog", () => {
  test("is a modal dialog, named by its title, drawn in the body of the page and not where it is used", () => {
    const { view } = mount();
    expect(box().getAttribute("aria-modal")).toBe("true");
    expect(screen.getByRole("dialog", { name: "Open a sound" })).toBe(box());
    expect(box().parentElement?.parentElement).toBe(document.body);
    expect(view.container.contains(box()), "not inside the element that made it").toBe(false);
  });

  test("has the focus on what is marked for it when it opens, else on the box, and gives the focus back to what had it when it goes", () => {
    const outside = document.createElement("button");
    document.body.append(outside);
    outside.focus();
    const view = render(
      <Dialog title="T" onClose={vi.fn()}>
        <input aria-label="Search" data-autofocus />
      </Dialog>,
    );
    expect(document.activeElement).toBe(screen.getByLabelText("Search"));
    view.unmount();
    expect(document.activeElement, "back where it was").toBe(outside);
    const bare = render(
      <Dialog title="T" onClose={vi.fn()}>
        <button type="button">Only</button>
      </Dialog>,
    );
    expect(document.activeElement).toBe(screen.getByRole("dialog"));
    bare.unmount();
    outside.remove();
  });

  test("does not give the focus back to what has gone from the page while it was open", () => {
    const gone = document.createElement("button");
    document.body.append(gone);
    gone.focus();
    const view = render(
      <Dialog title="T" onClose={vi.fn()}>
        <button type="button">x</button>
      </Dialog>,
    );
    gone.remove();
    expect(() => {
      view.unmount();
    }).not.toThrow();
    expect(document.activeElement).toBe(document.body);
  });

  test("is closed by Escape, once, and the key is not taken for the page's", () => {
    const { onClose } = mount();
    const heard = vi.fn();
    window.addEventListener("keydown", heard);
    expect(fireEvent.keyDown(screen.getByLabelText("Search"), { key: "Escape" }), "the key's default is prevented").toBe(false);
    window.removeEventListener("keydown", heard);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(heard).not.toHaveBeenCalled();
  });

  test("keeps the keys that are typed in it from the page behind it (the piano, undo, save), and lets a key that goes up through", () => {
    mount();
    const down = vi.fn();
    const up = vi.fn();
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    for (const key of ["a", "s", "z", "Enter", "ArrowDown"]) fireEvent.keyDown(screen.getByLabelText("Search"), { key });
    fireEvent.keyDown(screen.getByText("Cancel"), { key: "s", metaKey: true });
    fireEvent.keyUp(screen.getByLabelText("Search"), { key: "a" });
    window.removeEventListener("keydown", down);
    window.removeEventListener("keyup", up);
    expect(down, "no key of the dialog reached the page").not.toHaveBeenCalled();
    expect(up, "a key that goes up does: a note that it started is ended").toHaveBeenCalledTimes(1);
  });

  test("is closed by a press on the backdrop, and not by one in the box or by one that starts in the box and goes up outside it", () => {
    const { onClose } = mount();
    fireEvent.mouseDown(box());
    fireEvent.mouseDown(screen.getByText("Open a sound"));
    fireEvent.mouseUp(backdrop());
    fireEvent.click(backdrop());
    expect(onClose, "none of those").not.toHaveBeenCalled();
    fireEvent.mouseDown(backdrop());
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test("is gone from the page when it is not drawn", () => {
    const view = render(
      <Dialog title="T" onClose={vi.fn()}>
        <p>content</p>
      </Dialog>,
    );
    expect(screen.queryByRole("dialog")).not.toBeNull();
    view.unmount();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.body.querySelector(".tang-dialog__backdrop")).toBeNull();
  });
});

describe("Tab in a dialog", () => {
  test("goes round what is in it: from the last to the first, and with Shift from the first to the last or from the box itself to the last", () => {
    mount();
    const search = screen.getByLabelText("Search");
    const open = screen.getByRole("button", { name: "Open" });
    open.focus();
    expect(fireEvent.keyDown(open, { key: "Tab" }), "the Tab that would leave is stopped").toBe(false);
    expect(document.activeElement).toBe(search);
    expect(fireEvent.keyDown(search, { key: "Tab", shiftKey: true })).toBe(false);
    expect(document.activeElement).toBe(open);
    box().focus();
    expect(fireEvent.keyDown(box(), { key: "Tab", shiftKey: true })).toBe(false);
    expect(document.activeElement).toBe(open);
  });

  test("is left to the browser in the middle of what is in it", () => {
    mount();
    const cancel = screen.getByRole("button", { name: "Cancel" });
    cancel.focus();
    expect(fireEvent.keyDown(cancel, { key: "Tab" }), "not stopped").toBe(true);
    expect(fireEvent.keyDown(cancel, { key: "Tab", shiftKey: true })).toBe(true);
    expect(fireEvent.keyDown(screen.getByLabelText("Search"), { key: "Tab" })).toBe(true);
  });

  test("passes over what is disabled or hidden, and keeps the focus in the box when there is nothing to go to", () => {
    const view = render(
      <Dialog title="T" onClose={vi.fn()}>
        <button type="button">first</button>
        <button type="button" disabled>
          off
        </button>
        <button type="button">last</button>
        <span hidden>
          <button type="button">out of sight</button>
        </span>
      </Dialog>,
    );
    const last = screen.getByRole("button", { name: "last" });
    last.focus();
    expect(fireEvent.keyDown(last, { key: "Tab" }), "the last that can be reached is the last, whatever is after it that cannot").toBe(false);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "first" }));
    view.unmount();
    render(
      <Dialog title="T" onClose={vi.fn()}>
        <p>nothing to press</p>
      </Dialog>,
    );
    expect(fireEvent.keyDown(box(), { key: "Tab" })).toBe(false);
    expect(document.activeElement).toBe(box());
  });
});
