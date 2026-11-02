# dbLens

Personal notes for your own agent go in `CLAUDE.local.md` (git ignores it).

A native macOS app in SwiftUI. It runs on this Mac only, with a local
SwiftData database and no server.

## Layout

- `Sources/dbLens/` the app. `App.swift` has the window, menus, and the database location.
- `Sources/DBLensKit/` rules and data logic with no SwiftUI. Tests in `Tests/DBLensKitTests/`.
- `Package.swift` a Swift package. There is no Xcode project.
- `build.sh` builds, signs, and installs `~/Applications/dbLens Dev.app`: its own name, bundle id (`….dev`), and settings, no updates; the released app in /Applications stays as it is. `tools/release.sh` makes the real `dbLens.app`.
- `Icon/AppIcon.icon` the app icon, an Icon Composer file (JSON plus SVG layers). `build.sh` compiles it with `tools/make_icon.sh`; `tools/icon_review.sh` renders every look into one PNG.
- `docs/` the brief, the screen index, and the user stories, when they exist.

## Build and check

- Build and install: `./build.sh`. Add `--open` to restart the app with the new build. The build shows warnings; fix them.
- Unit tests: `swift test --scratch-path ~/Library/Developer/dbLens/test-build`.
- Self-test (the app's real functions on a /tmp store): `tools/selftest.sh`. Exit 0 when every check passes. Add a check for every story with logic in `SelfTest.swift`.
- After every UI change, take snapshots with sample data and look at them: `tools/snap.sh`.
  It writes `/tmp/dbLens-shots/<page>-light.png` and `-dark.png` for every sidebar page, and `settings-<tab>-light.png` and `-dark.png` for every Settings tab, then quits. `APP_SNAPSHOT_PAGES=all,settings tools/snap.sh` limits the pages.
  For a panel, sheet, or popover, add a capture to `Snapshot.swift` like the Settings loop.
- Snapshots, seed data, and the self-test refuse to run without `APP_STORE` in /tmp.
- Never test against the real database in `~/Library/Application Support/dbLens/`.
- Then check the grid: `tools/grid-lint.sh` and `swift tools/check-layout.swift /tmp/dbLens-shots/*-layout.json`. Both print PASS.
- A change is done when the build has no errors, the grid checks pass, and you looked at the light and dark snapshots.

## Open source and releases

- English is the source language in code; translations are `Localization/<lang>.json` (key → text), compiled into `<lang>.lproj` by `tools/strings.sh` during `build.sh`. After changing UI text, run `tools/check-strings.sh` (exits 1 on a missing or unused key) and add the Portuguese to `Localization/pt-BR.json`. Database data (names, SQL, values, Postgres messages) is never localized: use `Text(verbatim:)`.
- `tools/bundle_libs.sh` copies libpq and its Homebrew libraries into `Contents/Frameworks` (the app runs without Homebrew); `build.sh` signs them before the app.
- `VERSION` holds the version. `tools/release.sh` builds with a Developer ID certificate, the hardened runtime, notarizes, staples, and zips into `dist/`. `docs/homebrew/dblens.rb` is the cask template.
- Snapshots for the README: a database named `demo` with English sample data, `-AppleLanguages "(en)"`.

## Design rules

- Use Apple's controls and system styles. Only the accent color is custom: `UI/Theme.swift` (`ThemeStore.defaultID`). People change it, and light or dark mode, in Settings > Appearance.
- Views use `.tint`, `Palette.accent`, `Palette.accentText` (accent-colored text), and `Palette.onAccent` (text on an accent fill). Never hard-code colors, radii, or durations in views.
- Compare accents: add `APP_ACCENT=<id>` to the snapshot command.
- Spacing comes from `Space` in `UI/Grid.swift`, on a 4 pt grid. Mark custom pages and cards with `.gridArea` and `.gridProbe`.
- Windows open calm: `.noInitialFocus()` on every window and sheet (no focus ring or selected text on open), `.fitsContent()` on sheets and Settings tabs (no scroll bar with nothing to scroll). System scroll bars. Both in `UI/WindowBehavior.swift`.
- Settings: one tab per topic in `SettingsView.swift`, one grouped Form per tab. Add a case to `SettingsTab` for a new tab.
- Window chrome (title bar, toolbar, sidebar, menus, sheets) stays system. Buttons use system styles, like `.borderedProminent`.
- Colors: `.primary`, `.secondary`, `.tint`, and system colors. Never fixed grays or white for text.
- Every action has a menu bar item and a keyboard shortcut.
- `Label` for icon and text pairs. SF Symbols for icons.
- `ContentUnavailableView` for empty states.
- Liquid Glass belongs to floating controls only, never to content.

## Git

- Commit after each working change, with a message that says what the user can now do.

## dbLens specifics

- Engines: every driver is an actor conforming to `DatabaseSession` (DBLensKit/Session.swift); `Connector` picks it from the pasted text (`Engine.detect`). Postgres via libpq, MySQL/MariaDB via MariaDB Connector/C (its login plugins are bundled in `Frameworks/mariadb-plugins`), SQLite via the system library. MongoDB via libmongoc 2 (Mongo.swift; mongosh parsing in MongoShell.swift; Package.swift pins the header folder version `mongoVersion`). Redis/Valkey via an own RESP client over Network.framework (Redis.swift); keys are relations of kind `.key`, grouped by prefix, and the bottom panel runs commands (`AppModel.usesCommands`). Tests and the self-test take `DBLENS_TEST_URL`, `DBLENS_TEST_MYSQL_URL`, `DBLENS_TEST_MARIADB_URL`, `DBLENS_TEST_REDIS_URL`, `DBLENS_TEST_VALKEY_URL`, `DBLENS_TEST_MONGO_URL` for throwaway servers (Docker: `mysql:8.4`, `mariadb:11`, `redis:7.4`, `valkey/valkey:8`, `mongo:8`); SQLite tests always run on temp files. The schema board (SchemaBoard.swift) draws `DatabaseSession.schemaGraph()`.

- libpq comes from Homebrew `postgresql@16` (`Sources/CLibPQ`, linked in `Package.swift`). Without it the app does not build or start.
- Connection URLs live encrypted in `~/Library/Application Support/dbLens/connections.enc` (AES-GCM, `SecretBox` in DBLensKit) with the key bound to the Secure Enclave in `connections.key` (both 0600), not the Keychain: ad hoc and non-Developer-ID builds made macOS ask for the password after every rebuild. Saved passwords never show on screen: the edit sheet shows `ConnectionURL.mask` and `ConnectionURL.unmask` restores them on save. Test runs keep URLs in memory.
- Self-test and snapshots need a throwaway Postgres: `DBLENS_TEST_URL=postgres://… tools/selftest.sh` (it creates and drops one table).
- The app is always dark (`AppDelegate`), so snapshots are dark only.
- SSH: `SSHTunnel` (DBLensKit) runs the Mac's `/usr/bin/ssh -N -L` with BatchMode, so keys must come from ssh-agent, a key file, or ~/.ssh/config (no password prompts). The session owns its tunnel; a dropped tunnel reconnects in `AppModel.tunnelDropped`. To test it, run a throwaway sshd on a high port with its own host key and authorized_keys, then `DBLENS_TEST_SSH="127.0.0.1:<port>|<client key>" tools/selftest.sh` (and `swift test`). Never add keys to the real ~/.ssh/authorized_keys.

## desktop/ (Tauri, Windows and Linux)

- A separate Tauri 2 app with the same product; the macOS rules above (build.sh, snap.sh, selftest.sh, SwiftUI design) don't apply there. `desktop/CONTRACT.md` has the architecture, `desktop/README.md` the status.
- `desktop/src-tauri/` the Rust core: `session.rs` (the `DatabaseSession` trait, like Session.swift), `drivers/` (one file per engine, ported from the Swift drivers), `store.rs`, `ssh.rs`, `commands.rs` (IPC). `desktop/src/` the React UI; `api.ts` mirrors the Rust types, checked by `tests/contract.rs`. `src/mock.ts` answers in a plain browser.
- Build: `cd desktop && npm install && npm run tauri build` (`-- --bundles deb,appimage` on Linux). UI only: `npm run dev`, then http://localhost:1420.
- Check, in `desktop/src-tauri`: `cargo fmt`, `cargo clippy --all-targets -- -D warnings` (also with `--target x86_64-pc-windows-gnu`), `cargo test`. Integration tests take `DBLENS_TEST_URL`, `DBLENS_TEST_MYSQL_URL`, `DBLENS_TEST_REDIS_URL`, `DBLENS_TEST_MONGO_URL` and skip without them; SQLite tests use temp files.
- `DBLENS_STORE=memory` keeps saved connections in memory; never point the app at the real store when testing.
