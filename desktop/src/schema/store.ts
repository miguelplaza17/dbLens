// The schema board's state for this window (BoardState and the board part of
// AppModel in the Swift app): the graph read from the open connection, the
// schema shown, and the board itself (card positions, folded cards, pan and
// zoom), remembered per connection in localStorage.

import { useSyncExternalStore } from "react";
import { api, errorText, type Relation, type SchemaGraph, type SchemaLink, type SchemaTable } from "../api";
import type { AppModel } from "../model";
import type { Action } from "../shortcuts";
import { MINIMAP } from "./draw";
import {
  autoLayout,
  CARD_WIDTH,
  cardHeight,
  foreignKeyColumns,
  GAP_X,
  rows,
  tableId,
  union,
  type Point,
  type Rect,
} from "./layout";

export const MIN_ZOOM = 0.1;
export const MAX_ZOOM = 2.5;
const ZOOM_STEP = 1.25;

interface Saved {
  positions: Record<string, [number, number]>;
  collapsed: string[];
  pan: Point;
  zoom: number;
}

function readSaved(key: string): Saved | null {
  try {
    const text = localStorage.getItem(key);
    if (!text) return null;
    const saved = JSON.parse(text) as Saved;
    if (typeof saved !== "object" || !saved || typeof saved.positions !== "object") return null;
    return saved;
  } catch {
    return null;
  }
}

/** What the board shows: the tables of the picked schema and the links between them. */
export interface BoardView {
  tables: SchemaTable[];
  links: SchemaLink[];
  byId: Map<string, SchemaTable>;
  foreignKeys: Map<string, Set<string>>;
  schemas: string[];
}

/** One connection's board. Screen = world × zoom + pan. */
export class Board {
  positions = new Map<string, Point>();
  collapsed = new Set<string>();
  pan: Point = { x: 0, y: 0 };
  zoom = 1;
  /** Nothing saved yet: fit everything the first time the board is drawn. */
  needsFit = true;
  viewport = { width: 0, height: 0 };
  private saveTimer: ReturnType<typeof setTimeout> | undefined;
  private listeners = new Set<() => void>();

  constructor(readonly key: string) {
    const saved = readSaved(key);
    if (!saved) return;
    for (const [id, p] of Object.entries(saved.positions)) {
      if (Array.isArray(p) && Number.isFinite(p[0]) && Number.isFinite(p[1])) this.positions.set(id, { x: p[0], y: p[1] });
    }
    this.collapsed = new Set(Array.isArray(saved.collapsed) ? saved.collapsed : []);
    if (saved.pan && Number.isFinite(saved.pan.x) && Number.isFinite(saved.pan.y)) this.pan = { x: saved.pan.x, y: saved.pan.y };
    if (Number.isFinite(saved.zoom)) this.zoom = clamp(saved.zoom, MIN_ZOOM, MAX_ZOOM);
    this.needsFit = this.positions.size === 0;
  }

  /** Redraws on any change (the canvas listens). */
  onChange(listener: () => void) {
    this.listeners.add(listener);
    return () => void this.listeners.delete(listener);
  }

  changed(save = true) {
    for (const l of this.listeners) l();
    if (save) this.save();
  }

  /** Gives every table without a place one: a fresh layout on an empty board, new tables to the right. */
  place(tables: SchemaTable[], links: SchemaLink[]) {
    const missing = tables.filter((t) => !this.positions.has(tableId(t)));
    if (missing.length === 0) return;
    if (this.positions.size === 0) {
      this.positions = autoLayout(tables, links);
      this.needsFit = true;
    } else {
      const auto = autoLayout(missing, links);
      let right = -Infinity;
      for (const p of this.positions.values()) right = Math.max(right, p.x);
      right += CARD_WIDTH + GAP_X;
      let left = Infinity;
      for (const p of auto.values()) left = Math.min(left, p.x);
      for (const [id, p] of auto) this.positions.set(id, { x: p.x - left + right, y: p.y });
    }
    this.save();
  }

  /** Lays the given tables out again. */
  arrange(tables: SchemaTable[], links: SchemaLink[]) {
    for (const [id, p] of autoLayout(tables, links)) this.positions.set(id, p);
    this.changed();
  }

  save() {
    clearTimeout(this.saveTimer);
    const key = this.key;
    this.saveTimer = setTimeout(() => {
      const positions: Saved["positions"] = {};
      for (const [id, p] of this.positions) positions[id] = [Math.round(p.x), Math.round(p.y)];
      const saved: Saved = { positions, collapsed: [...this.collapsed], pan: this.pan, zoom: this.zoom };
      try {
        localStorage.setItem(key, JSON.stringify(saved));
      } catch {
        // Private windows and previews: the board lasts until the window closes.
      }
    }, 400);
  }

  toWorld(p: Point): Point {
    return { x: (p.x - this.pan.x) / this.zoom, y: (p.y - this.pan.y) / this.zoom };
  }

  /** Zooms keeping `anchor` (a screen point in the board) still; the middle by default. */
  zoomBy(factor: number, anchor?: Point) {
    const a = anchor ?? { x: this.viewport.width / 2, y: this.viewport.height / 2 };
    const next = clamp(this.zoom * factor, MIN_ZOOM, MAX_ZOOM);
    if (next === this.zoom) return;
    this.pan = { x: a.x - ((a.x - this.pan.x) * next) / this.zoom, y: a.y - ((a.y - this.pan.y) * next) / this.zoom };
    this.zoom = next;
    this.changed();
  }

  /** Fits a world rectangle in the view, never above 100%. */
  fit(rect: Rect | null) {
    const { width, height } = this.viewport;
    if (!rect || width <= 0 || height <= 0 || rect.width <= 0 || rect.height <= 0) return;
    const margin = 48;
    // The minimap sits in the bottom-right corner: fit beside it or above it,
    // whichever shows the tables larger, so it never covers a card.
    const mapW = MINIMAP.width + MINIMAP_GAP;
    const mapH = MINIMAP.height + MINIMAP_GAP;
    const zoomIn = (w: number, h: number) => clamp(Math.min((w - margin * 2) / rect.width, (h - margin * 2) / rect.height), MIN_ZOOM, 1);
    const beside = zoomIn(width - mapW, height);
    const above = zoomIn(width, height - mapH);
    const [w, h, zoom] = beside >= above ? [width - mapW, height, beside] : [width, height - mapH, above];
    this.zoom = zoom;
    this.pan = { x: (w - rect.width * zoom) / 2 - rect.x * zoom, y: (h - rect.height * zoom) / 2 - rect.y * zoom };
    this.needsFit = false;
    this.changed();
  }

  /** Centers the view on a world point. */
  center(p: Point) {
    this.pan = { x: this.viewport.width / 2 - p.x * this.zoom, y: this.viewport.height / 2 - p.y * this.zoom };
    this.changed();
  }
}

/** The minimap's distance from the board's edge (--space-md in schema.css). */
const MINIMAP_GAP = 12;

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

/** The board of the window's open connection. */
class SchemaState {
  sessionId: string | null = null;
  graph: SchemaGraph | null = null;
  loading = false;
  error: string | null = null;
  board: Board | null = null;
  /** The schema shown; null shows every schema. */
  schema: string | null = null;
  view: BoardView = emptyView();

  private version = 0;
  private listeners = new Set<() => void>();
  private request = 0;

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => void this.listeners.delete(listener);
  };
  getVersion = () => this.version;

  changed() {
    this.version++;
    for (const l of this.listeners) l();
  }

  /** Forgets the last connection's graph when the window's session changed. */
  private follow(model: AppModel) {
    const id = model.session?.sessionId ?? null;
    if (id === this.sessionId) return;
    this.sessionId = id;
    this.graph = null;
    this.error = null;
    this.loading = false;
    this.schema = null;
    this.view = emptyView();
    this.board = model.session ? new Board(boardKey(model)) : null;
  }

  /** Whether the open connection can have a board (Redis has no tables or keys to draw). */
  available(model: AppModel): boolean {
    return !!model.session && model.session.engine !== "redis";
  }

  async show(model: AppModel) {
    if (!this.available(model)) return;
    this.follow(model);
    model.showResultTab("schema");
    if (!this.graph && !this.loading) await this.load(model);
  }

  async load(model: AppModel) {
    this.follow(model);
    const info = model.session;
    if (!info) return;
    const request = ++this.request;
    this.loading = true;
    this.changed();
    try {
      const graph = await api.schemaGraph(info.sessionId);
      if (request !== this.request || model.session?.sessionId !== info.sessionId) return;
      this.graph = graph;
      this.error = null;
      const schemas = [...new Set(graph.tables.map((t) => t.schema))].sort();
      if (this.schema && !schemas.includes(this.schema)) this.schema = null;
      this.view = makeView(graph, this.schema);
      this.board?.place(graph.tables, graph.links);
    } catch (e) {
      if (request !== this.request) return;
      this.error = errorText(e);
    } finally {
      if (request === this.request) {
        this.loading = false;
        this.changed();
        this.board?.changed(false);
      }
    }
  }

  setSchema(schema: string | null) {
    this.schema = schema;
    if (this.graph) this.view = makeView(this.graph, schema);
    this.changed();
    this.board?.fit(this.bounds());
  }

  /** A card's height as drawn now (folded or not). */
  height(table: SchemaTable): number {
    const id = tableId(table);
    return cardHeight(rows(table, !!this.board?.collapsed.has(id), this.view.foreignKeys.get(id)).length);
  }

  /** The world rectangle holding every card shown. */
  bounds(): Rect | null {
    const board = this.board;
    if (!board) return null;
    let rect: Rect | null = null;
    for (const t of this.view.tables) {
      const p = board.positions.get(tableId(t));
      if (p) rect = union(rect, { x: p.x, y: p.y, width: CARD_WIDTH, height: this.height(t) });
    }
    return rect;
  }

  fit() {
    this.board?.fit(this.bounds());
  }

  arrange() {
    this.board?.arrange(this.view.tables, this.view.links);
    this.fit();
  }

  /** Every shown card folded to its key columns, or all unfolded. */
  get allCollapsed(): boolean {
    const board = this.board;
    return !!board && this.view.tables.length > 0 && this.view.tables.every((t) => board.collapsed.has(tableId(t)));
  }

  toggleAllCollapsed() {
    const board = this.board;
    if (!board) return;
    const fold = !this.allCollapsed;
    for (const t of this.view.tables) {
      if (fold) board.collapsed.add(tableId(t));
      else board.collapsed.delete(tableId(t));
    }
    board.changed();
    this.changed();
  }

  toggleCollapsed(id: string) {
    const board = this.board;
    if (!board) return;
    if (board.collapsed.has(id)) board.collapsed.delete(id);
    else board.collapsed.add(id);
    board.changed();
    this.changed();
  }

  /** A card's rows in the grid. */
  async open(model: AppModel, table: SchemaTable) {
    const found = model.relations.find((r) => r.schema === table.schema && r.name === table.name);
    const relation: Relation = found ?? {
      schema: table.schema,
      name: table.name,
      kind: table.isView ? "view" : "table",
      estimate: -1,
      subtype: "",
    };
    await model.openRelation(relation);
  }
}

function emptyView(): BoardView {
  return { tables: [], links: [], byId: new Map(), foreignKeys: new Map(), schemas: [] };
}

function makeView(graph: SchemaGraph, schema: string | null): BoardView {
  const tables = schema ? graph.tables.filter((t) => t.schema === schema) : graph.tables;
  const byId = new Map(tables.map((t) => [tableId(t), t]));
  const links = graph.links.filter((l) => byId.has(l.from) && byId.has(l.to));
  const schemas = [...new Set(graph.tables.map((t) => t.schema))].sort();
  return { tables, links, byId, foreignKeys: foreignKeyColumns(links), schemas };
}

/** Boards are kept per saved connection; a pasted URL keeps one per name. */
function boardKey(model: AppModel): string {
  const s = model.session;
  return `schemaBoard.${s?.savedId ? s.savedId : `url:${s?.name ?? ""}`}`;
}

export const schema = new SchemaState();

/** Redraws the calling component whenever the board's state changes. */
export function useSchema(): SchemaState {
  useSyncExternalStore(schema.subscribe, schema.getVersion);
  return schema;
}

/** Show Table / Query Result / Schema and the board's zoom, from keys and the menu. */
export function performSchema(model: AppModel, action: Action): boolean {
  switch (action) {
    case "showTable":
      if (model.session) model.showResultTab("table");
      return true;
    case "showQuery":
      if (model.execution) model.showResultTab("query");
      return true;
    case "showSchema":
      void schema.show(model);
      return true;
    case "zoomIn":
    case "zoomOut":
    case "zoomToFit":
      if (model.shown !== "schema") return true;
      if (action === "zoomToFit") schema.fit();
      else schema.board?.zoomBy(action === "zoomIn" ? ZOOM_STEP : 1 / ZOOM_STEP);
      return true;
    case "refresh":
      // Reload reads the schema again too; the model reloads the rest.
      if (model.shown === "schema") void schema.load(model);
      return false;
    default:
      return false;
  }
}

export { ZOOM_STEP };
