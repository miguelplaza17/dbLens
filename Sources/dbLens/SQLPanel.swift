import AppKit
import SwiftUI
import DBLensKit

/// The panel at the bottom of the window: two cards side by side, the SQL
/// editor with its Run button and the log. Drag the gap between them to
/// resize. Command-Return runs the selection, or everything when nothing is
/// selected; the result shows in the grid above.
struct SQLPanel: View {
    @Environment(AppModel.self) private var model
    @AppStorage("sqlPanel.logWidth") private var logWidth: Double = 360
    @State private var dragStart: Double?

    var body: some View {
        @Bindable var model = model
        GeometryReader { geometry in
            let maxLog = max(220, geometry.size.width - 320 - Space.md)
            HStack(spacing: 0) {
                VStack(spacing: 0) {
                    header
                    Divider()
                    SQLEditor(text: $model.sql, selection: $model.sqlSelection, language: model.editorLanguage)
                }
                .card()
                Color.clear
                    .frame(width: Space.md)
                    .contentShape(Rectangle())
                    .pointerStyle(.columnResize)
                    .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .global)
                        .onChanged { drag in
                            let start = dragStart ?? min(logWidth, maxLog)
                            dragStart = start
                            logWidth = min(max(start - drag.translation.width, 220), maxLog)
                        }
                        .onEnded { _ in dragStart = nil })
                LogView()
                    .card()
                    .frame(width: min(max(logWidth, 220), maxLog))
            }
        }
    }

    private var header: some View {
        HStack(spacing: Space.sm) {
            // The panel's own toggle sits here while it's open, like the sidebar's;
            // folded away, it moves to the window toolbar.
            Button("Hide SQL Panel", systemImage: "rectangle.bottomthird.inset.filled") {
                withAnimation(.snappy(duration: 0.25)) { model.showSQLPanel = false }
            }
            .labelStyle(.iconOnly)
            .buttonStyle(.borderless)
            .help("Hide the SQL panel (Command-Shift-Y)")
            Text(model.editorLanguage.title)
                .fontWeight(.semibold)
            if model.running {
                ProgressView().controlSize(.small)
                Text("Running…").foregroundStyle(.secondary)
            } else {
                Text(model.sqlSelection.isEmpty ? String(localized: "⌘↩ runs everything") : String(localized: "⌘↩ runs the selection"))
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Terminal", systemImage: "apple.terminal") { model.openInTerminal() }
                .buttonStyle(.bordered)
                .help("Open the database's own client (psql, mysql, sqlite3, redis-cli, mongosh), connected (Command-Shift-T)")
                .disabled(model.session == nil)
            if model.running {
                Button("Cancel") { model.cancel() }
            } else {
                Button("Run", systemImage: "play.fill") { Task { await model.run() } }
                    .buttonStyle(.borderedProminent)
                    .disabled(model.session == nil)
                    .help("Run (Command-Return)")
            }
        }
        .controlSize(.small)
        .padding(.horizontal, Space.md)
        .padding(.vertical, Space.sm)
    }
}

/// What the last run, edit, or connection did. Each new one replaces it.
struct LogView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label("Log", systemImage: "list.bullet.rectangle")
                    .fontWeight(.semibold)
                Spacer()
                Button("Copy Log", systemImage: "doc.on.doc") { if let entry = model.lastLog { copy(entry.copyText) } }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .disabled(model.lastLog == nil)
                    .help("Copy the log (message and SQL)")
            }
            .controlSize(.small)
            .frame(minHeight: Space.xxl)
            .padding(.horizontal, Space.md)
            .padding(.vertical, Space.sm)
            Divider()
            ScrollView {
                VStack {
                    if let entry = model.lastLog {
                        LogRow(entry: entry)
                            .padding(Space.md)
                    }
                }
                .background(ThinScrollBars().frame(width: 0, height: 0))
            }
            .overlay {
                if model.lastLog == nil {
                    Text("The result of the last run appears here.")
                        .font(.callout)
                        .foregroundStyle(.tertiary)
                        .multilineTextAlignment(.center)
                        .padding(Space.lg)
                }
            }
        }
    }
}

private struct LogRow: View {
    let entry: LogEntry

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            HStack(alignment: .firstTextBaseline, spacing: Space.sm) {
                Text(entry.date, format: .dateTime.hour().minute().second())
                    .foregroundStyle(.tertiary)
                Image(systemName: entry.isError ? "xmark.circle.fill" : "checkmark.circle.fill")
                    .foregroundStyle(entry.isError ? .red : .green)
                    .imageScale(.small)
                Text(entry.message)
                    .foregroundStyle(entry.isError ? .red : .primary)
                    .textSelection(.enabled)
            }
            if !entry.sql.isEmpty {
                Text(entry.sql)
                    .foregroundStyle(.secondary)
                    .lineLimit(8)
                    .textSelection(.enabled)
            }
        }
        .font(.system(.caption, design: .monospaced))
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
