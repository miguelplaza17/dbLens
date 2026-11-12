# dbLens

Personal notes for your own agent go in `CLAUDE.local.md` (git ignores it).

A database client for Mac, Windows and Linux, built with Tauri 2 (Rust core, React UI) in `desktop/`.
PostgreSQL (libpq-free, pure Rust), MySQL/MariaDB, SQLite, Redis/Valkey and MongoDB, an MCP server for
agents (`dblens --mcp`), and saved connections encrypted on this computer. The SwiftUI Mac app that was
dbLens up to 1.4 is retired and removed from the repository (it is in git history before 1.5.0).

## Layout and rules


- On macOS the Tauri app uses the Swift app's bundle id (`src-tauri/tauri.macos.conf.json`), installs over it, and on first launch imports its connections, folders, SQL history and settings (`import_swift.rs`). One version in `VERSION`, `tauri.conf.json`, `Cargo.toml`, `Cargo.lock` and `package.json`, and one tag `vX.Y.Z`: CI (`.github/workflows/release.yml`) releases Windows and Linux, `tools/release.sh` the Mac (universal, notarized, Tauri update, Sparkle appcast for old Swift installs). The number only goes up. The updater key is in `~/Library/Developer/dbLens/tauri-updater-key/` and the CI secret `TAURI_SIGNING_PRIVATE_KEY`; never commit it. `desktop/docs/MIGRATION.md` lists what is left. `desktop/CONTRACT.md` has the architecture, `desktop/README.md` the status, `desktop/docs/PERF.md` the numbers.
- `desktop/src-tauri/` the Rust core: `session.rs` (the `DatabaseSession` trait), `drivers/` (one per engine), `pool.rs` (a pool per open database; the SQL panel's transaction stays pinned to one connection), `wire.rs` (rows cross the IPC as column-major MessagePack, never JSON; SQL results stream over a `Channel`), `menu.rs` + `windows.rs` (native menu, several windows), `import_swift.rs` (macOS: the Swift app's connections), `settings.rs` (the Settings window, `settings.json`), `mcp.rs` (`dblens --mcp`, no window), `updater.rs`, `history.rs` (SQL history), `terminal.rs`, `store.rs`, `ssh.rs`, `commands.rs`. `desktop/src/` the React UI: `grid/` (Glide Data Grid, row blocks on demand, pending edits sent as diffs), `schema/` (the schema board, one canvas), `settings/` (Settings window, accents), `history/`, `nav.ts` (Back/Forward), `native/` (one action dispatcher for keys and menu clicks); `api.ts` mirrors the Rust types, checked by `tests/contract.rs`; `src/mock.ts` answers in a plain browser.
- Every action is one row in `desktop/src/shortcuts.json` (keys, title, menu) with an en + pt-BR string in `src/i18n/`; `menu.rs`'s tests fail when an action has no menu row. Settings live in `settings.json`, except "Let agents add and remove connections", which is in the store because the MCP server is a separate process.
- Build: `cd desktop && npm install && npm run tauri build` (`-- --bundles deb` on Linux; Linux needs `libxdo-dev`). UI only: `npm run dev`, then http://localhost:1420.
- Check, in `desktop/src-tauri`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo check --target x86_64-pc-windows-gnu`, `cargo test`; in `desktop`: `npm run build` and `npm test` (the sidebar's drag spots, `tests/dragSpots.test.ts`). Integration tests take `DBLENS_TEST_URL`, `DBLENS_TEST_MYSQL_URL`, `DBLENS_TEST_REDIS_URL`, `DBLENS_TEST_MONGO_URL` (with credentials) and skip without them; SQLite tests use temp files.
- `DBLENS_STORE=memory` keeps saved connections in memory; never point the app at the real store when testing. The perf HUD is Ctrl/Cmd+Shift+P.
- Real app on Linux without a display: `npm run build`, then `cargo build --features tauri/custom-protocol` and run `target/debug/dblens` under `Xvfb :91 -screen 0 1600x1000x24` with `DISPLAY=:91 WEBKIT_DISABLE_COMPOSITING_MODE=1 DBLENS_STORE=memory`; drive it with `xdotool`, capture with `import -window root`. Stop it with `pkill -x dblens` (`pkill -f` matches your own shell).

## Releases

- `desktop/README.md`, "Updates": push the tag `vX.Y.Z` (CI builds Windows and Linux and writes `latest.json`), then `PUBLISH=1 tools/release.sh` on the Mac (notarized universal app, DMG, signed update, Sparkle `appcast.xml` for old Swift installs), then the Homebrew cask in `docs/homebrew/dblens.rb` (the live one is in `miguelplaza17/homebrew-tap`).
- `tools/sign_update.sh` signs the Sparkle appcast with the key in the login keychain; keep it while Swift installs may still be out there.
- The Mac icon is `desktop/src-tauri/icons/mac/AppIcon.icon` (Icon Composer), compiled to `Assets.car` and `AppIcon.icns` beside it with `xcrun actool AppIcon.icon --compile <out> --platform macosx --minimum-deployment-target 26.0 --app-icon AppIcon --output-partial-info-plist <out>/p.plist`; `tools/release.sh` puts them in the app.

## Git

- Commit after each working change, with a message that says what the user can now do.
