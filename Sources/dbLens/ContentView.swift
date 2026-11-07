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
    /// The folder whose name is being typed (new or renamed).
    /// The folder whose name is being typed in place (new or renamed).
    @State private var editingFolder: UUID?
    @State private var folderName = ""
    @FocusState private var folderFieldFocused: Bool
    /// Where a dragged connection would land right now: a row, a folder, or the title.
    @State private var dropTarget: DropTarget?

    /// The folder being dragged, if a folder is: only then does a folder's top edge
    /// take it in front of that folder instead of into it.
    @State private var draggingFolder: UUID?

    private enum DropTarget: Hashable { case row(UUID), folder(UUID), beforeFolder(UUID), top }

    private func targeting(_ target: DropTarget) -> (Bool) -> Void {
        { inside in
            withAnimation(.snappy(duration: 0.18)) {
                if inside { dropTarget = target } else if dropTarget == target { dropTarget = nil }
            }
        }
    }

    private enum Row: Hashable { case connection(UUID), relation(String), folder(UUID) }

    /// A folder can be selected, like in Finder: the List only drags rows it can
    /// select, and File > New Subfolder works on the selected folder.
    private var selection: Binding<Row?> {
        Binding {
            if let id = model.selectedFolderID, model.folders.contains(where: { $0.id == id }) { return .folder(id) }
            return model.shown == .table ? model.selectedRelation.map(Row.relation) : nil
        } set: { row in
            if case .folder(let id) = row { model.selectedFolderID = id; return }
            model.selectedFolderID = nil
            switch row {
            case .connection(let id):
                if let connection = connections.first(where: { $0.uuid == id }) { Task { await model.connect(connection) } }
            case .relation(let id): Task { await model.showTable(id) }
            case .folder, nil: break
            }
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

    var body: some View {
        List(selection: selection) {
            Section {
                // The connections outside any folder, then the folders.
                // One flat run of rows, each with its own id and indent: nested
                // DisclosureGroups in a List could show rows under the wrong folder.
                ForEach(connectionTree) { item in
                    switch item.kind {
                    case .folder(let folder): folderRow(folder, depth: item.depth).tag(Row.folder(folder.id))
                    case .connection(let connection): connectionRow(connection, depth: item.depth)
                    case .slot: dropSlot.padding(.leading, CGFloat(item.depth) * Space.lg)
                    }
                }
            } header: {
                // Drop here to take a connection out of its folder.
                HStack(spacing: Space.sm) {
                    Text(dropTarget == .top ? String(localized: "Connections · drop to take it out of the folder") : String(localized: "Connections"))
                        .foregroundStyle(dropTarget == .top ? Palette.accentText : .secondary)
                    Spacer(minLength: 0)
                    Button("New Folder", systemImage: "folder.badge.plus") { newFolder() }
                        .help("New Folder (Option-Command-N)")
                    Button("New Connection…", systemImage: "plus") { model.showingNewConnection = true }
                        .help("New Connection (Command-N)")
                }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                // Clear of the list's scroll bar.
                .padding(.trailing, Space.sm)
                .contentShape(Rectangle())
                // The header is always there, so it carries the thin scroll bars for the list.
                .background(ThinScrollBars().frame(width: 0, height: 0))
                .dropDestination(for: String.self, action: { items, _ in drop(items, before: nil, into: "") },
                                 isTargeted: targeting(.top))
                    .contextMenu { Button("New Folder", systemImage: "folder.badge.plus") { newFolder() } }
            }
            if model.session != nil {
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
        }
        .searchable(text: $filter, placement: .sidebar, prompt: model.usesCommands ? Text("Filter Keys") : model.usesQueries ? Text("Filter Collections") : Text("Filter Tables"))
        .contextMenu { Button("New Folder", systemImage: "folder.badge.plus") { newFolder() } }
        .onChange(of: folderFieldFocused) { _, focused in
            if !focused, editingFolder != nil { commitFolderName() }
        }
        .onAppear(perform: loadFolded)
        // File > New Folder.
        .onChange(of: model.newFolderRequests) { newFolder() }
        // File > New Subfolder, in the selected folder.
        .onChange(of: model.newSubfolderRequests) { if let id = model.selectedFolderID { newFolder(in: id) } }
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

    /// A row of the Connections list: a folder, a connection, or the slot a drop would fill.
    private struct TreeItem: Identifiable {
        enum Kind { case folder(ConnectionFolder), connection(SavedConnection), slot }
        let id: String
        let kind: Kind
        let depth: Int
    }

    /// The connections outside any folder, then the folders, each followed
    /// (when open) by its own connections and then its subfolders, a level in.
    private var connectionTree: [TreeItem] {
        var items: [TreeItem] = []
        var seen: Set<UUID> = []
        // At every level the connections come first, then the subfolders: a
        // connection after an open folder looked like it was inside it.
        func add(_ parent: UUID?, depth: Int) {
            for connection in AppModel.ordered(connections, in: parent?.uuidString ?? "") {
                items.append(TreeItem(id: "connection-\(connection.uuid)", kind: .connection(connection), depth: depth))
            }
            // Where a connection dropped on the folder lands: last among its connections.
            if let parent, dropTarget == .folder(parent) {
                items.append(TreeItem(id: "slot-\(parent)", kind: .slot, depth: depth))
            }
            for folder in model.subfolders(of: parent) where seen.insert(folder.id).inserted {
                items.append(TreeItem(id: "folder-\(folder.id)", kind: .folder(folder), depth: depth))
                if folder.expanded { add(folder.id, depth: depth + 1) }
            }
        }
        add(nil, depth: 0)
        return items
    }

    private func folderRow(_ folder: ConnectionFolder, depth: Int) -> some View {
        let lit = dropTarget == .folder(folder.id)
        let before = dropTarget == .beforeFolder(folder.id)
        return HStack(spacing: Space.xs) {
            // The disclosure arrow, like a sidebar outline's.
            Image(systemName: "chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(lit ? Palette.onAccent : Color.secondary)
                .rotationEffect(.degrees(folder.expanded ? 90 : 0))
                .frame(width: Space.md)
                .accessibilityHidden(true)
            HStack(spacing: Space.sm) {
                Image(systemName: lit ? "folder.fill" : "folder")
                if editingFolder == folder.id {
                    // The name, typed in place: Return or a click elsewhere keeps it, Escape leaves it.
                    TextField("Folder Name", text: $folderName)
                        .textFieldStyle(.plain)
                        .focused($folderFieldFocused)
                        .onSubmit(commitFolderName)
                        .onExitCommand { editingFolder = nil }
                        .onAppear { folderFieldFocused = true }
                } else {
                    Text(verbatim: folder.name).lineLimit(1)
                }
                Spacer(minLength: 0)
                if lit {
                    Text("drop to move").font(.caption)
                }
            }
            .foregroundStyle(lit ? Palette.onAccent : Color.primary)
        }
        .padding(.horizontal, lit ? Space.xs : 0)
        .frame(maxWidth: .infinity, minHeight: Space.xxl, alignment: .leading)
        .background(Palette.accent.opacity(lit ? 1 : 0), in: RoundedRectangle(cornerRadius: 6, style: .continuous))  // grid: ok drop highlight
        .contentShape(Rectangle())
        .padding(.leading, CGFloat(depth) * Space.lg)
        // A click on the folder or its arrow opens or closes it.
        .onTapGesture {
            guard editingFolder != folder.id else { return }
            withAnimation(.snappy) { model.setFolderExpanded(folder.id, !folder.expanded) }
        }
        // Not one combined element: that turned the name field into a button,
        // so a new folder's name couldn't be typed.
        .accessibilityAction(named: folder.expanded ? Text("Collapse") : Text("Expand")) {
            model.setFolderExpanded(folder.id, !folder.expanded)
        }
        // Dragged onto another folder's top edge, a folder goes in front of it
        // (the same level); onto the rest of it, it becomes a subfolder there.
        .padding(.top, before ? Space.xxl + Space.xs : 0)
        .overlay(alignment: .top) {
            if before {
                dropSlot.transition(.opacity)
            }
        }
        .onDrag {
            draggingFolder = folder.id
            return NSItemProvider(object: (Self.folderPrefix + folder.id.uuidString) as NSString)
        } preview: {
            FolderDragCard(name: folder.name)
        }
        .onDrop(of: [.utf8PlainText, .plainText], delegate: FolderDropDelegate(
            folder: folder.id,
            reorders: draggingFolder != nil && draggingFolder != folder.id,
            target: $dropTarget
        ) { item, inFront in
            if inFront {
                drop([item], beforeFolder: folder)
            } else {
                drop([item], before: nil, into: folder.id.uuidString)
            }
        })
        .contextMenu { folderMenu(folder) }
        // Spring-loaded, like Finder: a closed folder opens after a moment under a drag.
        .task(id: lit) {
            guard lit, !folder.expanded else { return }
            try? await Task.sleep(for: .milliseconds(500))
            if !Task.isCancelled, dropTarget == .folder(folder.id) {
                withAnimation(.snappy) { model.setFolderExpanded(folder.id, true) }
            }
        }
    }

    /// What a dragged folder carries, so drops tell it from a connection.
    private static let folderPrefix = "folder:"

    /// A dragged folder lands in front of `target`, in its parent. Anything else goes into `target`.
    @discardableResult
    private func drop(_ items: [String], beforeFolder target: ConnectionFolder) -> Bool {
        guard let item = items.first, item.hasPrefix(Self.folderPrefix),
              let id = UUID(uuidString: String(item.dropFirst(Self.folderPrefix.count))) else {
            return drop(items, before: nil, into: target.id.uuidString)
        }
        dropTarget = nil
        draggingFolder = nil
        var moved = false
        withAnimation(.snappy) { moved = model.moveFolder(id, into: target.parentID, before: target.id) }
        return moved
    }

    /// Drops on a folder: where the pointer is says in front of it or into it.
    private struct FolderDropDelegate: DropDelegate {
        let folder: UUID
        let reorders: Bool
        @Binding var target: DropTarget?
        let perform: (String, Bool) -> Bool

        /// The top edge (and the slot it opens above the folder) means "in front of".
        private func zone(_ info: DropInfo) -> DropTarget {
            let edge = target == .beforeFolder(folder) ? Space.xxl + Space.xs + Space.md : Space.md
            return reorders && info.location.y < edge ? .beforeFolder(folder) : .folder(folder)
        }

        func dropEntered(info: DropInfo) { show(zone(info)) }

        func dropUpdated(info: DropInfo) -> DropProposal? {
            let next = zone(info)
            if next != target { show(next) }
            return DropProposal(operation: .move)
        }

        func dropExited(info: DropInfo) {
            if target == .folder(folder) || target == .beforeFolder(folder) { show(nil) }
        }

        func performDrop(info: DropInfo) -> Bool {
            let inFront = zone(info) == .beforeFolder(folder)
            guard let provider = info.itemProviders(for: [.utf8PlainText, .plainText]).first else { return false }
            _ = provider.loadObject(ofClass: NSString.self) { object, _ in
                guard let item = object as? String else { return }
                DispatchQueue.main.async { _ = perform(item, inFront) }
            }
            return true
        }

        private func show(_ next: DropTarget?) {
            withAnimation(.snappy(duration: 0.18)) { target = next }
        }
    }

    /// The outlined space where a dragged connection will land.
    private var dropSlot: some View {
        RoundedRectangle(cornerRadius: 6, style: .continuous)  // grid: ok drop slot
            .strokeBorder(Palette.accent, style: StrokeStyle(lineWidth: 1.5, dash: [4, 3]))  // grid: ok dashed outline
            .background(Palette.accent.opacity(0.1), in: RoundedRectangle(cornerRadius: 6, style: .continuous))  // grid: ok drop slot
            .frame(height: Space.xxl)
            .transition(.opacity.combined(with: .scale(scale: 0.9, anchor: .top)))
    }

    /// While a connection is dragged over this row, the row steps down and leaves
    /// an outlined slot above it: where the connection will land.
    private func connectionRow(_ connection: SavedConnection, depth: Int) -> some View {
        let open = dropTarget == .row(connection.uuid)
        return ConnectionRow(connection: connection)
            .padding(.leading, CGFloat(depth) * Space.lg)
            .padding(.top, open ? Space.xxl + Space.xs : 0)
            .overlay(alignment: .top) {
                if open {
                    RoundedRectangle(cornerRadius: 6, style: .continuous)  // grid: ok drop slot
                        .strokeBorder(Palette.accent, style: StrokeStyle(lineWidth: 1.5, dash: [4, 3]))  // grid: ok dashed outline
                        .background(Palette.accent.opacity(0.1), in: RoundedRectangle(cornerRadius: 6, style: .continuous))  // grid: ok drop slot
                        .frame(height: Space.xxl)
                        .transition(.opacity)
                }
            }
            .tag(Row.connection(connection.uuid))
            .draggable(connection.uuid.uuidString) { ConnectionDragCard(connection: connection) }
            .dropDestination(for: String.self, action: { items, _ in drop(items, before: connection, into: connection.folderID) },
                             isTargeted: targeting(.row(connection.uuid)))
            .contextMenu { menu(for: connection) }
    }

    /// A dragged connection id lands before `target`, or last in the folder.
    /// A dragged folder goes into that folder (never into itself or its own subfolders).
    @discardableResult
    private func drop(_ items: [String], before target: SavedConnection?, into folderID: String) -> Bool {
        dropTarget = nil
        draggingFolder = nil
        if let item = items.first, item.hasPrefix(Self.folderPrefix) {
            guard let id = UUID(uuidString: String(item.dropFirst(Self.folderPrefix.count))) else { return false }
            var moved = false
            withAnimation(.snappy) { moved = model.moveFolder(id, into: UUID(uuidString: folderID)) }
            return moved
        }
        guard let id = items.first.flatMap(UUID.init(uuidString:)),
              let dragged = connections.first(where: { $0.uuid == id }) else { return false }
        withAnimation(.snappy) { model.move(dragged, before: target, into: folderID, all: connections) }
        return true
    }

    /// Puts the folder's name in an editable field, selected, right in the list.
    private func startNaming(_ folder: ConnectionFolder) {
        folderName = folder.name
        model.setFolderExpanded(folder.id, true)
        editingFolder = folder.id
    }

    private func commitFolderName() {
        if let id = editingFolder { model.renameFolder(id, to: folderName) }
        editingFolder = nil
    }

    private func newFolder(in parent: UUID? = nil) {
        let folder = model.addFolder(named: String(localized: "New Folder"), in: parent)
        startNaming(folder)
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
                Button(model.path(of: other.id)) { withAnimation(.snappy) { _ = model.moveFolder(folder.id, into: other.id) } }
                    .disabled(folder.parentID == other.id)
            }
            if model.folders.count > 1 { Divider() }
            Button("Top Level") { withAnimation(.snappy) { _ = model.moveFolder(folder.id, into: nil) } }
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

/// What follows the pointer while a folder is dragged: a small card with its name.
private struct FolderDragCard: View {
    let name: String

    var body: some View {
        Label {
            Text(verbatim: name).lineLimit(1)
        } icon: {
            Image(systemName: "folder")
        }
        .font(.callout)
        .padding(.horizontal, Space.sm)
        .padding(.vertical, Space.xs)
        .frame(maxWidth: 160, alignment: .leading)
        .fixedSize()
        .background(.regularMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(Palette.accent.opacity(0.6)))
        .shadow(color: .black.opacity(0.35), radius: 8, y: 3)
        .opacity(0.85)
    }
}

/// What follows the pointer while a connection is dragged: a small card of the row.
private struct ConnectionDragCard: View {
    let connection: SavedConnection

    var body: some View {
        let engine = Engine(rawValue: connection.engine) ?? .postgres
        Label {
            Text(verbatim: connection.name).lineLimit(1)
        } icon: {
            EngineIcon(engine)
        }
        .font(.callout)
        .padding(.horizontal, Space.sm)
        .padding(.vertical, Space.xs)
        .frame(maxWidth: 160, alignment: .leading)
        .fixedSize()
        .background(.regularMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(Palette.accent.opacity(0.6)))
        .shadow(color: .black.opacity(0.35), radius: 8, y: 3)
        // Small and see-through, so the folder or slot under the pointer stays visible.
        .opacity(0.85)
    }
}

struct ConnectionRow: View {
    @Environment(AppModel.self) private var model
    let connection: SavedConnection

    private var isActive: Bool { connection.uuid == model.activeID && model.session != nil }

    var body: some View {
        Label {
            Text(connection.name)
                .fontWeight(isActive ? .semibold : .regular)
                .lineLimit(1)
        } icon: {
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
        .badge(isActive ? Text(Image(systemName: "circle.fill")).foregroundStyle(.green).font(.system(size: 7)) : nil)
        .help(connection.ssh.map { String(localized: "Via SSH \($0.label)") } ?? "")
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
