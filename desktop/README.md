# dbLens for Windows and Linux

A fast, minimal database client: paste a connection string, click a table, see
and edit the rows, run SQL. PostgreSQL, MySQL/MariaDB, SQLite, Redis/Valkey and
MongoDB. Built with Tauri 2 (Rust + React). The macOS app lives in the repo root.

An early version: every engine works against real servers, and the app builds
and starts on Linux. See "Status" below, and [CONTRACT.md](CONTRACT.md) for the
architecture.

## Prerequisites

- Rust (stable, 1.80 or later) and Node.js 20 or later.
- **Linux**: WebKitGTK 4.1 and friends. Debian/Ubuntu:
  `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev build-essential`
  (Fedora: `webkit2gtk4.1-devel gtk3-devel librsvg2-devel`).
- **Windows 10/11**: Visual Studio Build Tools with "Desktop development with C++",
  and WebView2 (built into Windows 11). SSH tunnels use Windows' built-in OpenSSH.

No database client libraries are needed: the drivers are pure Rust (SQLite is bundled).

## Run

```sh
cd desktop
npm install
npm run tauri dev      # the app, with hot reload
npm run tauri build    # Linux: .deb, .rpm, .AppImage   Windows: .msi, setup .exe
```

Bundles land in `src-tauri/target/release/bundle/`. On Linux,
`npm run tauri build -- --bundles deb,appimage` makes only those two.

In a plain browser, `npm run dev` (then http://localhost:1420) runs the UI on
the mock backend in `src/mock.ts`: a sample Postgres database and a Redis one.
`node docs/shots/take.cjs` takes screenshots of it (needs Playwright).

## Test

```sh
cd desktop/src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
```

Driver integration tests run only with these set to throwaway servers, and skip
without them (SQLite tests always run, on temp files):

```sh
DBLENS_TEST_URL=postgres://dblens:dblens@127.0.0.1:5432/dblens_test
DBLENS_TEST_MYSQL_URL=mysql://dblens:dblens@127.0.0.1:3306/dblens_test   # MySQL 8 or MariaDB
DBLENS_TEST_REDIS_URL=redis://127.0.0.1:6379/15                          # Redis or Valkey; db 15 is emptied of test keys
DBLENS_TEST_MONGO_URL=mongodb://root:pw@127.0.0.1:27017/shop?authSource=admin
```

Cross check for Windows from Linux (needs `rustup target add x86_64-pc-windows-gnu`
and mingw-w64): `cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings`.

Saved connections live in `~/.local/share/dblens` (Linux) or
`%APPDATA%\dblens\dbLens\data` (Windows), URLs encrypted with a key kept in the
system keyring. `DBLENS_STORE=memory` keeps them in memory instead.

## Status

Checked on Linux: clippy clean (Linux and the Windows target), unit tests,
integration tests against PostgreSQL 16, MariaDB 10.11, Redis 7 and MongoDB 8,
a `.deb` and an AppImage, and the app starting under Xvfb. A Windows `.exe`
cross-compiles but has not run on Windows yet.

### Per engine

| Engine | Works | Missing or different |
|---|---|---|
| PostgreSQL | browse, page/sort/search, count, edit (by primary key, or `ctid`+`xmin`), SQL, read-only, rehearse, all-or-nothing, schema graph, cancel, all `sslmode`s, `sslrootcert`, SSH | client certificates, `.pgpass`, `service`; `allow` acts like `prefer`; COPY; `verify-*` use bundled Mozilla roots, not the OS store |
| MySQL / MariaDB | the same, with `KILL QUERY` for cancel; DDL refused in rehearse/all-or-nothing (it commits by itself) | tested on MariaDB 10.11 only (not MySQL 8 or MariaDB 11 yet); TLS verify modes untested; `?socket=` untested |
| SQLite | the same, on a file; falls back to read-only; `rowid` keys | SSH does not apply |
| Redis / Valkey | keys grouped by prefix, pages for every type, edits with WATCH/MULTI, commands in the bottom panel (redis-cli quoting), cancel, TLS, SSH | no rehearse or all-or-nothing; stream pages read-only; not run against Valkey; bundled roots only (`ssl_cert_reqs=none` for a private CA) |
| MongoDB | collections and views, pages, search, edits by `_id` keeping BSON types, mongosh commands in the bottom panel, cancel (kills the server operation), SSH to a single host | legacy `$binary`/`$type` form; unknown option keys ignored; no rehearse or all-or-nothing |

### Compared with the macOS app

The desktop app has: saved connections in folders (URLs encrypted, key in the
OS keyring), paste-a-URL or fields, SSH tunnels, the table grid with inline and
sheet editing, the SQL panel with Rehearse and All or Nothing, a log, keyboard
shortcuts with a shortcuts sheet, dark and light themes, English and Portuguese.

Not yet:

- **Schema board.** The backend has `schema_graph`; the UI does not draw it.
- **MCP server** (`dbLens --mcp` for agents).
- **Export to a file.** Export copies CSV or JSON to the clipboard; there is no save dialog.
- **Updater** and release signing (no code signing for Windows or Linux yet).
- **Native menus.** Actions live in the window, context menus and keyboard
  shortcuts; there is no menu bar.
- **Settings window** (accent color, language, agents, data).
- **"Open in terminal"** with the engine's own client (`ClientCommand` in the macOS app).
- **Read-only runs** from the UI: `read_only` exists in the backend but the UI does not use it.
- Seed data, self-test and snapshot tools of the macOS app.
