import AppKit
import SwiftUI
import DBLensKit

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
/// The rows are the system's own table cells and context menus (not SwiftUI views
/// in hosting views, which drew a moment after the row animation had started).
struct ConnectionsOutline: NSViewRepresentable {
    let model: AppModel
    let connections: [SavedConnection]
    /// What the user is naming right now, so the row shows its field.
    let editingFolder: UUID?
    /// The sidebar's side of the folder and connection menus.
    let startNaming: (UUID) -> Void
    let newFolder: (UUID?) -> Void
    /// A folder's name was typed (nil: Escape).
    let rename: (UUID, String?) -> Void
    let askDelete: (SavedConnection) -> Void
    /// The most height the sidebar gives the outline; past it the rows scroll.
    let maxHeight: CGFloat
    /// The height all the rows need, reported back so the sidebar can size the outline.
    @Binding var contentHeight: CGFloat
    /// The height the rows seem to take: while a folder opens or closes it follows
    /// them, and whatever sits under the outline moves with it.
    @Binding var shownHeight: CGFloat

    /// Points per row (the system's sidebar rows).
    static let rowHeight: CGFloat = 28
    /// Room above and below the rows.
    static let inset: CGFloat = Space.xs

    /// The frame for rows of this height: a little room under them, to drop after
    /// the last one (or out of a folder, to the end).
    static func frameHeight(rows: CGFloat, maxHeight: CGFloat) -> CGFloat {
        max(min(rows + inset * 2 + Space.xxl, maxHeight), rowHeight)
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let outline = SidebarOutlineView()
        let column = NSTableColumn(identifier: .init("connections"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .sourceList
        outline.rowSizeStyle = .default
        outline.intercellSpacing = NSSize(width: 0, height: 2)
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
        outline.menuFor = { [weak coordinator = context.coordinator] row in coordinator?.menu(forRow: row) }
        outline.frameNeeded = { [weak coordinator = context.coordinator] item in
            (item as? SidebarNode).flatMap { coordinator?.frameNeeded(opening: $0) } ?? 0
        }
        outline.rowsSliding = { [weak coordinator = context.coordinator] bottom in coordinator?.slide(to: bottom) }
        outline.rowsSlid = { [weak coordinator = context.coordinator] in coordinator?.reportHeight() }

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
    final class Coordinator: NSObject, NSOutlineViewDataSource, NSOutlineViewDelegate, NSMenuDelegate, NSTextFieldDelegate {
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
                        let engine = Engine(rawValue: c?.engine ?? "") ?? .postgres
                        let look = EngineIconStore.shared.style(engine).rawValue + String(describing: NSColor(engine.color).hashValue)
                        return "c\(node.id)\(c?.name ?? "")\(c?.engine ?? "")\(c?.sshHost ?? "")\(look)"
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
        func reportHeight() {
            guard let outline else { return }
            let rows = outline.numberOfRows
            let height = rows > 0 ? outline.rect(ofRow: rows - 1).maxY : 0
            setHeight(height)
            // Mid-slide, `slide(to:)` has this in hand.
            guard !((outline as? SidebarOutlineView)?.isSliding ?? false) else { return }
            let shown = parent.$shownHeight
            if abs(shown.wrappedValue - height) > 0.5 {
                DispatchQueue.main.async { shown.wrappedValue = height }
            }
        }

        /// The rows have started to move: what's under the outline goes with them, on their curve.
        func slide(to bottom: CGFloat) {
            withAnimation(.timingCurve(SidebarOutlineView.slideCurve.0, SidebarOutlineView.slideCurve.1,
                                       SidebarOutlineView.slideCurve.2, SidebarOutlineView.slideCurve.3,
                                       duration: SidebarOutlineView.slideDuration)) {
                parent.shownHeight = bottom
            }
        }

        private func setHeight(_ height: CGFloat) {
            let binding = parent.$contentHeight
            if abs(binding.wrappedValue - height) > 0.5 {
                DispatchQueue.main.async { binding.wrappedValue = height }
            }
        }

        /// The rows an open folder adds: its children, and the open folders among them.
        private func visibleRows(inside item: SidebarNode) -> Int {
            (tree[item.id.uuidString] ?? []).reduce(0) { sum, child in
                sum + 1 + (child.kind == .folder && (folder(child.id)?.expanded ?? false) ? visibleRows(inside: child) : 0)
            }
        }

        /// The frame a folder needs to open into, asked for before it opens: the
        /// outline only makes the rows its frame shows.
        func frameNeeded(opening item: SidebarNode) -> CGFloat {
            guard let outline else { return 0 }
            let rows = outline.numberOfRows
            let pitch = rows > 1 ? outline.rect(ofRow: 1).minY - outline.rect(ofRow: 0).minY : outline.rowHeight + outline.intercellSpacing.height
            let current = rows > 0 ? outline.rect(ofRow: rows - 1).maxY : 0
            let target = current + CGFloat(visibleRows(inside: item)) * pitch
            setHeight(target)
            return ConnectionsOutline.frameHeight(rows: target, maxHeight: parent.maxHeight)
        }

        /// Open and close folders as the model says (a new subfolder, a drop that opened one).
        private func syncExpansion() {
            guard let outline else { return }
            let sidebar = outline as? SidebarOutlineView
            sidebar?.animatesDisclosure = false
            defer { sidebar?.animatesDisclosure = !dragging }
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
                      let cell = outline.view(atColumn: 0, row: row, makeIfNecessary: false) as? SidebarCell else { continue }
                configure(cell, for: item)
            }
            syncEditing()
        }

        /// What a row shows: the kind of database's icon and color, the name, the open dot.
        private func configure(_ cell: SidebarCell, for item: SidebarNode) {
            let model = parent.model
            switch item.kind {
            case .connection:
                guard let saved = connection(item.id) else { return }
                let engine = Engine(rawValue: saved.engine) ?? .postgres
                let active = saved.uuid == model.activeID && model.session != nil
                let image: NSImage?
                let tint = NSColor(engine.color)
                if !saved.sshHost.isEmpty {
                    image = NSImage(systemSymbolName: "lock.shield", accessibilityDescription: nil)
                } else if EngineIconStore.shared.style(engine) == .logo, let logo = EngineLogos.image(engine) {
                    image = logo.copy() as? NSImage
                    image?.isTemplate = true
                } else {
                    image = NSImage(systemSymbolName: engine.symbol, accessibilityDescription: nil)
                }
                cell.show(name: saved.name, bold: active, image: image, tint: tint, spinning: model.connectingID == saved.uuid,
                          dot: active, tooltip: saved.ssh.map { String(localized: "Via SSH \($0.label)") } ?? "")
            case .folder:
                guard let folder = folder(item.id) else { return }
                let image = NSImage(systemSymbolName: "folder", accessibilityDescription: nil)
                cell.show(name: folder.name, bold: false, image: image, tint: NSColor(Palette.accent), spinning: false, dot: false, tooltip: "")
            }
        }

        // MARK: Naming a folder in place

        private var editingNode: UUID?
        private var editCancelled = false

        /// The folder being named gets its text field, selected, right in the row.
        private func syncEditing() {
            guard let outline else { return }
            guard let id = parent.editingFolder else { editingNode = nil; return }
            guard editingNode != id else { return }
            let row = outline.row(forItem: node(.folder, id))
            guard row >= 0, let cell = outline.view(atColumn: 0, row: row, makeIfNecessary: true) as? SidebarCell else { return }
            editingNode = id
            editCancelled = false
            DispatchQueue.main.async { cell.beginEditing(delegate: self) }
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy commandSelector: Selector) -> Bool {
            guard commandSelector == #selector(NSResponder.cancelOperation(_:)) else { return false }
            editCancelled = true
            control.window?.makeFirstResponder(outline)
            return true
        }

        func controlTextDidEndEditing(_ notification: Notification) {
            guard let field = notification.object as? NSTextField, let id = editingNode else { return }
            editingNode = nil
            field.isEditable = false
            parent.rename(id, editCancelled ? nil : field.stringValue)
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
            let cell = outlineView.makeView(withIdentifier: id, owner: nil) as? SidebarCell ?? SidebarCell(identifier: id)
            configure(cell, for: item)
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

        /// During a drag the outline's own gap is animating: folders open plainly.
        private var dragging = false

        func outlineView(_ outlineView: NSOutlineView, draggingSession session: NSDraggingSession, willBeginAt screenPoint: NSPoint, forItems draggedItems: [Any]) {
            dragging = true
            (outlineView as? SidebarOutlineView)?.animatesDisclosure = false
        }

        func outlineView(_ outlineView: NSOutlineView, draggingSession session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
            dragging = false
            (outlineView as? SidebarOutlineView)?.animatesDisclosure = true
        }

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

        @objc private func makeFolder() { parent.newFolder(nil) }
    }
}

/// A row of the outline: icon, name, and the dot of the open connection. The
/// system's own table cell, so it draws with the row and animates with it.
final class SidebarCell: NSTableCellView {
    private let icon = NSImageView()
    private let label = NSTextField(labelWithString: "")
    private let dot = NSImageView()
    private let spinner = NSProgressIndicator()

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        imageView = icon
        textField = label
        icon.imageScaling = .scaleProportionallyDown
        label.lineBreakMode = .byTruncatingTail
        label.cell?.truncatesLastVisibleLine = true
        label.font = .systemFont(ofSize: NSFont.systemFontSize)
        label.focusRingType = .none
        dot.image = NSImage(systemSymbolName: "circle.fill", accessibilityDescription: nil)?
            .withSymbolConfiguration(.init(pointSize: 7, weight: .regular))
        dot.contentTintColor = .systemGreen
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isDisplayedWhenStopped = false
        for view in [icon, label, dot, spinner] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: Space.lg),
            icon.heightAnchor.constraint(equalToConstant: Space.lg),
            spinner.centerXAnchor.constraint(equalTo: icon.centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: icon.centerYAnchor),
            label.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: Space.sm),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.trailingAnchor.constraint(lessThanOrEqualTo: dot.leadingAnchor, constant: -Space.xs),
            dot.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Space.xs),
            dot.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }

    func show(name: String, bold: Bool, image: NSImage?, tint: NSColor, spinning: Bool, dot showsDot: Bool, tooltip: String) {
        label.stringValue = name
        label.font = .systemFont(ofSize: NSFont.systemFontSize, weight: bold ? .semibold : .regular)
        label.isEditable = false
        icon.image = spinning ? nil : image
        icon.contentTintColor = tint
        if spinning { spinner.startAnimation(nil) } else { spinner.stopAnimation(nil) }
        dot.isHidden = !showsDot
        toolTip = tooltip.isEmpty ? nil : tooltip
    }

    /// The name becomes a field, all selected; Return keeps it, Escape leaves it.
    func beginEditing(delegate: NSTextFieldDelegate) {
        label.isEditable = true
        label.isSelectable = true
        label.placeholderString = String(localized: "Folder Name")
        label.delegate = delegate
        window?.makeFirstResponder(label)
        label.selectText(nil)
    }
}

/// The outline, with a context menu per row (built by `ConnectionsOutline.Coordinator`)
/// and its own folder animation. The system's places the new rows at once, fully
/// drawn, and slides the rows below over them, which shows on a sidebar's
/// transparent rows. Here the folder opens and closes without animation, and
/// everything under it moves as one block, the new rows coming out from behind the folder.
final class SidebarOutlineView: NSOutlineView {
    var menuFor: ((Int) -> NSMenu?)?
    /// How tall the frame must be for a folder to open (the sidebar is told to grow it).
    var frameNeeded: ((Any) -> CGFloat)?
    /// Off for changes that don't come from the user, and during a drag.
    var animatesDisclosure = true
    /// A folder's rows start to move; the rows will end at this height.
    var rowsSliding: ((CGFloat) -> Void)?
    var rowsSlid: (() -> Void)?
    private var sliding = false
    var isSliding: Bool { sliding }

    static let slideDuration: CFTimeInterval = 0.25
    static let slideCurve: (Double, Double, Double, Double) = (0.25, 0.1, 0.25, 1)
    private static let slideKey = "disclosure"

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = row(at: convert(event.locationInWindow, from: nil))
        if row >= 0, let menu = menuFor?(row) { return menu }
        return super.menu(for: event)
    }

    private var slides: Bool {
        animatesDisclosure && window != nil && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }

    private func withoutAnimation(_ body: () -> Void) {
        NSAnimationContext.beginGrouping()
        NSAnimationContext.current.duration = 0
        NSAnimationContext.current.allowsImplicitAnimation = false
        body()
        NSAnimationContext.endGrouping()
    }

    override func expandItem(_ item: Any?, expandChildren: Bool) {
        guard slides, let item, !isItemExpanded(item) else { return super.expandItem(item, expandChildren: expandChildren) }
        guard !sliding else { return }
        sliding = true
        // The frame first: rows past the old frame wouldn't exist yet.
        afterFrame(reaches: frameNeeded?(item) ?? 0, tries: 15) { [self] in
            plainExpand(item, expandChildren)
            layoutSubtreeIfNeeded()
            slideContents(of: row(forItem: item), opening: true) { [self] in
                sliding = false
                rowsSlid?()
            }
        }
    }

    override func collapseItem(_ item: Any?, collapseChildren: Bool) {
        guard slides, let item, isItemExpanded(item) else { return super.collapseItem(item, collapseChildren: collapseChildren) }
        guard !sliding else { return }
        sliding = true
        // The rows leave first; only then is the folder really closed.
        slideContents(of: row(forItem: item), opening: false) { [self] in
            plainCollapse(item, collapseChildren)
            sliding = false
            rowsSlid?()
        }
    }

    private func plainExpand(_ item: Any, _ children: Bool) {
        withoutAnimation { super.expandItem(item, expandChildren: children) }
    }

    private func plainCollapse(_ item: Any, _ children: Bool) {
        withoutAnimation { super.collapseItem(item, collapseChildren: children) }
    }

    private func afterFrame(reaches height: CGFloat, tries: Int, then body: @escaping () -> Void) {
        if tries == 0 || (enclosingScrollView?.frame.height ?? height) >= height { return body() }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.004) { [self] in afterFrame(reaches: height, tries: tries - 1, then: body) }
    }

    /// Moves a folder's rows and all the rows below it as one block, between
    /// "folded behind the folder" and their places. Each of the folder's rows is
    /// masked at the folder's bottom edge, so it shows only as it comes out.
    private func slideContents(of parentRow: Int, opening: Bool, done: @escaping () -> Void) {
        guard parentRow >= 0 else { return done() }
        var last = parentRow
        while last + 1 < numberOfRows, level(forRow: last + 1) > level(forRow: parentRow) { last += 1 }
        guard last > parentRow else { return done() }
        let edge = rect(ofRow: parentRow).maxY
        let travel = rect(ofRow: last).maxY - edge
        let reach = visibleRect.maxY + travel
        rowsSliding?(rect(ofRow: numberOfRows - 1).maxY - (opening ? 0 : travel))

        func animation(_ from: CGFloat, _ to: CGFloat) -> CABasicAnimation {
            let move = CABasicAnimation(keyPath: "transform.translation.y")
            move.fromValue = from
            move.toValue = to
            move.duration = Self.slideDuration
            let curve = Self.slideCurve
            move.timingFunction = CAMediaTimingFunction(controlPoints: Float(curve.0), Float(curve.1), Float(curve.2), Float(curve.3))
            move.fillMode = .both
            move.isRemovedOnCompletion = false
            return move
        }

        var moved: [CALayer] = []
        for row in (parentRow + 1)..<numberOfRows where rect(ofRow: row).minY < reach {
            guard let layer = rowView(atRow: row, makeIfNecessary: true)?.layer else { continue }
            layer.add(animation(opening ? -travel : 0, opening ? 0 : -travel), forKey: Self.slideKey)
            moved.append(layer)
            guard row <= last else { continue }
            let frame = rect(ofRow: row)
            let hidden = edge - frame.minY  // the folder's edge, in the row's own coordinates
            let flipped = layer.contentsAreFlipped()
            let mask = CALayer()
            mask.backgroundColor = NSColor.black.cgColor
            mask.frame = CGRect(x: -frame.width, y: flipped ? hidden : 0, width: frame.width * 3, height: frame.height - hidden)
            // The row moves up by `travel`, so its mask moves down by as much: the edge stays put.
            let shift = flipped ? travel : -travel
            mask.add(animation(opening ? shift : 0, opening ? 0 : shift), forKey: Self.slideKey)
            layer.mask = mask
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.slideDuration) {
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            done()
            for layer in moved {
                layer.mask = nil
                layer.removeAnimation(forKey: Self.slideKey)
            }
            CATransaction.commit()
        }
    }
}

/// A menu item that runs a closure.
private final class MenuAction: NSObject {
    let run: () -> Void
    init(_ run: @escaping () -> Void) { self.run = run }
    @objc func fire() { run() }
}

private extension NSMenu {
    @discardableResult
    func add(_ title: String, symbol: String? = nil, checked: Bool = false, enabled: Bool = true, help: String? = nil,
             action: @escaping () -> Void) -> NSMenuItem {
        let target = MenuAction(action)
        let item = NSMenuItem(title: title, action: #selector(MenuAction.fire), keyEquivalent: "")
        item.target = target
        item.representedObject = target  // the menu item holds its target weakly
        item.state = checked ? .on : .off
        item.isEnabled = enabled
        item.toolTip = help
        if let symbol { item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) }
        addItem(item)
        return item
    }

    func addSection(_ title: String) { addItem(NSMenuItem.sectionHeader(title: title)) }
}

// MARK: Menus

extension ConnectionsOutline.Coordinator {
    /// The context menu of the row at `row`.
    func menu(forRow row: Int) -> NSMenu? {
        guard let outline, let item = outline.item(atRow: row) as? SidebarNode else { return nil }
        switch item.kind {
        case .connection:
            guard let saved = parent.connections.first(where: { $0.uuid == item.id }) else { return nil }
            return connectionMenu(saved)
        case .folder:
            guard let folder = parent.model.folders.first(where: { $0.id == item.id }) else { return nil }
            return folderMenu(folder)
        }
    }

    private func connectionMenu(_ saved: SavedConnection) -> NSMenu {
        let model = parent.model
        let menu = NSMenu()
        menu.add(String(localized: "Connect"), symbol: "bolt.horizontal") { Task { await model.connect(saved) } }
        menu.add(String(localized: "Edit…"), symbol: "pencil") { model.editing = saved }
        menu.add(String(localized: "Open in Terminal"), symbol: "apple.terminal") { model.openInTerminal(saved) }
        menu.add(String(localized: "Copy URL Without Password"), symbol: "doc.on.doc") {
            dbLens.copy(saved.url.map(ConnectionURL.redact) ?? "")
        }
        menu.add(String(localized: "Copy URL with Password"), help: String(localized: "Cleared from the clipboard after a minute")) {
            dbLens.copySecret(saved.url ?? "")
        }
        if saved.uuid == model.activeID {
            menu.add(String(localized: "Disconnect"), symbol: "bolt.horizontal.slash") { model.disconnect() }
        }
        // Move to
        let move = NSMenu()
        for folder in model.folders {
            move.add(model.path(of: folder.id), enabled: saved.folderID != folder.id.uuidString) {
                model.move(saved, before: nil, into: folder.id.uuidString, all: self.parent.connections)
            }
        }
        if !model.folders.isEmpty { move.addItem(.separator()) }
        move.add(String(localized: "No Folder"), enabled: !saved.folderID.isEmpty) {
            model.move(saved, before: nil, into: "", all: self.parent.connections)
        }
        move.add(String(localized: "New Folder"), symbol: "folder.badge.plus") {
            let folder = model.addFolder(named: String(localized: "New Folder"))
            model.move(saved, before: nil, into: folder.id.uuidString, all: self.parent.connections)
            self.parent.startNaming(folder.id)
        }
        let moveItem = menu.add(String(localized: "Move to"), symbol: "folder") {}
        moveItem.submenu = move
        menu.addItem(.separator())
        let tools = agentToolsMenu(saved)
        let toolsItem = menu.add(String(localized: "Agent Tools"), symbol: "sparkles") {}
        toolsItem.submenu = tools
        menu.addItem(.separator())
        menu.add(String(localized: "Delete…"), symbol: "trash") { self.parent.askDelete(saved) }
        return menu
    }

    /// What agents may do on a connection: seen or hidden, each MCP tool, and writing
    /// (the same switches as Settings > Agents).
    private func agentToolsMenu(_ saved: SavedConnection) -> NSMenu {
        let model = parent.model
        let save = { try? model.context.save() }
        let menu = NSMenu()
        menu.add(String(localized: "Visible to Agents"), checked: !saved.agentsHidden) {
            saved.agentsHidden.toggle()
            save()
        }
        menu.addItem(.separator())
        menu.addSection(String(localized: "Reading"))
        for name in MCPServer.connectionTools where !MCPServer.writingTools.contains(name) {
            menu.add(name, checked: saved.agentsMayUse(name), enabled: !saved.agentsHidden) {
                saved.setAgents(name, allowed: !saved.agentsMayUse(name))
                save()
            }
        }
        menu.addSection(String(localized: "Writing"))
        menu.add(String(localized: "Let Agents Write"), checked: saved.agentsCanWrite) {
            saved.agentsCanWrite.toggle()
            save()
        }
        for name in MCPServer.connectionTools where MCPServer.writingTools.contains(name) {
            menu.add(name, checked: saved.agentsMayUse(name), enabled: !saved.agentsHidden && saved.agentsCanWrite) {
                saved.setAgents(name, allowed: !saved.agentsMayUse(name))
                save()
            }
        }
        menu.addItem(.separator())
        menu.add(String(localized: "Allow All Tools"), enabled: !saved.agentsBlockedTools.isEmpty) {
            for name in MCPServer.connectionTools { saved.setAgents(name, allowed: true) }
            save()
        }
        return menu
    }

    private func folderMenu(_ folder: ConnectionFolder) -> NSMenu {
        let model = parent.model
        let menu = NSMenu()
        menu.add(String(localized: "Rename…"), symbol: "pencil") { self.parent.startNaming(folder.id) }
        menu.add(String(localized: "New Subfolder"), symbol: "folder.badge.plus") { self.parent.newFolder(folder.id) }
        menu.add(String(localized: "New Folder"), symbol: "folder.badge.plus") { self.parent.newFolder(nil) }
        let move = NSMenu()
        for other in model.folders where !model.folder(other.id, isIn: folder.id) {
            move.add(model.path(of: other.id), enabled: folder.parentID != other.id) {
                _ = model.moveFolder(folder.id, into: other.id)
            }
        }
        if model.folders.count > 1 { move.addItem(.separator()) }
        move.add(String(localized: "Top Level"), enabled: folder.parentID != nil) { _ = model.moveFolder(folder.id, into: nil) }
        let moveItem = menu.add(String(localized: "Move to"), symbol: "folder") {}
        moveItem.submenu = move
        menu.addItem(.separator())
        menu.add(String(localized: "Delete Folder"), symbol: "trash", help: String(localized: "Its subfolders and connections move up one level; none is deleted.")) { model.deleteFolder(folder.id) }
        return menu
    }
}
