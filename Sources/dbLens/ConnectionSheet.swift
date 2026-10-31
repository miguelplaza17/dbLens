import SwiftUI
import DBLensKit

/// New or edit connection, two ways: paste a URL (any of the usual formats,
/// see ConnectionURL), or fill in host, port, database, user, and password.
/// Connect proves it works before it is saved. Either way it is saved as a URL.
struct ConnectionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let editing: SavedConnection?

    private enum Mode: String, CaseIterable {
        case url = "URL"
        case fields = "Fields"

        var title: String {
            switch self {
            case .url: "URL"
            case .fields: String(localized: "Fields")
            }
        }
    }

    @State private var mode = Mode.url
    @State private var url = ""
    @State private var name = ""
    @State private var host = ""
    @State private var port = ""
    @State private var database = ""
    @State private var user = ""
    @State private var password = ""
    @State private var sslmode = ""
    /// The kind of database, in the form. In the URL it is read from the text.
    @State private var engine = Engine.postgres
    @State private var useSSH = false
    @State private var sshHost = ""
    @State private var sshPort = ""
    @State private var sshUser = ""
    @State private var sshKey = ""
    /// The saved URL, when editing: its password shows only as the mask.
    @State private var original: String?
    @State private var error: String?
    @State private var working = false

    private static let sslModes: [(String, String)] = [
        ("", String(localized: "Default (prefer)")), ("disable", String(localized: "Off")),
        ("require", String(localized: "Require")),
        ("verify-ca", String(localized: "Verify CA")), ("verify-full", String(localized: "Verify CA and Host")),
    ]

    /// The URL to save: as typed, or built from the fields.
    private var text: String {
        if mode == .fields {
            if engine == .sqlite { return "sqlite://" + database.trimmingCharacters(in: .whitespaces) }
            return ConnectionURL.compose(scheme: engine.scheme, host: host.trimmingCharacters(in: .whitespaces), port: port,
                                         database: database, user: user, password: password,
                                         sslmode: engine == .postgres ? sslmode : "")
        }
        return original.map { ConnectionURL.unmask(url, originalText: $0) } ?? url
    }
    /// The saved password, when editing.
    private var originalPassword: String? { original.flatMap { ConnectionURL.secrets(in: $0)["password"] } }
    /// The database the URL or the form is for.
    private var detected: Engine? { mode == .url ? Engine.detect(url) : engine }
    private var parsed: Connector.Summary? { Connector.summary(text) }
    private var ssh: SSHTunnel.Config? {
        let host = sshHost.trimmingCharacters(in: .whitespaces)
        guard useSSH, !host.isEmpty else { return nil }
        return SSHTunnel.Config(host: host, port: Int(sshPort.trimmingCharacters(in: .whitespaces)),
                                user: sshUser.trimmingCharacters(in: .whitespaces),
                                keyPath: sshKey.trimmingCharacters(in: .whitespaces))
    }
    private var canConnect: Bool {
        let target = mode == .url ? !url.trimmingCharacters(in: .whitespaces).isEmpty
            : !(engine == .sqlite ? database : host).trimmingCharacters(in: .whitespaces).isEmpty
        return target && detected?.isAvailable == true && (!useSSH || !sshHost.trimmingCharacters(in: .whitespaces).isEmpty)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Picker("Mode", selection: $mode) {
                ForEach(Mode.allCases, id: \.self) { Text($0.title).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 200)
            .frame(maxWidth: .infinity)
            .padding(.top, Space.xl)
            .onChange(of: mode) { _, new in switchMode(to: new) }

            Form {
                Section {
                    if mode == .url {
                        // One line: a long URL scrolls sideways instead of wrapping.
                        TextField("URL", text: $url, prompt: Text(verbatim: "postgres://user:password@host:5432/database"))
                            .font(.system(.body, design: .monospaced))
                            .lineLimit(1)
                            .autocorrectionDisabled()
                    } else {
                        Picker("Type", selection: $engine) {
                            ForEach(Engine.allCases, id: \.self) { kind in
                                Text(kind.isAvailable ? kind.name : String(localized: "\(kind.name) (coming soon)")).tag(kind)
                            }
                        }
                        if engine == .sqlite {
                            LabeledContent("File") {
                                HStack(spacing: Space.sm) {
                                    TextField("File", text: $database, prompt: Text(verbatim: "~/Projects/app/data.sqlite"))
                                        .labelsHidden()
                                        .autocorrectionDisabled()
                                    Button("Choose…", action: chooseDatabaseFile)
                                }
                            }
                        } else {
                            TextField("Host", text: $host, prompt: Text(verbatim: "db.example.com"))
                                .autocorrectionDisabled()
                            TextField("Port", text: $port, prompt: Text(verbatim: engine.defaultPort.map(String.init) ?? ""))
                            TextField("Database", text: $database, prompt: Text(verbatim: user.isEmpty ? "postgres" : user))
                                .autocorrectionDisabled()
                            TextField("User", text: $user, prompt: Text(verbatim: "postgres"))
                                .autocorrectionDisabled()
                            SecureField("Password", text: $password)
                            if engine == .postgres {
                                Picker("SSL", selection: $sslmode) {
                                    ForEach(Self.sslModes, id: \.0) { Text($0.1).tag($0.0) }
                                }
                            }
                        }
                    }
                    TextField("Name", text: $name, prompt: Text(parsed?.suggestedName ?? String(localized: "Optional")))
                } header: {
                    HStack {
                        Text(editing == nil ? String(localized: "New Connection") : String(localized: "Edit Connection"))
                        Spacer()
                        // The database read from the URL, as soon as it is recognized.
                        if mode == .url, let detected { EngineTag(engine: detected) }
                    }
                } footer: {
                    Text(footer)
                        .foregroundStyle(.secondary)
                }
                Section {
                    // A SQLite file is on this Mac: no server to tunnel to.
                    if detected != .sqlite {
                        Toggle("Connect via SSH", isOn: $useSSH)
                    }
                    if useSSH && detected != .sqlite {
                        TextField("Server", text: $sshHost, prompt: Text("host, IP, or user@host"))
                            .autocorrectionDisabled()
                        TextField("Port", text: $sshPort, prompt: Text("22"))
                        TextField("User", text: $sshUser, prompt: Text("from ~/.ssh/config"))
                            .autocorrectionDisabled()
                        LabeledContent("Key") {
                            HStack(spacing: Space.sm) {
                                TextField("Key", text: $sshKey, prompt: Text("ssh-agent or ~/.ssh/config"))
                                    .labelsHidden()
                                    .autocorrectionDisabled()
                                Button("Choose…", action: chooseKey)
                            }
                        }
                    }
                } footer: {
                    if useSSH {
                        Text("Uses the Mac's ssh: your keys, ssh-agent, and ~/.ssh/config. The database host is resolved by the server, so use localhost for a database on the server itself.")
                            .foregroundStyle(.secondary)
                    }
                }
                if let error {
                    Section {
                        Label(error, systemImage: "exclamationmark.triangle.fill")
                            .foregroundStyle(.red)
                            .textSelection(.enabled)
                    }
                }
            }
            .formStyle(.grouped)
            .fitsContent()

            HStack {
                if working {
                    ProgressView().controlSize(.small)
                    Text("Connecting…").foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(editing == nil ? String(localized: "Connect") : String(localized: "Save and Connect")) { Task { await save() } }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(!canConnect || working)
            }
            .padding(.horizontal, Space.xl)
            .padding(.bottom, Space.xl)
        }
        .frame(width: 560)
        .noInitialFocus()
        .onAppear {
            if editing == nil, let draft = model.connectionDraft {
                url = draft
                model.connectionDraft = nil
            }
            if let editing {
                original = editing.url
                url = editing.url.map(ConnectionURL.redact) ?? ""
                name = editing.name
                if let ssh = editing.ssh {
                    useSSH = true
                    sshHost = ssh.host
                    sshPort = ssh.port.map(String.init) ?? ""
                    sshUser = ssh.user
                    sshKey = ssh.keyPath
                }
            }
        }
    }

    private var footer: String {
        if let detected, !detected.isAvailable {
            return String(localized: "dbLens can't connect to \(detected.name) yet. Support is on the way.")
        }
        if mode == .url, detected == nil, !url.trimmingCharacters(in: .whitespaces).isEmpty {
            return String(localized: "This doesn't look like a database URL.")
        }
        if mode == .fields { return String(localized: "Stored only for your Mac user account.") }
        if let parsed {
            var parts = [String(localized: "database \(parsed.database)")]
            if let host = parsed.host { parts.append(String(localized: "host \(host)")) }
            if let port = parsed.port { parts.append(String(localized: "port \(port)")) }
            if let user = parsed.user { parts.append(String(localized: "user \(user)")) }
            if let tls = parsed.tls { parts.append(String(localized: "SSL \(tls)")) }
            return String(localized: "Understood: \(parts.joined(separator: " · "))")
        }
        return String(localized: "Paste any connection URL: the kind of database is read from it. .env lines (DATABASE_URL=…) work too. Stored only for your Mac user account.")
    }

    /// Carries what was typed across: URL → fields fills them in; fields → URL builds the URL.
    private func switchMode(to new: Mode) {
        error = nil
        switch new {
        case .fields:
            let full = original.map { ConnectionURL.unmask(url, originalText: $0) } ?? url
            engine = Engine.detect(full) ?? engine
            if engine == .sqlite {
                database = Connector.clean(full).replacingOccurrences(of: "^(?i)(sqlite3?|file):(//)?", with: "", options: .regularExpression)
            } else if let pg = try? ConnectionURL(full), engine == .postgres {
                host = pg.hosts.split(separator: ",").first.map(String.init) ?? ""
                port = pg.settings["port"]?.split(separator: ",").first.map(String.init) ?? ""
                database = pg.settings["dbname"] ?? ""
                user = pg.user ?? ""
                password = pg.password ?? ""
                sslmode = Self.sslModes.contains { $0.0 == pg.sslmode } ? pg.sslmode ?? "" : ""
            } else if let parts = URLParts(full) {
                host = parts.hosts.first?.name ?? ""
                port = parts.hosts.first?.port.map(String.init) ?? ""
                database = parts.path
                user = parts.user ?? ""
                password = parts.password ?? ""
            }
        case .url:
            let target = (engine == .sqlite ? database : host).trimmingCharacters(in: .whitespaces)
            if !target.isEmpty {
                // The saved password goes back on screen as the mask; a new one shows as typed.
                url = engine == .sqlite ? text : ConnectionURL.compose(
                    scheme: engine.scheme, host: host.trimmingCharacters(in: .whitespaces), port: port, database: database,
                    user: user, password: password, sslmode: engine == .postgres ? sslmode : "",
                    maskPassword: !password.isEmpty && password == originalPassword)
            }
        }
    }

    /// Picks a SQLite file.
    private func chooseDatabaseFile() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.message = String(localized: "Choose a SQLite database file")
        if panel.runModal() == .OK, let path = panel.url?.path {
            database = path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
            if name.isEmpty { name = panel.url?.lastPathComponent ?? "" }
        }
    }

    /// Picks a private key, starting in ~/.ssh with hidden files shown.
    private func chooseKey() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = false
        panel.showsHiddenFiles = true
        panel.directoryURL = FileManager.default.homeDirectoryForCurrentUser.appending(path: ".ssh")
        panel.message = String(localized: "Choose the private key (not the .pub file)")
        if panel.runModal() == .OK, let path = panel.url?.path {
            sshKey = path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
        }
    }

    private func save() async {
        working = true
        error = nil
        defer { working = false }
        do {
            try await model.save(url: text, name: name, ssh: ssh, editing: editing)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
