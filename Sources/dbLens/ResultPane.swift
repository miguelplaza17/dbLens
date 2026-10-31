import SwiftUI
import DBLensKit

/// The center grid: the picked table a page at a time, or the last query's result.
struct ResultPane: View {
    @Environment(AppModel.self) private var model
    @State private var selection = GridSelection()

    var body: some View {
        if model.shown == .schema {
            SchemaBoard()
        } else if model.shown == .table {
            tableContent
        } else if let main = model.execution?.main {
            if main.hasRows {
                grid(main, version: model.runVersion)
            } else {
                ContentUnavailableView(main.command.isEmpty ? String(localized: "Done") : main.command,
                                       systemImage: "checkmark.circle",
                                       description: Text("The command returned no rows. See the log in the SQL panel."))
            }
        } else {
            ContentUnavailableView("No Result", systemImage: "terminal",
                                   description: Text("Write a query and press Command-Return."))
        }
    }

    @ViewBuilder
    private var tableContent: some View {
        if model.relation == nil {
            ContentUnavailableView("Choose a Table", systemImage: "tablecells",
                                   description: Text("\(model.relations.count) tables and views in this database. Or run a query in the SQL panel."))
        } else if let error = model.tableError {
            ContentUnavailableView {
                Label("Couldn't Read the Table", systemImage: "exclamationmark.triangle")
            } description: {
                Text(error).textSelection(.enabled)
            }
        } else if let result = model.tableResult {
            grid(result, version: model.tableVersion)
        } else {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func grid(_ result: QueryResult, version: Int) -> some View {
        VStack(spacing: 0) {
            ResultGrid(result: result, version: version,
                       startIndex: model.shown == .table ? model.offset : 0,
                       onEditCell: model.canEditRows ? { try await model.updateCell(row: $0, column: $1, value: $2) } : nil,
                       onOpenRow: model.canEditRows ? { model.rowEdit = RowEditTarget(row: $0) } : nil,
                       onSelectionChange: { [selection] in selection.rows = $0 },
                       sort: model.shownSort,
                       onSort: { sort in Task { await model.sort(by: sort) } })
            Divider()
            footer(result)
        }
        .onChange(of: version) { selection.rows = [] }
    }

    private func footer(_ result: QueryResult) -> some View {
        HStack(spacing: Space.sm) {
            if model.shown == .table {
                ControlGroup {
                    Button("Previous", systemImage: "chevron.left") { Task { await model.previousPage() } }
                        .help("Previous page (Command-Option-[)")
                        .disabled(!model.canGoBack)
                    Button("Next", systemImage: "chevron.right") { Task { await model.nextPage() } }
                        .help("Next page (Command-Option-])")
                        .disabled(!model.canGoForward)
                }
                .fixedSize()
            }
            Text(status(result))
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .lineLimit(1)
            if model.shown == .table {
                Menu {
                    ForEach(AppModel.pageSizes, id: \.self) { size in
                        Button {
                            Task { await model.setPageSize(size) }
                        } label: {
                            if size == model.pageSize { Label(size.formatted(), systemImage: "checkmark") } else { Text(size.formatted()) }
                        }
                    }
                } label: {
                    Text("\(model.pageSize.formatted()) per page")
                }
                .menuStyle(.button)
                .fixedSize()
                .help("How many rows each page shows")
            }
            if model.tableLoading && model.shown == .table { ProgressView().controlSize(.small) }
            Spacer()
            if model.shown == .table {
                TextField("Search rows", text: Binding(get: { model.tableSearch }, set: { model.searchTable($0) }))
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 200)
                    .help("Rows with this text in any column, across every page")
            }
            if model.shown == .table, model.tableResult != nil, !model.canEditRows {
                Text("Read Only")
                    .foregroundStyle(.tertiary)
                    .help("Views and partitioned tables without a primary key can't be edited here.")
            }
            if model.canInsertRows {
                Button("New Row", systemImage: "plus") { model.rowEdit = RowEditTarget(row: nil) }
                    .help("Add a row (Command-Shift-N)")
            }
            CopyRowsButton(result: result, selection: selection)
        }
        .controlSize(.small)
        .padding(.horizontal, Space.md)
        .padding(.vertical, Space.sm)
    }

    private func status(_ result: QueryResult) -> String {
        if model.shown == .query, let execution = model.execution { return AppModel.summary(execution) }
        let searching = model.shown == .table && !model.tableSearch.isEmpty
        guard !result.rows.isEmpty else {
            return searching ? String(localized: "No rows match") : result.note ?? String(localized: "Empty table")
        }
        let first = model.offset + 1
        let last = model.offset + result.rows.count
        let total = model.tableCount.map { $0.formatted() }
            ?? (searching ? nil : model.relation.map { "~\(Int(max($0.estimate, 0)).formatted())" }) ?? "?"
        let rows = String(localized: "Rows \(first.formatted())–\(last.formatted()) of \(total)")
        return result.note.map { "\(rows) · \($0)" } ?? rows
    }
}

/// The grid's selected rows. A reference the grid writes into, so only the
/// copy button redraws when the selection changes, not the grid itself
/// (redrawing the grid mid-click dropped the selection).
@Observable
final class GridSelection {
    var rows: Set<Int> = []
}

/// Copy All, or Copy Selected while rows are selected.
private struct CopyRowsButton: View {
    let result: QueryResult
    let selection: GridSelection

    var body: some View {
        let selected = selection.rows.filter { $0 < result.rows.count }
        if selected.isEmpty {
            Button("Copy All", systemImage: "doc.on.doc") { copy(ResultGrid.tsv(result, header: true)) }
                .help("Copy all rows on screen, with the header, to paste into a spreadsheet (Command-Shift-C)")
        } else {
            Button("Copy Selected", systemImage: "doc.on.doc") { copy(ResultGrid.tsv(result, rows: selected, header: true)) }
                .help("Copy the selected rows, with the header, to paste into a spreadsheet")
        }
    }
}
