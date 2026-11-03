// A module that is closed, as the canvas draws it (docs/web-ui.md §9.3, step 108; the user's "the goal when we re-add a module is that it gets rendered like just a single box"): one node with the
// module's name, a line that says what is in it, a port on its edge for each of the module's inputs and outputs (a cable that is dragged to one ends at the node inside that it stands for: `connection.ts`),
// and the buttons that open it and take it out. Open, the module is its nodes, and the tag that is over each of its lanes closes it again.
import { Handle, Position } from "@xyflow/react";
import type { NodeProps } from "@xyflow/react";
import { useContext } from "react";
import type { CSSProperties } from "react";

import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { boxDetail, boxRows } from "./boxes";
import type { Box, BoxPort } from "./boxes";
import { ConnectingContext, keyOf } from "./connection";
import { memberHandle } from "./handles";
import { ADD_FREE_EFFECT_ID, Header } from "./nodes";
import { OpenModulesContext, openModuleId } from "./openModules";
import { BOX_ROW_HEIGHT } from "./toFlow";
import type { PatchNode } from "./toFlow";

/** How far down a box the first row of its ports is (the node's top border is 3 px, and its header 40, which a handle is measured under). */
const PORTS_TOP = 40;

/**
 * A port on the left or the right edge of a box, in the row that its label is in: an input of audio or of notes on the left (a cable ends there and none is started), an output of audio on the right
 * (a cable is started there). It is the port of a node inside the module (`BoxPort`), by the name of the handle that says which, and while a cable is dragged it says whether that node takes it: the strip
 * that takes it is the row.
 */
function BoxHandle({ port, direction, row }: { port: BoxPort; direction: "in" | "out"; row: number }) {
  const input = direction === "in";
  const dragged = useContext(ConnectingContext);
  // (The ports are asked by the node inside, which is the one the controller would be asked to draw the cable to.)
  const takes = dragged?.has(keyOf(port.member, port.handle)) ?? false;
  const classes = ["tang-port", `tang-port--${port.kind}`, input ? "tang-port--in" : "tang-port--out", ...(takes ? ["tang-port--takes"] : [])];
  const top = PORTS_TOP + row * BOX_ROW_HEIGHT;
  return (
    <Handle
      isConnectableStart={!input}
      isConnectableEnd={input}
      type={input ? "target" : "source"}
      position={input ? Position.Left : Position.Right}
      id={memberHandle(port.handle, port.member)}
      className={classes.join(" ")}
      style={takes ? { top, height: BOX_ROW_HEIGHT } : { top: top + BOX_ROW_HEIGHT / 2 }}
      title={port.label}
    />
  );
}

/**
 * A port on the top or the bottom edge of a box for a cable of modulation that crosses it: along the top, where a modulator outside the module drives something inside (another modulator can be drawn to the
 * same port, since the same node inside can be driven by more than one), and along the bottom, where a modulator inside drives something outside (its cable is drawn from the node inside, which is on the canvas
 * while the module is open).
 */
function BoxModulationHandle({ port, direction, place, count }: { port: BoxPort; direction: "in" | "out"; place: number; count: number }) {
  const input = direction === "in";
  const dragged = useContext(ConnectingContext);
  const takes = dragged?.has(keyOf(port.member, port.handle)) ?? false;
  const classes = ["tang-port", input ? "tang-port--param" : "tang-port--mod", ...(input ? ["tang-port--driven"] : []), ...(takes ? ["tang-port--takes"] : [])];
  return (
    <Handle
      isConnectableStart={false}
      isConnectableEnd={input}
      type={input ? "target" : "source"}
      position={input ? Position.Top : Position.Bottom}
      id={memberHandle(port.handle, port.member)}
      className={classes.join(" ")}
      style={{ left: `${String(((place + 1) / (count + 1)) * 100)}%` }}
      title={port.label}
    />
  );
}

/** The `id` of the button that takes the module `node` out (the focus goes to the bar's "+ Effect" when it has: the box is gone). */
const removeModuleId = (node: number): string => `remove-module-${String(node)}`;

/**
 * The button of a box that takes the module out of the session, with everything that is in it (its lanes, their plugins and the modulators, and the cables at them): a menu that asks first, in words that say what goes
 * (undo puts it all back, in one step). A session keeps its last lane, so that a module that has them all cannot be taken out.
 */
function RemoveModuleMenu({ node, name, box }: { node: number; name: string; box: Box }) {
  const editor = useEditor();
  const items: MenuItem<"keep" | "remove">[] = [
    { id: "keep", label: `Keep ${name}` },
    { id: "remove", label: `Remove ${name} for good: the ${boxDetail(box)} in it go with it` },
  ];
  return (
    <MenuButton
      id={removeModuleId(node)}
      label={`Remove ${name}`}
      className="tang-node__remove-menu"
      items={items}
      onChoose={(choice) => {
        if (choice === "keep") return;
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.removeNode(node).then((removed) => {
          if (removed) focusWhenPossible(ADD_FREE_EFFECT_ID);
        });
      }}
    >
      ×
    </MenuButton>
  );
}

/**
 * A module that is closed: one node, with the ports of the module on its edges and a line that says what is in it. Its button opens it, which puts the nodes that are in it on the canvas (and takes this
 * one away), and its "×" asks whether to take it out with all that is in it.
 */
export function ModuleNode({ data }: NodeProps<PatchNode>) {
  const modules = useContext(OpenModulesContext);
  const box = data.box;
  if (box === null) return null;
  const rows = boxRows(box);
  return (
    <div className="tang-node tang-node--module nokey" style={{ "--box-rows": rows } as CSSProperties}>
      {box.inputs.map((port, row) => (
        <BoxHandle key={memberHandle(port.handle, port.member)} port={port} direction="in" row={row} />
      ))}
      {box.outputs.map((port, row) => (
        <BoxHandle key={memberHandle(port.handle, port.member)} port={port} direction="out" row={row} />
      ))}
      {box.modIn.map((port, place) => (
        <BoxModulationHandle key={memberHandle(port.handle, port.member)} port={port} direction="in" place={place} count={box.modIn.length} />
      ))}
      {box.modOut.map((port, place) => (
        <BoxModulationHandle key={memberHandle(port.handle, port.member)} port={port} direction="out" place={place} count={box.modOut.length} />
      ))}
      <Header name={data.name} badge="Module">
        <span className="tang-node__actions">
          <button
            type="button"
            id={openModuleId(data.node)}
            className="tang-menu-button tang-node__open nodrag nopan"
            aria-label={`Open ${data.name}`}
            title={`Show what is in ${data.name}: ${boxDetail(box)}`}
            aria-expanded={false}
            onClick={() => {
              modules.toggle(data.node);
            }}
          >
            ▸
          </button>
          <RemoveModuleMenu node={data.node} name={data.name} box={box} />
        </span>
      </Header>
      <ul className="tang-box__ports" aria-label={`Ports of ${data.name}`}>
        {Array.from({ length: rows }, (_, row) => {
          const input = box.inputs[row];
          const output = box.outputs[row];
          return (
            <li key={row} className="tang-box__row">
              <span className={`tang-box__in${input?.kind === "notes" ? " tang-box__in--notes" : ""}`} title={input?.label}>
                {input ? `${input.kind === "notes" ? "♪ " : ""}${input.label}` : ""}
              </span>
              <span className="tang-box__out" title={output?.label}>
                {output?.label ?? ""}
              </span>
            </li>
          );
        })}
      </ul>
      <p className="tang-node__detail tang-box__detail">{boxDetail(box)}</p>
    </div>
  );
}
