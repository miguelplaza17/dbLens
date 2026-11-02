# dbLens desktop: the contract

The Windows and Linux version of dbLens, in Tauri 2. Same product as the macOS
app: paste a connection string, click a table, see and edit rows, run SQL. Fast,
minimal, one window: sidebar | grid | foldable SQL panel.

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
| `src-tauri/src/connector.rs` | `summary`, `connect` (SSH tunnel, then driver) |
| `src-tauri/src/drivers/postgres.rs` | Postgres (tokio-postgres + rustls) |
| `src-tauri/src/drivers/mysql.rs` | MySQL/MariaDB (mysql_async) |
| `src-tauri/src/drivers/sqlite.rs` | SQLite (rusqlite, bundled) |
| `src-tauri/src/drivers/redis.rs` | Redis/Valkey (own RESP client on tokio, like `Redis.swift`) |
| `src-tauri/src/drivers/mongo.rs`, `mongo_shell.rs` | MongoDB (official mongodb crate) and the mongosh parser |
| `src-tauri/src/ssh.rs` | SSH tunnel via the system `ssh` |
| `src-tauri/src/store.rs` | saved connections, folders, sealed URLs |
| `src-tauri/src/commands.rs`, `lib.rs` | IPC commands and wiring |
| `src-tauri/tests/*.rs` | one integration file per engine, plus `contract.rs` (the JSON shape `api.ts` expects) |
| `src/api.ts` | typed IPC wrappers and TS types; keep in sync with Rust |
| `src/mock.ts` | answers every command in a plain browser (vite dev without Tauri) |
| `src/i18n.ts`, `src/i18n/*.json` | `t()`; English source, `pt-BR` |
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
}
```

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
| `QueryResult { columns, rows, command, truncated, total_rows, keys, note }` | `{ columns, rows, command, truncated, totalRows, keys, note }` |
| `Execution { results, duration_ms }` | `{ results, durationMs }` |
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
| `AtomicOutcome` | `{ kind: "ran", execution } \| { kind: "notAtomic", message }` |
| `DbError` | a plain string (its message) |

`DbError` variants: `Message` (server/driver text), `NotAtomic`, `Cancelled`,
`Unsupported`, `NoSession`. Over IPC all are just the message string;
`execute_atomically` turns `NotAtomic` into `AtomicOutcome::NotAtomic`.

## IPC commands (`src-tauri/src/commands.rs`)

JS argument names are camelCase (Tauri maps them to the snake_case parameters).
`src/api.ts` has a wrapper for each; the UI should call `api.*`, not `invoke`.

| Command | Arguments | Returns |
|---|---|---|
| `detect_engine` | `text` | `Engine \| null` |
| `summarize_url` | `text` | `Summary \| null` |
| `mask_url` | `text` | `string` (passwords → •••) |
| `connect` | `url?`, `savedId?`, `ssh?` | `ConnectInfo` (by saved id: URL and SSH come from the store, last used is updated; with `url` too: tries that edit of the saved connection, masked passwords filled in, nothing saved) |
| `disconnect` | `sessionId` | `void` |
| `cancel` | `sessionId` | `void` (sync; never waits) |
| `relations` | `sessionId` | `Relation[]` |
| `page` | `sessionId, relation, limit, offset, search, sort` | `QueryResult` |
| `count` | `sessionId, relation, search` | `number \| null` |
| `execute` | `sessionId, sql` | `Execution` |
| `read_only` | `sessionId, text` | `Execution` |
| `rehearse` | `sessionId, text` | `Execution` |
| `execute_atomically` | `sessionId, text` | `AtomicOutcome` |
| `update_row` | `sessionId, relation, key: RowKey, changes: CellValue[]` | `EditOutcome` |
| `insert_row` | `sessionId, relation, values: CellValue[]` | `EditOutcome` |
| `delete_row` | `sessionId, relation, key: RowKey` | `EditOutcome` |
| `schema_graph` | `sessionId` | `SchemaGraph` |
| `qualified_name` | `sessionId, relation` | `string` |
| `export_result` | `result: QueryResult, format: "csv" \| "json"` | `string` |
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

A saved URL with its password never reaches the frontend.

## Saved connections

`store.rs` keeps `connections.json` (names, engine, folder, order, SSH, dates)
and `connections.enc` (AES-256-GCM: 12-byte nonce + ciphertext of a JSON map
id → URL), both 0600 on Unix, written via temp file + rename. The key is in the
OS keyring (service `app.dblens.desktop`, user `connections-key`; Windows
Credential Manager, Linux Secret Service via zbus) or, when there is no keyring,
in a 0600 `connections.key` file. A damaged key is refused, never replaced.
Folder: Linux `~/.local/share/dblens`, Windows `%APPDATA%\dblens\dbLens\data`.
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

tauri 2.12, tokio 1, async-trait, thiserror 2, serde/serde_json, regex,
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
npm run tauri build                 # bundles: deb, rpm, AppImage (Linux); msi, nsis (Windows)

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
DBLENS_TEST_MONGO_URL=mongodb://…              (Docker mongo:8; skip when unset)
```

`DBLENS_TEST_MARIADB_URL` and `DBLENS_TEST_VALKEY_URL` (set by CI) are read by
no test yet: to test MariaDB or Valkey, point `DBLENS_TEST_MYSQL_URL` or
`DBLENS_TEST_REDIS_URL` at them and run `cargo test --test mysql` / `--test redis`.

SQLite tests always use temp files (`tempfile` is a dev-dependency).
