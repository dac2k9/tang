import { useEffect, useId, useRef } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { createPortal } from "react-dom";

import "./Dialog.css";

export interface DialogProps {
  /** What the dialog is for, which a screen reader reads when it opens. */
  title: string;
  /** Asked for by Escape and by a press on what is behind the dialog. */
  onClose: () => void;
  /** More classes for the box, for a dialog that is wider than the usual. */
  className?: string;
  children: ReactNode;
}

/** What a Tab can land on in a dialog. */
const FOCUSABLE = 'button:not(:disabled), input:not(:disabled):not([type="hidden"]), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])';

/** The elements of `box` that a Tab goes between, in order: what is shown, and can take the focus. */
function focusables(box: HTMLElement): HTMLElement[] {
  return [...box.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((element) => element.closest("[hidden]") === null);
}

/**
 * A box over the page that has to be answered before the page is used again (step 116: the picker of sounds): drawn in the page's body, over a backdrop, with the focus in it (on what is marked `data-autofocus`, else on the box) and
 * given back to what had it when the dialog goes. Escape, and a press on the backdrop, ask it to close; Tab goes round what is in it and does not leave. Its keys are its own: the page behind it has keys for the piano, for undo, for save
 * and for the canvas, and none of them is for a person who is typing in a dialog (a key that goes up is let through: a note that a key started before the dialog is ended by it).
 */
export function Dialog({ title, onClose, className, children }: DialogProps) {
  const titleId = useId();
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const before = document.activeElement;
    (box.current?.querySelector<HTMLElement>("[data-autofocus]") ?? box.current)?.focus();
    return () => {
      if (before instanceof HTMLElement && before.isConnected) before.focus();
    };
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab" || !box.current) return;
    const items = focusables(box.current);
    const first = items[0];
    const last = items.at(-1);
    const active = document.activeElement;
    if (first === undefined || last === undefined) {
      event.preventDefault();
      box.current.focus();
    } else if (event.shiftKey && (active === first || active === box.current)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return createPortal(
    <div
      className="tang-dialog__backdrop"
      onMouseDown={(event) => {
        // (Only a press on the backdrop itself: one that starts in the box and lets go outside it is a drag, of a selection of text, say.)
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div ref={box} className={className === undefined ? "tang-dialog" : `tang-dialog ${className}`} role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1} onKeyDown={onKeyDown}>
        <h2 id={titleId} className="tang-dialog__title">
          {title}
        </h2>
        {children}
      </div>
    </div>,
    document.body,
  );
}
