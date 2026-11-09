import SwiftData
import SwiftUI
import UniformTypeIdentifiers
import DBLensKit

/// Connections and tables on the left; on the right one grid that shows either
/// the picked table or the last query's result, with the SQL panel (editor and
/// log) folding out at the bottom.
struct ContentView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openSettings) private var openSettings
    @Environment(Updater.self) private var updater

    var body: some View {
        @Bindable var model = model
        NavigationSplitView {
            Sidebar()
                // An agent added or removed a connection: fetch the list again.
                .id(model.connectionsVersion)
                .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 360)
        } detail: {
            BottomPanelLayout(showPanel: model.showSQLPanel) {
                Detail()
            } panel: {
                SQLPanel()
            }
        }
        .navigationTitle(model.activeConnection?.name ?? "dbLens")
        .navigationSubtitle(subtitle)
        .sheet(isPresented: $model.showingNewConnection) {
            ConnectionSheet(editing: nil)
        }
        .sheet(item: $model.editing) { connection in
            ConnectionSheet(editing: connection)
        }
        .sheet(item: $model.rowEdit) { target in
            RowEditor(row: target.row)
        }
        .task {
            await SeedData.insertIfRequested(into: model)
            SelfTest.runIfRequested(model: model)
            Snapshot.runIfRequested(model: model, updater: updater, openSettings: openSettings)
        }
    }
}

extension ContentView {
    fileprivate var subtitle: String {
        guard model.session != nil else { return "" }
        if model.shown == .query { return String(localized: "Query Result") }
        if model.shown == .schema { return String(localized: "Schema") }
        // A Redis key is its own full name; tables show schema.table.
        return model.relation.map { $0.kind == .key || $0.schema.isEmpty ? $0.name : "\($0.schema).\($0.name)" } ?? ""
    }
}

// MARK: Sidebar

/// Saved connections on top; the open database's tables below, one section per schema.
struct Sidebar: View {
    @Environment(AppModel.self) private var model
    @Query(sort: \SavedConnection.name) private var connections: [SavedConnection]
    @AppStorage("confirmDelete") private var confirmDelete = true
    @State private var filter = ""
    /// Folded groups (schemas, Redis prefixes) of the open connection, remembered per connection.
    @State private var folded: Set<String> = []
    @State private var deleting: SavedConnection?
    @State private var deletingKey: Relation?
    /// The folder whose name is being typed in place (new or renamed).
    @State private var editingFolder: UUID?
    /// What the connection rows need, as the outline reports it.
    @State private var outlineHeight: CGFloat = 0

    private enum Row: Hashable { case relation(String) }

    private var selection: Binding<Row?> {
        Binding {
            model.shown == .table ? model.selectedRelation.map(Row.relation) : nil
        } set: { row in
            if case .relation(let id)? = row { Task { await model.showTable(id) } }
        }
    }

    private func expanded(_ group: String) -> Binding<Bool> {
        Binding {
            !filter.isEmpty || !folded.contains(group)
        } set: { open in
            guard filter.isEmpty else { return }
            if open { folded.remove(group) } else { folded.insert(group) }
            if let id = model.activeID { UserDefaults.standard.set(Array(folded), forKey: "sidebar.folded.\(id.uuidString)") }
        }
    }

    private func loadFolded() {
        folded = model.activeID.flatMap { UserDefaults.standard.stringArray(forKey: "sidebar.folded.\($0.uuidString)") }.map(Set.init) ?? []
    }

    private var schemas: [(String, [Relation])] {
        let shown = filter.isEmpty ? model.relations : model.relations.filter {
            $0.name.localizedStandardContains(filter) || $0.schema.localizedStandardContains(filter)
        }
        var order: [String] = []
        var groups: [String: [Relation]] = [:]
        for relation in shown {
            if groups[relation.schema] == nil { order.append(relation.schema) }
            groups[relation.schema, default: []].append(relation)
        }
        return order.map { ($0, groups[$0] ?? []) }
    }

    /// The share of the sidebar the connections may take, while a database is
    /// open and its tables need the rest; past it they scroll.
    private static let connectionsShare: CGFloat = 0.5

    var body: some View {
        GeometryReader { geometry in
            // A little empty room under the rows, so a row can be dropped after the last one (or out of a folder, to the end).
            let wanted = outlineHeight + ConnectionsOutline.inset * 2 + Space.xxl
            let room = model.session == nil ? geometry.size.height : geometry.size.height * Self.connectionsShare
            VStack(spacing: 0) {
                connectionsHeader
                ConnectionsOutline(model: model, connections: connections, editingFolder: editingFolder,
                                   content: rowContent, newFolder: { newFolder() }, contentHeight: $outlineHeight)
                    .frame(height: max(min(wanted, room), ConnectionsOutline.rowHeight))
                if model.session != nil {
                    List(selection: selection) {
                        ForEach(schemas, id: \.0) { schema, relations in
                            // Redis keys group by prefix; keys without one go under "keys".
                            // Each group folds with its disclosure arrow; a filter shows every match.
                            Section(isExpanded: expanded(schema)) {
                                ForEach(relations) { relation in
                                    Label(relation.name, systemImage: relation.symbol)
                                        .tag(Row.relation(relation.id))
                                        .contextMenu { relationMenu(relation) }
                                }
                            } header: {
                                Text(schema.isEmpty ? String(localized: "keys") : schema)
                            }
                        }
                        if model.usesCommands, model.relations.count >= 5_000 {
                            Text("Showing the first 5,000 keys. Use the filter, or SCAN in the command panel.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                } else {
                    Spacer(minLength: 0)
                }
            }
        }
        .searchable(text: $filter, placement: .sidebar, prompt: model.usesCommands ? Text("Filter Keys") : model.usesQueries ? Text("Filter Collections") : Text("Filter Tables"))
        .onAppear(perform: loadFolded)
        // File > New Folder.
        .onChange(of: model.newFolderRequests) { newFolder() }
        .onChange(of: model.activeID) { loadFolded() }
        .confirmationDialog("Delete the key “\(deletingKey?.name ?? "")”?", isPresented: .init(
            get: { deletingKey != nil }, set: { if !$0 { deletingKey = nil } }
        )) {
            Button("Delete Key", role: .destructive) {
                if let key = deletingKey { Task { await model.deleteKey(key) } }
            }
        } message: {
            Text("DEL runs on the server at once. It can't be undone here.")
        }
        .confirmationDialog("Delete “\(deleting?.name ?? "")”?", isPresented: .init(
            get: { deleting != nil }, set: { if !$0 { deleting = nil } }
        )) {
            Button("Delete", role: .destructive) { if let deleting { model.delete(deleting) } }
        } message: {
            Text("The saved URL is erased. The database is not changed.")
        }
    }

    /// "Connections" with its buttons, above the outline.
    private var connectionsHeader: some View {
        HStack(spacing: Space.sm) {
            Text("Connections")
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.secondary)
            Spacer(minLength: 0)
            Button("New Folder", systemImage: "folder.badge.plus") { newFolder() }
                .help("New Folder (Option-Command-N)")
            Button("New Connection…", systemImage: "plus") { model.showingNewConnection = true }
                .help("New Connection (Command-N)")
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .padding(.horizontal, Space.md)
        .padding(.vertical, Space.xs)
    }

    /// What one row of the connections outline shows, with its context menu.
    private func rowContent(_ node: SidebarNode) -> AnyView {
        switch node.kind {
        case .connection:
            guard let connection = connections.first(where: { $0.uuid == node.id }) else { return AnyView(EmptyView()) }
            return AnyView(OutlineConnectionRow(connection: connection)
                .contextMenu { menu(for: connection) }
                .environment(model)
                .themed())
        case .folder:
            guard let folder = model.folders.first(where: { $0.id == node.id }) else { return AnyView(EmptyView()) }
            return AnyView(OutlineFolderRow(
                name: folder.name,
                editing: editingFolder == folder.id,
                commit: { name in
                    model.renameFolder(folder.id, to: name)
                    editingFolder = nil
                },
                cancel: { editingFolder = nil }
            )
            .contextMenu { folderMenu(folder) }
            .environment(model)
            .themed())
        }
    }

    /// Puts the folder's name in an editable field, right in the list.
    private func startNaming(_ folder: ConnectionFolder) {
        model.setFolderExpanded(folder.id, true)
        editingFolder = folder.id
    }

    private func newFolder(in parent: UUID? = nil) {
        let folder = model.addFolder(named: String(localized: "New Folder"), in: parent)
        startNaming(folder)
    }

    @ViewBuilder
    private func relationMenu(_ relation: Relation) -> some View {
        if relation.kind == .key {
            Button("Inspect in Commands", systemImage: "terminal") { model.queryTable(relation) }
            Button("Copy Key", systemImage: "doc.on.doc") { copy(relation.name) }
            Divider()
            Button("Delete Key…", systemImage: "trash", role: .destructive) { deletingKey = relation }
        } else {
            Button(model.usesQueries ? LocalizedStringKey("Query in Panel") : LocalizedStringKey("Query in SQL"), systemImage: "terminal") { model.queryTable(relation) }
            Button("Copy Name", systemImage: "doc.on.doc") { copy(model.session?.qualified(relation) ?? relation.qualified) }
        }
    }

    @ViewBuilder
    private func folderMenu(_ folder: ConnectionFolder) -> some View {
        Button("Rename…", systemImage: "pencil") {
            startNaming(folder)
        }
        Button("New Subfolder", systemImage: "folder.badge.plus") { newFolder(in: folder.id) }
        Button("New Folder", systemImage: "folder.badge.plus") { newFolder() }
        Menu("Move to", systemImage: "folder") {
            ForEach(model.folders.filter { !model.folder($0.id, isIn: folder.id) }) { other in
                Button(model.path(of: other.id)) { withAnimation(Motion.land) { _ = model.moveFolder(folder.id, into: other.id) } }
                    .disabled(folder.parentID == other.id)
            }
            if model.folders.count > 1 { Divider() }
            Button("Top Level") { withAnimation(Motion.land) { _ = model.moveFolder(folder.id, into: nil) } }
                .disabled(folder.parentID == nil)
        }
        Divider()
        Button("Delete Folder", systemImage: "trash", role: .destructive) { model.deleteFolder(folder.id) }
            .help("Its subfolders and connections move up one level; none is deleted.")
    }

    @ViewBuilder
    private func menu(for connection: SavedConnection) -> some View {
        Button("Connect", systemImage: "bolt.horizontal") { Task { await model.connect(connection) } }
        Button("Edit…", systemImage: "pencil") { model.editing = connection }
        Button("Open in Terminal", systemImage: "apple.terminal") { model.openInTerminal(connection) }
        Button("Copy URL Without Password", systemImage: "doc.on.doc") {
            copy(connection.url.map(ConnectionURL.redact) ?? "")
        }
        Button("Copy URL with Password") { copySecret(connection.url ?? "") }
            .help("Cleared from the clipboard after a minute")
        if connection.uuid == model.activeID {
            Button("Disconnect", systemImage: "bolt.horizontal.slash") { model.disconnect() }
        }
        Menu("Move to", systemImage: "folder") {
            ForEach(model.folders) { folder in
                Button(model.path(of: folder.id)) { model.move(connection, before: nil, into: folder.id.uuidString, all: connections) }
                    .disabled(connection.folderID == folder.id.uuidString)
            }
            if !model.folders.isEmpty { Divider() }
            Button("No Folder") { model.move(connection, before: nil, into: "", all: connections) }
                .disabled(connection.folderID.isEmpty)
            Button("New Folder", systemImage: "folder.badge.plus") {
                let folder = model.addFolder(named: String(localized: "New Folder"))
                model.move(connection, before: nil, into: folder.id.uuidString, all: connections)
                startNaming(folder)
            }
        }
        Divider()
        // dbLens's MCP server: whether agents see this connection, which tools they may use, and writing (opt-in).
        Menu("Agent Tools", systemImage: "sparkles") {
            AgentAccessItems(connection: connection) { try? model.context.save() }
        }
        Divider()
        Button("Delete…", systemImage: "trash", role: .destructive) {
            if confirmDelete { deleting = connection } else { model.delete(connection) }
        }
    }
}

/// A saved connection's row in the outline: its icon (a lock when it goes
/// through SSH), its name, and a green dot while it is open.
private struct OutlineConnectionRow: View {
    @Environment(AppModel.self) private var model
    let connection: SavedConnection

    private var isActive: Bool { connection.uuid == model.activeID && model.session != nil }

    var body: some View {
        HStack(spacing: Space.sm) {
            icon.frame(width: Space.lg)
            Text(verbatim: connection.name)
                .fontWeight(isActive ? .semibold : .regular)
                .lineLimit(1)
            Spacer(minLength: 0)
            if isActive {
                Image(systemName: "circle.fill").foregroundStyle(.green).font(.system(size: 7))  // grid: ok status dot
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .help(connection.ssh.map { String(localized: "Via SSH \($0.label)") } ?? "")
    }

    @ViewBuilder
    private var icon: some View {
        if model.connectingID == connection.uuid {
            ProgressView().controlSize(.small)
        } else {
            // The kind of database, in its color; a lock when it goes through SSH.
            let engine = Engine(rawValue: connection.engine) ?? .postgres
            if connection.sshHost.isEmpty {
                EngineIcon(engine)
            } else {
                Image(systemName: "lock.shield").foregroundStyle(engine.color)
            }
        }
    }
}

/// A folder's row: its name, or a field for typing it.
private struct OutlineFolderRow: View {
    let name: String
    let editing: Bool
    let commit: (String) -> Void
    let cancel: () -> Void

    var body: some View {
        HStack(spacing: Space.sm) {
            Image(systemName: "folder").foregroundStyle(.tint).frame(width: Space.lg)
            if editing {
                FolderNameField(initial: name, commit: commit, cancel: cancel)
            } else {
                Text(verbatim: name).lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }
}

/// The name of a folder being typed: Return or a click elsewhere keeps it, Escape leaves it.
private struct FolderNameField: View {
    @State private var text: String
    @State private var done = false
    @FocusState private var focused: Bool
    let commit: (String) -> Void
    let cancel: () -> Void

    init(initial: String, commit: @escaping (String) -> Void, cancel: @escaping () -> Void) {
        _text = State(initialValue: initial)
        self.commit = commit
        self.cancel = cancel
    }

    private func finish(_ save: Bool) {
        guard !done else { return }
        done = true
        if save { commit(text) } else { cancel() }
    }

    var body: some View {
        TextField("Folder Name", text: $text)
            .textFieldStyle(.plain)
            .focused($focused)
            .onSubmit { finish(true) }
            .onExitCommand { finish(false) }
            .onChange(of: focused) { _, now in if !now { finish(true) } }
            .onAppear { focused = true }
    }
}

extension Relation {
    var symbol: String {
        switch kind {
        case .table, .partitioned: subtype == "collection" ? "doc.on.doc" : "tablecells"
        case .view: "eye"
        case .materialized: "square.stack.3d.up"
        case .foreign: "link"
        case .key: switch subtype {
            case "hash": "number.square"
            case "list": "list.bullet"
            case "set": "circle.grid.2x2"
            case "zset": "chart.bar"
            case "stream": "water.waves"
            case "ReJSON-RL": "curlybraces"
            default: "textformat"
            }
        }
    }
}

func copy(_ text: String) {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
}

/// Copies a secret: marked concealed, so clipboard managers skip it, and
/// cleared after a minute unless something else was copied since.
func copySecret(_ text: String) {
    let pasteboard = NSPasteboard.general
    pasteboard.clearContents()
    pasteboard.setString(text, forType: .string)
    pasteboard.setString("", forType: NSPasteboard.PasteboardType("org.nspasteboard.ConcealedType"))
    let change = pasteboard.changeCount
    Task {
        try? await Task.sleep(for: .seconds(60))
        if pasteboard.changeCount == change { pasteboard.clearContents() }
    }
}

// MARK: Detail

/// The center: the grid, or why there is none.
struct Detail: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        content
            .toolbar {
                ToolbarItemGroup(placement: .navigation) {
                    Button("Back", systemImage: "chevron.backward") { Task { await model.goBack() } }
                        .help("Go back to the last page (Command-[)")
                        .disabled(!model.canGoBackInHistory)
                    Button("Forward", systemImage: "chevron.forward") { Task { await model.goForward() } }
                        .help("Go forward again (Command-])")
                        .disabled(!model.canGoForwardInHistory)
                }
                ToolbarItem(placement: .primaryAction) {
                    Toggle(isOn: Binding(get: { model.shown == .schema },
                                         set: { on in Task { if on { await model.showSchema() } else { model.shown = .table } } })) {
                        Label("Schema", systemImage: "point.3.connected.trianglepath.dotted")
                    }
                    .help("Show the tables and their relations on a board (Command-3)")
                    .disabled(!model.hasSchemaBoard)
                }
                ToolbarItem(placement: .primaryAction) {
                    Button("Refresh", systemImage: "arrow.clockwise") { Task { await model.refresh() } }
                        .help("Reload tables and rows (Command-R)")
                        .disabled(model.session == nil)
                }
                // Folded away, the SQL panel's toggle waits here, apart from the rest;
                // open, it sits in the panel's own header.
                if !model.showSQLPanel {
                    ToolbarSpacer(.fixed, placement: .primaryAction)
                    ToolbarItem(placement: .primaryAction) {
                        Button("Show SQL Panel", systemImage: "rectangle.bottomthird.inset.filled") {
                            withAnimation(.snappy(duration: 0.25)) { model.showSQLPanel = true }
                        }
                        .help("Show the SQL panel (Command-Shift-Y)")
                    }
                }
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.session != nil {
            ResultPane()
        } else if model.connectingID != nil {
            ProgressView("Connecting…")
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = model.connectError {
            ContentUnavailableView {
                Label("Couldn't Connect", systemImage: "bolt.horizontal.circle")
            } description: {
                Text(error).textSelection(.enabled)
            } actions: {
                if let connection = model.activeConnection {
                    Button("Try Again") { Task { await model.connect(connection) } }
                        .buttonStyle(.borderedProminent)
                    Button("Edit Connection…") { model.editing = connection }
                }
            }
        } else {
            ContentUnavailableView {
                Label("No Database Open", systemImage: "cylinder.split.1x2")
            } description: {
                Text("Choose a connection in the sidebar or paste the URL of a new database.")
            } actions: {
                Button("New Connection…") { model.showingNewConnection = true }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            }
        }
    }
}

// MARK: Bottom panel

/// The main content with a panel docked at the bottom, like Xcode's debug
/// area. Drag the line between them to resize; the height is remembered.
/// (VSplitView ignored the toolbar's safe area and slid the grid under it.)
struct BottomPanelLayout<Content: View, Panel: View>: View {
    let showPanel: Bool
    @ViewBuilder let content: Content
    @ViewBuilder let panel: Panel

    @AppStorage("sqlPanel.height") private var height: Double = 280
    @State private var dragStart: Double?

    private static var minPanel: Double { 140 }
    private static var minContent: Double { 160 }

    var body: some View {
        GeometryReader { geometry in
            let limit = max(Self.minPanel, geometry.size.height - Self.minContent)
            VStack(spacing: 0) {
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                if showPanel {
                    // The gap above the card is the handle: drag it to resize.
                    Capsule()
                        .fill(.tertiary)
                        .frame(width: 36, height: 4)
                        .frame(maxWidth: .infinity)
                        .frame(height: Space.md)
                        .contentShape(Rectangle())
                        .pointerStyle(.rowResize)
                        .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .global)
                            .onChanged { drag in
                                let start = dragStart ?? min(height, limit)
                                dragStart = start
                                height = min(max(start - drag.translation.height, Self.minPanel), limit)
                            }
                            .onEnded { _ in dragStart = nil })
                    // The panel draws its own cards; here it only gets its place.
                    panel
                        .frame(height: min(max(height, Self.minPanel), limit))
                        .padding([.horizontal, .bottom], Space.sm)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
        }
    }
}

extension View {
    /// A floating card like the sidebar: rounded, bordered, with a soft shadow.
    func card() -> some View {
        modifier(CardStyle())
    }
}

private struct CardStyle: ViewModifier {
    /// Matches the sidebar card's corners.
    private let shape = RoundedRectangle(cornerRadius: 16, style: .continuous)

    func body(content: Content) -> some View {
        content
            .clipShape(shape)
            .background(.background.secondary, in: shape)
            .overlay(shape.strokeBorder(.separator, lineWidth: 1))
            .shadow(color: .black.opacity(0.35), radius: 12, y: 4)
    }
}
