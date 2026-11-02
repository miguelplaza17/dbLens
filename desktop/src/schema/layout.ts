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
export const GAP_X = 112;
export const GAP_Y = 40;
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
  const usable = links.filter((l) => ids.has(l.from) && ids.has(l.to) && l.from !== l.to);
  const linked = new Set(usable.flatMap((l) => [l.from, l.to]));
  const height = new Map(tables.map((t) => [tableId(t), cardHeight(t.columns.length)]));
  // About as tall as a 16:10 board holding every card would be.
  let area = 0;
  for (const h of height.values()) area += (CARD_WIDTH + GAP_Y) * (h + GAP_Y);
  const limit = Math.max(900, Math.sqrt(area / 1.6));

  const result = new Map<string, Point>();
  let right = MARGIN;
  let bottom = MARGIN;
  const connected = tables.filter((t) => linked.has(tableId(t)));
  if (connected.length > 0) {
    const g = new Graph();
    g.setGraph({ rankdir: "LR", nodesep: GAP_Y, ranksep: GAP_X, marginx: MARGIN, marginy: MARGIN });
    g.setDefaultEdgeLabel(() => ({}));
    for (const t of connected) g.setNode(tableId(t), { width: CARD_WIDTH, height: height.get(tableId(t))! });
    // Referenced table first, so it lands on the left.
    for (const l of usable) g.setEdge(l.to, l.from);
    dagreLayout(g);
    // dagre keeps each rank in one column; a rank taller than the limit
    // (fifty tables pointing at one) wraps into more columns, in dagre's order.
    const ranks = new Map<number, Array<{ id: string; y: number }>>();
    for (const t of connected) {
      const id = tableId(t);
      const n = g.node(id);
      const x = Math.round(n.x);
      if (!ranks.has(x)) ranks.set(x, []);
      ranks.get(x)!.push({ id, y: n.y });
    }
    interface Column {
      nodes: Array<{ id: string; y: number }>;
      rank: number;
      wrapped: boolean;
    }
    const columns: Column[] = [];
    for (const rankX of [...ranks.keys()].sort((a, b) => a - b)) {
      const rank = ranks.get(rankX)!.sort((a, b) => a.y - b.y);
      let total = 0;
      for (const n of rank) total += height.get(n.id)! + GAP_Y;
      if (total <= limit) {
        columns.push({ nodes: rank, rank: rankX, wrapped: false });
        continue;
      }
      const share = total / Math.ceil(total / limit);
      let nodes: Array<{ id: string; y: number }> = [];
      let used = 0;
      for (const n of rank) {
        const h = height.get(n.id)! + GAP_Y;
        if (nodes.length > 0 && used + h / 2 > share) {
          columns.push({ nodes, rank: rankX, wrapped: true });
          nodes = [];
          used = 0;
        }
        nodes.push(n);
        used += h;
      }
      if (nodes.length > 0) columns.push({ nodes, rank: rankX, wrapped: true });
    }
    // Columns side by side, a wrapped rank's columns closer together. Plain
    // ranks keep dagre's heights (shorter edges); wrapped ones are stacked and centered.
    const stacked = columns.map((c) => c.nodes.reduce((sum, n) => sum + height.get(n.id)! + GAP_Y, -GAP_Y));
    let top = Infinity;
    let bottomPlain = -Infinity;
    for (const c of columns) {
      if (c.wrapped) continue;
      for (const n of c.nodes) {
        top = Math.min(top, n.y - height.get(n.id)! / 2);
        bottomPlain = Math.max(bottomPlain, n.y + height.get(n.id)! / 2);
      }
    }
    const tallest = Math.max(isFinite(top) ? bottomPlain - top : 0, ...stacked);
    let x = MARGIN;
    columns.forEach((column, i) => {
      let y = MARGIN + (tallest - stacked[i]) / 2;
      for (const n of column.nodes) {
        const h = height.get(n.id)!;
        const at = column.wrapped ? y : n.y - h / 2 - (isFinite(top) ? top : 0) + MARGIN;
        result.set(n.id, { x: snap(x), y: snap(at) });
        y += h + GAP_Y;
        bottom = Math.max(bottom, at + h);
      }
      right = x + CARD_WIDTH;
      const next = columns[i + 1];
      x += CARD_WIDTH + (next && next.rank === column.rank ? GAP_Y : GAP_X);
    });
  }

  // Loose tables: in columns, each card into the shortest one; beside the
  // linked ones when those are tall, under them when they are wide.
  const loose = tables.filter((t) => !linked.has(tableId(t))).sort((a, b) => tableId(a).localeCompare(tableId(b)));
  if (loose.length > 0) {
    const step = CARD_WIDTH + GAP_Y;
    const beside = connected.length > 0 && bottom - MARGIN > (right - MARGIN) * 0.6;
    let left = MARGIN;
    let top = MARGIN;
    let count: number;
    if (connected.length === 0) {
      count = Math.max(1, Math.min(loose.length, Math.round(Math.sqrt(area * 1.6) / step)));
    } else if (beside) {
      left = right + GAP_X;
      let looseArea = 0;
      for (const t of loose) looseArea += step * (height.get(tableId(t))! + GAP_Y);
      count = Math.max(1, Math.ceil(looseArea / step / Math.max(bottom - MARGIN, 600)));
    } else {
      top = bottom + GAP_X;
      count = Math.max(1, Math.floor((right - MARGIN + GAP_Y) / step));
    }
    const heights = new Array<number>(count).fill(top);
    for (const t of loose) {
      let column = 0;
      for (let i = 1; i < count; i++) if (heights[i] < heights[column]) column = i;
      result.set(tableId(t), { x: left + column * step, y: heights[column] });
      heights[column] += height.get(tableId(t))! + GAP_Y;
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
