// Card geometry and the automatic layout of the schema board (BoardLayout in
// SchemaBoard.swift). Sizes are board units: CSS pixels at 100% zoom.
//
// Tables that have foreign keys go through dagre (left to right: the tables
// others point at on the left, the ones pointing at them to their right);
// tables without any link are packed in columns below them.

import { Graph, layout as dagreLayout } from "@dagrejs/dagre";
import type { SchemaColumn, SchemaLink, SchemaTable } from "../api";

export const CARD_WIDTH = 248;
export const HEADER = 40;
export const ROW = 24;
/** Space under the last row. */
export const FOOT = 4;
export const GAP_X = 144;
export const GAP_Y = 48;
export const MARGIN = 40;
/** Cards snap to this grid while dragged. */
export const SNAP = 8;

export interface Point {
  x: number;
  y: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** "schema.name", the id links use. */
export const tableId = (t: SchemaTable) => `${t.schema}.${t.name}`;

/** The columns a card shows: all, or only primary and foreign keys when folded. */
export function rows(table: SchemaTable, collapsed: boolean, foreignKeys: Set<string> | undefined): SchemaColumn[] {
  if (!collapsed) return table.columns;
  return table.columns.filter((c) => c.isPrimaryKey || !!foreignKeys?.has(c.name));
}

export function cardHeight(rowCount: number): number {
  return HEADER + rowCount * ROW + FOOT;
}

/** Foreign-key columns per table id. */
export function foreignKeyColumns(links: SchemaLink[]): Map<string, Set<string>> {
  const map = new Map<string, Set<string>>();
  for (const link of links) {
    let set = map.get(link.from);
    if (!set) map.set(link.from, (set = new Set()));
    for (const c of link.fromColumns) set.add(c);
  }
  return map;
}

/** A place for every table, laid out from scratch. */
export function autoLayout(tables: SchemaTable[], links: SchemaLink[]): Map<string, Point> {
  const ids = new Set(tables.map(tableId));
  const usable = links.filter((l) => ids.has(l.from) && ids.has(l.to));
  const linked = new Set(usable.flatMap((l) => [l.from, l.to]));
  const result = new Map<string, Point>();

  let bottom = MARGIN;
  let right = MARGIN + CARD_WIDTH;
  const connected = tables.filter((t) => linked.has(tableId(t)));
  if (connected.length > 0) {
    const g = new Graph({ multigraph: false });
    g.setGraph({ rankdir: "LR", nodesep: GAP_Y, ranksep: GAP_X, marginx: MARGIN, marginy: MARGIN });
    g.setDefaultEdgeLabel(() => ({}));
    for (const t of connected) g.setNode(tableId(t), { width: CARD_WIDTH, height: cardHeight(t.columns.length) });
    for (const l of usable) {
      // Referenced table first, so it lands on the left.
      if (l.from !== l.to) g.setEdge(l.to, l.from);
    }
    dagreLayout(g);
    for (const t of connected) {
      const id = tableId(t);
      const n = g.node(id);
      const h = cardHeight(t.columns.length);
      const p = { x: snap(n.x - CARD_WIDTH / 2), y: snap(n.y - h / 2) };
      result.set(id, p);
      bottom = Math.max(bottom, p.y + h);
      right = Math.max(right, p.x + CARD_WIDTH);
    }
    bottom += GAP_X;
  }

  // Loose tables: columns, each card into the shortest one.
  const loose = tables.filter((t) => !linked.has(tableId(t))).sort((a, b) => tableId(a).localeCompare(tableId(b)));
  if (loose.length > 0) {
    const step = CARD_WIDTH + GAP_Y;
    const byWidth = connected.length > 0 ? Math.floor((right - MARGIN + GAP_Y) / step) : 0;
    const count = Math.max(1, Math.min(loose.length, Math.max(byWidth, Math.ceil(Math.sqrt(loose.length * 1.5)))));
    const heights = new Array<number>(count).fill(connected.length > 0 ? bottom : MARGIN);
    for (const t of loose) {
      let column = 0;
      for (let i = 1; i < count; i++) if (heights[i] < heights[column]) column = i;
      result.set(tableId(t), { x: MARGIN + column * step, y: heights[column] });
      heights[column] += cardHeight(t.columns.length) + GAP_Y;
    }
  }
  return result;
}

export function snap(value: number): number {
  return Math.round(value / SNAP) * SNAP;
}

export function union(a: Rect | null, b: Rect): Rect {
  if (!a) return b;
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return { x, y, width: Math.max(a.x + a.width, b.x + b.width) - x, height: Math.max(a.y + a.height, b.y + b.height) - y };
}

export function intersects(a: Rect, b: Rect): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}
