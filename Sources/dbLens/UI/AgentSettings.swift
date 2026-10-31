import AppKit
import SwiftData
import SwiftUI

/// Settings > Agents: dbLens's MCP server, installed into agent apps with one
/// click, the tools they get, and which connections they may write to.
struct AgentSettings: View {
    @Query(sort: \SavedConnection.name) private var connections: [SavedConnection]
    @Environment(\.modelContext) private var context
    @State private var installed: Set<AgentClient> = []
    @State private var working: AgentClient?
    @State private var problem: String?

    var body: some View {
        Section {
            Text("Agents read your databases through the connections saved here. They see names, never URLs or passwords, and queries are read-only, enforced by the database.")
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
                    action(client)
                } label: {
                    Label {
                        Text(client.name)
                        Text(client.isSupported ? (client.isAvailable ? client.note : client.missing) : client.unsupported)
                    } icon: {
                        AgentIcon(client: client)
                    }
                }
            }
            LabeledContent {
                Button("Copy JSON") { copy(AgentClient.json) }
            } label: {
                Label {
                    Text("Other apps")
                    Text("Most take this in their MCP settings.")
                } icon: {
                    AgentIcon(client: nil)
                }
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
            ForEach(MCPServer.Server.tools.indices, id: \.self) { index in
                let tool = MCPServer.Server.tools[index]
                let writes = (tool["annotations"] as? [String: Bool])?["readOnlyHint"] == false
                LabeledContent {
                    Text(writes ? "Writes" : "Reads")
                        .font(.caption.weight(.medium))
                        .foregroundStyle(writes ? .orange : .green)
                        .padding(.horizontal, Space.sm)
                        .padding(.vertical, 2)  // grid: ok badge
                        .background((writes ? Color.orange : Color.green).opacity(0.15), in: Capsule())
                } label: {
                    Text(verbatim: tool["name"] as? String ?? "").font(.system(.body, design: .monospaced))
                    Text(verbatim: Self.firstSentence(tool["description"] as? String ?? ""))
                }
            }
        } header: {
            Text("Tools")
        } footer: {
            Text("Tools that write work only on connections allowed below.")
                .foregroundStyle(.secondary)
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

    @ViewBuilder
    private func action(_ client: AgentClient) -> some View {
        if !client.isSupported {
            EmptyView()
        } else if working == client {
            ProgressView().controlSize(.small)
        } else if installed.contains(client) {
            HStack(spacing: Space.sm) {
                Label("Installed", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                Button("Remove") { run(client, install: false) }
            }
        } else {
            Button("Install") { run(client, install: true) }
                .disabled(!client.isAvailable)
        }
    }

    private static func firstSentence(_ text: String) -> String {
        text.range(of: ". ").map { String(text[..<$0.lowerBound]) + "." } ?? text
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
                problem = "\(client.name): \(error.localizedDescription)"
            }
            working = nil
            refresh()
        }
    }
}

/// The app's logo (Resources/AgentLogos) on a dark tile; a symbol for "Other apps".
private struct AgentIcon: View {
    let client: AgentClient?

    var body: some View {
        if let client, let logo = client.logo {
            Image(nsImage: logo)
                .resizable()
                .scaledToFit()
                .foregroundStyle(.white)
                .padding(Space.xs)
                .frame(width: 24, height: 24)
                .background(Color(white: 0.16), in: RoundedRectangle(cornerRadius: 6, style: .continuous))  // grid: ok icon tile
        } else {
            Image(systemName: client == nil ? "ellipsis" : "terminal.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 20, height: 20)
                .background(Color.gray.gradient, in: RoundedRectangle(cornerRadius: 5, style: .continuous))  // grid: ok icon tile
                .frame(width: 24, height: 24)
        }
    }
}

/// An app that runs MCP servers, and how dbLens goes into its settings.
enum AgentClient: String, CaseIterable, Identifiable {
    case claudeCode, claudeDesktop, chatGPT, cursor, devin, vsCode, geminiCLI, grokCLI, zed

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
        case .claudeDesktop: String(localized: "Claude (chat)")
        case .chatGPT: "ChatGPT (Codex)"
        case .cursor: "Cursor"
        case .devin: "Devin Desktop (Windsurf)"
        case .vsCode: "VS Code (Copilot)"
        case .geminiCLI: "Gemini CLI"
        case .grokCLI: "Grok CLI"
        case .zed: "Zed"
        }
    }

    var note: String {
        switch self {
        case .claudeCode: String(localized: "The claude command and the Claude app's Code tab, every project.")
        case .claudeDesktop: String(localized: "Chats in the Claude app. Restart it afterwards.")
        case .chatGPT: String(localized: "The app and the codex command (~/.codex/config.toml).")
        case .geminiCLI, .grokCLI: String(localized: "For every project.")
        default: String(localized: "Restart \(name) afterwards.")
        }
    }

    var missing: String {
        switch self {
        case .claudeCode: String(localized: "Neither the Claude app nor the claude command was found.")
        case .geminiCLI, .grokCLI: String(localized: "Not installed (command not found).")
        default: String(localized: "\(name) isn't installed.")
        }
    }

    /// Every listed app runs local MCP servers.
    var isSupported: Bool { true }
    var unsupported: String { "" }

    /// The logo bundled in Resources/AgentLogos; monochrome ones take the tile's color.
    var logo: NSImage? {
        let file: String = switch self {
        case .claudeCode: "claudecode"
        case .claudeDesktop: "claude"
        case .chatGPT: "chatgpt"
        case .cursor: "cursor"
        case .devin: "devin"
        case .vsCode: "vscode"
        case .geminiCLI: "geminicli"
        case .grokCLI: "grok"
        case .zed: "zed"
        }
        guard let url = Bundle.main.url(forResource: file, withExtension: "svg", subdirectory: "AgentLogos"),
              let image = NSImage(contentsOf: url) else { return nil }
        image.isTemplate = [.cursor, .grokCLI, .zed].contains(self)
        return image
    }

    private static let home = FileManager.default.homeDirectoryForCurrentUser
    private static let files = FileManager.default

    /// The app bundle, for its icon and to know it is installed.
    var appPath: String? {
        let names: [String] = switch self {
        case .claudeCode, .claudeDesktop: ["Claude"]
        case .chatGPT: ["ChatGPT", "Codex"]
        case .cursor: ["Cursor"]
        case .devin: ["Devin", "Windsurf"]
        case .vsCode: ["Visual Studio Code"]
        case .zed: ["Zed"]
        case .geminiCLI, .grokCLI: []
        }
        return names.flatMap { ["/Applications/\($0).app", Self.home.appending(path: "Applications/\($0).app").path] }
            .first { Self.files.fileExists(atPath: $0) }
    }

    private var binary: String? {
        let name: String? = switch self {
        case .claudeCode: "claude"
        case .chatGPT: "codex"
        case .geminiCLI: "gemini"
        case .grokCLI: "grok"
        default: nil
        }
        guard let name else { return nil }
        return ["~/.local/bin", "~/.claude/local", "/opt/homebrew/bin", "/usr/local/bin", "~/.npm-global/bin", "~/.bun/bin"]
            .map { NSString(string: "\($0)/\(name)").expandingTildeInPath }
            .first { Self.files.isExecutableFile(atPath: $0) }
    }

    var isAvailable: Bool {
        switch self {
        case .claudeCode, .chatGPT: binary != nil || appPath != nil
        case .geminiCLI, .grokCLI: binary != nil
        default: appPath != nil
        }
    }

    // MARK: Config files

    private enum Format { case json(key: String), toml }

    private var configFile: URL {
        switch self {
        case .claudeCode: Self.home.appending(path: ".claude.json")
        case .claudeDesktop: Self.home.appending(path: "Library/Application Support/Claude/claude_desktop_config.json")
        case .chatGPT: Self.home.appending(path: ".codex/config.toml")
        case .cursor: Self.home.appending(path: ".cursor/mcp.json")
        case .devin: Self.home.appending(path: ".codeium/windsurf/mcp_config.json")
        case .vsCode: Self.home.appending(path: "Library/Application Support/Code/User/mcp.json")
        case .geminiCLI: Self.home.appending(path: ".gemini/settings.json")
        case .grokCLI: Self.home.appending(path: ".grok/config.toml")
        case .zed: Self.home.appending(path: ".config/zed/settings.json")
        }
    }

    private var format: Format {
        switch self {
        case .chatGPT, .grokCLI: .toml
        case .vsCode: .json(key: "servers")
        case .zed: .json(key: "context_servers")
        default: .json(key: "mcpServers")
        }
    }

    /// dbLens's entry in this app's format.
    private var entry: [String: Any] {
        switch self {
        case .vsCode: ["type": "stdio", "command": Self.executable, "args": ["--mcp"]]
        default: ["command": Self.executable, "args": ["--mcp"]]
        }
    }

    var isInstalled: Bool {
        guard isSupported, let text = try? String(contentsOf: configFile, encoding: .utf8) else { return false }
        switch format {
        case .toml:
            return text.contains("[mcp_servers.dblens]")
        case .json(let key):
            guard let root = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] else { return false }
            return (root[key] as? [String: Any])?["dblens"] != nil
        }
    }

    /// Claude Code through its own command (its settings file holds much more),
    /// or like the others when only the app is there: adding or removing
    /// "dblens" in the app's config, keeping the rest.
    func set(installed: Bool) async throws {
        if self == .claudeCode, let claude = binary {
            if installed {
                _ = try? await Self.run(claude, ["mcp", "remove", "--scope", "user", "dblens"])  // replaces an older entry
                try await Self.run(claude, ["mcp", "add", "--scope", "user", "dblens", "--", Self.executable, "--mcp"])
            } else {
                try await Self.run(claude, ["mcp", "remove", "--scope", "user", "dblens"])
            }
            return
        }
        try Self.files.createDirectory(at: configFile.deletingLastPathComponent(), withIntermediateDirectories: true)
        let current = (try? String(contentsOf: configFile, encoding: .utf8)) ?? ""
        switch format {
        case .toml:
            var text = Self.removingTOMLTable("mcp_servers.dblens", from: current)
            if installed {
                if !text.isEmpty, !text.hasSuffix("\n") { text += "\n" }
                text += "\n[mcp_servers.dblens]\ncommand = \"\(Self.executable)\"\nargs = [\"--mcp\"]\n"
            }
            try text.write(to: configFile, atomically: true, encoding: .utf8)
        case .json(let key):
            var root: [String: Any] = [:]
            if !current.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                guard let parsed = try? JSONSerialization.jsonObject(with: Data(current.utf8)) as? [String: Any] else {
                    throw AgentSetupError(String(localized: "\(configFile.lastPathComponent) has comments or isn't plain JSON; add dbLens by hand with Copy JSON."))
                }
                root = parsed
            }
            var servers = root[key] as? [String: Any] ?? [:]
            servers["dblens"] = installed ? entry : nil
            root[key] = servers
            let data = try JSONSerialization.data(withJSONObject: root, options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
            try data.write(to: configFile, options: .atomic)
        }
    }

    /// The text without one [table] and its keys (up to the next table).
    private static func removingTOMLTable(_ name: String, from text: String) -> String {
        var lines: [Substring] = []
        var skipping = false
        for line in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("[") { skipping = trimmed == "[\(name)]" }
            if !skipping { lines.append(line) }
        }
        return lines.joined(separator: "\n").replacingOccurrences(of: "\n\n\n", with: "\n\n")
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
            guard process.terminationStatus == 0 else { throw AgentSetupError(text.trimmingCharacters(in: .whitespacesAndNewlines)) }
            return text
        }.value
    }
}

/// An install step that failed, with what to tell the user.
struct AgentSetupError: LocalizedError {
    let errorDescription: String?
    init(_ message: String) { errorDescription = message }
}
