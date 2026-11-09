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
            BottomPanelLayout(showPanel: model.showSQLPanel, collapse: {
                withAnimation(.snappy(duration: 0.25)) { model.showSQLPanel = false }
            }) {
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
            let room = model.session == nil ? geometry.size.height : geometry.size.height * Self.connectionsShare
            VStack(spacing: 0) {
                connectionsHeader
                ConnectionsOutline(model: model, connections: connections, editingFolder: editingFolder,
                                   startNaming: startNaming, newFolder: { newFolder(in: $0) },
                                   rename: { id, name in
                                       if let name { model.renameFolder(id, to: name) }
                                       editingFolder = nil
                                   },
                                   askDelete: { connection in
                                       if confirmDelete { deleting = connection } else { model.delete(connection) }
                                   },
                                   maxHeight: room, contentHeight: $outlineHeight)
                    .frame(height: ConnectionsOutline.frameHeight(rows: outlineHeight, maxHeight: room))
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

    /// Puts the folder's name in an editable field, right in the list.
    private func startNaming(_ id: UUID) {
        model.setFolderExpanded(id, true)
        editingFolder = id
    }

    private func newFolder(in parent: UUID? = nil) {
        let folder = model.addFolder(named: String(localized: "New Folder"), in: parent)
        startNaming(folder.id)
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
/// area: flush with the window, under a thin divider. Drag the divider to
/// resize (the height is remembered), or past the panel's smallest size to fold it away.
/// (VSplitView ignored the toolbar's safe area and slid the grid under it.)
struct BottomPanelLayout<Content: View, Panel: View>: View {
    let showPanel: Bool
    let collapse: () -> Void
    @ViewBuilder let content: Content
    @ViewBuilder let panel: Panel

    @AppStorage("sqlPanel.height") private var height: Double = 280
    @State private var dragStart: Double?

    private static var minPanel: Double { 140 }
    private static var minContent: Double { 160 }
    /// How far past the smallest size a drag folds the panel away.
    private static var collapseMargin: Double { 60 }

    var body: some View {
        GeometryReader { geometry in
            let limit = max(Self.minPanel, geometry.size.height - Self.minContent)
            VStack(spacing: 0) {
                content
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                if showPanel {
                    PaneDivider(axis: .horizontal) { translation in
                        let start = dragStart ?? min(height, limit)
                        dragStart = start
                        let wanted = start - translation
                        if wanted < Self.minPanel - Self.collapseMargin {
                            // It reopens at the height it had before this drag.
                            height = start
                            dragStart = nil
                            collapse()
                        } else {
                            height = min(max(wanted, Self.minPanel), limit)
                        }
                    } ended: {
                        dragStart = nil
                    }
                    panel
                        .frame(height: min(max(height, Self.minPanel), limit))
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
        }
    }
}

/// The line between two panes: the system's divider, one point thick, with a
/// wider area around it to grab, as in a split view.
struct PaneDivider: View {
    let axis: Axis
    let changed: (CGFloat) -> Void
    let ended: () -> Void

    var body: some View {
        Divider()
            .overlay {
                Color.clear
                    .frame(width: axis == .vertical ? Space.sm : nil, height: axis == .horizontal ? Space.sm : nil)
                    .contentShape(Rectangle())
                    .pointerStyle(axis == .horizontal ? .rowResize : .columnResize)
                    .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .global)
                        .onChanged { changed(axis == .horizontal ? $0.translation.height : $0.translation.width) }
                        .onEnded { _ in ended() })
            }
            .zIndex(1)
    }
}
