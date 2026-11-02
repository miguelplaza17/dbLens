// Typed wrappers for every Tauri command in src-tauri/src/commands.rs.
// The types mirror the Rust serde types field for field (camelCase).
// Every command rejects with a plain string: the message to show.

import { decode } from "@msgpack/msgpack";
import { Channel, invoke as tauriInvoke } from "@tauri-apps/api/core";

/** True inside the Tauri window; false in a plain browser (vite dev), where src/mock.ts answers. */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** What File > Import Connections from dbLens for Mac… did. Errors name connections, never URLs. */
export interface ImportReport {
  imported: number;
  /** Already saved here: same URL and name. */
  skipped: number;
  errors: string[];
}

/** Every command goes through here: Tauri in the app, the mock backend in a browser. */
function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return tauriInvoke<T>(command, args);
  return import("./mock").then((mock) => mock.mockInvoke<T>(command, args ?? {}));
}

/** Decodes a MessagePack reply (ArrayBuffer, or bytes as a number array on old webviews). */
function unpack<T>(reply: ArrayBuffer | Uint8Array | number[]): T {
  const bytes = reply instanceof Uint8Array ? reply : reply instanceof ArrayBuffer ? new Uint8Array(reply) : Uint8Array.from(reply);
  return decode(bytes) as T;
}

/** A command that answers with MessagePack bytes (page, page_block, execute, …).
 *  The mock answers in the old row-major JSON; `fromMock` turns that into blocks. */
async function binary<T>(command: string, args: Record<string, unknown>, fromMock: (reply: unknown) => T): Promise<T> {
  if (inTauri) return unpack<T>(await tauriInvoke<ArrayBuffer>(command, args));
  const mock = await import("./mock");
  return fromMock(await mock.mockInvoke<unknown>(command, args));
}

// MARK: - Types (src-tauri/src/session.rs, engine.rs, connector.rs, ssh.rs, store.rs)

export type Engine = "postgres" | "mysql" | "sqlite" | "redis" | "mongodb";

export const engineNames: Record<Engine, string> = {
  postgres: "PostgreSQL",
  mysql: "MySQL",
  sqlite: "SQLite",
  redis: "Redis",
  mongodb: "MongoDB",
};

/** Results bigger than this are cut by the drivers (session::ROW_LIMIT). */
export const ROW_LIMIT = 5000;

/** A cell: text as the server sends it, null for NULL. */
export type Cell = string | null;

export interface Column {
  name: string;
  /** "text", "int(11)", "INTEGER", or "" when unknown. */
  type: string;
}

export type RowKeyKind = "primaryKey" | "ctid" | "rowid";

export interface RowKeys {
  /** The primary key, ["ctid", "xmin"], or ["rowid"]. */
  columns: string[];
  kind: RowKeyKind;
  /** One entry per row of the page, in `columns` order. */
  values: Cell[][];
}

/** The key of one row, sent back with an edit. Build it with `rowKey(keys, i)`. */
export interface RowKey {
  columns: string[];
  kind: RowKeyKind;
  values: Cell[];
}

export interface CellValue {
  column: string;
  value: Cell;
}

/** A value in a result block: the server's text, NULL, or a number when the
 *  text is an integer JS holds exactly (String(v) gives the text back). Read
 *  cells with `cellText`, which gives everything as text. */
export type Value = string | number | null;

/** Row keys of a block, column-major like its data: `data[keyColumn][row]`.
 *  Always strings (the driver's exact form): send them back as they came. */
export interface BlockKeys {
  /** The primary key, ["ctid", "xmin"], or ["rowid"]. */
  columns: string[];
  kind: RowKeyKind;
  data: Cell[][];
}

/** Rows as they cross the IPC (MessagePack, src-tauri/src/wire.rs): column-major. */
export interface ResultBlock {
  columns: Column[];
  /** One array per column: `data[column][row]`. */
  data: Value[][];
  rowCount: number;
  /** Where the first row sits in the table (0 for SQL results). */
  offset: number;
  /** "SELECT 3", "UPDATE 2", "CREATE TABLE". */
  command: string;
  /** More rows than were kept: the rest were cut. */
  truncated: boolean;
  /** Rows the statement had (or changed), counting cut ones. */
  totalRows: number;
  /** Only table blocks that can be edited. */
  keys: BlockKeys | null;
  /** A short footer line ("hash · TTL 3,600 s"). */
  note: string | null;
}

/** Row-major, as JSON: what `export_result` takes (and the mock answers). */
export interface QueryResult {
  columns: Column[];
  rows: Cell[][];
  /** "SELECT 3", "UPDATE 2", "CREATE TABLE". */
  command: string;
  /** More than ROW_LIMIT rows: the rest were cut. */
  truncated: boolean;
  totalRows: number;
  /** Only table pages that can be edited. */
  keys: RowKeys | null;
  /** A short footer line ("hash · TTL 3,600 s"). */
  note: string | null;
}

export interface Execution {
  results: ResultBlock[];
  durationMs: number;
}

/** What `execute_stream` sends, in order, over its channel (MessagePack each).
 *  `result` numbers the statements' results from 0; a statement without rows
 *  sends only `end`. `done` comes last, always. */
export type StreamMessage =
  | { type: "columns"; result: number; columns: Column[] }
  | { type: "rows"; result: number; offset: number; rowCount: number; data: Value[][] }
  | { type: "end"; result: number; command: string; totalRows: number; truncated: boolean }
  | StreamDone;

export interface StreamDone {
  type: "done";
  durationMs: number;
  /** The run failed with this message (results before it still arrived). */
  error: string | null;
  cancelled: boolean;
}

/** Rows a streamed run keeps per result by default (wire::STREAM_ROW_CAP). */
export const STREAM_ROW_CAP = 100_000;

export type RelationKind = "table" | "partitioned" | "view" | "materialized" | "foreign" | "key";

export interface Relation {
  /** Schema (Postgres), database (MySQL, Mongo), "main" (SQLite), key prefix group (Redis). */
  schema: string;
  name: string;
  kind: RelationKind;
  /** Row estimate, -1 when unknown. */
  estimate: number;
  /** Redis key type ("hash", "zset", …); "" elsewhere. */
  subtype: string;
}

export interface ColumnSort {
  column: string;
  ascending: boolean;
}

export interface EditOutcome {
  sql: string;
  command: string;
  affected: number;
}

export interface SchemaColumn {
  name: string;
  type: string;
  isPrimaryKey: boolean;
  isNullable: boolean;
}

export interface SchemaTable {
  schema: string;
  name: string;
  isView: boolean;
  columns: SchemaColumn[];
}

export interface SchemaLink {
  name: string;
  /** "schema.table" */
  from: string;
  fromColumns: string[];
  to: string;
  toColumns: string[];
}

export interface SchemaGraph {
  tables: SchemaTable[];
  links: SchemaLink[];
}

export type AtomicOutcome =
  | { kind: "ran"; execution: Execution }
  | { kind: "notAtomic"; message: string };

export interface Summary {
  engine: Engine;
  /** Database name, "db 3" for Redis, the file path for SQLite. */
  database: string;
  host: string | null;
  port: number | null;
  user: string | null;
  tls: string | null;
  suggestedName: string;
}

export interface SshConfig {
  host: string;
  port: number | null;
  user: string;
  keyPath: string;
}

export interface ConnectInfo {
  sessionId: string;
  engine: Engine;
  serverVersion: string;
  name: string;
  savedId: string | null;
  sshLabel: string | null;
}

export interface SavedConnection {
  id: string;
  name: string;
  engine: Engine;
  /** RFC 3339 */
  createdAt: string;
  lastUsed: string | null;
  /** "" is no folder. */
  folderId: string;
  sortIndex: number;
  ssh: SshConfig | null;
}

export interface ConnectionFolder {
  id: string;
  name: string;
  expanded: boolean;
}

export interface StoreSnapshot {
  connections: SavedConnection[];
  folders: ConnectionFolder[];
  /** No system keyring: the sealing key is in a file beside the sealed URLs. */
  keyInFile?: boolean;
}

export interface ConnectionDraft {
  /** Set to edit that connection; a url still showing ••• keeps its password. */
  id?: string | null;
  /** Empty uses the URL's suggested name. */
  name?: string;
  url: string;
  ssh?: SshConfig | null;
  folderId?: string | null;
}

// MARK: - Helpers

/** What stands in for a saved password on screen (url::MASK). */
export const MASK = "•••";

/** "schema.name", unique within a connection. */
export const relationId = (r: Relation) => `${r.schema}.${r.name}`;

/** The key of row `index` of a block, for update_row and delete_row. */
export function rowKey(keys: BlockKeys, index: number): RowKey {
  return { columns: keys.columns, kind: keys.kind, values: keys.data.map((column) => column[index] ?? null) };
}

/** One cell as text (NULL is null). */
export function cellText(block: ResultBlock, row: number, column: number): Cell {
  const v = block.data[column]?.[row];
  return v === null || v === undefined ? null : typeof v === "string" ? v : String(v);
}

/** One row's cells as text. */
export function rowCells(block: ResultBlock, row: number): Cell[] {
  return block.columns.map((_, c) => cellText(block, row, c));
}

/** The block with one cell changed (copies only that column). */
export function withCell(block: ResultBlock, row: number, column: number, value: Cell): ResultBlock {
  const data = block.data.slice();
  data[column] = data[column].slice();
  data[column][row] = value;
  return { ...block, data };
}

/** The block's rows in another order (`order[i]` is the old index of new row i). */
export function permuted(block: ResultBlock, order: number[]): ResultBlock {
  const pick = <T,>(column: T[]) => order.map((i) => column[i]);
  return {
    ...block,
    data: block.data.map(pick),
    keys: block.keys ? { ...block.keys, data: block.keys.data.map(pick) } : null,
  };
}

/** An empty block with these columns, to append streamed rows to. */
export function emptyBlock(columns: Column[]): ResultBlock {
  return { columns, data: columns.map(() => []), rowCount: 0, offset: 0, command: "", truncated: false, totalRows: 0, keys: null, note: null };
}

/** Appends a `rows` message to a block, in place (push, no copies). */
export function appendRows(block: ResultBlock, data: Value[][], rowCount: number) {
  for (let c = 0; c < block.data.length; c++) {
    const target = block.data[c];
    const source = data[c] ?? [];
    for (let r = 0; r < rowCount; r++) target.push(source[r] ?? null);
  }
  block.rowCount += rowCount;
}

/** Row-major, for export_result. */
export function toQueryResult(block: ResultBlock, rows?: number[]): QueryResult {
  const picked = rows ?? Array.from({ length: block.rowCount }, (_, i) => i);
  return {
    columns: block.columns,
    rows: picked.map((r) => rowCells(block, r)),
    command: block.command,
    truncated: block.truncated,
    totalRows: block.totalRows,
    keys: null,
    note: block.note,
  };
}

/** A row-major result (from the mock) as a block. */
export function fromQueryResult(result: QueryResult, offset = 0): ResultBlock {
  return {
    columns: result.columns,
    data: result.columns.map((_, c) => result.rows.map((row) => row[c] ?? null)),
    rowCount: result.rows.length,
    offset,
    command: result.command,
    truncated: result.truncated,
    totalRows: result.totalRows,
    keys: result.keys
      ? { columns: result.keys.columns, kind: result.keys.kind, data: result.keys.columns.map((_, c) => result.keys!.values.map((row) => row[c] ?? null)) }
      : null,
    note: result.note,
  };
}

const executionFromMock = (reply: unknown): Execution => {
  const e = reply as { results: QueryResult[]; durationMs: number };
  return { results: e.results.map((r) => fromQueryResult(r)), durationMs: e.durationMs };
};

/** Runs SQL on the SQL panel's connection, calling `onMessage` for each
 *  streamed message; resolves with the `done` message. Rejects only when the
 *  run can't start (the session is gone). */
function executeStream(
  sessionId: string,
  sql: string,
  onMessage: (message: StreamMessage) => void,
  rowCap: number = STREAM_ROW_CAP,
): Promise<StreamDone> {
  return new Promise<StreamDone>((resolve, reject) => {
    const deliver = (message: StreamMessage) => {
      onMessage(message);
      if (message.type === "done") resolve(message);
    };
    if (!inTauri) {
      // The mock runs it whole and replays it as messages.
      invoke<{ results: QueryResult[]; durationMs: number }>("execute", { sessionId, sql })
        .then((e) => {
          e.results.forEach((r, i) => {
            if (r.columns.length > 0) {
              deliver({ type: "columns", result: i, columns: r.columns });
              const block = fromQueryResult(r);
              deliver({ type: "rows", result: i, offset: 0, rowCount: block.rowCount, data: block.data });
            }
            deliver({ type: "end", result: i, command: r.command, totalRows: r.totalRows, truncated: r.truncated });
          });
          deliver({ type: "done", durationMs: e.durationMs, error: null, cancelled: false });
        })
        .catch((error) => deliver({ type: "done", durationMs: 0, error: errorText(error), cancelled: false }));
      return;
    }
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (bytes) => deliver(unpack<StreamMessage>(bytes));
    tauriInvoke<void>("execute_stream", { sessionId, sql, channel, rowCap }).catch(reject);
  });
}

/** Builds an Execution from stream messages as they come; `snapshot()` gives
 *  what has arrived so far (blocks keep growing in place). */
export class StreamCollector {
  results: ResultBlock[] = [];
  done: StreamDone | null = null;
  private ended = new Set<number>();

  add(message: StreamMessage) {
    switch (message.type) {
      case "columns":
        this.results[message.result] = emptyBlock(message.columns);
        break;
      case "rows": {
        const block = this.results[message.result];
        if (block) appendRows(block, message.data, message.rowCount);
        break;
      }
      case "end": {
        const block = this.results[message.result] ?? emptyBlock([]);
        block.command = message.command;
        block.totalRows = message.totalRows;
        block.truncated = message.truncated;
        this.results[message.result] = block;
        this.ended.add(message.result);
        break;
      }
      case "done":
        this.done = message;
        // A failed or cancelled run sends no `end` for the result in progress.
        this.results.forEach((block, i) => {
          if (block && !this.ended.has(i)) {
            block.totalRows = block.rowCount;
            block.truncated = true;
          }
        });
        break;
    }
  }

  execution(): Execution {
    return { results: this.results.filter(Boolean), durationMs: this.done?.durationMs ?? 0 };
  }
}

/** The result to show: the last one with columns, else the last one. */
export function mainResult(execution: Execution): ResultBlock | undefined {
  const withRows = [...execution.results].reverse().find((r) => r.columns.length > 0);
  return withRows ?? execution.results[execution.results.length - 1];
}

/** The error text of a rejected command. */
export function errorText(error: unknown): string {
  return typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
}

// MARK: - Commands

export const api = {
  // URLs
  detectEngine: (text: string) => invoke<Engine | null>("detect_engine", { text }),
  summarizeUrl: (text: string) => invoke<Summary | null>("summarize_url", { text }),
  maskUrl: (text: string) => invoke<string>("mask_url", { text }),

  // Sessions
  /** By saved id, or by a pasted url (with optional ssh). Both: tries an edit of
   *  that saved connection (the url may still show the mask) without saving it. */
  connect: (target: { savedId: string } | { url: string; ssh?: SshConfig | null } | { savedId: string; url: string; ssh: SshConfig | null }) =>
    invoke<ConnectInfo>("connect", {
      url: "url" in target ? target.url : null,
      savedId: "savedId" in target ? target.savedId : null,
      ssh: "ssh" in target ? target.ssh ?? null : null,
    }),
  disconnect: (sessionId: string) => invoke<void>("disconnect", { sessionId }),
  /** Stops the SQL panel's run (or stream); `scope: "all"` stops browsing queries too. */
  cancel: (sessionId: string, scope: "sql" | "all" = "sql") => invoke<void>("cancel", { sessionId, scope }),
  /** Whether the user's transaction is open on the SQL panel's connection. */
  transactionOpen: (sessionId: string) => invoke<boolean>("transaction_open", { sessionId }),
  relations: (sessionId: string) => invoke<Relation[]>("relations", { sessionId }),
  page: (
    sessionId: string,
    relation: Relation,
    opts: { limit: number; offset: number; search?: string; sort?: ColumnSort | null },
  ) =>
    binary<ResultBlock>(
      "page",
      {
        sessionId,
        relation,
        limit: opts.limit,
        offset: opts.offset,
        search: opts.search ?? "",
        sort: opts.sort ?? null,
      },
      (r) => fromQueryResult(r as QueryResult, opts.offset),
    ),
  /** A block of rows for the grid (200–500 rows is a good size). Blocks fetched
   *  in order, or near ones already fetched, use keyset pagination; any offset works. */
  pageBlock: (
    sessionId: string,
    relation: Relation,
    opts: { offset: number; limit: number; search?: string; sort?: ColumnSort | null },
  ) =>
    binary<ResultBlock>(
      "page_block",
      {
        sessionId,
        relation,
        offset: opts.offset,
        limit: opts.limit,
        search: opts.search ?? "",
        sort: opts.sort ?? null,
      },
      (r) => fromQueryResult(r as QueryResult, opts.offset),
    ),
  count: (sessionId: string, relation: Relation, search = "") =>
    invoke<number | null>("count", { sessionId, relation, search }),
  execute: (sessionId: string, sql: string) => binary<Execution>("execute", { sessionId, sql }, executionFromMock),
  /** `execute`, streamed: rows arrive in blocks as the server sends them. */
  executeStream,
  readOnly: (sessionId: string, text: string) => binary<Execution>("read_only", { sessionId, text }, executionFromMock),
  rehearse: (sessionId: string, text: string) => binary<Execution>("rehearse", { sessionId, text }, executionFromMock),
  executeAtomically: (sessionId: string, text: string) =>
    binary<AtomicOutcome>("execute_atomically", { sessionId, text }, (reply) => {
      const r = reply as { kind: "ran"; execution: unknown } | { kind: "notAtomic"; message: string };
      return r.kind === "ran" ? { kind: "ran", execution: executionFromMock(r.execution) } : r;
    }),
  updateRow: (sessionId: string, relation: Relation, key: RowKey, changes: CellValue[]) =>
    invoke<EditOutcome>("update_row", { sessionId, relation, key, changes }),
  insertRow: (sessionId: string, relation: Relation, values: CellValue[]) =>
    invoke<EditOutcome>("insert_row", { sessionId, relation, values }),
  deleteRow: (sessionId: string, relation: Relation, key: RowKey) =>
    invoke<EditOutcome>("delete_row", { sessionId, relation, key }),
  schemaGraph: (sessionId: string) => invoke<SchemaGraph>("schema_graph", { sessionId }),
  qualifiedName: (sessionId: string, relation: Relation) =>
    invoke<string>("qualified_name", { sessionId, relation }),
  exportResult: (result: ResultBlock, format: "csv" | "json", rows?: number[]) =>
    invoke<string>("export_result", { result: toQueryResult(result, rows), format }),

  // Saved connections
  listConnections: () => invoke<StoreSnapshot>("list_connections"),
  saveConnection: (draft: ConnectionDraft) => invoke<SavedConnection>("save_connection", { draft }),
  deleteConnection: (id: string) => invoke<void>("delete_connection", { id }),
  renameConnection: (id: string, name: string) => invoke<SavedConnection>("rename_connection", { id, name }),
  /** The saved URL with passwords masked, for the edit sheet. */
  connectionUrl: (id: string) => invoke<string>("connection_url", { id }),
  moveConnection: (id: string, folderId: string, beforeId: string | null = null) =>
    invoke<void>("move_connection", { id, folderId, beforeId }),
  addFolder: (name: string) => invoke<ConnectionFolder>("add_folder", { name }),
  renameFolder: (id: string, name: string) => invoke<void>("rename_folder", { id, name }),
  deleteFolder: (id: string) => invoke<void>("delete_folder", { id }),
  setFolderExpanded: (id: string, expanded: boolean) => invoke<void>("set_folder_expanded", { id, expanded }),
  /** macOS only: copies dbLens for Mac's saved connections into this app (src-tauri/src/import_swift.rs). */
  importSwiftConnections: () => invoke<ImportReport>("import_swift_connections"),
};
