# dbLens

A fast, minimal database client for Windows, Linux and macOS: paste a connection
string, click a table, see and edit the rows, run SQL. PostgreSQL, MySQL/MariaDB,
SQLite, Redis/Valkey and MongoDB. Built with Tauri 2 (Rust + React). This is the
only dbLens from now on: the Swift macOS app in the repo root is discontinued
(see [docs/MIGRATION.md](docs/MIGRATION.md)).

Every engine works against real servers; the app is built and run on Linux. See
"Status" below, and [CONTRACT.md](CONTRACT.md) for the architecture.

## Prerequisites

- Rust (stable, 1.80 or later) and Node.js 20 or later.
- **Linux**: WebKitGTK 4.1 and friends, and libxdo (the menu bar). Debian/Ubuntu:
  `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev build-essential`
  (Fedora: `webkit2gtk4.1-devel gtk3-devel librsvg2-devel libxdo-devel`).
- **Windows 10/11**: Visual Studio Build Tools with "Desktop development with C++",
  and WebView2 (built into Windows 11). SSH tunnels use Windows' built-in OpenSSH.
- **macOS 13 or later**: Xcode or its Command Line Tools (`xcode-select --install`);
  `build.rs` compiles `src-tauri/swift/SecureEnclaveOpen.swift` with `swiftc`.
  Universal app: `rustup target add aarch64-apple-darwin x86_64-apple-darwin`, then
  `npm run tauri build -- --target universal-apple-darwin` (.app and .dmg).
  Signing and notarization come only from the environment (`APPLE_SIGNING_IDENTITY`,
  `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`,
  `APPLE_TEAM_ID`); `APPLE_SIGNING_IDENTITY=-` signs ad hoc.

No database client libraries are needed: the drivers are pure Rust (SQLite is bundled).

## Run

```sh
cd desktop
npm install
npm run tauri dev      # the app, with hot reload
npm run tauri build    # Linux: .deb, .rpm, .AppImage   Windows: .msi, setup .exe   macOS: .app, .dmg
```

Bundles land in `src-tauri/target/release/bundle/`. On Linux,
`npm run tauri build -- --bundles deb,appimage` makes only those two.

In a plain browser, `npm run dev` (then http://localhost:1420) runs the UI on
the mock backend in `src/mock.ts`: a sample Postgres database and a Redis one.
`node docs/shots/take.cjs` takes screenshots of it (needs Playwright).

## Updates

Help > Check for Updates… (macOS: the app menu; shortcut Ctrl/Cmd+Alt+U) asks the
release feed now. With Settings > Updates > "Check for updates automatically"
(on by default) the app checks once a day, and "Update automatically" downloads
an update when it is found and installs it when the app quits. A small notice
in the corner offers "Install and Restart". The code is in
`src-tauri/src/updater.rs` (`tauri-plugin-updater`, the same version as
`@tauri-apps/plugin-updater` in `package.json`; the two must match exactly).

The feed is `https://github.com/miguelplaza17/dbLens/releases/latest/download/latest.json`,
set in `plugins.updater` in `src-tauri/tauri.conf.json`, with the public key
there too. The committed key is a placeholder: until it is replaced, updates are
off, and a check says "Updates are not configured in this build". Nothing is
fetched or installed.

To turn updates on (once, on a trusted machine; never commit the private key):

1. `cd desktop && npx tauri signer generate -w ~/.tauri/dblens.key` (asks for a
   password). It prints the public key.
2. Put that public key (the whole line, base64) in `plugins.updater.pubkey` in
   `src-tauri/tauri.conf.json`.
3. In CI secrets, set `TAURI_SIGNING_PRIVATE_KEY` (the file's contents, or its
   path on the runner) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
4. Build with signed updater artifacts: add
   `--config '{"bundle":{"createUpdaterArtifacts":true}}'` to `npm run tauri build`
   (it is left off in the committed config, so builds without a key still work).
   This writes a `.sig` next to each package (AppImage, NSIS/MSI, the macOS
   `.app.tar.gz`).
5. Publish a `latest.json` as a release asset named `latest.json`: the version,
   notes, and for each platform (`linux-x86_64`, `windows-x86_64`,
   `darwin-aarch64`, `darwin-x86_64`) the package URL and the contents of its
   `.sig`. `tauri-apps/tauri-action` writes it when it builds the release.

Updates cover AppImage, Windows installers and the macOS app. A `.deb` or `.rpm`
user updates by installing the new package.

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
DBLENS_TEST_MONGO_URL=mongodb://root:pw@127.0.0.1:57017/shop?authSource=admin  # the server needs a root user
```

Cross check for Windows from Linux (needs `rustup target add x86_64-pc-windows-gnu`
and mingw-w64): `cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings`.

Saved connections live in `~/.local/share/dblens` (Linux),
`%APPDATA%\dblens\dbLens\data` (Windows) or
`~/Library/Application Support/app.dblens.dbLens` (macOS), URLs encrypted with a
key kept in the system keyring (Keychain on macOS). `DBLENS_STORE=memory` keeps them in memory instead.

## Status

Checked on Linux (2026-11-02, after the Settings / schema board / MCP / updater /
history integration): `cargo fmt --check`, clippy clean with `-D warnings`,
`cargo check --target x86_64-pc-windows-gnu`, unit tests, integration tests against
PostgreSQL 16, MariaDB 10.11, Redis 7 and MongoDB 8 (including `tests/mcp.rs` and
`tests/stream_nosql.rs`), `npm run build`, and the debug app run under Xvfb against
the test PostgreSQL: connect, the schema board (Ctrl+3, fit, Ctrl+= zoom, double-click
to open rows), a query, the SQL history popover (Ctrl+Shift+H), Back (Ctrl+[), File >
Let Agents Write (Ctrl+Alt+A), and the Settings window (Ctrl+,): every tab, Light and
the Mint accent applied live to both windows, Check Now, Clear All History…, Ctrl+W.
Screenshots: `docs/shots/linux-*.png`. Earlier: a release `.deb` and a
1,000,000-row table under Xvfb. Windows: cross-compiles, never run. macOS: configured
(bundle, signing from the environment, CI job, the Swift-app import) but never
compiled or run.

- **Grid**: Glide Data Grid (canvas). Table rows come in 300-row blocks as you
  scroll (visible first, neighbours after a short pause, 50 blocks kept); the
  footer shows "Rows a–b of N" with an estimate ("~") until the count is in. Sort
  by header, resize, Freeze First Column, NULL dimmed, numbers right-aligned.
  Edits stay pending (highlighted) until you leave the row or press Ctrl/Cmd+S,
  then go out as one `update_row` with only the changed columns; Escape or
  Revert Changes drops them.
- **Performance core**: table data crosses the IPC as column-major MessagePack
  (never JSON), blocks use keyset pagination where there is a key, SQL results
  stream in blocks (first 500 rows at once, up to 100,000 kept), and each open
  database has a small pool so browsing, counting and the SQL panel don't wait
  for each other (your transaction stays on one connection). Numbers, from the
  bench and from the app's perf HUD, in [docs/PERF.md](docs/PERF.md).
- **Native**: a menu bar with an accelerator for every action that has keys
  (built from `src/shortcuts.json`, the same table as the key handler), several
  windows (Ctrl/Cmd+Alt+N), Open SQLite File… (Ctrl/Cmd+O), dropped database
  files, single instance on Windows and Linux (a second launch opens its files in
  a new window), window size and position remembered.
- **Settings window** (Ctrl/Cmd+,): Appearance (System / Light / Dark and the 12
  accents, live in every window), Connections (Clear History per connection),
  Agents (the `--mcp` command, Claude Code / Codex commands, JSON, "Let agents add
  and remove connections"), Updates, Data & Privacy. Saved in `settings.json`.
- **Schema board** (Ctrl/Cmd+3, `src/schema/`): cards, foreign-key curves, dagre
  layout, pan, zoom (Ctrl/Cmd+= / - / 0), minimap, Key Columns Only, positions kept
  per connection.
- **MCP server**: `dblens --mcp` (JSON-RPC on stdio, no window; `src-tauri/src/mcp.rs`).
  Reads are read-only in the database; writes need File > Let Agents Write
  (Ctrl/Cmd+Alt+A, or the sidebar menu) on the connection.
- **SQL history** per saved connection (Ctrl/Cmd+Shift+H), **Open in Terminal**
  (Ctrl/Cmd+Shift+T), **Copy URL** with or without the password, **Back / Forward**
  (Ctrl/Cmd+[ and ]).

### Per engine

| Engine | Works | Missing or different |
|---|---|---|
| PostgreSQL | browse, page/sort/search, count, edit (by primary key, or `ctid`+`xmin`), SQL, read-only, rehearse, all-or-nothing, schema graph, cancel, all `sslmode`s, `sslrootcert`, SSH | client certificates, `.pgpass`, `service`; `allow` acts like `prefer`; COPY; `verify-*` use bundled Mozilla roots, not the OS store |
| MySQL / MariaDB | the same, with `KILL QUERY` for cancel; DDL refused in rehearse/all-or-nothing (it commits by itself) | tested on MariaDB 10.11 only (not MySQL 8 or MariaDB 11 yet); TLS verify modes untested; `?socket=` untested |
| SQLite | the same, on a file; falls back to read-only; `rowid` keys | SSH does not apply |
| Redis / Valkey | keys grouped by prefix, pages for every type, edits with WATCH/MULTI, commands in the bottom panel (redis-cli quoting), cancel, TLS, SSH | no rehearse or all-or-nothing; stream pages read-only; not run against Valkey; bundled roots only (`ssl_cert_reqs=none` for a private CA) |
| MongoDB | collections and views, pages, search, edits by `_id` keeping BSON types, mongosh commands in the bottom panel, cancel (kills the server operation), SSH to a single host | legacy `$binary`/`$type` form; unknown option keys ignored; no rehearse or all-or-nothing |

### Moving from dbLens for Mac

On macOS, File > Import Connections from dbLens for Mac… (also a button in the
empty window while there are no saved connections; command
`import_swift_connections`) copies the Swift app's saved connections, their
URLs, folders, SSH settings and dates. It reads
`~/Library/Application Support/dbLens/` from a temporary copy and writes nothing
there; this app keeps its own folder. Importing twice adds nothing. URLs sealed
with the Secure Enclave open only on the Mac (and user) that saved them. Not yet
run on a Mac.

### Compared with the macOS app

The desktop app has: saved connections in folders (URLs encrypted, key in the
OS keyring), paste-a-URL or fields, SSH tunnels, the table grid with inline and
sheet editing, the SQL panel with Rehearse, All or Nothing and history, a log,
the schema board, the MCP server for agents, the Settings window with accent
colors, the updater, native menus and keyboard shortcuts with a shortcuts sheet,
several windows, dark and light themes, English and Portuguese.

Not yet (full list in [docs/MIGRATION.md](docs/MIGRATION.md)):

- **Schema board**: no sticky notes, card colors or marquee selection.
- **Updates**: wired, but off until a real public key and a `latest.json` release
  feed exist (see Updates above). Code signing for Windows and Linux.
- **Agents**: no one-click install into agent apps; what agents run is not in the
  SQL history.
- **Export to a file.** Export copies CSV or JSON to the clipboard; there is no save dialog.
- **Read-only runs** from the UI: `read_only` exists in the backend but the UI does not use it.
- Seed data, self-test and snapshot tools of the macOS app.
