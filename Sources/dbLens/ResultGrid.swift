import AppKit
import SwiftUI
import DBLensKit

/// Any result as a grid, on AppKit's NSTableView: one column per result
/// column, NULL dimmed, text monospaced. Select rows and press Command-C to
/// copy them as tab-separated text.
/// With `onEditCell`, double-clicking a cell edits it in place: Return saves,
/// Escape cancels, a click elsewhere asks first. `onOpenRow` opens the whole row in
/// the form (for NULL and delete).
struct ResultGrid: NSViewRepresentable {
    let result: QueryResult
    /// Changes when another result arrives, so the columns rebuild and the grid scrolls to the top.
    let version: Int
    /// The number of the first row, for the row-number column.
    var startIndex = 0
    var onEditCell: ((_ row: Int, _ column: Int, _ value: String?) async throws -> Void)?
    var onOpenRow: ((Int) -> Void)?
    /// The selected rows (indexes into the result), whenever they change.
    var onSelectionChange: ((Set<Int>) -> Void)?

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let coordinator = context.coordinator
        let table = GridTableView()
        table.style = .fullWidth
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = true
        table.allowsColumnReordering = false
        table.columnAutoresizingStyle = .noColumnAutoresizing
        table.rowHeight = 22
        table.dataSource = coordinator
        table.delegate = coordinator
        table.target = coordinator
        table.doubleAction = #selector(Coordinator.doubleClicked(_:))
        table.copyHandler = { [weak coordinator] in coordinator?.copySelection(header: false) }
        let menu = NSMenu()
        menu.delegate = coordinator
        table.menu = menu
        coordinator.table = table

        let scroll = ThinScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        coordinator.rebuildColumns()
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        guard let table = coordinator.table else { return }
        if coordinator.version != version {
            coordinator.version = version
            coordinator.rebuildColumns()
            table.reloadData()
            table.scrollRowToVisible(0)
        } else if table.currentEditor() == nil {
            // Same table and page: new values in place, scroll and selection kept.
            table.reloadData()
        }
    }

    // MARK: Coordinator

    final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate, NSMenuDelegate {
        var parent: ResultGrid
        var version: Int
        weak var table: NSTableView?

        /// The cell being edited and its value before the edit.
        private var editing: (row: Int, column: Int, original: String?)?
        private var cancelled = false

        private static let rowNumberID = NSUserInterfaceItemIdentifier("#")
        private static let font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)

        init(_ parent: ResultGrid) {
            self.parent = parent
            version = parent.version
        }

        private var result: QueryResult { parent.result }

        func rebuildColumns() {
            guard let table else { return }
            for column in table.tableColumns { table.removeTableColumn(column) }
            let number = NSTableColumn(identifier: Self.rowNumberID)
            number.title = "#"
            number.width = 48
            number.minWidth = 36
            table.addTableColumn(number)
            for (index, column) in result.columns.enumerated() {
                let tableColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("\(index)"))
                tableColumn.title = column.type.isEmpty ? column.name : "\(column.name)  \(column.type)"
                tableColumn.headerToolTip = column.type.isEmpty ? column.name : "\(column.name) (\(column.type))"
                tableColumn.minWidth = 60
                tableColumn.maxWidth = 600
                tableColumn.width = idealWidth(index, title: tableColumn.title)
                table.addTableColumn(tableColumn)
            }
        }

        /// Wide enough for the header and a typical value, read from the first rows.
        private func idealWidth(_ index: Int, title: String) -> CGFloat {
            let sample = result.rows.prefix(50).map { index < $0.count ? ($0[index]?.count ?? 4) : 4 }
            let chars = max(title.count, sample.max() ?? 4)
            return CGFloat(min(max(chars, 4), 48)) * 7.5 + 24
        }

        private func value(_ row: Int, _ column: Int) -> String? {
            guard row < result.rows.count, column < result.rows[row].count else { return nil }
            return result.rows[row][column]
        }

        // MARK: Data

        func numberOfRows(in tableView: NSTableView) -> Int { result.rows.count }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard let table = notification.object as? NSTableView else { return }
            parent.onSelectionChange?(Set(table.selectedRowIndexes))
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let identifier = tableColumn?.identifier else { return nil }
            let cell = (tableView.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView) ?? makeCell(identifier)
            guard let field = cell.textField else { return cell }
            if identifier == Self.rowNumberID {
                field.stringValue = "\(parent.startIndex + row + 1)"
                field.textColor = .tertiaryLabelColor
                field.alignment = .right
            } else {
                let column = Int(identifier.rawValue) ?? 0
                show(value(row, column), in: field)
                field.toolTip = (value(row, column)?.count ?? 0) > 40 ? String(value(row, column)!.prefix(2_000)) : nil
            }
            return cell
        }

        private func show(_ value: String?, in field: NSTextField) {
            if let value {
                // Long values are cut for the grid; Copy gives the full text.
                field.stringValue = value.count > 300 ? String(value.prefix(300)) + "…" : value
                field.textColor = .labelColor
            } else {
                field.stringValue = "NULL"
                field.textColor = .tertiaryLabelColor
            }
        }

        private func makeCell(_ identifier: NSUserInterfaceItemIdentifier) -> NSTableCellView {
            let cell = NSTableCellView()
            cell.identifier = identifier
            let field = NSTextField(labelWithString: "")
            field.font = Self.font
            field.lineBreakMode = .byTruncatingTail
            field.cell?.usesSingleLineMode = true
            field.isEditable = false
            field.delegate = self
            field.translatesAutoresizingMaskIntoConstraints = false
            cell.addSubview(field)
            cell.textField = field
            NSLayoutConstraint.activate([
                field.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: Space.xs),
                field.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -Space.xs),
                field.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            return cell
        }

        // MARK: Editing

        @objc func doubleClicked(_ sender: NSTableView) {
            let row = sender.clickedRow
            let tableColumn = sender.clickedColumn
            guard parent.onEditCell != nil, row >= 0, tableColumn >= 1, editing == nil,
                  let cell = sender.view(atColumn: tableColumn, row: row, makeIfNecessary: true) as? NSTableCellView,
                  let field = cell.textField else { return }
            let column = tableColumn - 1
            let original = value(row, column)
            editing = (row, column, original)
            cancelled = false
            field.isEditable = true
            field.stringValue = original ?? ""
            field.textColor = .labelColor
            sender.window?.makeFirstResponder(field)
            field.currentEditor()?.selectAll(nil)
        }

        /// Escape cancels; Return and Tab end editing through controlTextDidEndEditing.
        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            guard selector == #selector(NSResponder.cancelOperation(_:)) else { return false }
            cancelled = true
            control.window?.makeFirstResponder(table)
            return true
        }

        func controlTextDidEndEditing(_ notification: Notification) {
            guard let field = notification.object as? NSTextField, let edit = editing else { return }
            editing = nil
            field.isEditable = false
            let text = field.stringValue
            // After Return the window holds focus itself; give it back to the grid
            // so arrows and Command-C work. A click elsewhere keeps its own focus.
            if let window = field.window, window.firstResponder === window { window.makeFirstResponder(table) }
            guard !cancelled, text != (edit.original ?? ""), let onEditCell = parent.onEditCell else {
                show(edit.original, in: field)
                return
            }
            // Return and Tab save at once. Leaving the cell any other way (a
            // click elsewhere) asks first, so a stray click never writes.
            let movement = notification.userInfo?["NSTextMovement"] as? Int
            let deliberate = [NSTextMovement.return.rawValue, NSTextMovement.tab.rawValue,
                              NSTextMovement.backtab.rawValue].contains(movement)
            if deliberate {
                save(edit, text: text, in: field, using: onEditCell)
            } else {
                confirm(edit, text: text, in: field, using: onEditCell)
            }
        }

        private func confirm(_ edit: (row: Int, column: Int, original: String?), text: String, in field: NSTextField,
                             using onEditCell: @escaping (Int, Int, String?) async throws -> Void) {
            guard let window = field.window ?? table?.window else { return }
            let name = edit.column < result.columns.count ? result.columns[edit.column].name : String(localized: "column")
            let alert = NSAlert()
            alert.messageText = String(localized: "Save the change to “\(name)”?")
            alert.informativeText = "\(Self.preview(edit.original)) → \(Self.preview(text))"
            alert.addButton(withTitle: String(localized: "Save"))
            alert.addButton(withTitle: String(localized: "Discard"))
            field.textColor = .secondaryLabelColor
            alert.beginSheetModal(for: window) { [weak self] response in
                if response == .alertFirstButtonReturn {
                    self?.save(edit, text: text, in: field, using: onEditCell)
                } else {
                    self?.show(edit.original, in: field)
                }
            }
        }

        private static func preview(_ value: String?) -> String {
            guard let value else { return "NULL" }
            return value.count > 80 ? String(value.prefix(80)) + "…" : "“\(value)”"
        }

        private func save(_ edit: (row: Int, column: Int, original: String?), text: String, in field: NSTextField,
                          using onEditCell: @escaping (Int, Int, String?) async throws -> Void) {
            field.textColor = .secondaryLabelColor
            Task { @MainActor [weak self] in
                do {
                    try await onEditCell(edit.row, edit.column, text)
                } catch {
                    // The error is in the log; put the old value back.
                    self?.show(edit.original, in: field)
                }
                field.textColor = .labelColor
            }
        }

        // MARK: Menu and copy

        func menuNeedsUpdate(_ menu: NSMenu) {
            menu.removeAllItems()
            guard let table, table.clickedRow >= 0 else { return }
            if !table.selectedRowIndexes.contains(table.clickedRow) {
                table.selectRowIndexes([table.clickedRow], byExtendingSelection: false)
            }
            if parent.onOpenRow != nil, table.selectedRowIndexes.count == 1 {
                menu.addItem(item(String(localized: "Edit in Form…"), #selector(openRow)))
                menu.addItem(.separator())
            }
            menu.addItem(item(String(localized: "Copy Rows"), #selector(copyRows)))
            menu.addItem(item(String(localized: "Copy with Header"), #selector(copyRowsWithHeader)))
        }

        private func item(_ title: String, _ action: Selector) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            return item
        }

        @objc private func openRow() {
            if let row = table?.selectedRowIndexes.first { parent.onOpenRow?(row) }
        }

        @objc private func copyRows() { copySelection(header: false) }
        @objc private func copyRowsWithHeader() { copySelection(header: true) }

        func copySelection(header: Bool) {
            guard let table, !table.selectedRowIndexes.isEmpty else { return }
            dbLens.copy(ResultGrid.tsv(result, rows: Set(table.selectedRowIndexes), header: header))
        }
    }

    // MARK: Copy

    /// Rows as tab-separated text, ready to paste into a spreadsheet. NULL is empty.
    /// `rows` nil means every row.
    static func tsv(_ result: QueryResult, rows ids: Set<Int>? = nil, header: Bool = false) -> String {
        let picked = ids.map { $0.sorted() } ?? Array(result.rows.indices)
        var lines = picked.map { result.rows[$0].map { clean($0 ?? "") }.joined(separator: "\t") }
        if header { lines.insert(result.columns.map(\.name).joined(separator: "\t"), at: 0) }
        return lines.joined(separator: "\n")
    }

    /// Tabs and line breaks inside a value would split it into extra cells.
    private static func clean(_ value: String) -> String {
        value.replacingOccurrences(of: "\t", with: " ").replacingOccurrences(of: "\n", with: " ")
    }
}

/// NSTableView with Edit > Copy (Command-C) for the selected rows.
final class GridTableView: NSTableView {
    var copyHandler: (() -> Void)?

    @objc func copy(_ sender: Any?) { copyHandler?() }

    override func validateUserInterfaceItem(_ item: any NSValidatedUserInterfaceItem) -> Bool {
        if item.action == #selector(copy(_:)) { return !selectedRowIndexes.isEmpty }
        return super.validateUserInterfaceItem(item)
    }
}
