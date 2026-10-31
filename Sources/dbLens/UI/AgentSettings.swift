import SwiftData
import SwiftUI

/// Settings > Agents: dbLens's MCP server, installed into the agent apps with
/// one click, and which connections agents may write to.
struct AgentSettings: View {
    @Query(sort: \SavedConnection.name) private var connections: [SavedConnection]
    @Environment(\.modelContext) private var context
    @State private var installed: Set<AgentClient> = []
    @State private var working: AgentClient?
    @State private var problem: String?

    var body: some View {
        Section {
            Text("Agents like Claude Code read your databases through the connections saved here. They see names, never URLs or passwords, and queries are read-only, enforced by the database.")
                .foregroundStyle(.secondary)
            LabeledContent("Command") {
                HStack(spacing: Space.sm) {
                    Text(verbatim: AgentClient.command)
                        .font(.system(size: 11, design: .monospaced))
                        .lineLimit(1)
                        .truncationMode(.head)
                        .textSelection(.enabled)
                    Button("Copy", systemImage: "doc.on.doc") { copy(AgentClient.command) }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.borderless)
                        .help("Copy the command that starts the MCP server")
                }
            }
        } header: {
            Text("MCP Server")
        }

        Section {
            ForEach(AgentClient.allCases) { client in
                LabeledContent {
                    if working == client {
                        ProgressView().controlSize(.small)
                    } else if installed.contains(client) {
                        HStack(spacing: Space.sm) {
                            Label("Installed", systemImage: "checkmark.circle.fill")
                                .foregroundStyle(.green)
                            Button("Remove") { run(client, install: false) }
                        }
                    } else {
                        Button("Install") { run(client, install: true) }
                            .disabled(!client.isAvailable)
                    }
                } label: {
                    Text(client.name)
                    Text(client.isAvailable ? client.note : client.missing)
                }
            }
            LabeledContent {
                Button("Copy JSON") { copy(AgentClient.json) }
            } label: {
                Text("Other apps")
                Text("Paste into the app's MCP settings.")
            }
            if let problem {
                Text(verbatim: problem)
                    .font(.callout)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }
        } header: {
            Text("Install")
        }

        Section {
            if connections.isEmpty {
                Text("No saved connections yet.").foregroundStyle(.secondary)
            }
            ForEach(connections) { connection in
                Toggle(isOn: Binding(get: { connection.agentsCanWrite }, set: { on in
                    connection.agentsCanWrite = on
                    try? context.save()
                })) {
                    Text(verbatim: connection.name)
                }
            }
        } header: {
            Text("Let Agents Write")
        } footer: {
            Text("Off: agents only read. On: they may change data and install extensions on that connection. Everything they run shows in the SQL panel's History.")
                .foregroundStyle(.secondary)
        }
        .onAppear(perform: refresh)
    }

    private func refresh() {
        installed = Set(AgentClient.allCases.filter(\.isInstalled))
    }

    private func run(_ client: AgentClient, install: Bool) {
        working = client
        problem = nil
        Task {
            do {
                try await client.set(installed: install)
            } catch {
                problem = error.localizedDescription
            }
            working = nil
            refresh()
        }
    }
}

/// An app that runs MCP servers, and how dbLens goes into its settings.
enum AgentClient: String, CaseIterable, Identifiable {
    case claudeCode, claudeDesktop, cursor

    var id: String { rawValue }

    static var executable: String { Bundle.main.executableURL?.path ?? "/Applications/dbLens.app/Contents/MacOS/dbLens" }
    static var command: String { "\(executable) --mcp" }
    static var json: String {
        """
        {
          "mcpServers": {
            "dblens": { "command": "\(executable)", "args": ["--mcp"] }
          }
        }
        """
    }

    var name: String {
        switch self {
        case .claudeCode: "Claude Code"
        case .claudeDesktop: "Claude Desktop"
        case .cursor: "Cursor"
        }
    }

    var note: String {
        switch self {
        case .claudeCode: String(localized: "For every project (claude mcp add --scope user).")
        case .claudeDesktop: String(localized: "Restart Claude Desktop afterwards.")
        case .cursor: String(localized: "Restart Cursor afterwards.")
        }
    }

    var missing: String {
        switch self {
        case .claudeCode: String(localized: "The claude command wasn't found.")
        case .claudeDesktop: String(localized: "Claude Desktop isn't installed.")
        case .cursor: String(localized: "Cursor isn't installed.")
        }
    }

    private static let home = FileManager.default.homeDirectoryForCurrentUser

    /// The JSON file the app reads its MCP servers from.
    private var configFile: URL {
        switch self {
        case .claudeCode: Self.home.appending(path: ".claude.json")
        case .claudeDesktop: Self.home.appending(path: "Library/Application Support/Claude/claude_desktop_config.json")
        case .cursor: Self.home.appending(path: ".cursor/mcp.json")
        }
    }

    private static var claudeBinary: String? {
        ["~/.local/bin/claude", "~/.claude/local/claude", "/opt/homebrew/bin/claude", "/usr/local/bin/claude"]
            .map { NSString(string: $0).expandingTildeInPath }
            .first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    var isAvailable: Bool {
        switch self {
        case .claudeCode: Self.claudeBinary != nil
        case .claudeDesktop: FileManager.default.fileExists(atPath: "/Applications/Claude.app")
        case .cursor: FileManager.default.fileExists(atPath: "/Applications/Cursor.app")
                      || FileManager.default.fileExists(atPath: Self.home.appending(path: ".cursor").path)
        }
    }

    var isInstalled: Bool {
        guard let data = try? Data(contentsOf: configFile),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let servers = root["mcpServers"] as? [String: Any] else { return false }
        return servers["dblens"] != nil
    }

    /// Claude Code through its own command (its settings file holds much more);
    /// the others by adding or removing "dblens" in their JSON, keeping the rest.
    func set(installed: Bool) async throws {
        if self == .claudeCode {
            guard let claude = Self.claudeBinary else { throw DatabaseErrorText(String(localized: "The claude command wasn't found.")) }
            var arguments = ["mcp", "remove", "--scope", "user", "dblens"]
            if installed {
                _ = try? await Self.run(claude, arguments)  // replaces an older entry
                arguments = ["mcp", "add", "--scope", "user", "dblens", "--", Self.executable, "--mcp"]
            }
            try await Self.run(claude, arguments)
            return
        }
        var root: [String: Any] = [:]
        if let data = try? Data(contentsOf: configFile) {
            guard let parsed = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                throw DatabaseErrorText(String(localized: "\(configFile.lastPathComponent) isn't valid JSON; fix it or edit it by hand."))
            }
            root = parsed
        }
        var servers = root["mcpServers"] as? [String: Any] ?? [:]
        servers["dblens"] = installed ? ["command": Self.executable, "args": ["--mcp"]] : nil
        root["mcpServers"] = servers
        try FileManager.default.createDirectory(at: configFile.deletingLastPathComponent(), withIntermediateDirectories: true)
        let data = try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
        try data.write(to: configFile, options: .atomic)
    }

    @discardableResult
    private static func run(_ tool: String, _ arguments: [String]) async throws -> String {
        try await Task.detached {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: tool)
            process.arguments = arguments
            let output = Pipe()
            process.standardOutput = output
            process.standardError = output
            try process.run()
            process.waitUntilExit()
            let text = String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            guard process.terminationStatus == 0 else { throw DatabaseErrorText(text.trimmingCharacters(in: .whitespacesAndNewlines)) }
            return text
        }.value
    }
}

/// An error that is just its message.
struct DatabaseErrorText: LocalizedError {
    let errorDescription: String?
    init(_ message: String) { errorDescription = message }
}
