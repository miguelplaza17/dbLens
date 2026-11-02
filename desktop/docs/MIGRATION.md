# Migration: discontinuing the Swift app

Checklist for replacing the macOS Swift app (`Sources/`, `build.sh`, `tools/release.sh`)
with the Tauri app in `desktop/`, which becomes the only dbLens on Windows, Linux and macOS.

Status as of 2026-11-02, updated after the grid / native menu / macOS integration. Facts come from reading `Sources/dbLens/*.swift`, `Sources/dbLens/UI/*.swift`,
`desktop/src`, `desktop/src-tauri/src` and `desktop/docs/PERF.md`. Items marked **unknown** were not
checked, or cannot be checked from this Linux container (no Mac, no Windows).

Legend: **Done** = works in the Tauri app as in Swift. **Partial** = some of it. **Missing** = not in the
Tauri app. **Unknown** = not verified.

---

## (a) Benchmark protocol: Swift app vs Tauri app on the same Mac

Goal: show that the Tauri app is as fast as the Swift app before the Swift app is retired. Run both on
the same Mac, same database server, same data, same display, same power state.

### 0. Setup

1. Use a Mac that runs both apps. Plug in power, close other apps, leave the display at its normal refresh
   rate (ProMotion 120 Hz if the Mac has it; note which). Turn off Low Power Mode.
2. Use a throwaway database on a local server, never a real one. Suggested: PostgreSQL 16 and MariaDB
   10.11 (Homebrew or Docker), a database named `bench`.
3. Record versions: macOS, Mac model, the Swift app version (`VERSION` is 1.0.3 for the released app) and
   how it was built (the released `/Applications/dbLens.app`, or `./build.sh`), and the Tauri build
   (`cd desktop && npm run tauri build`, release, not `tauri dev`).
4. Use release builds for both. A dev build (`build.sh` for the Swift app is a development build with its own
   name; `tauri dev`) is not a valid baseline.
5. Quit both apps before each run. Run each measurement 5 times, in alternating order (Swift, Tauri,
   Swift, Tauri, ...), and use the median.

### 1. Create the 1,000,000-row table

Same table for both apps. The table definition is in `desktop/src-tauri/tests/bench.rs`:

- PostgreSQL: `create table bench_rows (id bigint primary key, n int not null, t text, ts timestamp,
  num numeric(12,2), flag bool, doc jsonb)` then the `generate_series(1, 1000000)` insert and
  `vacuum analyze` (bench.rs, about lines 92–104).
- MySQL/MariaDB: the same columns with `datetime`, `decimal`, `json`, and a digit-cross-join insert
  (bench.rs, about lines 107–125).

Easiest way: run the bench test once with the table kept (see `desktop/docs/PERF.md`, "The bench"),
pointing the test URL at the `bench` database:

```sh
cd desktop/src-tauri
DBLENS_BENCH_KEEP=1 \
DBLENS_TEST_URL=postgres://dblens:dblens@127.0.0.1:5432/bench \
DBLENS_TEST_MYSQL_URL=mysql://dblens:dblens@127.0.0.1:3306/bench \
cargo test --release --test bench -- --ignored bench --nocapture
```

Notes: this is a release build (a few GB of disk; delete `target/release` afterwards). Without
`DBLENS_BENCH_KEEP=1` the table is dropped at the end. Then add the same server as a connection in both apps,
pointing at `bench`.

### 2. Measurements

Each row in the results table below is one measurement. Procedure:

| # | Measure | How, in each app |
|---|---|---|
| M1 | Connect | Click the saved connection. Swift: stopwatch or screen capture from click to the sidebar showing tables. Tauri: same. |
| M2 | Time to first rows | Click `bench_rows`. Tauri: read "first rows" in the perf HUD (Ctrl/Cmd+Shift+P). Swift: no built-in timer; capture the screen at 240 fps (QuickTime slow motion or similar) from click to the first row being visible, and count frames. Record the precision (1 frame = 4 ms at 240 fps). |
| M3 | Steady scroll | Scroll with the trackpad for 10 seconds from the top, at a constant speed, without stopping. Tauri: read "Scroll fps" and "Longest frame" in the HUD at the end. Swift: Instruments (Animation Hitches or Core Animation FPS) if it works with the app; otherwise screen capture at 240 fps and count frames that stall. Also watch: any visible stutter, tearing, or blank rows. |
| M4 | Cold jump | Drag the scrollbar to row 900,000 from the top, then wait for the rows. Tauri: HUD block latency (p50/p95). Swift: stopwatch from release to rows. |
| M5 | Scroll after the jump | Scroll 100 rows down from the jump point. Measures neighbour blocks. Tauri: keyset, about 2 ms per block; Swift: OFFSET, slower with depth. |
| M6 | Memory, idle | Table open, nothing else done, after 30 seconds. Activity Monitor, Memory column. Tauri: sum `dbLens` and all its `WebKit Web Content` and `WebKit Networking` processes (they are children of the app). Swift: the `dbLens` process. |
| M7 | Memory after scroll | Scroll to the end of the table (about 1,000,000 rows), wait 30 seconds, read M6 again. Tauri: the grid keeps blocks in the webview; record the peak as well. |
| M8 | 100k-row SELECT | In the SQL panel, run `select * from bench_rows limit 100000`. Record time to first rows and time until all 100,000 rows are shown. Tauri: `done` message time in the log (or the HUD); Swift: stopwatch until the grid stops changing. Record peak memory during the run. |
| M9 | Run stays responsive | While M8 runs, scroll the table in the other pane. Pass if scrolling and the Cancel button respond. |

Fixed rules: quit and reopen the app before each run (cold start), and note if a run had a background
task (Spotlight, backups, updates).

### 3. Results template

Targets are proposals to confirm before the Swift app is retired. "Ratio" is Tauri / Swift on the same run.

| # | Measure | Swift (median) | Tauri (median) | Ratio | Target | Pass |
|---|---|---|---|---|---|---|
| M1 | Connect (Postgres) | | | | ≤ 1.5× Swift | |
| M1 | Connect (MariaDB) | | | | ≤ 1.5× Swift | |
| M2 | First rows of 1M table | | | | ≤ 1.5× Swift | |
| M3 | Steady scroll, frames per second (120 Hz display) | | | | ≥ 110 fps; no visible stutter | |
| M3 | Steady scroll, worst frame | | | | ≤ Swift worst frame + 8 ms | |
| M4 | Cold jump to 900,000 | | | | ≤ 1.5× Swift | |
| M5 | Neighbour blocks after the jump | | | | ≤ 10 ms per block | |
| M6 | Memory, idle (MB) | | | | ≤ 2× Swift | |
| M7 | Memory after scrolling to the end (MB) | | | | ≤ 2× Swift, no growth past 500 MB | |
| M8 | 100k SELECT, first rows | | | | ≤ 1.5× Swift | |
| M8 | 100k SELECT, all rows | | | | ≤ 1.5× Swift | |
| M8 | 100k SELECT, peak memory (MB) | | | | ≤ 2× Swift | |
| M9 | Responsive during a run | | | | yes / no | |

Decision rule: the Swift app is retired only when every row passes, or each failing row has a written reason
and a fix. Any visible dropped frame in M3 is a fail, whatever the numbers.

Known differences to expect (not failures on their own): Tauri's WebKit runs in separate processes, so
memory is always reported as several processes; Swift pages with OFFSET, so M4 and M5 favour Tauri at depth.
The Tauri numbers in `desktop/docs/PERF.md` are from Linux and are not a substitute for this table: the JS
decode time in WebKit (macOS) and the grid's frame time have not been measured yet.

---

## (b) Native features of the Swift app: status in the Tauri app

Source references are to `Sources/dbLens/` unless noted. Shortcuts from `desktop/src/shortcuts.json` are
the Tauri names (`Mod` = Cmd on macOS, Ctrl elsewhere).

### App menus and shortcuts (App.swift)

| Feature (Swift) | Swift shortcut | Tauri | Notes |
|---|---|---|---|
| Check for Updates… | | Missing | Sparkle, `Updater.swift`. See Updates below. |
| New Connection… | Cmd+N | Done | `newConnection` |
| Back / Forward | Cmd+[ / Cmd+] | Missing | No navigation history in Tauri (`model.canGoBack` was the old pager, not history). |
| Show Table / Query Result / Show Schema | Cmd+1 / 2 / 3 | Missing | Schema part depends on the schema board. |
| Zoom In / Out / Zoom to Fit (schema) | Cmd+= / Cmd+- / Cmd+0 | Missing | Depends on the schema board. |
| Show / Hide SQL Panel | Cmd+Shift+Y | Done | `toggleSQLPanel` |
| Show / Hide Sidebar | (SidebarCommands) | Done | `toggleSidebar`, Mod+B |
| Run SQL | Cmd+Return | Done | `run` |
| Cancel Query | Cmd+. | Done | `cancel` |
| Refresh | Cmd+R | Done | `refresh` |
| Previous Page / Next Page | Cmd+Opt+[ / ] | Different | Same keys and menu rows; the grid has no pages any more, so they scroll a screen up or down. |
| Save Changes / Revert Changes | (grid) | Done | Mod+S and Edit > Save Changes / Revert Changes (`saveChanges`, `revertChanges`, handled by the grid). |
| Import Connections from dbLens for Mac… | – | Done (macOS) | File menu, macOS only (`importSwift`, `os: "mac"` in `shortcuts.json`). Never run on a Mac. |
| New Row… | Cmd+Shift+N | Done | `newRow` |
| Copy All | Cmd+Shift+C | Done | `copyRows` |
| Open in Terminal | Cmd+Shift+T | Missing | `ClientCommand`, `Models.swift` `openInTerminal`. Also in the SQL panel and the sidebar menu. |
| Disconnect | Cmd+Shift+D | Partial | Action exists, but the key is Mod+Shift+W in Tauri. Decide which key to keep. |
| Settings | (Settings scene) | Missing | Menu row and Mod+, exist; `perform.ts` shows the toast "Settings are not available yet". |
| Keyboard shortcuts sheet | (not in Swift menus) | Done | Mod+/, Tauri only. |
| Performance HUD | (not in Swift) | Done | Mod+Shift+P, Tauri only: first rows, block fetch p50/p95, scroll fps, longest frame, JS heap, rows cached (`src/grid/PerfHud.tsx`). |
| Menu bar | (SwiftUI menus) | Done (Linux checked) | `menu.rs`, built from `shortcuts.json`; menu labels English only. macOS app menu and Window menu never compiled on a Mac. |

### Settings (SettingsView.swift, UI/*.swift)

Swift has six tabs: General, Appearance, Connections, Agents, Updates, Data & Privacy. Tauri has no
Settings window (README: "Settings window … missing").

| Setting (Swift) | Tauri | Notes |
|---|---|---|
| General > Rows per page (50–1000) | Not needed | The grid scrolls through the whole table in blocks; `model.pageSize` is left over and unused. |
| General > Show the SQL panel | Partial | Only the toggle action. |
| General > Ask before deleting a connection | Partial | A confirmation dialog exists; no setting to turn it off. |
| Appearance > Accent color (`Theme.all`, `ThemeStore`) | Missing | `theme.css` has one `--accent`; no accent list, no picker. Light/dark toggle is Done (Mod+Shift+L). |
| Connections > Edit, delete, folders | Done | Sidebar context menus. |
| Connections > Clear SQL history | Missing | No SQL history in Tauri (see below). |
| Agents > MCP server setup and copy commands | Missing | See MCP below. |
| Agents > Let agents add and remove connections | Missing | MCP. |
| Updates > Check automatically, Update automatically, Check Now | Missing | Updater. |
| Data & Privacy > Clear all history, Show in Finder | Missing | Finder is macOS only; a "show data folder" action would be the equivalent. |

### Connections and sidebar (ContentView.swift, ConnectionSheet.swift)

| Feature (Swift) | Tauri | Notes |
|---|---|---|
| Folders: new, rename, delete, expand/collapse | Done | `add_folder`, `rename_folder`, `delete_folder`, `set_folder_expanded`. |
| Drag connections between and into folders | Done | `native/dragConnections.ts`. |
| Move to folder / No Folder (menu) | Done | Sidebar menu. |
| Connect, Edit, Rename, Delete (with confirmation) | Done | |
| Disconnect (menu) | Done | |
| Copy URL without password | Missing | `connection_url` and `mask_url` commands exist; no menu item. |
| Copy URL with password | Missing | Same. Needs a confirmation, as Swift has. |
| Open in Terminal (connection menu) | Missing | See above. |
| Let Agents Write (per connection) | Missing | MCP. |
| Connection sheet: URL or fields, SSL mode, SSH, SQLite file picker | Done | `ConnectionSheet.tsx`, `urlForm.ts`, `ssh.rs`. |
| Connection error with Try Again / Edit Connection | Done | `connection_problem.rs`. |
| Drop a connection string or SQLite file on the window | Done (files) | `native/drop.ts`. Text drops depend on the webview delivering them; untested. |
| Open SQLite file (Cmd+O) | Done | File dialog plugin. |
| New Window | Done | Cmd+Alt+N, `windows.rs`. |

### Table grid and result pane (ResultGrid.swift, ResultPane.swift, RowEditor.swift)

| Feature (Swift) | Tauri | Notes |
|---|---|---|
| Sort ascending / descending / clear | Done | |
| Freeze first column | Done | |
| Copy value, copy selected rows, copy grid (TSV) | Done | |
| Set NULL (Set NULL action) | Done | `setNull` |
| Delete rows with confirmation | Done | |
| Row editor sheet (add, edit, NULL toggles, delete) | Done | `RowEditor.tsx`. |
| Revert changes, edit row | Done | Edits stay pending (highlighted) and are sent as one `update_row` with only the changed columns. |
| Pending edits: Save / Discard prompt when leaving | Different | No prompt: pending edits are saved when you leave the row, press Mod+S, open another table, reload, search, sort or disconnect; Escape or Revert Changes drops them. A failed save keeps the edits, marks the row red and shows the error. |
| Search rows (Cmd+F), filter tables (Cmd+Shift+F) | Done | |
| Previous / Next page buttons | Different | Replaced by scrolling (blocks of 300 rows fetched on demand); the footer reads "Rows a–b of N", with "~N" while the exact count runs. |
| Inspect in Commands / Query in SQL / Copy name / Copy key | Done | |
| Export CSV / JSON to a file (save panel) | Partial | Tauri copies CSV or JSON to the clipboard only (`ResultPane.tsx`). No save panel. |
| Grid rendering | Done | Glide Data Grid (canvas), as decided. |

### SQL panel (SQLPanel.swift, SQLEditor.swift)

| Feature (Swift) | Tauri | Notes |
|---|---|---|
| Run / Cancel / Log / Copy Log | Done | |
| Rehearse (rolled back) | Done | |
| All or Nothing (one transaction) | Done | |
| Read-only run | Partial | `read_only` exists in the backend, not in the UI. |
| SQL history (per connection, popover, Use / Run) | Missing | Swift stores it in user defaults (`sqlHistory.<id>`); the import does not read it. |
| Terminal button | Missing | See Open in Terminal. |
| Database-specific commands (Redis, MongoDB) | Done | Bottom panel. |

### Schema board (SchemaBoard.swift, about 950 lines)

Missing. The backend has `schema_graph`; nothing draws it. Swift has: cards with columns and keys,
collapsed view, colored cards, sticky notes, arrange, pan and zoom, fit, schema picker, Open Rows from a card,
keyboard navigation, and the layout saved per connection. Decide whether a canvas board is still wanted
(it would be a second canvas next to the grid).

### MCP server for agents (MCPServer.swift, UI/AgentSettings.swift)

Missing. `dbLens --mcp` runs a JSON-RPC server on stdio. Tools in Swift: `list_connections`,
`create_connection`, `delete_connection`, `list_tables`, `describe_schema`, `list_extensions`,
`install_extension`, `describe_table`, `query`, `test_changes`, `execute`. Reads are read-only (the database
refuses writes); writes need "Let Agents Write" on the connection; agents never see URLs. Swift's
Settings > Agents also shows the `claude mcp add` command and JSON to copy. This is the largest feature
gap for agent users. Decide: port, or drop and say so in the release notes.

### Updates (Updater.swift, UI/UpdateNotice.swift, tools/release.sh)

Missing in Tauri. Swift uses Sparkle: a daily check of an appcast on GitHub Releases
(`dist/appcast.xml`, signed with an Ed25519 key), automatic download, and a notice in the window. Windows and
Linux need a different updater; the Tauri updater plugin is an option (**unknown**: not evaluated). Also
unknown: whether the macOS Tauri build will be signed and notarized like `tools/release.sh`.

### Import (desktop/src-tauri/src/import_swift.rs)

Done on macOS only (see section c).

### Platform and tooling (not user features, decide case by case)

| Swift item | Tauri | Notes |
|---|---|---|
| Secure Enclave key for saved URLs (`Secrets.swift`, `SecretBox`) | Different | Tauri uses the OS keyring (`store.rs`; macOS Keychain via keyring's `apple-native`), or a 0600 key file when there is none. Unsigned or ad hoc macOS builds may ask for the Keychain password after each rebuild (the problem the Swift app avoided). |
| Seed data (`SeedData.swift`), self-test (`SelfTest.swift`), snapshots (`Snapshot.swift`, `tools/snap.sh`) | Different | Tauri: `src/mock.ts` and `docs/shots/take.cjs`. No self-test script equivalent; `cargo test` covers the drivers. |
| Localization: English and Portuguese | Done | `desktop/src/i18n/`. Check strings with the Swift `Localization/` keys. |
| Accent themes (`UI/Theme.swift`) | Missing | See Settings. |
| Dark mode forced (`AppDelegate`) | Different | Tauri follows the theme toggle. |
| `Store` path with a Dev build copy | Not needed | Tauri has one store, see README. |

### Summary of what is missing (Tauri, compared with Swift)

1. Schema board (whole feature).
2. MCP server and the Agents settings (including "Let Agents Write").
3. Settings window (all tabs) and accent colors.
4. Updater (Sparkle replacement), and release signing for Windows and Linux. macOS signing and notarization are
   wired into CI from the `APPLE_*` secrets but have never run.
5. SQL history (per connection), with Clear and the Data & Privacy actions.
6. Open in Terminal (menu, SQL panel, sidebar).
7. Copy URL with and without password.
8. Export to a file (save panel); Tauri only copies to the clipboard.
9. Back/Forward (no history at all) and Show Table / Query / Schema shortcuts.
10. Read-only run in the UI. (Rows per page no longer applies: the grid scrolls.)
11. Localized menu labels (the menu bar is English only; the window is translated).

Documentation note: `desktop/README.md` now describes the menu bar (`menu.rs`: File, Edit, View, Query, Help; on
macOS also the app menu and Window). It was checked under Xvfb on Linux only.

---

## (c) User migration: importing from the Swift app

### What exists

- Command: `import_swift_connections` (`desktop/src-tauri/src/import_swift.rs`, registered in `lib.rs`).
  It is compiled for macOS only (`#[cfg(target_os = "macos")]`).
- Entry points in the UI (macOS only): File > Import Connections from dbLens for Mac… (`importSwift` in
  `src/shortcuts.json`, `native/perform.ts`), and a button in the empty state of the result pane while there are no
  saved connections (`desktop/src/components/ResultPane.tsx`). Both call `native/importSwift.ts`.
- Override for tests: `DBLENS_SWIFT_APP_DIR` replaces the Swift data folder.

### What it reads (read-only)

From `~/Library/Application Support/dbLens/`:

- `dbLens.store`: SwiftData table `ZSAVEDCONNECTION`. Read from a private temporary copy, so the original and its
  `-wal`/`-shm` files are never opened or changed.
- `connections.enc` (AES-GCM, CryptoKit layout) with `connections.key`. Kind `software` is opened in Rust;
  kind `secure-enclave` needs the Mac's Secure Enclave, through `desktop/src-tauri/swift/SecureEnclaveOpen.swift`.
- Older versions: a plain `connections.json` instead of `connections.enc`.
- Folders: the `connectionFolders` key from the preferences of `io.github.miguelplaza17.dblens`, read with
  `/usr/bin/defaults export`.

Then it writes only to the Tauri app's own store (`store.rs`, `ConnectionDraft`). The Swift folder must not be
the Tauri store folder; the command refuses that case.

### What it does not carry over

- SQL history (`sqlHistory.<id>` in user defaults).
- "Let Agents Write" flags (no MCP in Tauri).
- Settings (rows per page, accent, panel visibility) and the schema board layouts.
- Keychain items from older versions (the current code does not read the Keychain; `Secrets.swift`).

### Checks before telling users

- Run the import on a real Mac with a copy of the Swift data (not the only copy): one `software` key, one
  `secure-enclave` key, a folder, an SSH connection, a SQLite file, and a Redis or MongoDB URL. Confirm each
  connection connects after import. **Unknown**: none of this has been run on a Mac from this environment.
- Confirm the Secure Enclave helper (`swift/SecureEnclaveOpen.swift`) builds and runs in the Tauri macOS build.
- Confirm the Swift data folder is unchanged after the import (file hashes before and after).

### Guidance for users

- The import copies. Saved URLs stay encrypted in the Tauri store; the Swift app keeps working until it is uninstalled.
- Do not delete `~/Library/Application Support/dbLens/` (it is the only copy of the Swift app's data until the
  user confirms the import).

---

## (d) Release plan

### Current state

- Swift app: `VERSION` 1.0.3. `tools/release.sh` builds with a Developer ID, notarizes, staples, and zips to
  `dist/`, and writes `dist/appcast.xml` for Sparkle.
- Homebrew cask template: `docs/homebrew/dblens.rb` (version 1.0.3, arm64 only, `depends_on macos: ">= :tahoe"`,
  `app "dbLens.app"`, `zap` removes `~/Library/Application Support/dbLens` and the Swift preferences plist). The
  live cask is in the tap `miguelplaza17/homebrew-tap`, Casks/dblens.rb (not checked from here: **unknown**).
- Tauri: `.github/workflows/desktop.yml` builds on `ubuntu-22.04` and `windows-latest`, and has a `macos-14` job
  (fmt, clippy, tests, an x86_64 check, a universal `.app`/`.dmg`, signed from the `APPLE_*` secrets when set,
  ad hoc otherwise). That job has never run. `tauri.conf.json` has identifier `app.dblens.desktop`, version `0.1.0`,
  minimum macOS 13.0, hardened runtime.

### Steps

1. **Close the feature gaps that are decided as must-have** (section b: at least the items the user needs).
   Decide each of the 10 missing items in the summary: port, or drop and say so in the release notes.
2. **Run the macOS Tauri build** in CI (the `macos-14` job in `desktop.yml`), first ad hoc, then with the Developer
   ID secrets, and check notarization and stapling as `tools/release.sh` does. **Unknown**: whether it passes; the
   Swift helper, the Keychain path and the universal build have never been compiled.
3. **Run the benchmark protocol** (section a) on a Mac, with both apps. Both must pass.
4. **Last Swift release.** Build `VERSION` 1.0.4 (or the next number; **decide**) from `Sources/`
   with `tools/release.sh`. Its release notes say: this is the last macOS release of the Swift app; dbLens is now a
   single cross-platform app; how to import (section c); what is not yet in the new app. Its `appcast.xml` offers
   this version to existing users. Do not add features to the Swift app after this.
   The Swift app's Sparkle notice is the only in-app way to reach existing users; this release must happen before
   the Swift app stops being published, or users stay on 1.0.3 with no pointer.
5. **Announce**: update the root `README.md` and `desktop/README.md` so both point to `desktop/` as the app, with
   download links for macOS, Windows and Linux.
6. **Homebrew cask.** Two options, decide before step 6:
   - Option A (keep the name `dblens`): publish the Tauri macOS build, and update the cask in place: new `version`,
     `sha256`, `url` (the Tauri `.dmg`), `app` name as the Tauri bundle, `depends_on` for the minimum macOS the
     Tauri build supports (**unknown**: verify), `zap` pointing at the Tauri data folder
     (`~/Library/Application Support/app.dblens.dbLens`, from `directories::ProjectDirs::from("app", "dblens",
     "dbLens")` in `store.rs`; the Keychain item is service `app.dblens.desktop`) and not at the Swift folder, so `brew
     uninstall --zap` does not delete Swift data by accident.
   - Option B (deprecate): keep the cask at the last Swift version, add `caveats` that the app is discontinued, and
     add a second cask (for example `dblens-desktop`) for the Tauri app. Simpler to roll back; users must reinstall.
   Either way: `docs/homebrew/dblens.rb` is the template; update the copy in the tap repository after each release,
   as its header says.
7. **Keep the Swift repository buildable** for one more release (tag, source archive) so the last version can be
   rebuilt. Archive the `Sources/` folder at its last tag; do not delete it from history.
8. **Retire Swift tooling** once the last release is out: `build.sh`, `tools/snap.sh`, `tools/selftest.sh`,
   `tools/check-*.sh`, `Package.swift`, `Localization/` (move the keys still used to `desktop/src/i18n/`).
   Remove the docs that describe the Swift app as current.

### Open decisions

- Port or drop: schema board, MCP server, updater, SQL history, open in terminal, accent colors, export to file.
- Version number of the last Swift release.
- Cask option A or B.
- Updater for Windows and Linux (Tauri updater plugin or none).
- Minimum macOS and architectures (Intel, Apple silicon) for the Tauri macOS build.
- Whether the Swift app's appcast stays online after the last release (it should, so Sparkle keeps offering the
  final version).
