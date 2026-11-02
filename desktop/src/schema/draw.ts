// Paints the schema board on a 2D canvas: the dot grid, foreign-key curves,
// table cards, and the minimap. Only what is in view is drawn, and below
// about 45% zoom cards drop their column text for bars, so a board with
// hundreds of tables stays smooth. Colors come from theme.css.

import type { SchemaTable } from "../api";
import { CARD_WIDTH, FOOT, HEADER, ROW, intersects, rows, tableId, type Point, type Rect } from "./layout";
import type { Board, BoardView } from "./store";

export interface Colors {
  background: string;
  dots: string;
  card: string;
  header: string;
  border: string;
  text: string;
  secondary: string;
  tertiary: string;
  accent: string;
  accentText: string;
  accentSoft: string;
  accentFaint: string;
  key: string;
  shadow: string;
  link: string;
  minimap: string;
  fontUI: string;
  fontMono: string;
}

export function readColors(): Colors {
  const style = getComputedStyle(document.documentElement);
  const v = (name: string) => style.getPropertyValue(name).trim();
  return {
    background: v("--surface"),
    dots: v("--separator-strong"),
    card: v("--raised"),
    header: v("--control"),
    border: v("--separator-strong"),
    text: v("--text"),
    secondary: v("--text-secondary"),
    tertiary: v("--text-tertiary"),
    accent: v("--accent"),
    accentText: v("--accent-text"),
    accentSoft: v("--accent-soft"),
    accentFaint: v("--accent-faint"),
    key: v("--warning"),
    shadow: v("--overlay"),
    link: v("--text-tertiary"),
    minimap: v("--window"),
    fontUI: v("--font-ui"),
    fontMono: v("--font-mono"),
  };
}

/** What the pointer and selection add to the picture. */
export interface Marks {
  hovered: string | null;
  selection: Set<string>;
  /** Cards in drawing order, last on top. */
  order: string[];
}

/** Below this zoom cards show bars instead of column text. */
const DETAIL_ZOOM = 0.45;
const RADIUS = 10;

// Truncated text, by font, width and text: widths in board units don't change with zoom.
const fitted = new Map<string, string>();

function fit(ctx: CanvasRenderingContext2D, text: string, width: number): string {
  const key = `${ctx.font}|${Math.round(width)}|${text}`;
  const known = fitted.get(key);
  if (known !== undefined) return known;
  let result = text;
  if (width <= 0) result = "";
  else if (ctx.measureText(text).width > width) {
    let lo = 0;
    let hi = text.length;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (ctx.measureText(text.slice(0, mid) + "…").width <= width) lo = mid;
      else hi = mid - 1;
    }
    result = lo > 0 ? text.slice(0, lo) + "…" : "";
  }
  if (fitted.size > 20000) fitted.clear();
  fitted.set(key, result);
  return result;
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

/** The world rectangle on screen. */
export function visibleRect(board: Board): Rect {
  return {
    x: -board.pan.x / board.zoom,
    y: -board.pan.y / board.zoom,
    width: board.viewport.width / board.zoom,
    height: board.viewport.height / board.zoom,
  };
}

export function cardRect(board: Board, view: BoardView, table: SchemaTable): Rect | null {
  const id = tableId(table);
  const p = board.positions.get(id);
  if (!p) return null;
  const n = rows(table, board.collapsed.has(id), view.foreignKeys.get(id)).length;
  return { x: p.x, y: p.y, width: CARD_WIDTH, height: HEADER + n * ROW + FOOT };
}

/** The card under a board point, topmost first. */
export function hitCard(board: Board, view: BoardView, marks: Marks, world: Point): SchemaTable | null {
  for (let i = marks.order.length - 1; i >= 0; i--) {
    const table = view.byId.get(marks.order[i]);
    if (!table) continue;
    const r = cardRect(board, view, table);
    if (r && world.x >= r.x && world.x <= r.x + r.width && world.y >= r.y && world.y <= r.y + r.height) return table;
  }
  return null;
}

export function drawBoard(ctx: CanvasRenderingContext2D, dpr: number, board: Board, view: BoardView, marks: Marks, c: Colors, showSchema: boolean) {
  const { width, height } = board.viewport;
  const z = board.zoom;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.fillStyle = c.background;
  ctx.fillRect(0, 0, width, height);
  drawDots(ctx, board, c);

  const seen = visibleRect(board);
  const margin = 80 / z;
  const area = { x: seen.x - margin, y: seen.y - margin, width: seen.width + margin * 2, height: seen.height + margin * 2 };
  ctx.setTransform(dpr * z, 0, 0, dpr * z, dpr * board.pan.x, dpr * board.pan.y);

  const lit = (id: string) => marks.hovered === id || marks.selection.has(id);
  drawLinks(ctx, board, view, area, c, lit);

  const detail = z >= DETAIL_ZOOM;
  const shadows = detail && view.tables.length <= 400;
  for (const id of marks.order) {
    const table = view.byId.get(id);
    if (!table) continue;
    const r = cardRect(board, view, table);
    if (!r || !intersects(r, area)) continue;
    drawCard(ctx, board, view, table, r, c, {
      z,
      detail,
      shadow: shadows,
      selected: marks.selection.has(id),
      hovered: marks.hovered === id,
      showSchema,
    });
  }
}

function drawDots(ctx: CanvasRenderingContext2D, board: Board, c: Colors) {
  const { width, height } = board.viewport;
  let step = 24 * board.zoom;
  while (step < 16) step *= 2;
  const dot = Math.max(1, 1.5 * Math.min(board.zoom, 1));
  ctx.fillStyle = c.dots;
  let x0 = board.pan.x % step;
  if (x0 < 0) x0 += step;
  let y0 = board.pan.y % step;
  if (y0 < 0) y0 += step;
  ctx.beginPath();
  for (let x = x0; x <= width; x += step) {
    for (let y = y0; y <= height; y += step) ctx.rect(x, y, dot, dot);
  }
  ctx.fill();
}

/** Where a link leaves its card: the side facing the other card, at the column's row. */
function rowY(board: Board, view: BoardView, table: SchemaTable, origin: Point, column: string | undefined): number {
  const id = tableId(table);
  const shown = rows(table, board.collapsed.has(id), view.foreignKeys.get(id));
  const index = column ? shown.findIndex((r) => r.name === column) : -1;
  return index < 0 ? origin.y + HEADER / 2 : origin.y + HEADER + index * ROW + ROW / 2;
}

function drawLinks(ctx: CanvasRenderingContext2D, board: Board, view: BoardView, area: Rect, c: Colors, lit: (id: string) => boolean) {
  const z = board.zoom;
  const plain = new Path2D();
  const bright = new Path2D();
  const plainEnds = new Path2D();
  const brightEnds = new Path2D();
  let anyBright = false;
  const w = CARD_WIDTH;
  for (const link of view.links) {
    const source = view.byId.get(link.from);
    const target = view.byId.get(link.to);
    if (!source || !target) continue;
    const from = board.positions.get(link.from);
    const to = board.positions.get(link.to);
    if (!from || !to) continue;
    const y1 = rowY(board, view, source, from, link.fromColumns[0]);
    const y2 = rowY(board, view, target, to, link.toColumns[0]);
    let sx: number, ex: number, bend: number;
    if (to.x > from.x + w) {
      sx = from.x + w;
      ex = to.x;
      bend = 1;
    } else if (to.x + w < from.x) {
      sx = from.x;
      ex = to.x + w;
      bend = -1;
    } else {
      sx = from.x + w;
      ex = to.x + w;
      bend = 0;
    }
    const minX = Math.min(sx, ex) - 80;
    const minY = Math.min(y1, y2);
    if (!intersects({ x: minX, y: minY, width: Math.abs(ex - sx) + 160, height: Math.abs(y2 - y1) + 1 }, area)) continue;
    const on = lit(link.from) || lit(link.to);
    const path = on ? bright : plain;
    const ends = on ? brightEnds : plainEnds;
    anyBright ||= on;
    path.moveTo(sx, y1);
    if (bend === 0) {
      const out = 72;
      path.bezierCurveTo(sx + out, y1, ex + out, y2, ex, y2);
    } else {
      const reach = Math.max(Math.abs(ex - sx) / 2, 48);
      path.bezierCurveTo(sx + reach * bend, y1, ex - reach * bend, y2, ex, y2);
    }
    const r = 3 / Math.max(z, 0.5);
    ends.moveTo(sx + r, y1);
    ends.arc(sx, y1, r, 0, Math.PI * 2);
    ends.moveTo(ex + r, y2);
    ends.arc(ex, y2, r, 0, Math.PI * 2);
  }
  ctx.lineCap = "round";
  ctx.setLineDash([4 / z, 4 / z]);
  ctx.lineWidth = 1.25 / z;
  ctx.strokeStyle = c.link;
  ctx.stroke(plain);
  ctx.fillStyle = c.link;
  ctx.fill(plainEnds);
  if (anyBright) {
    ctx.lineWidth = 2 / z;
    ctx.strokeStyle = c.accent;
    ctx.stroke(bright);
    ctx.fillStyle = c.accent;
    ctx.fill(brightEnds);
  }
  ctx.setLineDash([]);
}

interface CardStyle {
  z: number;
  detail: boolean;
  shadow: boolean;
  selected: boolean;
  hovered: boolean;
  showSchema: boolean;
}

function drawCard(ctx: CanvasRenderingContext2D, board: Board, view: BoardView, table: SchemaTable, r: Rect, c: Colors, s: CardStyle) {
  const id = tableId(table);
  const collapsed = board.collapsed.has(id);
  const fks = view.foreignKeys.get(id);
  const shown = rows(table, collapsed, fks);
  const px = 1 / s.z;

  // Body and shadow.
  ctx.save();
  ctx.beginPath();
  roundRect(ctx, r.x, r.y, r.width, r.height, RADIUS);
  if (s.shadow) {
    ctx.shadowColor = c.shadow;
    ctx.shadowBlur = (s.selected ? 18 : 10) * Math.min(s.z, 1);
    ctx.shadowOffsetY = 3 * Math.min(s.z, 1);
  }
  ctx.fillStyle = c.card;
  ctx.fill();
  ctx.restore();

  // Header band, clipped to the card's corners.
  ctx.save();
  ctx.beginPath();
  roundRect(ctx, r.x, r.y, r.width, r.height, RADIUS);
  ctx.clip();
  ctx.fillStyle = s.selected ? c.accentSoft : s.hovered ? c.accentFaint : c.header;
  ctx.fillRect(r.x, r.y, r.width, HEADER);
  ctx.fillStyle = c.border;
  ctx.fillRect(r.x, r.y + HEADER - px, r.width, px);
  ctx.restore();

  const pad = 12;
  if (s.detail) {
    drawTableIcon(ctx, r.x + pad, r.y + HEADER / 2, table.isView, c.accentText, px);
    let right = r.x + r.width - pad;
    ctx.textBaseline = "middle";
    ctx.textAlign = "right";
    ctx.font = `11px ${c.fontUI}`;
    ctx.fillStyle = c.tertiary;
    if (collapsed && shown.length < table.columns.length) {
      const more = `+${table.columns.length - shown.length}`;
      ctx.fillText(more, right, r.y + HEADER / 2);
      right -= ctx.measureText(more).width + 8;
    }
    if (s.showSchema) {
      const label = fit(ctx, table.schema, 80);
      ctx.fillText(label, right, r.y + HEADER / 2);
      right -= ctx.measureText(label).width + 8;
    }
    ctx.textAlign = "left";
    ctx.font = `600 13px ${c.fontUI}`;
    ctx.fillStyle = c.text;
    const nameX = r.x + pad + 22;
    ctx.fillText(fit(ctx, table.name, right - nameX), nameX, r.y + HEADER / 2);

    for (let i = 0; i < shown.length; i++) {
      const column = shown[i];
      const cy = r.y + HEADER + i * ROW + ROW / 2;
      drawMarker(ctx, r.x + pad + 5, cy, column.isPrimaryKey ? "pk" : fks?.has(column.name) ? "fk" : column.isNullable ? "null" : "plain", c, px);
      ctx.font = `11px ${c.fontMono}`;
      ctx.textAlign = "right";
      ctx.fillStyle = c.secondary;
      const type = fit(ctx, column.type, r.width * 0.42);
      const typeWidth = ctx.measureText(type).width;
      ctx.fillText(type, r.x + r.width - pad, cy);
      ctx.textAlign = "left";
      ctx.font = `12px ${c.fontMono}`;
      ctx.fillStyle = c.text;
      const nameLeft = r.x + pad + 18;
      ctx.fillText(fit(ctx, column.name, r.x + r.width - pad - typeWidth - 8 - nameLeft), nameLeft, cy);
    }
  } else {
    // Far out: bars for the rows, and the name big enough to read.
    ctx.fillStyle = c.tertiary;
    for (let i = 0; i < shown.length; i++) {
      const column = shown[i];
      const w = Math.min(r.width - pad * 2, 40 + column.name.length * 6);
      ctx.fillStyle = column.isPrimaryKey ? c.key : fks?.has(column.name) ? c.accentText : c.tertiary;
      ctx.fillRect(r.x + pad, r.y + HEADER + i * ROW + ROW / 2 - 3, w, 6);
    }
    const size = Math.min(Math.max(13, 11 / s.z), 64);
    if (size * s.z >= 4) {
      ctx.font = `600 ${size}px ${c.fontUI}`;
      ctx.textBaseline = "middle";
      ctx.textAlign = "left";
      ctx.fillStyle = c.text;
      const y = r.y + Math.max(HEADER / 2, size * 0.6);
      ctx.fillText(fit(ctx, table.name, r.width - pad * 2), r.x + pad, y);
    }
  }

  // Outline.
  ctx.beginPath();
  roundRect(ctx, r.x, r.y, r.width, r.height, RADIUS);
  ctx.lineWidth = (s.selected ? 2 : 1) * px;
  ctx.strokeStyle = s.selected ? c.accent : s.hovered ? c.accentText : c.border;
  ctx.stroke();
}

function drawTableIcon(ctx: CanvasRenderingContext2D, x: number, cy: number, isView: boolean, color: string, px: number) {
  ctx.save();
  ctx.strokeStyle = color;
  ctx.lineWidth = Math.max(1.2, px);
  ctx.beginPath();
  if (isView) {
    ctx.ellipse(x + 7, cy, 7, 4.2, 0, 0, Math.PI * 2);
    ctx.moveTo(x + 9, cy);
    ctx.arc(x + 7, cy, 2, 0, Math.PI * 2);
  } else {
    ctx.rect(x, cy - 5.5, 14, 11);
    ctx.moveTo(x, cy - 1.5);
    ctx.lineTo(x + 14, cy - 1.5);
    ctx.moveTo(x, cy + 2);
    ctx.lineTo(x + 14, cy + 2);
    ctx.moveTo(x + 5, cy - 1.5);
    ctx.lineTo(x + 5, cy + 5.5);
  }
  ctx.stroke();
  ctx.restore();
}

/** Primary key: a key; foreign key: two links; other columns: a circle, dashed when nullable. */
function drawMarker(ctx: CanvasRenderingContext2D, x: number, cy: number, kind: "pk" | "fk" | "null" | "plain", c: Colors, px: number) {
  ctx.save();
  ctx.lineWidth = Math.max(1.2, px);
  ctx.beginPath();
  if (kind === "pk") {
    ctx.strokeStyle = c.key;
    ctx.arc(x - 2, cy, 2.6, 0, Math.PI * 2);
    ctx.moveTo(x + 0.6, cy);
    ctx.lineTo(x + 6, cy);
    ctx.moveTo(x + 4.5, cy);
    ctx.lineTo(x + 4.5, cy + 2.5);
    ctx.stroke();
  } else if (kind === "fk") {
    ctx.strokeStyle = c.accentText;
    ctx.moveTo(x - 1, cy + 3);
    ctx.lineTo(x + 3, cy - 1);
    ctx.stroke();
    ctx.beginPath();
    ctx.ellipse(x - 2.2, cy + 1.6, 2.6, 1.6, -Math.PI / 4, 0, Math.PI * 2);
    ctx.moveTo(x + 6.4, cy - 2);
    ctx.ellipse(x + 4.2, cy - 1.6, 2.6, 1.6, -Math.PI / 4, 0, Math.PI * 2);
    ctx.stroke();
  } else {
    ctx.strokeStyle = c.tertiary;
    if (kind === "null") ctx.setLineDash([1.5, 1.5]);
    ctx.arc(x, cy, 3, 0, Math.PI * 2);
    ctx.stroke();
  }
  ctx.restore();
}

// MARK: - Minimap

export const MINIMAP = { width: 180, height: 120, inset: 8 };

/** World ↔ minimap, fixed for a drag so the map doesn't move under the pointer. */
export interface Mapping {
  world: Rect;
  scale: number;
  origin: Point;
}

export function minimapMapping(content: Rect | null, seen: Rect): Mapping {
  const pad = 40;
  const base = content ? { x: content.x - pad, y: content.y - pad, width: content.width + pad * 2, height: content.height + pad * 2 } : seen;
  const x = Math.min(base.x, seen.x);
  const y = Math.min(base.y, seen.y);
  const world = { x, y, width: Math.max(base.x + base.width, seen.x + seen.width) - x, height: Math.max(base.y + base.height, seen.y + seen.height) - y };
  const roomW = MINIMAP.width - MINIMAP.inset * 2;
  const roomH = MINIMAP.height - MINIMAP.inset * 2;
  const scale = Math.min(roomW / Math.max(world.width, 1), roomH / Math.max(world.height, 1));
  return { world, scale, origin: { x: (MINIMAP.width - world.width * scale) / 2, y: (MINIMAP.height - world.height * scale) / 2 } };
}

export function unmap(m: Mapping, p: Point): Point {
  return { x: m.world.x + (p.x - m.origin.x) / m.scale, y: m.world.y + (p.y - m.origin.y) / m.scale };
}

export function drawMinimap(ctx: CanvasRenderingContext2D, dpr: number, board: Board, view: BoardView, marks: Marks, c: Colors, m: Mapping) {
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, MINIMAP.width, MINIMAP.height);
  const map = (r: Rect) => ({
    x: m.origin.x + (r.x - m.world.x) * m.scale,
    y: m.origin.y + (r.y - m.world.y) * m.scale,
    width: r.width * m.scale,
    height: r.height * m.scale,
  });
  const plain = new Path2D();
  const chosen = new Path2D();
  for (const table of view.tables) {
    const r = cardRect(board, view, table);
    if (!r) continue;
    const s = map(r);
    (marks.selection.has(tableId(table)) ? chosen : plain).rect(s.x, s.y, Math.max(s.width, 1), Math.max(s.height, 1));
  }
  ctx.fillStyle = c.tertiary;
  ctx.fill(plain);
  ctx.fillStyle = c.accent;
  ctx.fill(chosen);
  const v = map(visibleRect(board));
  ctx.fillStyle = c.accentFaint;
  ctx.fillRect(v.x, v.y, v.width, v.height);
  ctx.strokeStyle = c.accent;
  ctx.lineWidth = 1.5;
  ctx.strokeRect(v.x, v.y, v.width, v.height);
}
