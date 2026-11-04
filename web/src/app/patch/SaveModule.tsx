// (Step 109: the field for a name is the field of the button that collapses what is chosen into one box in place, too: `CollapseSelectionButton`, with the name that the box is called by and `Collapse` for its button.)
// "Save as a module" (steps 104 and 105): the button that saves plugins as a module of the library: all of what is asked for, as it is (the plugins in order, each with its preset, its values, its mix or its volume, its pinned
// knobs and whether the stage before it feeds it, and the cables between them), to be added to this session or to another from the bar's "+ Effect". There are two of them: the one in the cell of a lane, beside its mute and its solo,
// which saves the chain of effects of the lane (step 104), and the one on the canvas's bar of choosing, which saves the nodes that were chosen (step 105). Either opens a small field for the name under the button; Enter or Save asks
// the server (`Editor.saveModule`, `saveSelectionAsModule`: the server says what a name may be, a module that has the name is written over), Escape and Cancel leave. A name that the server refuses is said among the notices and the
// field stays, with the name selected, to be tried again; when it is saved the notices say so. The field is drawn in the page and not in the node (the canvas moves and the node is in front of the next), so it closes when the page is
// pressed anywhere else, scrolled or sized.
import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";

import type { NodeId, NodeUi } from "../../protocol";
import { useEditor } from "../ClientProvider";
import { COLLAPSE_CHOSEN } from "./contextMenus";
import { collapseTitle, collapseWhyNot } from "./selection";
import type { Collapsible } from "./selection";
import "./SaveModule.css";

/** About how wide the field is, to keep it in the window when the button is near its right edge. */
const PANEL_WIDTH = 300;

/** Where the field for the name is, in the pixels of the window. */
export interface DialogPlace {
  left: number;
  top: number;
}

/** The place for a field under a button, or at a point (the pointer, for a menu that opened it): in the window, whatever the edge it is near. */
export function dialogPlace(left: number, top: number): DialogPlace {
  return { left: Math.max(8, Math.min(left, window.innerWidth - PANEL_WIDTH - 8)), top };
}

/** How the field for a name differs from the one that saves a module (the file name of a module of the library). */
export interface NameOptions {
  /** What the button that asks is called: `Save`, or `Collapse`. */
  action?: string;
  /** What the field has in it when it opens, selected: a name that Enter keeps. */
  initial?: string;
  /** What the field says it is for, to a pointer and a screen reader. */
  hint?: string;
}

/** What the field says it is for when it is for the file name of a module of the library. */
const FILE_HINT = "The name of the module: a file name, to which .toml is added; a module that has the name is written over";

interface ModuleNameDialogProps extends NameOptions {
  /** What the field is called (a dialog of that name). */
  label: string;
  at: DialogPlace;
  /** Asks the server to save under the name as it was typed (the page says why if it was refused); true if it was saved. */
  save: (name: string) => Promise<boolean>;
  /** The field is done with: it was saved, or left. `refocus` is whether the focus goes back to what opened it (Escape and Cancel, and a save: the person is still there). */
  onClose: (refocus: boolean) => void;
  /** What opened it, if it is a button: a press on it is not a press elsewhere, and does not close the field (the button does that itself). */
  anchor?: RefObject<HTMLElement | null>;
}

/** The field for the name of a module, with its buttons, drawn in the page at `at`: it has the focus when it opens, and goes when the page is pressed anywhere else, scrolled or sized. */
export function ModuleNameDialog({ label, at, save, onClose, anchor, action = "Save", initial = "", hint = FILE_HINT }: ModuleNameDialogProps) {
  const [text, setText] = useState(initial);
  const [asking, setAsking] = useState(false);
  const busy = useRef(false);
  const panel = useRef<HTMLFormElement>(null);
  const field = useRef<HTMLInputElement>(null);

  // The field has the focus when it opens (before the page is painted, so that a letter that is typed is never the page's: its keys play notes), with the name that it starts with selected, so that a letter replaces it.
  useLayoutEffect(() => {
    field.current?.focus();
    field.current?.select();
  }, []);

  // A press anywhere else, the page being scrolled or sized: the field goes (and the focus stays where the person put it).
  useEffect(() => {
    const away = (event: Event): void => {
      const target = event.target;
      if (target instanceof Node && (panel.current?.contains(target) === true || anchor?.current?.contains(target) === true)) return;
      onClose(false);
    };
    const leave = (): void => {
      onClose(false);
    };
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("wheel", leave, true);
    window.addEventListener("resize", leave);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("wheel", leave, true);
      window.removeEventListener("resize", leave);
    };
  }, [anchor, onClose]);

  // Asks for one at a time, and for a name that is something. (The server decides what a name may be; the page does not have its rule.)
  const ask = (): void => {
    if (busy.current || text.trim() === "") return;
    busy.current = true;
    setAsking(true);
    // (If it is refused the page says so: `Editor` puts the reason among the notices.)
    void save(text).then((done) => {
      busy.current = false;
      setAsking(false);
      if (done) {
        onClose(true);
      } else {
        field.current?.focus();
        field.current?.select();
      }
    });
  };

  return createPortal(
    <form
      ref={panel}
      role="dialog"
      aria-label={label}
      className="tang-module-name"
      style={{ position: "fixed", left: `${String(at.left)}px`, top: `${String(at.top)}px`, width: `${String(PANEL_WIDTH)}px` }}
      onSubmit={(event) => {
        event.preventDefault();
        ask();
      }}
      onKeyDown={(event) => {
        if (event.key !== "Escape") return;
        // (Not the page's: Escape closes what is open there.)
        event.preventDefault();
        event.stopPropagation();
        onClose(true);
      }}
    >
      <input
        ref={field}
        className="tang-module-name__field"
        type="text"
        aria-label="Module name"
        placeholder="Module name"
        title={hint}
        spellCheck={false}
        autoComplete="off"
        value={text}
        onChange={(event) => {
          setText(event.target.value);
        }}
      />
      <button type="submit" className="tang-module-name__button" disabled={asking || text.trim() === ""}>
        {action}
      </button>
      <button
        type="button"
        className="tang-module-name__button"
        onClick={() => {
          onClose(true);
        }}
      >
        Cancel
      </button>
    </form>,
    document.body,
  );
}

interface ModuleNameButtonProps extends NameOptions {
  /** What the button is called, and the field that it opens (a dialog of that name). */
  label: string;
  /** What it says to a pointer (the label and an ellipsis if it is not told). */
  title?: string;
  /** The class of the button. */
  className: string;
  /** What is on the button. */
  children: ReactNode;
  /** Whether it can be pressed: nothing is asked of the server for a button that cannot. */
  disabled?: boolean;
  /** Asks the server to save under the name as it was typed (the page says why if it was refused); true if it was saved. */
  save: (name: string) => Promise<boolean>;
}

/** The button and the field for the name that opens under it, whatever it is that is saved. */
function ModuleNameButton({ label, title, className, children, disabled = false, save, ...options }: ModuleNameButtonProps) {
  const [place, setPlace] = useState<DialogPlace | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const open = place !== null;

  const close = useCallback((refocus: boolean): void => {
    setPlace(null);
    if (refocus) trigger.current?.focus();
  }, []);

  const show = (): void => {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    setPlace(dialogPlace(box.left, box.bottom + 4));
  };

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={className}
        disabled={disabled}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={title ?? `${label}…`}
        onClick={() => {
          if (open) close(false);
          else show();
        }}
      >
        {children}
      </button>
      {place ? <ModuleNameDialog label={label} at={place} save={save} onClose={close} anchor={trigger} {...options} /> : null}
    </>
  );
}

/** The button in the cell of a lane that saves the chain of effects of the lane (`node` is the lane: the server finds the chain), whose name the nodes call it by is `name`. */
export const SaveModuleButton = memo(function SaveModuleButton({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const save = useCallback((text: string) => editor.saveModule(node, text), [editor, node]);
  return (
    <ModuleNameButton label={`Save the chain of ${name} as a module`} className="tang-flag tang-flag--module nodrag nopan" save={save}>
      ⤓
    </ModuleNameButton>
  );
});

/**
 * The button on the canvas's bar of choosing that saves the nodes that were chosen (`nodes`: the plugins and the modulators that a module can hold, which `modulePluginsOf` picks out of what is chosen, `plugins` of them plugins).
 * It cannot be pressed with no plugin: a module is made of effects and instruments, and a modulator goes with the ones that it drives.
 */
export function SaveSelectionButton({ nodes, plugins }: { nodes: readonly NodeId[]; plugins: number }) {
  const editor = useEditor();
  const save = useCallback((text: string) => editor.saveSelectionAsModule(nodes, text), [editor, nodes]);
  return (
    <ModuleNameButton label="Save the chosen nodes as a module" className="tang-patch__arrange" disabled={plugins === 0} save={save}>
      Save as module…
    </ModuleNameButton>
  );
}

/** What the field for the name of a box says, and starts with: the name is only what the box is called, and a name of nothing is `Module`. */
export const COLLAPSE_NAME: NameOptions = { action: "Collapse", initial: "Module", hint: "What the box is called: it is the module's own name, and not a file" };

/**
 * The button on the canvas's bar of choosing that collapses the nodes that were chosen into one module where they are (step 109: the user's "compact/collapse them into a module in place ... to clean up the ui a bit"): `collapsible`
 * is what that takes (`collapsibleOf`), and `place` where the box is drawn, the top left of what it hides. It opens the field for the name that the box is called by, which starts as `Module`; Enter or Collapse asks the server
 * (`Editor.collapseIntoModule`), which says what it refuses. It cannot be pressed with nothing that a module can take.
 */
export function CollapseSelectionButton({ collapsible, chosen, place }: { collapsible: Collapsible; chosen: number; place: NodeUi | undefined }) {
  const editor = useEditor();
  const collapse = useCallback((text: string) => editor.collapseIntoModule(collapsible.nodes, text, place).then((box) => box !== null), [editor, collapsible.nodes, place]);
  const why = collapseWhyNot(collapsible, chosen);
  const title = collapseTitle(collapsible, chosen);
  return (
    <ModuleNameButton label={COLLAPSE_CHOSEN} title={title} className="tang-patch__arrange" disabled={why !== null} save={collapse} {...COLLAPSE_NAME}>
      Collapse…
    </ModuleNameButton>
  );
}
