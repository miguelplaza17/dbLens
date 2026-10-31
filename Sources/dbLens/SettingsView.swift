import AppKit
import DBLensKit
import SwiftData
import SwiftUI

/// The Settings window (Command-Comma), laid out like System Settings: a
/// sidebar of topics and a grouped form for the one picked.
struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        // A plain split: the Settings window doesn't honor NavigationSplitView's column widths.
        HStack(spacing: 0) {
            List(SettingsTab.allCases, selection: $model.settingsTab) { tab in
                Label {
                    Text(tab.title)
                } icon: {
                    Image(systemName: tab.symbol)
                        .foregroundStyle(.white)
                        .font(.system(size: 11, weight: .semibold))
                        .frame(width: 20, height: 20)
                        .background(tab.tint.gradient, in: RoundedRectangle(cornerRadius: 5, style: .continuous))  // grid: ok System Settings icon tile
                }
                .tag(tab)
            }
            .listStyle(.sidebar)
            .frame(width: 220)
            Divider()
            Form {
                switch model.settingsTab {
                case .general: GeneralSettings()
                case .appearance: AppearanceSettings()
                case .connections: ConnectionSettings()
                case .agents: AgentSettings()
                case .updates: UpdateSettings()
                case .data: DataSettings()
                }
            }
            .formStyle(.grouped)
        }
        .navigationTitle(model.settingsTab.title)
        .frame(width: 800, height: 560)
        .noInitialFocus()
    }
}

/// The Settings topics. Snapshot mode walks through every case.
enum SettingsTab: String, CaseIterable, Identifiable {
    case general, appearance, connections, agents, updates, data

    var id: String { rawValue }

    var title: String {
        switch self {
        case .general: String(localized: "General")
        case .appearance: String(localized: "Appearance")
        case .connections: String(localized: "Connections")
        case .agents: String(localized: "Agents")
        case .updates: String(localized: "Updates")
        case .data: String(localized: "Data & Privacy")
        }
    }

    var symbol: String {
        switch self {
        case .general: "gearshape.fill"
        case .appearance: "paintpalette.fill"
        case .connections: "cylinder.split.1x2.fill"
        case .agents: "sparkles"
        case .updates: "arrow.down.circle.fill"
        case .data: "lock.fill"
        }
    }

    var tint: Color {
        switch self {
        case .general: .gray
        case .appearance: .pink
        case .connections: .blue
        case .agents: .purple
        case .updates: .green
        case .data: .orange
        }
    }
}

struct GeneralSettings: View {
    @Environment(AppModel.self) private var model
    @AppStorage("confirmDelete") private var confirmDelete = true

    var body: some View {
        @Bindable var model = model
        Section("Tables") {
            Picker("Rows per page", selection: $model.pageSize) {
                ForEach(AppModel.pageSizes, id: \.self) { Text($0.formatted()).tag($0) }
            }
        }
        Section("SQL Panel") {
            Toggle("Show the SQL panel", isOn: $model.showSQLPanel)
        }
        Section("Connections") {
            Toggle("Ask before deleting a connection", isOn: $confirmDelete)
        }
    }
}

/// Settings > Connections: what is saved, and each one's SQL history.
struct ConnectionSettings: View {
    @Environment(AppModel.self) private var model
    @Query(sort: \SavedConnection.name) private var connections: [SavedConnection]
    @State private var counts: [UUID: Int] = [:]

    var body: some View {
        Section {
            if connections.isEmpty {
                Text("No saved connections yet.").foregroundStyle(.secondary)
            }
            ForEach(connections) { connection in
                let engine = Engine(rawValue: connection.engine) ?? .postgres
                LabeledContent {
                    HStack(spacing: Space.sm) {
                        if let count = counts[connection.uuid], count > 0 {
                            Button("Clear History (\(count))") {
                                model.clearHistory(for: connection.uuid)
                                refresh()
                            }
                        }
                        Button("Edit…") { model.editing = connection }
                    }
                } label: {
                    Label {
                        Text(verbatim: connection.name)
                        Text(details(connection, engine))
                    } icon: {
                        Image(systemName: engine.symbol).foregroundStyle(engine.color)
                    }
                }
            }
        } footer: {
            Text("New connections and deleting are in the main window's sidebar.")
                .foregroundStyle(.secondary)
        }
        .onAppear(perform: refresh)
    }

    private func details(_ connection: SavedConnection, _ engine: Engine) -> String {
        var parts = [engine.name]
        if let ssh = connection.ssh { parts.append(String(localized: "via SSH \(ssh.label)")) }
        if let used = connection.lastUsed { parts.append(String(localized: "used \(used.formatted(.relative(presentation: .named)))")) }
        if connection.agentsCanWrite { parts.append(String(localized: "agents may write")) }
        return parts.joined(separator: " · ")
    }

    private func refresh() {
        counts = Dictionary(uniqueKeysWithValues: connections.map { ($0.uuid, AppModel.loadHistory(for: $0.uuid).count) })
    }
}

/// Settings > Updates: Sparkle's daily check and the version running.
struct UpdateSettings: View {
    @Environment(Updater.self) private var updater

    var body: some View {
        @Bindable var updater = updater
        Section {
            LabeledContent("Version") {
                Text(verbatim: "\(Self.info("CFBundleShortVersionString")) (\(Self.info("CFBundleVersion")))")
                    .textSelection(.enabled)
            }
            Toggle("Check for updates automatically", isOn: $updater.automaticallyChecks)
                .disabled(!updater.isEnabled)
            LabeledContent {
                Button("Check Now") { updater.checkForUpdates() }
                    .disabled(!updater.isEnabled)
            } label: {
                Text("Last checked")
                Text(updater.lastCheck.map { $0.formatted(.relative(presentation: .named)) } ?? String(localized: "Never"))
            }
        } footer: {
            Text("Updates come from GitHub Releases and install only when their signature matches dbLens's key.")
                .foregroundStyle(.secondary)
        }
    }

    private static func info(_ key: String) -> String {
        Bundle.main.object(forInfoDictionaryKey: key) as? String ?? "–"
    }
}

/// Settings > Data & Privacy: where dbLens keeps things, and clearing history.
struct DataSettings: View {
    @Environment(AppModel.self) private var model
    @Query private var connections: [SavedConnection]
    @State private var confirmClear = false

    var body: some View {
        Section {
            file("Connection names and SSH settings", Store.url)
            file("Connection URLs, encrypted", Secrets.fileURL)
        } header: {
            Text("Stored on this Mac")
        } footer: {
            Text("URLs are encrypted with AES-GCM under a key bound to this Mac's Secure Enclave; only your user can read the files. On another Mac, paste the URLs again.")
                .foregroundStyle(.secondary)
        }
        Section {
            LabeledContent {
                Button("Clear All History…", role: .destructive) { confirmClear = true }
            } label: {
                Text("SQL history")
                Text("What you and agents ran, per connection (up to 100 each).")
            }
        }
        .confirmationDialog("Clear the SQL history of every connection?", isPresented: $confirmClear) {
            Button("Clear All History", role: .destructive) {
                for connection in connections { model.clearHistory(for: connection.uuid) }
            }
        }
    }

    private func file(_ title: LocalizedStringKey, _ url: URL) -> some View {
        LabeledContent {
            Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([url]) }
        } label: {
            Text(title)
            Text(verbatim: (url.path as NSString).abbreviatingWithTildeInPath)
        }
    }
}
