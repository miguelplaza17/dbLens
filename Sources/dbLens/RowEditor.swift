import SwiftUI
import DBLensKit

/// Edits one row of the table page, or adds a new one. Values are typed as
/// text; Postgres converts them to each column's type and says when it can't.
/// Only changed columns are sent. In a new row, an empty field gets the
/// column's default.
struct RowEditor: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    /// The row on the page, or nil for a new row.
    let row: Int?

    private struct Field {
        var text: String
        var isNull: Bool
        let original: String?
    }

    @State private var fields: [Field] = []
    @State private var error: String?
    @State private var working = false
    @State private var confirmingDelete = false

    private var columns: [Column] { model.tableResult?.columns ?? [] }
    private var isNew: Bool { row == nil }

    /// The columns to send: changed ones when editing, filled ones when adding.
    private var values: [(String, String?)] {
        zip(columns, fields).compactMap { column, field in
            if isNew {
                if field.isNull { return (column.name, nil) }
                return field.text.isEmpty ? nil : (column.name, field.text)
            }
            let value: String? = field.isNull ? nil : field.text
            return value == field.original ? nil : (column.name, value)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    ForEach(Array(columns.enumerated()), id: \.offset) { index, column in
                        if index < fields.count { fieldRow(index, column) }
                    }
                } header: {
                    Text(isNew ? String(localized: "New row in \(model.relation?.name ?? "")")
                         : String(localized: "Row \(model.offset + (row ?? 0) + 1) of \(model.relation?.name ?? "")"))
                } footer: {
                    Text(isNew ? String(localized: "Empty fields get the column's default value.") : String(localized: "Only the changed columns are sent."))
                        .foregroundStyle(.secondary)
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

            HStack {
                if !isNew {
                    Button("Delete Row…", role: .destructive) { confirmingDelete = true }
                        .tint(.red)
                        .disabled(working)
                }
                if working { ProgressView().controlSize(.small) }
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(isNew ? String(localized: "Add") : String(localized: "Save")) { Task { await save() } }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
                    .disabled(working || (!isNew && values.isEmpty))
            }
            .padding(.horizontal, Space.xl)
            .padding(.bottom, Space.xl)
        }
        .frame(width: 600)
        .frame(minHeight: 280, idealHeight: min(CGFloat(columns.count) * 52 + 200, 640), maxHeight: 760)
        .noInitialFocus()
        .onAppear(perform: load)
        .confirmationDialog("Delete this row?", isPresented: $confirmingDelete) {
            Button("Delete", role: .destructive) { Task { await delete() } }
        } message: {
            Text("The DELETE is sent to the database right away and can't be undone here.")
        }
    }

    private func fieldRow(_ index: Int, _ column: Column) -> some View {
        LabeledContent {
            HStack(alignment: .firstTextBaseline, spacing: Space.sm) {
                TextField(column.name, text: $fields[index].text,
                          prompt: Text(fields[index].isNull ? "NULL" : (isNew ? String(localized: "default") : "")), axis: .vertical)
                    .labelsHidden()
                    .multilineTextAlignment(.leading)
                    .font(.system(.body, design: .monospaced))
                    .lineLimit(1...6)
                    .disabled(fields[index].isNull)
                    .autocorrectionDisabled()
                Toggle("NULL", isOn: $fields[index].isNull)
                    .toggleStyle(.button)
                    .controlSize(.small)
                    .help("Write NULL to this column")
            }
        } label: {
            VStack(alignment: .leading, spacing: 0) {
                Text(column.name)
                    .fontWeight(changed(index) ? .semibold : .regular)
                    .foregroundStyle(changed(index) ? Palette.accentText : .primary)
                Text(column.type)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func changed(_ index: Int) -> Bool {
        let field = fields[index]
        if isNew { return field.isNull || !field.text.isEmpty }
        return (field.isNull ? nil : field.text) != field.original
    }

    private func load() {
        let cells = row.flatMap { model.tableResult?.rows[$0] }
        fields = columns.indices.map { index in
            let value = cells.flatMap { index < $0.count ? $0[index] : nil }
            return Field(text: value ?? "", isNull: cells != nil && value == nil, original: value)
        }
    }

    private func save() async {
        working = true
        error = nil
        defer { working = false }
        do {
            try await model.saveRow(row, values: values)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func delete() async {
        guard let row else { return }
        working = true
        defer { working = false }
        do {
            try await model.deleteRow(row)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
