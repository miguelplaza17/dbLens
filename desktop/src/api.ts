// Typed wrappers for every Tauri command in src-tauri/src/commands.rs.
// The types mirror the Rust serde types field for field (camelCase).
// Every command rejects with a plain string: the message to show.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/** True inside the Tauri window; false in a plain browser (vite dev), where src/mock.ts answers. */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Every command goes through here: Tauri in the app, the mock backend in a browser. */
function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return tauriInvoke<T>(command, args);
  return import("./mock").then((mock) => mock.mockInvoke<T>(command, args ?? {}));
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
  results: QueryResult[];
  durationMs: number;
}

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

/** The key of row `index` of a page, for update_row and delete_row. */
export function rowKey(keys: RowKeys, index: number): RowKey {
  return { columns: keys.columns, kind: keys.kind, values: keys.values[index] ?? [] };
}

/** The result to show: the last one with columns, else the last one. */
export function mainResult(execution: Execution): QueryResult | undefined {
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
  cancel: (sessionId: string) => invoke<void>("cancel", { sessionId }),
  relations: (sessionId: string) => invoke<Relation[]>("relations", { sessionId }),
  page: (
    sessionId: string,
    relation: Relation,
    opts: { limit: number; offset: number; search?: string; sort?: ColumnSort | null },
  ) =>
    invoke<QueryResult>("page", {
      sessionId,
      relation,
      limit: opts.limit,
      offset: opts.offset,
      search: opts.search ?? "",
      sort: opts.sort ?? null,
    }),
  count: (sessionId: string, relation: Relation, search = "") =>
    invoke<number | null>("count", { sessionId, relation, search }),
  execute: (sessionId: string, sql: string) => invoke<Execution>("execute", { sessionId, sql }),
  readOnly: (sessionId: string, text: string) => invoke<Execution>("read_only", { sessionId, text }),
  rehearse: (sessionId: string, text: string) => invoke<Execution>("rehearse", { sessionId, text }),
  executeAtomically: (sessionId: string, text: string) =>
    invoke<AtomicOutcome>("execute_atomically", { sessionId, text }),
  updateRow: (sessionId: string, relation: Relation, key: RowKey, changes: CellValue[]) =>
    invoke<EditOutcome>("update_row", { sessionId, relation, key, changes }),
  insertRow: (sessionId: string, relation: Relation, values: CellValue[]) =>
    invoke<EditOutcome>("insert_row", { sessionId, relation, values }),
  deleteRow: (sessionId: string, relation: Relation, key: RowKey) =>
    invoke<EditOutcome>("delete_row", { sessionId, relation, key }),
  schemaGraph: (sessionId: string) => invoke<SchemaGraph>("schema_graph", { sessionId }),
  qualifiedName: (sessionId: string, relation: Relation) =>
    invoke<string>("qualified_name", { sessionId, relation }),
  exportResult: (result: QueryResult, format: "csv" | "json") =>
    invoke<string>("export_result", { result, format }),

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
};
