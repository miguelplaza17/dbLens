# dbLens desktop: the contract

dbLens in Tauri 2, for Windows, Linux and macOS: the only dbLens from now on
(the Swift app in the repo root is discontinued). Paste a connection string,
click a table, see and edit rows, run SQL. Fast, minimal: per window, sidebar |
grid | foldable SQL panel; as many windows as you like.

The Rust core mirrors `Sources/DBLensKit` in the repo root. When in doubt about
behavior, read the Swift file named in each module's header and do the same.

The macOS rules in the root `CLAUDE.md` (build.sh, snap.sh, selftest.sh, Secure
Enclave, SwiftUI design rules) do not apply here. These do:

- English is the source text. UI strings go through `t("English key")` from
  `src/i18n.ts`, reusing keys from `../Localization/pt-BR.json` where the text
  matches. Database data (names, SQL, values, server messages) is never translated.
- Never test against real user databases. Integration tests read the env vars
  below and return early (skip) when they are unset.
- `cargo clippy --all-targets -- -D warnings` stays clean, on Linux and for
  `--target x86_64-pc-windows-gnu`.

## Layout

What works and what is missing: see "Status" in [README.md](README.md).

| Path | What |
|---|---|
| `src-tauri/src/engine.rs` | `Engine`, `Engine::detect` |
| `src-tauri/src/url.rs` | `clean`, `ConnectionUrl` (Postgres), `UrlParts` (any scheme), `SqliteLocation`, `redact`/`unmask`/`MASK`, `quote_ident`, `compose` |
| `src-tauri/src/session.rs` | `DatabaseSession` trait, all shared types, `DbError` |
| `src-tauri/src/sql_script.rs` | statement splitter, `verb`, `checked`, `run` (for rehearse/atomic) |
| `src-tauri/src/export.rs` | CSV and JSON of a `QueryResult` |
| `src-tauri/src/connection_problem.rs` | one-line cause for a failed connect |
| `src-tauri/src/connector.rs` | `summary`, `connect` (SSH tunnel, then driver, wrapped in its pool), `pooled` |
| `src-tauri/src/pool.rs` | `PooledSession`: a pinned connection for the SQL panel plus browse connections; block-boundary hints for keyset pagination |
| `src-tauri/src/wire.rs` | MessagePack encoding of result blocks and executions, `StreamSink` for `execute_stream` |
| `src-tauri/src/drivers/postgres.rs` | Postgres (tokio-postgres + rustls) |
| `src-tauri/src/drivers/mysql.rs` | MySQL/MariaDB (mysql_async) |
| `src-tauri/src/drivers/sqlite.rs` | SQLite (rusqlite, bundled) |
| `src-tauri/src/drivers/redis.rs` | Redis/Valkey (own RESP client on tokio, like `Redis.swift`) |
| `src-tauri/src/drivers/mongo.rs`, `mongo_shell.rs` | MongoDB (official mongodb crate) and the mongosh parser |
| `src-tauri/src/ssh.rs` | SSH tunnel via the system `ssh` |
| `src-tauri/src/store.rs` | saved connections, folders, sealed URLs |
| `src-tauri/src/import_swift.rs`, `src-tauri/swift/` | macOS: import the Swift app's saved connections (Core Data store, sealed URLs; the Secure Enclave key through a tiny Swift helper that `build.rs` compiles for macOS targets only) |
| `src-tauri/src/menu.rs` | the native menu bar, built from `src/shortcuts.json`; enabled rules (`MenuState`) |
| `src-tauri/src/windows.rs` | new windows (labels `main`, `w2`, `w3`…), files to open passed at launch or by a second launch |
| `src-tauri/src/commands.rs`, `lib.rs` | IPC commands and wiring (plugins: window-state, dialog, single-instance on Windows/Linux) |
| `src-tauri/tests/*.rs` | one integration file per engine, `pool.rs` (pool, keyset blocks, streaming on Postgres/MySQL/SQLite), `contract.rs` (the MessagePack and JSON shapes `api.ts` expects), `bench.rs` (ignored; see [docs/PERF.md](docs/PERF.md)) |
| `src/api.ts` | typed IPC wrappers and TS types; keep in sync with Rust |
| `src/mock.ts` | answers every command in a plain browser (vite dev without Tauri) |
| `src/i18n.ts`, `src/i18n/*.json` | `t()`; English source, `pt-BR` |
| `src/shortcuts.json`, `src/shortcuts.ts` | every action: id, keys, title (one table for keys, tooltips and the native menu) |
| `src/native/` | `actions.ts` (one dispatcher for keys and menu clicks), `perform.ts` (window actions), `bridge.ts` (menu events, menu state, file and text drops), `dragConnections.ts`, `importSwift.ts` |
| `src/grid/` | the table grid (Glide Data Grid, canvas): `blocks.ts` (row blocks fetched on demand, LRU, pending edits), `CellEditor.tsx`, `commands.ts` (grid actions), `perf.ts`/`PerfHud.tsx` (the perf HUD), `theme.ts` |
| `src/App.tsx`, `src/components/`, `src/theme.css`, `src/styles.css` | the UI |

Shared types change in Rust, `src/api.ts` and `tests/contract.rs` together.

## Driver interface

Every `drivers/<engine>.rs` exports:

```rust
/// What the connection sheet shows; None when the text isn't this engine's URL.
pub fn summary(text: &str) -> Option<Summary>;
/// `url` is already cleaned (url::clean). With `local_port` (SSH tunnel), connect
/// to 127.0.0.1:<local_port> instead of the URL's host/port (keep the URL's
/// host for TLS server names).
pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>>;
```

Error messages from `connect` carry the server's own text (`DbError::Message`); `connector::connect`
adds the headline (refused, unknown host, timeout, login, missing database, TLS).

## The trait (`src-tauri/src/session.rs`)

```rust
#[async_trait]
pub trait DatabaseSession: Send + Sync {
    fn engine(&self) -> Engine;
    fn server_version(&self) -> String;               // "16.4", "8.4.3", "3.51.0"
    fn cancel(&self);                                 // stop the running query; never waits for the query's lock
    fn quote(&self, identifier: &str) -> String;      // "x" / `x`
    fn qualified(&self, relation: &Relation) -> String;          // default: quote(schema).quote(name)

    async fn execute(&self, sql: &str) -> DbResult<Execution>;   // one or more statements, as typed
    async fn read_only(&self, text: &str) -> DbResult<Execution>;// one statement, writes refused by the DB itself
    async fn rehearse(&self, text: &str) -> DbResult<Execution>; // default: error; run in a txn, always rolled back
    async fn execute_atomically(&self, text: &str) -> DbResult<Execution>; // default: DbError::NotAtomic
    async fn relations(&self) -> DbResult<Vec<Relation>>;
    async fn page(&self, relation: &Relation, limit: u32, offset: u64, search: &str,
                  sort: Option<&ColumnSort>) -> DbResult<QueryResult>;
    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>>; // None = too slow to count
    async fn update(&self, relation: &Relation, key: &RowKey, changes: &[CellValue]) -> DbResult<EditOutcome>;
    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome>;
    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome>;
    async fn schema_graph(&self) -> DbResult<SchemaGraph>;       // default: empty (Redis)
    async fn close(&self);                                       // default: no-op; dropping must also be safe

    fn transaction_open(&self) -> bool;                          // default false; the user's txn, as of the last run (no round trip)
    async fn page_block(&self, relation: &Relation, offset: u64, limit: u32, search: &str,
                        sort: Option<&ColumnSort>, hint: Option<&KeyHint>) -> DbResult<QueryResult>;
                                                                 // default: page(); Postgres/MySQL/SQLite: keyset pagination
    async fn execute_stream(&self, sql: &str, sink: &mut wire::StreamSink) -> DbResult<()>;
                                                                 // default: execute() sent whole; Postgres/MySQL/SQLite stream rows
}
```

`KeyHint { offset, values }` is the key of the row at `offset` (same order, search
and sort) from an earlier block. With no sort, or the sort on the one key column,
a driver turns it into `where key > hint` (`<` descending, row values for
composite keys, `ctid > hint::tid` in Postgres) and skips only the rows between
it and `offset`. A skip of `DEEP_OFFSET` (10,000) or more without a search first
finds the block's first key with a key-only query (an index-only scan), then
reads `where key >= that`. Any other order is plain `LIMIT/OFFSET`, as before.

## Pools (`pool.rs`)

`connector::connect` wraps every driver session in a `PooledSession`:

| Engine | Browse connections | Edits go to |
|---|---|---|
| Postgres, MySQL/MariaDB | up to 3 (one opened right after connect, the rest when needed) | a browse connection |
| SQLite | up to 2, opened read-only | the pinned (writer) connection |
| Redis/Valkey | 1 (commands on the pinned one, browsing on the other) | a browse connection |
| MongoDB | none (the driver's client pools itself) | the pinned session |

- The **pinned** connection runs `execute`, `execute_stream`, `read_only`,
  `rehearse`, `execute_atomically`: a `BEGIN` in one run and `COMMIT` in a later
  one stay on one connection.
- `relations`, `page`, `page_block`, `count`, `schema_graph` and edits use a free
  browse connection, **except while the user's transaction is open** on the
  pinned one (`transaction_open()`): then they run there too, so the grid shows
  the transaction's own rows and an edit joins it behind a savepoint (the
  drivers' existing `dblens_edit` logic).
- `transaction_open` is tracked by the drivers without extra round trips in the
  common case: Postgres probes after a run that has BEGIN/START or while one is
  open; MySQL reads `@@in_transaction` after BEGIN/START/SET/XA or while open;
  SQLite reads `sqlite3_get_autocommit` after every call.
- A browse connection that fails to open marks the pool broken: everything
  falls back to the pinned connection (a server at its connection limit still works).
- `cancel` (default scope) stops the pinned connection's run; `cancel` with
  scope `"all"` and `disconnect` stop every connection. Each connection cancels
  its own backend (Postgres cancel token, MySQL `KILL QUERY <its id>`, SQLite
  interrupt handle).
- Session state set in the SQL panel (`SET search_path`, temp tables, Redis
  `SELECT n`) applies to the pinned connection only; browsing does not see it.
- The pool remembers each block's last key per (relation, search, sort) and
  passes it as the `KeyHint` for later blocks; a block at offset 0 starts that
  view over; any write through the session (`execute*`, edits) forgets all hints.

Rules every driver follows (from the Swift drivers):

- All methods take `&self`; the app may call `cancel` while `execute` runs. Keep
  the connection behind a `tokio::sync::Mutex` and a cancel handle outside it
  (`tokio_postgres::CancelToken`, `KILL QUERY <id>` on a second connection,
  `rusqlite::InterruptHandle`, …). A cancelled run returns `DbError::Cancelled`.
- Values are text as the server prints them; NULL is `None`. Results over
  `ROW_LIMIT` (5,000) rows are cut: `truncated: true`, `total_rows` = rows seen.
- `page` sorts by the primary key when there is one; with `sort`, by that column
  first, then the key, so pages stay stable. A non-empty `search` keeps rows with
  that text in any column, case-insensitive (`like_pattern` escapes LIKE wildcards).
  It returns column types and, when rows can be edited, `keys`
  (`PrimaryKey`, Postgres `Ctid` = ["ctid","xmin"], SQLite `Rowid` = ["rowid"]).
- `update` and `delete` touch exactly one row: in a transaction, rolled back if
  the count is not 1. `EditOutcome.sql` is the statement for the log.
- `rehearse` and `execute_atomically` split with `sql_script::checked` (refuses
  BEGIN/COMMIT/ROLLBACK) and run with `sql_script::run` (names the failing statement).
- `Relation.schema`: Postgres schema, MySQL database, SQLite "main", Mongo database,
  Redis key prefix group ("user" for "user:42"). Redis keys are `RelationKind::Key`
  with `subtype` set to the key type.
- `SchemaGraph::build(columns, links)` turns two catalog result sets into the graph.

## Types (Rust → JSON → `src/api.ts`)

All structs serialize camelCase. `Option` is `null`.

| Rust | JSON / TS |
|---|---|
| `Engine` | `"postgres" \| "mysql" \| "sqlite" \| "redis" \| "mongodb"` |
| `Column { name, type_name }` | `{ name, type }` |
| `RowKeyKind` | `"primaryKey" \| "ctid" \| "rowid"` |
| `RowKeys { columns, kind, values: Vec<Vec<Option<String>>> }` | `{ columns, kind, values: Cell[][] }` |
| `RowKey { columns, kind, values: Vec<Option<String>> }` | one row of `RowKeys` (`rowKey(keys, i)` in TS) |
| | Key values are the driver's exact form, not what the grid shows (MySQL binary keys as full hex, SQLite values tagged `i:`/`r:`/`t:`/`b:` with their storage class). Send them back as they came; reload the page after an edit that can change a key. |
| `CellValue { column, value }` | `{ column, value: string \| null }` |
| `QueryResult { columns, rows, command, truncated, total_rows, keys, note }` | JSON `{ columns, rows, command, truncated, totalRows, keys, note }` only for `export_result`; over the IPC it is a `ResultBlock` (MessagePack, below) |
| `Execution { results, duration_ms }` | MessagePack `{ results: ResultBlock[], durationMs }` |
| `RelationKind` | `"table" \| "partitioned" \| "view" \| "materialized" \| "foreign" \| "key"` |
| `Relation { schema, name, kind, estimate, subtype }` | same; `estimate` -1 = unknown |
| `ColumnSort { column, ascending }` | same |
| `EditOutcome { sql, command, affected }` | same |
| `SchemaGraph { tables, links }`, `SchemaTable { schema, name, is_view, columns }`, `SchemaColumn { name, type_name, is_primary_key, is_nullable }`, `SchemaLink { name, from, from_columns, to, to_columns }` | camelCase; `type` for `type_name` |
| `Summary { engine, database, host, port, user, tls, suggested_name }` | camelCase |
| `SshConfig { host, port, user, key_path }` | `{ host, port, user, keyPath }` |
| `ConnectInfo { session_id, engine, server_version, name, saved_id, ssh_label }` | camelCase |
| `SavedConnection { id, name, engine, created_at, last_used, folder_id, sort_index, ssh }` | camelCase; dates RFC 3339; `folderId` "" = none |
| `ConnectionFolder { id, name, expanded }`, `StoreSnapshot { connections, folders }` | same |
| `ConnectionDraft { id?, name, url, ssh?, folder_id? }` | camelCase |
| `execute_atomically`'s answer | MessagePack `{ kind: "ran", execution } \| { kind: "notAtomic", message }` |
| `DbError` | a plain string (its message) |

`DbError` variants: `Message` (server/driver text), `NotAtomic`, `Cancelled`,
`Unsupported`, `NoSession`. Over IPC all are just the message string;
`execute_atomically` turns `NotAtomic` into `AtomicOutcome::NotAtomic`.

## Binary results (MessagePack, `wire.rs`)

Table data never crosses the IPC as JSON. `page`, `page_block`, `execute`,
`read_only`, `rehearse` and `execute_atomically` return `tauri::ipc::Response`
bytes (an `ArrayBuffer` in JS) holding one MessagePack map; `api.ts` decodes it
with `@msgpack/msgpack`. Small metadata commands stay JSON. Errors are still a
rejected promise with the message string.

A **ResultBlock** (`wire::write_block`), column-major:

```ts
type Value = string | number | null;   // text as the server prints it; NULL; or an integer
interface ResultBlock {
  columns: { name: string; type: string }[];
  data: Value[][];          // data[column][row], one array per column
  rowCount: number;         // rows in data
  offset: number;           // the table offset of row 0 (page/page_block); 0 for SQL results
  command: string;          // "SELECT 3", "UPDATE 2"
  truncated: boolean;
  totalRows: number;        // rows the statement had (or changed), counting cut ones
  keys: { columns: string[]; kind: "primaryKey" | "ctid" | "rowid"; data: (string | null)[][] } | null;
                            // column-major like data; always strings (the driver's exact form)
  note: string | null;
}
```

A value is a number only when the text is an integer's canonical form within
±(2^53−1) (no leading zeros, no `-0`), so `String(v)` is always the server's text
byte for byte; the encoder tries integers per column until one value isn't. Read
cells with `cellText(block, row, col)` (`api.ts`), which returns `string | null`.
Floats, numerics, booleans, dates stay strings. A statement without rows has
`columns: []`, `data: []`.

An **Execution** is `{ results: ResultBlock[], durationMs: number }`;
`execute_atomically` answers `{ kind: "ran", execution } | { kind: "notAtomic", message }`.

### Streaming (`execute_stream`)

`execute_stream(sessionId, sql, channel, rowCap?)` runs on the pinned connection
and sends MessagePack messages over a `tauri::ipc::Channel` as raw bytes
(`InvokeResponseBody::Raw`; JS receives `ArrayBuffer`s, in order):

```ts
type StreamMessage =
  | { type: "columns"; result: number; columns: Column[] }                           // a result set starts
  | { type: "rows"; result: number; offset: number; rowCount: number; data: Value[][] } // a block, column-major
  | { type: "end"; result: number; command: string; totalRows: number; truncated: boolean } // a statement ends
  | { type: "done"; durationMs: number; error: string | null; cancelled: boolean };  // always last
```

- `result` numbers the statement results from 0. A statement without rows sends
  only `end` (`totalRows` = rows changed).
- The first `rows` block of a result comes after 500 rows (or at its end), then
  blocks of 5,000 rows (sooner past 2 MB or after 200 ms of waiting rows).
  `offset` is the row index of the block's first row within its result.
- Up to `rowCap` rows per result are sent (default 100,000); later rows are only
  counted (`end.totalRows`, `truncated: true`).
- An error ends the run: earlier messages stand, `done.error` has the message
  (`cancelled` after `cancel`). The command's promise resolves after `done` is
  queued; it rejects only when the run can't start (closed session). Wait for
  `done`, not for the promise: the channel's messages can still be in flight.
- If the window closes (the channel is gone), the driver stops and cancels the
  query on the server.
- Postgres streams via the simple query protocol (`simple_query_raw`), MySQL via
  `query_iter`, SQLite by stepping the statement; Redis and MongoDB run
  `execute` and send each result whole (Mongo results stay cut at 5,000).

`api.executeStream(sessionId, sql, onMessage, rowCap?)` wraps it and resolves with
the `done` message; `StreamCollector` builds an `Execution` from the messages
(blocks grow in place with `appendRows`).

## IPC commands (`src-tauri/src/commands.rs`)

JS argument names are camelCase (Tauri maps them to the snake_case parameters).
`src/api.ts` has a wrapper for each; the UI should call `api.*`, not `invoke`.
**Binary** marks a MessagePack reply (above); the rest are JSON.

| Command | Arguments | Returns |
|---|---|---|
| `detect_engine` | `text` | `Engine \| null` |
| `summarize_url` | `text` | `Summary \| null` |
| `mask_url` | `text` | `string` (passwords → •••) |
| `connect` | `url?`, `savedId?`, `ssh?` | `ConnectInfo` (by saved id: URL and SSH come from the store, last used is updated; with `url` too: tries that edit of the saved connection, masked passwords filled in, nothing saved) |
| `disconnect` | `sessionId` | `void` |
| `cancel` | `sessionId, scope?: "sql" \| "all"` | `void` (sync; never waits). `"sql"` (default): the SQL panel's run or stream; `"all"`: browsing too |
| `transaction_open` | `sessionId` | `boolean` (the user's transaction is open on the pinned connection) |
| `relations` | `sessionId` | `Relation[]` |
| `page` | `sessionId, relation, limit, offset, search, sort` | **binary** `ResultBlock` |
| `page_block` | `sessionId, relation, offset, limit, search, sort` | **binary** `ResultBlock` (for the grid's on-demand blocks; keyset where possible) |
| `count` | `sessionId, relation, search` | `number \| null` |
| `execute` | `sessionId, sql` | **binary** `Execution` (results cut at 5,000 rows) |
| `execute_stream` | `sessionId, sql, channel: Channel, rowCap?` | `void`; the rows come as `StreamMessage`s on `channel` |
| `read_only` | `sessionId, text` | **binary** `Execution` |
| `rehearse` | `sessionId, text` | **binary** `Execution` |
| `execute_atomically` | `sessionId, text` | **binary** `{kind: "ran", execution} \| {kind: "notAtomic", message}` |
| `update_row` | `sessionId, relation, key: RowKey, changes: CellValue[]` | `EditOutcome` |
| `insert_row` | `sessionId, relation, values: CellValue[]` | `EditOutcome` |
| `delete_row` | `sessionId, relation, key: RowKey` | `EditOutcome` |
| `schema_graph` | `sessionId` | `SchemaGraph` |
| `qualified_name` | `sessionId, relation` | `string` |
| `export_result` | `result: QueryResult` (row-major JSON; `api.exportResult` converts a block with `toQueryResult`), `format: "csv" \| "json"` | `string` |
| `list_connections` | – | `StoreSnapshot` |
| `save_connection` | `draft: ConnectionDraft` | `SavedConnection` (with `id` and a masked url, the saved password is kept) |
| `delete_connection` | `id` | `void` |
| `rename_connection` | `id, name` | `SavedConnection` |
| `connection_url` | `id` | `string` (masked, for the edit sheet) |
| `move_connection` | `id, folderId, beforeId?` | `void` |
| `add_folder` | `name` | `ConnectionFolder` |
| `rename_folder` | `id, name` | `void` |
| `delete_folder` | `id` | `void` (its connections move to the top level) |
| `set_folder_expanded` | `id, expanded` | `void` |
| `import_swift_connections` | – | `ImportReport { imported, skipped, errors: string[] }` (macOS only; elsewhere it rejects. Skips a connection with the same URL and name; errors name connections, never URLs) |

Window and menu commands (`menu.rs`, `windows.rs`):

| Command | Arguments | Returns |
|---|---|---|
| `new_window` | – | `void` (opens a window with the smallest free label `w2`, `w3`…) |
| `take_pending_open` | – | `string[]`: database files this window should open (from the command line or a second launch), once |
| `set_menu_state` | `state: {connected, hasTable, canInsert, canEdit, hasSelection, hasResult, canBack, canForward, sqlPanel, running}` | `void` (the focused window greys out menu rows) |

A saved URL with its password never reaches the frontend.

## Menus, shortcuts and windows

- `src/shortcuts.json` lists every action once: `action`, `keys` ("Mod+Shift+N";
  `Mod` is Cmd on macOS, Ctrl elsewhere; empty: menu only), `title`, optional
  `menu` (other wording in the menu), `inFields` (works while typing) and `os`
  (`"mac"`: that OS only, e.g. `importSwift`). `menu.rs` compiles the same file
  in; its tests fail when an action has no menu row, a row has no action, an
  accelerator repeats or `shortcuts.ts` lacks the action. A new action: a row
  here, the `Action` type, a place in `menu::layout`, a rule in `menu::enabled`
  if it can grey out, and a case in `App.perform`, `native/perform.ts` or
  `grid/commands.ts`.
- A menu click sends event **`menu-action`** (payload: the action id) to the
  focused window; window actions (`newWindow`, `closeWindow`, `quit`) run in Rust.
  Keys and clicks go through `native/actions.ts`'s `dispatch`, which runs a
  repeat of the same action within 120 ms once (accelerator and key handler
  both firing). The grid gets the first say (`gridAction`): copy, paging,
  Save/Revert Changes, Freeze First Column, the perf HUD; Set NULL is the window
  event `dblens:set-null`.
- Menus: File, Edit, View, Query, Help; on macOS the app menu (About,
  Settings…, Quit) and Window as well. Settings… is under File on Windows and
  Edit on Linux. Keys without a modifier (Delete) have no accelerator. On Linux,
  Undo/Redo/Quit are custom rows (muda has no predefined ones there). Menu
  labels are English for now.
- Windows: each window has its own sessions, closed when it is destroyed; size
  and position are kept per label (window-state plugin). On Windows and Linux a
  second launch raises the running app (single-instance) and opens any
  `.db`/`.sqlite`/`.sqlite3`/`.db3` it was given in a new window.
- Drops: a dropped database file connects and is saved under its file name; a
  dropped connection string opens the connect sheet filled in. Sidebar
  connections move between folders by pointer drag (not HTML5 drag and drop,
  which Tauri's file-drop handler turns off on Windows).

## Saved connections

`store.rs` keeps `connections.json` (names, engine, folder, order, SSH, dates)
and `connections.enc` (AES-256-GCM: 12-byte nonce + ciphertext of a JSON map
id → URL), both 0600 on Unix, written via temp file + rename. The key is in the
OS keyring (service `app.dblens.desktop`, user `connections-key`; Windows
Credential Manager, Linux Secret Service via zbus) or, when there is no keyring,
in a 0600 `connections.key` file. A damaged key is refused, never replaced.
Folder: Linux `~/.local/share/dblens`, Windows `%APPDATA%\dblens\dbLens\data`,
macOS `~/Library/Application Support/app.dblens.dbLens` (never the Swift app's
`~/Library/Application Support/dbLens`; on macOS the key is in the Keychain,
keyring feature `apple-native`).
`DBLENS_STORE_DIR=<dir>` moves it; `DBLENS_STORE=memory` keeps everything in
memory (use it for screenshots and UI work).

## SSH

`ssh.rs` runs the system `ssh` (`ssh` on PATH; on Windows,
`%SystemRoot%\System32\OpenSSH\ssh.exe` first) with `-N -T -L
127.0.0.1:<free port>:<db host>:<db port>`, BatchMode, accept-new host keys,
`--` before the host. `problem()` refuses values that could inject options.
`connector::connect` opens the tunnel from the driver's `summary` host/port and
passes `local_port` to the driver; the tunnel lives with the session and is
killed on disconnect or drop.

## Crates

tauri 2.12, tokio 1, async-trait, thiserror 2, serde/serde_json, rmp 0.8 and
rmp-serde 1.3 (MessagePack; JS: `@msgpack/msgpack` 3.1), regex,
uuid, chrono, percent-encoding, futures-util;
tokio-postgres 0.7.18 + tokio-postgres-rustls 0.14 (ring, webpki-roots),
rustls 0.23 (ring; the process-wide provider is installed in `lib.rs`),
mysql_async 0.37 (`default-rustls-ring`), rusqlite 0.41 (`bundled`),
tokio-rustls 0.26 and webpki-roots (the Redis driver's own TLS),
mongodb 3.9 (rustls, dns-resolver);
aes-gcm 0.10, base64 0.22, keyring 3.6 (`windows-native`, `async-secret-service`,
`async-io`, `crypto-rust`), directories 6. No OpenSSL and no aws-lc anywhere.

## Build and test

```sh
cd desktop
npm install
npm run build                       # tsc + vite → dist/
npm run tauri dev                   # the app, with hot reload
npm run tauri build                 # bundles: deb, rpm, AppImage (Linux); msi, nsis (Windows); app, dmg (macOS)

cd src-tauri
cargo check
cargo test                          # unit tests; integration tests skip without env vars
cargo clippy --all-targets -- -D warnings
cargo check --target x86_64-pc-windows-gnu    # cross check from Linux
```

Integration test env vars (throwaway servers only):

```
DBLENS_TEST_URL=postgres://dblens:dblens@127.0.0.1:5432/dblens_test
DBLENS_TEST_MYSQL_URL=mysql://dblens:dblens@127.0.0.1:3306/dblens_test
DBLENS_TEST_REDIS_URL=redis://127.0.0.1:6379/15
DBLENS_TEST_MONGO_URL=mongodb://root:<pw>@127.0.0.1:57017/shop?authSource=admin   (Docker mongo:8 with a root user; skip when unset)
```

`DBLENS_TEST_MARIADB_URL` and `DBLENS_TEST_VALKEY_URL` (set by CI) are read by
no test yet: to test MariaDB or Valkey, point `DBLENS_TEST_MYSQL_URL` or
`DBLENS_TEST_REDIS_URL` at them and run `cargo test --test mysql` / `--test redis`.

SQLite tests always use temp files (`tempfile` is a dev-dependency).

Linux build needs, besides WebKitGTK: `libxdo-dev` (muda, the menu crate, links
libxdo). `tests/contract.rs` writes the real MessagePack bytes to
`$DBLENS_WIRE_FIXTURES` when it is set, so the JS decoder can be checked against
them (see docs/PERF.md, "Contract check").
