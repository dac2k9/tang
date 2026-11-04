// Where the canvas has to go to show a node (docs/web-ui.md §9.3): the rectangle a node has from
// the layout, and the view that puts a rectangle in the middle of the screen. All of it is
// arithmetic on numbers; moving the canvas is the view's business (`PatchView`).
import type { Graph, NodeId } from "../../protocol";
import { heightOfData, NODE_WIDTH } from "./toFlow";
import type { Flow } from "./toFlow";

/** A rectangle, in the canvas's own units. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** React Flow's viewport: a point of the canvas is on the screen at (x, y) plus the zoom times the point. */
export interface View {
  x: number;
  y: number;
  zoom: number;
}

export interface Size {
  width: number;
  height: number;
}

/** How much of the screen is left clear round a node that is shown, in pixels. */
export const MARGIN = 32;

/**
 * Where node `id` is on the canvas, as the layout has it: its place, as wide as every node is and as tall as the layout
 * allows for it, with the scope it shows and the rows of the cables it lists (the components' styles keep to that). Null for a node that is not there.
 */
export function rectOf(graph: Graph, flow: Flow, id: NodeId): Rect | null {
  const placed = flow.nodes.find((node) => node.id === String(id));
  const node = graph.nodes.find((n) => n.id === id);
  if (!placed || !node) return null;
  return { x: placed.position.x, y: placed.position.y, width: NODE_WIDTH, height: heightOfData(node, placed.data) };
}

/**
 * The view that shows `rect` on a screen of `size`, if the view `view` does not show all of it, with `MARGIN` to spare: the
 * rectangle in the middle of the screen, at the zoom the view has (it is the person's), or lower if the rectangle does not
 * fit at that, and never lower than `minZoom`. Null if all of it is in the view already, and if the screen has no size to
 * judge by (the canvas is not on the page).
 */
export function viewToShow(rect: Rect, view: View, size: Size, minZoom: number): View | null {
  if (size.width <= 0 || size.height <= 0) return null;
  const left = rect.x * view.zoom + view.x;
  const top = rect.y * view.zoom + view.y;
  const right = left + rect.width * view.zoom;
  const bottom = top + rect.height * view.zoom;
  if (left >= MARGIN && top >= MARGIN && right <= size.width - MARGIN && bottom <= size.height - MARGIN) return null;
  // Out as far as it takes to fit, and never in.
  const fits = Math.min((size.width - 2 * MARGIN) / rect.width, (size.height - 2 * MARGIN) / rect.height);
  const zoom = Math.max(minZoom, Math.min(view.zoom, fits));
  return { x: size.width / 2 - (rect.x + rect.width / 2) * zoom, y: size.height / 2 - (rect.y + rect.height / 2) * zoom, zoom };
}
