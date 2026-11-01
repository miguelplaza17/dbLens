# dbLens for Windows and Linux

A fast, minimal database client: paste a connection string, click a table, see
and edit the rows, run SQL. PostgreSQL, MySQL/MariaDB, SQLite, Redis/Valkey and
MongoDB. Built with Tauri 2 (Rust + React). The macOS app lives in the repo root.

Work in progress: see [CONTRACT.md](CONTRACT.md) for the architecture.

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

Bundles land in `src-tauri/target/release/bundle/`.

## Test

```sh
cd desktop/src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
```

Driver integration tests run only with `DBLENS_TEST_URL`, `DBLENS_TEST_MYSQL_URL`,
`DBLENS_TEST_REDIS_URL` or `DBLENS_TEST_MONGO_URL` set to throwaway servers.

Saved connections live in `~/.local/share/dblens` (Linux) or
`%APPDATA%\dblens\dbLens\data` (Windows), URLs encrypted with a key kept in the
system keyring. `DBLENS_STORE=memory` keeps them in memory instead.
