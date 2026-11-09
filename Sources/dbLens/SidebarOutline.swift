import AppKit
import SwiftUI

/// One row of the connections outline: a saved connection or a folder.
/// Instances are kept by id (`ConnectionsOutline.Coordinator.node`), so the
/// outline view's expansion and the drag's items stay the same objects.
nonisolated final class SidebarNode: NSObject {
    nonisolated enum Kind: Equatable { case connection, folder }
    let kind: Kind
    let id: UUID

    init(_ kind: Kind, _ id: UUID) {
        self.kind = kind
        self.id = id
    }

    nonisolated override var hash: Int { id.hashValue &+ (kind == .folder ? 1 : 0) }

    nonisolated override func isEqual(_ object: Any?) -> Bool {
        guard let other = object as? SidebarNode else { return false }
        return other.kind == kind && other.id == id
    }
}

/// The saved connections and their folders as the system's own outline view
/// (an `NSOutlineView` in the source list style), so dragging works the way it
/// does in Reminders: rows part to show where a row will land, a folder lights
/// up when a row will go into it, and a closed folder opens under a drag.
/// The rows are SwiftUI views (`content`), with their own context menus.
struct ConnectionsOutline: NSViewRepresentable {
    let model: AppModel
    let connections: [SavedConnection]
    /// What the user is naming right now, so the row shows its field.
    let editingFolder: UUID?
    let content: (SidebarNode) -> AnyView
    let newFolder: () -> Void
    /// The height all the rows need, reported back so the sidebar can size the outline.
    @Binding var contentHeight: CGFloat

    /// Points per row (the system's sidebar rows).
    static let rowHeight: CGFloat = 28
    /// Room above and below the rows.
    static let inset: CGFloat = Space.xs

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let outline = NSOutlineView()
        let column = NSTableColumn(identifier: .init("connections"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .sourceList
        outline.rowSizeStyle = .default
        outline.rowHeight = Self.rowHeight
        outline.usesAutomaticRowHeights = false
        outline.backgroundColor = .clear
        outline.floatsGroupRows = false
        outline.allowsEmptySelection = true
        outline.allowsMultipleSelection = false
        outline.autosaveExpandedItems = false
        // Rows part to make room, as in Reminders, instead of a line between them.
        outline.draggingDestinationFeedbackStyle = .gap
        outline.registerForDraggedTypes([Coordinator.pasteboardType])
        outline.setDraggingSourceOperationMask(.move, forLocal: true)
        outline.setDraggingSourceOperationMask([], forLocal: false)
        outline.dataSource = context.coordinator
        outline.delegate = context.coordinator
        outline.menu = context.coordinator.emptyMenu()

        let scroll = NSScrollView()
        scroll.documentView = outline
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = true
        scroll.scrollerStyle = .overlay
        scroll.automaticallyAdjustsContentInsets = false
        scroll.contentInsets = NSEdgeInsets(top: Self.inset, left: 0, bottom: Self.inset, right: 0)
        context.coordinator.outline = outline
        context.coordinator.reload(force: true)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.parent = self
        context.coordinator.reload(force: false)
    }

    // MARK: Coordinator

    @MainActor
    final class Coordinator: NSObject, NSOutlineViewDataSource, NSOutlineViewDelegate, NSMenuDelegate {
        static let pasteboardType = NSPasteboard.PasteboardType("io.github.miguelplaza17.dblens.sidebar-node")

        var parent: ConnectionsOutline
        weak var outline: NSOutlineView?
        /// Children by folder id ("" is the top level): connections first, then subfolders.
        private var tree: [String: [SidebarNode]] = [:]
        private var nodes: [String: SidebarNode] = [:]
        /// What the tree looked like when the outline last reloaded.
        private var signature = ""
        /// Set while the outline is being filled or synced from the model, so its
        /// own notifications (expanded, selected) are not taken for the user's.
        private var syncing = false

        init(_ parent: ConnectionsOutline) {
            self.parent = parent
        }

        // MARK: Tree

        private func node(_ kind: SidebarNode.Kind, _ id: UUID) -> SidebarNode {
            let key = (kind == .folder ? "f:" : "c:") + id.uuidString
            if let known = nodes[key] { return known }
            let made = SidebarNode(kind, id)
            nodes[key] = made
            return made
        }

        private func children(inFolder folderID: String) -> [SidebarNode] {
            let connections = AppModel.ordered(parent.connections, in: folderID).map { node(.connection, $0.uuid) }
            let parentID = UUID(uuidString: folderID)
            let folders = parent.model.subfolders(of: parentID).map { node(.folder, $0.id) }
            return connections + folders
        }

        private func buildTree() {
            var built: [String: [SidebarNode]] = [:]
            built[""] = children(inFolder: "")
            for folder in parent.model.folders { built[folder.id.uuidString] = children(inFolder: folder.id.uuidString) }
            tree = built
        }

        private func kids(of item: Any?) -> [SidebarNode] {
            guard let item = item as? SidebarNode else { return tree[""] ?? [] }
            return item.kind == .folder ? tree[item.id.uuidString] ?? [] : []
        }

        private func connection(_ id: UUID) -> SavedConnection? { parent.connections.first { $0.uuid == id } }
        private func folder(_ id: UUID) -> ConnectionFolder? { parent.model.folders.first { $0.id == id } }

        /// A line per row, with everything a row shows, so a change in any reloads the outline.
        private func currentSignature() -> String {
            var lines: [String] = []
            for (key, list) in tree.sorted(by: { $0.key < $1.key }) {
                lines.append("[\(key)] " + list.map { node in
                    switch node.kind {
                    case .connection:
                        let c = connection(node.id)
                        return "c\(node.id)\(c?.name ?? "")\(c?.engine ?? "")\(c?.sshHost ?? "")"
                    case .folder:
                        return "f\(node.id)\(folder(node.id)?.name ?? "")"
                    }
                }.joined(separator: ","))
            }
            return lines.joined(separator: "\n")
        }

        // MARK: Reloading

        /// Fills the outline from the model: a full reload when the tree changed,
        /// otherwise only the rows' content, expansion and selection.
        func reload(force: Bool) {
            guard let outline else { return }
            buildTree()
            let next = currentSignature()
            syncing = true
            defer { syncing = false }
            if force || next != signature {
                signature = next
                outline.reloadData()
            }
            syncExpansion()
            refreshCells()
            syncSelection()
            reportHeight()
        }

        /// What the rows need, for the sidebar to size the outline by.
        private func reportHeight() {
            guard let outline else { return }
            let rows = outline.numberOfRows
            let height = rows > 0 ? outline.rect(ofRow: rows - 1).maxY : 0
            let binding = parent.$contentHeight
            if abs(binding.wrappedValue - height) > 0.5 {
                DispatchQueue.main.async { binding.wrappedValue = height }
            }
        }

        /// Open and close folders as the model says (a new subfolder, a drop that opened one).
        private func syncExpansion() {
            guard let outline else { return }
            NSAnimationContext.beginGrouping()
            NSAnimationContext.current.duration = 0
            func visit(_ list: [SidebarNode]) {
                for item in list where item.kind == .folder {
                    let open = folder(item.id)?.expanded ?? false
                    if open != outline.isItemExpanded(item) {
                        if open { outline.expandItem(item) } else { outline.collapseItem(item) }
                    }
                    if open { visit(tree[item.id.uuidString] ?? []) }
                }
            }
            visit(tree[""] ?? [])
            NSAnimationContext.endGrouping()
        }

        private func refreshCells() {
            guard let outline else { return }
            for row in 0..<outline.numberOfRows {
                guard let item = outline.item(atRow: row) as? SidebarNode,
                      let cell = outline.view(atColumn: 0, row: row, makeIfNecessary: false) as? HostCell else { continue }
                cell.host.rootView = parent.content(item)
            }
        }

        /// The open connection is the selected row.
        private func syncSelection() {
            guard let outline, let active = parent.model.activeID, parent.model.session != nil || parent.model.connectingID != nil else {
                if outline?.selectedRow ?? -1 >= 0, parent.model.activeID == nil { outline?.deselectAll(nil) }
                return
            }
            let row = outline.row(forItem: node(.connection, active))
            if row >= 0, outline.selectedRow != row {
                outline.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            }
        }

        // MARK: Data source

        func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int { kids(of: item).count }

        func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any { kids(of: item)[index] }

        func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
            (item as? SidebarNode)?.kind == .folder
        }

        // MARK: Delegate: rows

        func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
            guard let item = item as? SidebarNode else { return nil }
            let id = NSUserInterfaceItemIdentifier("cell")
            let cell = outlineView.makeView(withIdentifier: id, owner: nil) as? HostCell ?? HostCell(identifier: id)
            cell.host.rootView = parent.content(item)
            return cell
        }

        func outlineViewItemDidExpand(_ notification: Notification) { expansionChanged(notification, open: true) }
        func outlineViewItemDidCollapse(_ notification: Notification) { expansionChanged(notification, open: false) }

        private func expansionChanged(_ notification: Notification, open: Bool) {
            guard !syncing, let item = notification.userInfo?["NSObject"] as? SidebarNode, item.kind == .folder else { return }
            parent.model.setFolderExpanded(item.id, open)
            reportHeight()
        }

        func outlineViewSelectionDidChange(_ notification: Notification) {
            guard !syncing, let outline, let item = outline.item(atRow: outline.selectedRow) as? SidebarNode,
                  item.kind == .connection, let saved = connection(item.id) else { return }
            let model = parent.model
            Task { await model.connect(saved) }
        }

        // MARK: Delegate: dragging

        func outlineView(_ outlineView: NSOutlineView, pasteboardWriterForItem item: Any) -> NSPasteboardWriting? {
            guard let item = item as? SidebarNode else { return nil }
            let pasteboardItem = NSPasteboardItem()
            pasteboardItem.setString((item.kind == .folder ? "f:" : "c:") + item.id.uuidString, forType: Self.pasteboardType)
            return pasteboardItem
        }

        private func dragged(_ info: NSDraggingInfo) -> SidebarNode? {
            guard let text = info.draggingPasteboard.string(forType: Self.pasteboardType), text.count > 2,
                  let id = UUID(uuidString: String(text.dropFirst(2))) else { return nil }
            return node(text.hasPrefix("f:") ? .folder : .connection, id)
        }

        private enum Place { case refuse, inside(SidebarNode), among(SidebarNode?, Int) }

        /// Where a dragged folder lands, from the pointer's place on the folder row under it
        /// (nil: not over a folder row, so the outline's own proposal stands).
        private func folderPlace(for moving: SidebarNode, info: NSDraggingInfo, in outline: NSOutlineView) -> Place? {
            let point = outline.convert(info.draggingLocation, from: nil)
            let row = outline.row(at: point)
            guard row >= 0, let over = outline.item(atRow: row) as? SidebarNode, over.kind == .folder else { return nil }
            // Never into itself or its own subfolders.
            if parent.model.folder(over.id, isIn: moving.id) { return .refuse }
            let rect = outline.rect(ofRow: row)
            let fraction = rect.height > 0 ? (point.y - rect.minY) / rect.height : 0.5
            let owner = outline.parent(forItem: over) as? SidebarNode
            let siblings = kids(of: owner)
            guard let at = siblings.firstIndex(of: over) else { return nil }
            if fraction < 0.25 { return .among(owner, at) }
            if fraction > 0.75 { return .among(owner, at + 1) }
            return .inside(over)
        }

        /// Where a row may land. A connection goes among a folder's connections
        /// (they come first), a folder among its subfolders; the outline is asked
        /// to show exactly that place, so the gap opens where the drop will go.
        func outlineView(_ outlineView: NSOutlineView, validateDrop info: NSDraggingInfo,
                         proposedItem item: Any?, proposedChildIndex index: Int) -> NSDragOperation {
            guard let moving = dragged(info) else { return [] }
            var target = item as? SidebarNode
            var position = index
            // A folder is placed by where the pointer is on the row under it, not by
            // where the outline guesses: the top quarter puts it in front of that
            // folder, the bottom quarter after it, the middle inside it. (The
            // outline offers "inside an open folder" for a drop on its top edge,
            // so a folder could never be put in order at its own level.)
            if moving.kind == .folder, let place = folderPlace(for: moving, info: info, in: outlineView) {
                switch place {
                case .refuse: return []
                case .inside(let over):
                    outlineView.setDropItem(over, dropChildIndex: NSOutlineViewDropOnItemIndex)
                    return .move
                case .among(let parentNode, let at):
                    target = parentNode
                    position = at
                }
            }
            // Over a connection: after it, in its folder.
            if let over = target, over.kind == .connection {
                guard let saved = connection(over.id) else { return [] }
                target = UUID(uuidString: saved.folderID).map { node(.folder, $0) }
                let siblings = kids(of: target)
                position = (siblings.firstIndex(of: over) ?? siblings.count - 1) + 1
            }
            // Never a folder into itself or its own subfolders.
            if moving.kind == .folder, let target, target.kind == .folder, parent.model.folder(target.id, isIn: moving.id) {
                return []
            }
            let siblings = kids(of: target)
            let connectionCount = siblings.filter { $0.kind == .connection }.count
            // Any negative index means "on" the item (or the empty part of the list), not a place among its rows.
            if position < 0 {
                // On a folder: into it. On the empty part: the end of the top level.
                if target != nil { outlineView.setDropItem(target, dropChildIndex: NSOutlineViewDropOnItemIndex); return .move }
                position = siblings.count
            }
            position = moving.kind == .connection ? min(position, connectionCount) : max(position, connectionCount)
            outlineView.setDropItem(target, dropChildIndex: position)
            return .move
        }

        func outlineView(_ outlineView: NSOutlineView, acceptDrop info: NSDraggingInfo, item: Any?, childIndex index: Int) -> Bool {
            guard let moving = dragged(info) else { return false }
            let target = item as? SidebarNode
            let folderID = target?.id.uuidString ?? ""
            let siblings = kids(of: target)
            let model = parent.model
            switch moving.kind {
            case .connection:
                guard let saved = connection(moving.id) else { return false }
                let own = siblings.filter { $0.kind == .connection }
                let before = index >= 0 && index < own.count ? connection(own[index].id) : nil
                model.move(saved, before: before, into: folderID, all: parent.connections)
            case .folder:
                let own = siblings.filter { $0.kind == .folder }
                let connectionCount = siblings.count - own.count
                let slot = index - connectionCount
                let before = slot >= 0 && slot < own.count ? own[slot].id : nil
                if before != moving.id { guard model.moveFolder(moving.id, into: target?.id, before: before) else { return false } }
            }
            info.animatesToDestination = true
            applyMove()
            return true
        }

        /// The row is already where it was dropped (the dragged image settles there
        /// by itself, `animatesToDestination`), so the outline only takes the new
        /// order, without animating the row from its old place a second time.
        private func applyMove() {
            guard let outline else { return }
            buildTree()
            signature = currentSignature()
            syncing = true
            NSAnimationContext.beginGrouping()
            NSAnimationContext.current.duration = 0
            outline.reloadData()
            syncExpansion()
            NSAnimationContext.endGrouping()
            syncing = false
            refreshCells()
            syncSelection()
            reportHeight()
        }

        // MARK: Menu

        /// The context menu for the empty part of the list.
        func emptyMenu() -> NSMenu {
            let menu = NSMenu()
            menu.delegate = self
            return menu
        }

        func menuNeedsUpdate(_ menu: NSMenu) {
            menu.removeAllItems()
            let item = NSMenuItem(title: String(localized: "New Folder"), action: #selector(makeFolder), keyEquivalent: "")
            item.target = self
            item.image = NSImage(systemSymbolName: "folder.badge.plus", accessibilityDescription: nil)
            menu.addItem(item)
        }

        @objc private func makeFolder() { parent.newFolder() }
    }
}

/// A row's cell: a SwiftUI view in the outline's own row.
final class HostCell: NSTableCellView {
    let host = NSHostingView(rootView: AnyView(EmptyView()))

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        host.translatesAutoresizingMaskIntoConstraints = false
        addSubview(host)
        NSLayoutConstraint.activate([
            host.leadingAnchor.constraint(equalTo: leadingAnchor),
            host.trailingAnchor.constraint(equalTo: trailingAnchor),
            host.topAnchor.constraint(equalTo: topAnchor),
            host.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }
}
