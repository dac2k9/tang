// The names of the handles of the nodes on the canvas: a handle is where a cable starts or ends, and its name is the port that it is (docs/web-ui.md §9.3). A port of a node that is inside a
// module that is drawn as a box (step 108) is a handle of the box, named after the port with the node that it is the port of (`memberHandle`): a cable that crosses the edge of the box ends there.
import type { EdgeId, NodeId, Port } from "../../protocol";

/**
 * The settings of a modulator that a cable ends at, by the names of their handles (the names of the ports), in the order a
 * modulator has them: an LFO's rate, the four stages of an envelope.
 */
export const SETTING_HANDLES = ["rate", "attack", "decay", "sustain", "release"] as const;

export type SettingHandle = (typeof SETTING_HANDLES)[number];

/** The name of the handle of a parameter of a plugin, by the plugin's own index. */
export const paramHandle = (index: number): string => `param-${String(index)}`;

/** The name of the handle in a modulator's node where a cable can end to drive the depth of the cable `edge`, which starts at that modulator. */
export const depthHandle = (edge: EdgeId): string => `depth-${String(edge)}`;

/** The name of the handle of a node where a port is. */
export function handleOf(port: Port): string {
  switch (port.type) {
    case "audioIn":
    case "audioInSide":
    case "audioOut":
    case "eventsIn":
    case "eventsOut":
    case "modOut":
    case "rate":
    case "attack":
    case "decay":
    case "sustain":
    case "release":
    case "volume":
    case "mix":
    case "pan":
      // (A cable ends at the volume of a lane or a group, the mix of an effect and the pan of a group by the name of the port: the host-side values.)
      return port.type;
    case "param":
      return paramHandle(port.index);
    case "depth":
      return depthHandle(port.edge);
  }
}

/** The name of the handle of a box where the port `handle` of the node `member`, which is inside the module, is: the port's name and the node that has it. */
export const memberHandle = (handle: string, member: NodeId): string => `${handle}@${String(member)}`;

const MEMBER_HANDLE = /^(.+)@(\d+)$/;

/**
 * What a handle of a box stands for: the port (by its name) of the node inside the module that it is the port of. None for a handle that is not a box's (the handle of a node's own port, whose name has no `@`).
 */
export function memberOfHandle(handle: string | null | undefined): { handle: string; member: NodeId } | null {
  const found = handle ? MEMBER_HANDLE.exec(handle) : null;
  const [, name, member] = found ?? [];
  return name !== undefined && member !== undefined ? { handle: name, member: Number(member) } : null;
}

/** The name of the port that `handle` is, whether it is a handle of a node or of a box: a box's handle is the handle of the node inside that it stands for. */
export const plainHandle = (handle: string | null | undefined): string | null => memberOfHandle(handle)?.handle ?? handle ?? null;
