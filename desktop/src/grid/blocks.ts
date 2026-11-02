// Where the canvas grid gets its cells. A table is read a block at a time with
// page_block as the grid scrolls (the visible blocks first, then a debounced
// prefetch of their neighbours), kept in an LRU of blocks; a SQL result is the
// block the run streamed in. Edits wait here as pending diffs per row until
// the row is saved (only the changed columns are sent) or reverted.

import {
  api,
  cellText,
  errorText,
  rowKey,
  type Cell,
  type CellValue,
  type Column,
  type ColumnSort,
  type Relation,
  type ResultBlock,
  type RowKey,
  type Value,
} from "../api";
import { recordCache, recordFetch } from "./perf";

/** Rows per block fetched with page_block. */
export const BLOCK_ROWS = 300;
/** Blocks kept in memory (about 15,000 rows). */
export const MAX_BLOCKS = 50;
/** Blocks fetched at once (the pool has up to three browse connections). */
const PARALLEL = 2;
/** Blocks before and after the visible ones fetched once scrolling pauses. */
const PREFETCH = 2;
const PREFETCH_DELAY = 120;
/** Most rows a copy or export of the whole table reads. */
export const COLLECT_CAP = 100_000;

/** What changed: rows from–to (exclusive), or null for "anything, and maybe the row count". */
export type Change = { from: number; to: number } | null;

/** The grid's view of its rows, a table or a query result. */
export interface GridData {
  readonly columns: Column[];
  /** Rows to show; may be an estimate while counting (`exact` false). */
  readonly rows: number;
  readonly exact: boolean;
  /** Edits allowed (a table with a key). */
  readonly editable: boolean;
  /** The cell's value; undefined while its block is loading. */
  value(row: number, column: number): Value | undefined;
  /** The value with any pending edit applied. */
  shown(row: number, column: number): Value | undefined;
  /** The pending edit of a cell, or undefined. */
  pending(row: number, column: number): Cell | undefined;
  /** The message of the last failed save of this row. */
  failure(row: number): string | undefined;
  /** Changes one cell, pending until its row is saved. */
  edit(row: number, column: number, value: Cell): void;
  /** A row's pending changes, as update_row takes them. */
  changes(row: number): CellValue[];
  /** Rows with pending changes, in order. */
  pendingRows(): number[];
  readonly hasPending: boolean;
  /** Drops the pending changes of a row, or of every row. */
  revert(row?: number): void;
  /** The grid shows rows first–last (inclusive). */
  view(first: number, last: number): void;
  subscribe(listener: (change: Change) => void): () => void;
  /** The rows' cells as a block (loading them if needed), at most `cap` rows. */
  collect(rows: number[] | null, cap?: number): Promise<{ block: ResultBlock; cut: boolean }>;
  /** Rows cached in memory. */
  readonly cachedRows: number;
  /** A fetch failed. */
  readonly error: string | null;
}

/** A finished (or streaming) query result: every row is in one block. */
export class BlockData implements GridData {
  readonly editable = false;
  readonly exact = true;
  readonly error = null;
  constructor(readonly block: ResultBlock) {}
  get columns() {
    return this.block.columns;
  }
  get rows() {
    return this.block.rowCount;
  }
  get cachedRows() {
    return this.block.rowCount;
  }
  value(row: number, column: number): Value | undefined {
    const v = this.block.data[column]?.[row];
    return v === undefined ? null : v;
  }
  shown(row: number, column: number) {
    return this.value(row, column);
  }
  pending() {
    return undefined;
  }
  failure() {
    return undefined;
  }
  readonly hasPending = false;
  edit() {}
  changes(): CellValue[] {
    return [];
  }
  pendingRows(): number[] {
    return [];
  }
  revert() {}
  view() {}
  subscribe() {
    return () => undefined;
  }
  async collect(rows: number[] | null, cap = Infinity) {
    const picked = rows ?? Array.from({ length: Math.min(this.block.rowCount, cap) }, (_, i) => i);
    const cut = rows ? rows.length > cap : this.block.rowCount > cap;
    return { block: pick(this.block.columns, picked.slice(0, cap), (r, c) => cellText(this.block, r, c)), cut };
  }
}

function pick(columns: Column[], rows: number[], cell: (row: number, column: number) => Cell): ResultBlock {
  return {
    columns,
    data: columns.map((_, c) => rows.map((r) => cell(r, c))),
    rowCount: rows.length,
    offset: 0,
    command: "",
    truncated: false,
    totalRows: rows.length,
    keys: null,
    note: null,
  };
}

interface PendingRow {
  /** The row's key when the first change was made (the block may be evicted later). */
  key: RowKey | null;
  cells: Map<number, Cell>;
}

/** A table, read in blocks. Make it with `TableSource.open`. */
export class TableSource implements GridData {
  readonly columns: Column[];
  readonly keyKind: RowKey["kind"] | null;
  readonly keyColumns: string[];
  readonly note: string | null;
  readonly editable: boolean;
  error: string | null = null;

  private blocks = new Map<number, ResultBlock>();
  private loading = new Set<number>();
  private queue: number[] = [];
  private first = 0;
  private last = 0;
  private total: number;
  private counted: number | null = null;
  /** Rows end here: a block came back short. */
  private end: number | null = null;
  private prefetchTimer: ReturnType<typeof setTimeout> | undefined;
  private listeners = new Set<(change: Change) => void>();
  /** Bumps on invalidate: answers to older requests are dropped. */
  private epoch = 0;
  private closed = false;
  private edits = new Map<number, PendingRow>();
  private failures = new Map<number, string>();

  private constructor(
    readonly sessionId: string,
    readonly relation: Relation,
    readonly search: string,
    readonly sort: ColumnSort | null,
    head: ResultBlock,
  ) {
    this.columns = head.columns;
    this.keyKind = head.keys?.kind ?? null;
    this.keyColumns = head.keys?.columns ?? [];
    this.note = head.note;
    this.editable = !!head.keys && relation.kind !== "key";
    this.blocks.set(0, head);
    if (head.rowCount < BLOCK_ROWS) this.end = head.rowCount;
    this.total = this.guess();
    this.report();
  }

  /** Reads the first block; rejects with the message when the table can't be read. */
  static async open(sessionId: string, relation: Relation, search: string, sort: ColumnSort | null): Promise<TableSource> {
    const started = performance.now();
    const head = await api.pageBlock(sessionId, relation, { offset: 0, limit: BLOCK_ROWS, search, sort });
    recordFetch(performance.now() - started);
    return new TableSource(sessionId, relation, search, sort, head);
  }

  /** The first block (columns, keys, note). */
  get head(): ResultBlock | undefined {
    return this.blocks.get(0);
  }

  get rows(): number {
    return this.total;
  }

  get exact(): boolean {
    return this.counted !== null || this.end !== null;
  }

  get cachedRows(): number {
    let n = 0;
    for (const b of this.blocks.values()) n += b.rowCount;
    return n;
  }

  /** The exact count arrived (null: too slow to count, keep the estimate). */
  setCount(count: number | null) {
    if (count === null || this.closed) return;
    this.counted = count;
    this.update();
  }

  close() {
    this.closed = true;
    clearTimeout(this.prefetchTimer);
    this.listeners.clear();
  }

  subscribe(listener: (change: Change) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private emit(change: Change) {
    for (const l of this.listeners) l(change);
  }

  /** Rows to show before the count is in: the end if known, else the estimate, else what was read plus a block. */
  private guess(): number {
    if (this.counted !== null) return this.counted;
    if (this.end !== null) return this.end;
    let read = 0;
    for (const [index, block] of this.blocks) read = Math.max(read, index * BLOCK_ROWS + block.rowCount);
    const estimate = this.search.trim() === "" && this.relation.estimate > 0 ? this.relation.estimate : 0;
    return Math.max(estimate, read + BLOCK_ROWS);
  }

  private update() {
    const next = this.guess();
    if (next !== this.total) {
      this.total = next;
      this.emit(null);
    }
  }

  private report() {
    recordCache(this.cachedRows, this.blocks.size);
  }

  // MARK: Cells

  private locate(row: number): { block: ResultBlock; index: number } | undefined {
    const block = this.blocks.get(Math.floor(row / BLOCK_ROWS));
    if (!block) return undefined;
    const index = row - block.offset;
    return index < block.rowCount ? { block, index } : undefined;
  }

  value(row: number, column: number): Value | undefined {
    const at = this.locate(row);
    if (!at) return undefined;
    const v = at.block.data[column]?.[at.index];
    return v === undefined ? null : v;
  }

  shown(row: number, column: number): Value | undefined {
    const edit = this.edits.get(row)?.cells;
    if (edit?.has(column)) return edit.get(column) as Cell;
    return this.value(row, column);
  }

  /** The row's cells as text, or null when its block isn't loaded. */
  rowCells(row: number): Cell[] | null {
    const at = this.locate(row);
    if (!at) return null;
    return this.columns.map((_, c) => cellText(at.block, at.index, c));
  }

  /** The row's key for update_row and delete_row (null when not loaded). */
  key(row: number): RowKey | null {
    const at = this.locate(row);
    if (at?.block.keys) return rowKey(at.block.keys, at.index);
    return this.edits.get(row)?.key ?? null;
  }

  /** Keys of these rows, loading blocks that aren't in memory. */
  async keys(rows: number[]): Promise<RowKey[]> {
    await this.load(rows);
    return rows.map((r) => this.key(r)).filter((k): k is RowKey => k !== null);
  }

  // MARK: Fetching

  view(first: number, last: number) {
    if (this.closed) return;
    this.first = Math.max(0, first);
    this.last = Math.max(this.first, last);
    const from = Math.floor(this.first / BLOCK_ROWS);
    const to = Math.floor(this.last / BLOCK_ROWS);
    const wanted: number[] = [];
    for (let b = from; b <= to; b++) {
      const block = this.blocks.get(b);
      if (block) {
        // Most recently used: move to the end of the map.
        this.blocks.delete(b);
        this.blocks.set(b, block);
      } else if (this.inRange(b)) {
        wanted.push(b);
      }
    }
    // Only what is on screen now; blocks queued for a place scrolled past are dropped.
    this.queue = wanted;
    this.pump();
    clearTimeout(this.prefetchTimer);
    this.prefetchTimer = setTimeout(() => this.prefetch(from, to), PREFETCH_DELAY);
  }

  private inRange(block: number): boolean {
    const start = block * BLOCK_ROWS;
    return this.end === null ? start < Math.max(this.total, BLOCK_ROWS) : start < this.end;
  }

  private prefetch(from: number, to: number) {
    if (this.closed) return;
    const near: number[] = [];
    for (let d = 1; d <= PREFETCH; d++) near.push(to + d, from - d);
    for (const b of near) {
      if (b >= 0 && this.inRange(b) && !this.blocks.has(b) && !this.loading.has(b) && !this.queue.includes(b)) this.queue.push(b);
    }
    this.pump();
  }

  private pump() {
    while (this.loading.size < PARALLEL && this.queue.length > 0) {
      const b = this.queue.shift()!;
      if (this.blocks.has(b) || this.loading.has(b)) continue;
      void this.fetch(b);
    }
  }

  private async fetch(b: number) {
    const epoch = this.epoch;
    this.loading.add(b);
    const started = performance.now();
    try {
      const block = await api.pageBlock(this.sessionId, this.relation, {
        offset: b * BLOCK_ROWS,
        limit: BLOCK_ROWS,
        search: this.search,
        sort: this.sort,
      });
      recordFetch(performance.now() - started);
      if (this.closed || epoch !== this.epoch) return;
      this.error = null;
      this.store(b, block);
      if (block.rowCount < BLOCK_ROWS) {
        // A short block is the end of the rows.
        this.end = b * BLOCK_ROWS + block.rowCount;
      }
      this.update();
      this.emit({ from: b * BLOCK_ROWS, to: b * BLOCK_ROWS + BLOCK_ROWS });
    } catch (e) {
      if (this.closed || epoch !== this.epoch) return;
      this.error = errorText(e);
      this.emit(null);
    } finally {
      if (epoch === this.epoch) this.loading.delete(b);
      if (!this.closed) this.pump();
    }
  }

  private store(b: number, block: ResultBlock) {
    this.blocks.set(b, block);
    const from = Math.floor(this.first / BLOCK_ROWS);
    const to = Math.floor(this.last / BLOCK_ROWS);
    for (const key of this.blocks.keys()) {
      if (this.blocks.size <= MAX_BLOCKS) break;
      if (key >= from && key <= to) continue;
      this.blocks.delete(key);
    }
    this.report();
  }

  /** Makes sure these rows' blocks are in memory (copy, delete, row editor). */
  async load(rows: number[]) {
    const missing = [...new Set(rows.map((r) => Math.floor(r / BLOCK_ROWS)))].filter((b) => !this.blocks.has(b)).sort((a, b) => a - b);
    for (const b of missing) {
      const started = performance.now();
      const block = await api.pageBlock(this.sessionId, this.relation, { offset: b * BLOCK_ROWS, limit: BLOCK_ROWS, search: this.search, sort: this.sort });
      recordFetch(performance.now() - started);
      this.store(b, block);
    }
  }

  /** Forgets cached blocks (all, or those holding these rows) and reads the visible ones again. */
  invalidate(rows?: number[]) {
    if (rows) {
      for (const b of new Set(rows.map((r) => Math.floor(r / BLOCK_ROWS)))) this.blocks.delete(b);
    } else {
      this.blocks.clear();
      this.end = null;
      this.counted = null;
    }
    this.epoch++;
    this.loading.clear();
    this.report();
    this.update();
    this.emit(null);
    this.view(this.first, this.last);
  }

  async collect(rows: number[] | null, cap = COLLECT_CAP): Promise<{ block: ResultBlock; cut: boolean }> {
    const limit = rows ? rows.length : this.total;
    const picked = rows ? [...rows].sort((a, b) => a - b).slice(0, cap) : Array.from({ length: Math.min(limit, cap) }, (_, i) => i);
    // Read in order without filling the cache (keyset pagination keeps each block cheap).
    const extra = new Map<number, ResultBlock>();
    const needed = [...new Set(picked.map((r) => Math.floor(r / BLOCK_ROWS)))].sort((a, b) => a - b);
    for (const b of needed) {
      if (this.blocks.has(b)) continue;
      const block = await api.pageBlock(this.sessionId, this.relation, { offset: b * BLOCK_ROWS, limit: BLOCK_ROWS, search: this.search, sort: this.sort });
      extra.set(b, block);
      if (!rows && block.rowCount < BLOCK_ROWS) break;
    }
    const present = picked.filter((r) => {
      const block = this.blocks.get(Math.floor(r / BLOCK_ROWS)) ?? extra.get(Math.floor(r / BLOCK_ROWS));
      return !!block && r - block.offset < block.rowCount;
    });
    const cell = (r: number, c: number): Cell => {
      const edit = this.edits.get(r)?.cells;
      if (edit?.has(c)) return edit.get(c) as Cell;
      const block = (this.blocks.get(Math.floor(r / BLOCK_ROWS)) ?? extra.get(Math.floor(r / BLOCK_ROWS)))!;
      return cellText(block, r - block.offset, c);
    };
    return { block: pick(this.columns, present, cell), cut: limit > cap };
  }

  // MARK: Pending edits

  pending(row: number, column: number): Cell | undefined {
    const cells = this.edits.get(row)?.cells;
    return cells?.has(column) ? (cells.get(column) as Cell) : undefined;
  }

  failure(row: number): string | undefined {
    return this.failures.get(row);
  }

  /** Rows with edits not yet saved, in order. */
  pendingRows(): number[] {
    return [...this.edits.keys()].sort((a, b) => a - b);
  }

  get hasPending(): boolean {
    return this.edits.size > 0;
  }

  /** Changes one cell, pending until the row is saved. Typing the original value back removes the edit. */
  edit(row: number, column: number, value: Cell) {
    const original = this.value(row, column);
    const originalCell: Cell | undefined = original === undefined ? undefined : original === null ? null : String(original);
    let entry = this.edits.get(row);
    if (originalCell !== undefined && value === originalCell) {
      entry?.cells.delete(column);
      if (entry && entry.cells.size === 0) this.edits.delete(row);
    } else {
      if (!entry) {
        entry = { key: this.key(row), cells: new Map() };
        this.edits.set(row, entry);
      }
      entry.cells.set(column, value);
    }
    this.failures.delete(row);
    this.emit({ from: row, to: row + 1 });
  }

  /** The row's pending changes, as update_row takes them. */
  changes(row: number): CellValue[] {
    const cells = this.edits.get(row)?.cells;
    if (!cells) return [];
    return [...cells].map(([c, value]) => ({ column: this.columns[c].name, value }));
  }

  revert(row?: number) {
    if (row === undefined) {
      this.edits.clear();
      this.failures.clear();
      this.emit(null);
      return;
    }
    this.edits.delete(row);
    this.failures.delete(row);
    this.emit({ from: row, to: row + 1 });
  }

  /** The save of a row failed: keep its edits, show the message. */
  failed(row: number, message: string) {
    this.failures.set(row, message);
    this.emit({ from: row, to: row + 1 });
  }

  /** A row was saved: patch the cached block, or read it again when the key may have changed
   *  (a key column, or Postgres's ctid and xmin, which every update renews). */
  saved(row: number, changes: CellValue[]) {
    this.edits.delete(row);
    this.failures.delete(row);
    const renewsKey = this.keyKind === "ctid" || changes.some((c) => this.keyColumns.includes(c.column));
    const at = this.locate(row);
    if (renewsKey || !at) {
      this.invalidate([row]);
      return;
    }
    const block = at.block;
    for (const change of changes) {
      const c = this.columns.findIndex((col) => col.name === change.column);
      if (c >= 0) block.data[c][at.index] = change.value;
    }
    this.emit({ from: row, to: row + 1 });
  }
}
