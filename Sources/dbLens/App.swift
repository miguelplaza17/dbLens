import AppKit
import SwiftData
import SwiftUI
import DBLensKit

// dbLens
//   dbLens                      the app
//   APP_STORE=<file> dbLens     use another database (tests, experiments); URLs stay in memory, not in connections.json
//   APP_SNAPSHOT=<dir> dbLens   save a PNG of every page and Settings tab, then quit
//   APP_SEED_URL=<url> dbLens   with APP_STORE in /tmp: save and open this database, pick a table, run a query (tools/snap.sh)
//   dbLens --selftest           run the self-test on a /tmp store and quit with 0 or 1 (tools/selftest.sh, needs APP_SEED_URL)
//   APP_ACCENT=<id> dbLens      run with another accent from Theme.all (UI/Theme.swift)
//   APP_GRID=1 dbLens           draw the 8 pt grid and the layout probes over the window

@main
struct DBLensApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var model = AppModel(container: Store.makeContainer())
    @State private var updater = Updater()

    init() {
        // `dbLens --mcp`: an MCP server on stdio for agents, no window.
        if MCPServer.requested { MCPServer.run() }
    }

    var body: some Scene {
        Window("dbLens", id: "main") {
            ContentView()
                .overlay(alignment: .top) { UpdateNotice().animation(.snappy, value: updater.notice) }
                .environment(model)
                .environment(updater)
                .themed()
                .overlay { if GridOverlay.enabled { GridOverlay() } }
                .frame(minWidth: 1000, minHeight: 560)
        }
        .modelContainer(model.container)
        .defaultSize(width: 1440, height: 860)
        .windowToolbarStyle(.unified)
        .commands {
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") { updater.checkForUpdates() }
                    .disabled(!updater.isEnabled)
            }
            SidebarCommands()
            ToolbarCommands()
            CommandGroup(replacing: .newItem) {
                Button("New Connection…") { model.showingNewConnection = true }
                    .keyboardShortcut("n")
            }
            CommandGroup(after: .sidebar) {
                Divider()
                Button("Back") { Task { await model.goBack() } }
                    .keyboardShortcut("[")
                    .disabled(!model.canGoBackInHistory)
                Button("Forward") { Task { await model.goForward() } }
                    .keyboardShortcut("]")
                    .disabled(!model.canGoForwardInHistory)
                Divider()
                Button("Show Table") { model.shown = .table }
                    .keyboardShortcut("1")
                    .disabled(model.session == nil)
                Button("Show Query Result") { model.shown = .query }
                    .keyboardShortcut("2")
                    .disabled(model.execution == nil)
                Button("Show Schema") { Task { await model.showSchema() } }
                    .keyboardShortcut("3")
                    .disabled(!model.hasSchemaBoard)
                Divider()
                Button("Zoom In") { model.board?.zoom(by: 1.25) }
                    .keyboardShortcut("=")
                    .disabled(model.shown != .schema)
                Button("Zoom Out") { model.board?.zoom(by: 1 / 1.25) }
                    .keyboardShortcut("-")
                    .disabled(model.shown != .schema)
                Button("Zoom to Fit") { model.fitBoard() }
                    .keyboardShortcut("0")
                    .disabled(model.shown != .schema)
                Button(model.showSQLPanel ? String(localized: "Hide SQL Panel") : String(localized: "Show SQL Panel")) {
                    withAnimation(.snappy(duration: 0.25)) { model.showSQLPanel.toggle() }
                }
                .keyboardShortcut("y", modifiers: [.command, .shift])
            }
            CommandMenu("Database") {
                Button("Run SQL") { Task { await model.run() } }
                    .keyboardShortcut(.return)
                    .disabled(model.session == nil || model.running)
                Button("Cancel Query") { model.cancel() }
                    .keyboardShortcut(".")
                    .disabled(!model.running)
                Divider()
                Button("Refresh") { Task { await model.refresh() } }
                    .keyboardShortcut("r")
                    .disabled(model.session == nil)
                Button("Previous Page") { Task { await model.previousPage() } }
                    .keyboardShortcut("[", modifiers: [.command, .option])
                    .disabled(!model.canGoBack)
                Button("Next Page") { Task { await model.nextPage() } }
                    .keyboardShortcut("]", modifiers: [.command, .option])
                    .disabled(!model.canGoForward)
                Divider()
                Button("New Row…") { model.rowEdit = RowEditTarget(row: nil) }
                    .keyboardShortcut("n", modifiers: [.command, .shift])
                    .disabled(!model.canInsertRows)
                Button("Copy All") { if let result = model.shownResult { copy(ResultGrid.tsv(result, header: true)) } }
                    .keyboardShortcut("c", modifiers: [.command, .shift])
                    .disabled(model.shownResult?.hasRows != true)
                Divider()
                Button("Open in Terminal") { model.openInTerminal() }
                    .keyboardShortcut("t", modifiers: [.command, .shift])
                    .disabled(model.session == nil)
                                Button("Disconnect") { model.disconnect() }
                    .keyboardShortcut("d", modifiers: [.command, .shift])
                    .disabled(model.session == nil)
            }
        }

        Settings {
            SettingsView()
                .environment(model)
                .themed()
                .modelContainer(model.container)
                .environment(updater)
        }
        .windowResizability(.contentSize)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        // Tooltips after a quarter second, not the system's second: most buttons here are icons.
        UserDefaults.standard.register(defaults: ["NSInitialToolTipDelay": 250])
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Read the saved URLs now, so an older plain or Keychain copy is encrypted at once.
        if !Store.isOverridden { _ = Secrets.url(for: UUID()) }
        // dbLens is always dark, whatever the Mac uses.
        NSApp.appearance = NSAppearance(named: .darkAqua)
        // Started from a terminal, the process is not a foreground app yet.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    /// The self-test's result becomes the exit code, after a normal quit.
    func applicationWillTerminate(_ notification: Notification) {
        // An ssh left running would keep its local port open after the app is gone.
        SSHTunnel.closeAll()
        if SelfTest.requested { exit(SelfTest.exitCode) }
    }
}

/// Where the data lives: ~/Library/Application Support/dbLens/dbLens.store
enum Store {
    /// True when APP_STORE points somewhere else (tests, snapshots, seed data).
    static var isOverridden: Bool {
        !(ProcessInfo.processInfo.environment["APP_STORE"] ?? "").isEmpty
    }

    static var url: URL {
        if let path = ProcessInfo.processInfo.environment["APP_STORE"], !path.isEmpty {
            return URL(fileURLWithPath: path)
        }
        let released = URL.applicationSupportDirectory.appending(path: "dbLens", directoryHint: .isDirectory)
        moveFromFirstName(to: released)
        // The development build (build.sh, bundle id ….dev) keeps its own copy: a newer
        // model must never change the store the released app opens.
        let dir = isDevelopmentBuild ? URL.applicationSupportDirectory.appending(path: "dbLens Dev", directoryHint: .isDirectory) : released
        if isDevelopmentBuild { copyOnce(from: released, to: dir) }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true,
                                                 attributes: [.posixPermissions: 0o700])
        return dir.appending(path: "dbLens.store")
    }

    static var isDevelopmentBuild: Bool { Bundle.main.bundleIdentifier?.hasSuffix(".dev") == true }

    /// The first time the development build runs, it starts from the released app's
    /// connections (store, encrypted URLs, and key), copied.
    private static func copyOnce(from source: URL, to dir: URL) {
        let files = FileManager.default
        guard !files.fileExists(atPath: dir.path), files.fileExists(atPath: source.path) else { return }
        try? files.copyItem(at: source, to: dir)
        try? files.setAttributes([.posixPermissions: 0o700], ofItemAtPath: dir.path)
    }

    /// The app was first called PgLens: its folder and store move here once.
    private static func moveFromFirstName(to dir: URL) {
        let files = FileManager.default
        let old = URL.applicationSupportDirectory.appending(path: "PgLens", directoryHint: .isDirectory)
        guard !files.fileExists(atPath: dir.path), files.fileExists(atPath: old.path),
              (try? files.moveItem(at: old, to: dir)) != nil else { return }
        for suffix in ["", "-shm", "-wal"] {
            try? files.moveItem(at: dir.appending(path: "PgLens.store\(suffix)"),
                                to: dir.appending(path: "dbLens.store\(suffix)"))
        }
    }

    static func makeContainer() -> ModelContainer {
        do {
            return try ModelContainer(for: SavedConnection.self, configurations: ModelConfiguration(url: url))
        } catch {
            fatalError("Could not open the database at \(url.path): \(error)")
        }
    }
}
