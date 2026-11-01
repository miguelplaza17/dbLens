import AppKit
import SwiftUI
import DBLensKit

// MARK: - State

/// A sticky note on the board.
struct BoardNote: Codable, Identifiable, Hashable {
    var id = UUID()
    var text = ""
    var position: CGPoint
    var color = 3
    var selectionID: String { "note:" + id.uuidString }
}

/// Everything about one connection's board: where cards and notes are, their
/// colors, which cards are folded, and the view (pan and zoom). Saved in the
/// user defaults per connection, so the board looks the same next time.
@Observable
final class BoardState {
    let connection: UUID
    var positions: [String: CGPoint] = [:]
    var colors: [String: Int] = [:]
    var collapsed: Set<String> = []
    var notes: [BoardNote] = []
    /// Screen = world × zoom + pan.
    var pan = CGPoint(x: 0, y: 0)
    var zoom: CGFloat = 1
    /// Table ids and note selection ids.
    var selection: Set<String> = []
    var editingNote: UUID?
    /// Space held down: dragging the background moves the board.
    var spaceHeld = false
    @ObservationIgnored var viewport: CGSize = .zero
    /// No saved view yet: fit everything the first time the board is shown.
    @ObservationIgnored var needsFit = true
    @ObservationIgnored private var saveTask: Task<Void, Never>?

    static let minZoom: CGFloat = 0.2
    static let maxZoom: CGFloat = 2.5

    private struct Saved: Codable {
        var positions: [String: CGPoint]
        var colors: [String: Int]
        var collapsed: [String]
        var notes: [BoardNote]
        var pan: CGPoint
        var zoom: CGFloat
    }

    private var defaultsKey: String { "board.\(connection.uuidString)" }

    init(connection: UUID) {
        self.connection = connection
        guard !Store.isOverridden, let data = UserDefaults.standard.data(forKey: defaultsKey),
              let saved = try? JSONDecoder().decode(Saved.self, from: data) else { return }
        positions = saved.positions
        colors = saved.colors
        collapsed = Set(saved.collapsed)
        notes = saved.notes
        pan = saved.pan
        zoom = saved.zoom
        needsFit = false
    }

    /// Gives every table without a place one: all of them laid out on a fresh
    /// board, new tables to the right of the existing ones.
    func place(_ tables: [SchemaGraph.Table], links: [SchemaGraph.Link]) {
        let missing = tables.filter { positions[$0.id] == nil }
        guard !missing.isEmpty else { return }
        let auto = BoardLayout.positions(tables, links: links)
        if positions.isEmpty {
            positions = auto
        } else {
            let right = positions.values.map(\.x).max().map { $0 + BoardLayout.cardWidth + BoardLayout.gapX } ?? 0
            let left = missing.compactMap { auto[$0.id]?.x }.min() ?? 0
            for table in missing { if let point = auto[table.id] { positions[table.id] = CGPoint(x: point.x - left + right, y: point.y) } }
        }
        save()
    }

    /// Lays every table out again; notes stay where they are.
    func arrange(_ tables: [SchemaGraph.Table], links: [SchemaGraph.Link]) {
        positions.merge(BoardLayout.positions(tables, links: links)) { _, new in new }
        save()
    }

    func save() {
        saveTask?.cancel()
        guard !Store.isOverridden else { return }
        let saved = Saved(positions: positions, colors: colors, collapsed: Array(collapsed), notes: notes, pan: pan, zoom: zoom)
        let key = defaultsKey
        saveTask = Task {
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled, let data = try? JSONEncoder().encode(saved) else { return }
            UserDefaults.standard.set(data, forKey: key)
        }
    }

    // Geometry

    func toWorld(_ point: CGPoint) -> CGPoint { CGPoint(x: (point.x - pan.x) / zoom, y: (point.y - pan.y) / zoom) }
    func toScreen(_ point: CGPoint) -> CGPoint { CGPoint(x: point.x * zoom + pan.x, y: point.y * zoom + pan.y) }

    /// Zooms keeping `anchor` (a screen point) still, like zooming at the pointer.
    func zoom(by factor: CGFloat, at anchor: CGPoint? = nil) {
        let anchor = anchor ?? CGPoint(x: viewport.width / 2, y: viewport.height / 2)
        let new = min(max(zoom * factor, Self.minZoom), Self.maxZoom)
        pan = CGPoint(x: anchor.x - (anchor.x - pan.x) * new / zoom, y: anchor.y - (anchor.y - pan.y) * new / zoom)
        zoom = new
        save()
    }

    /// Fits a world rectangle in the viewport, never above 100%.
    func fit(_ rect: CGRect) {
        guard viewport.width > 0, viewport.height > 0, rect.width > 0, rect.height > 0 else { return }
        let margin: CGFloat = 48
        let new = min(max(min((viewport.width - margin * 2) / rect.width, (viewport.height - margin * 2) / rect.height), Self.minZoom), 1)
        zoom = new
        pan = CGPoint(x: (viewport.width - rect.width * new) / 2 - rect.minX * new,
                      y: (viewport.height - rect.height * new) / 2 - rect.minY * new)
        save()
    }

    /// Centers the view on a world point.
    func center(on point: CGPoint) {
        pan = CGPoint(x: viewport.width / 2 - point.x * zoom, y: viewport.height / 2 - point.y * zoom)
        save()
    }
}

/// Colors for cards and notes: index 0 is "none".
enum BoardPalette {
    static let colors: [Color] = [.clear, Color(hex: 0xE5534B), Color(hex: 0xF29111), Color(hex: 0xE3B341),
                                  Color(hex: 0x4DB33D), Color(hex: 0x5B9BE0), Color(hex: 0xA371F7)]
    static let names: [LocalizedStringKey] = ["No Color", "Red", "Orange", "Yellow", "Green", "Blue", "Purple"]

    static func note(_ index: Int) -> Color { index == 0 ? Color(hex: 0x3A3A40) : colors[index].opacity(0.85) }
}

// MARK: - Board

/// The schema board as an endless canvas, like a mood board: table cards with
/// their columns and foreign keys, sticky notes, colors, and folded cards.
///
/// Mouse and trackpad: drag a card (or a selection) to move it; drag the
/// background to move the board, Shift-drag it to select; scroll or pinch to
/// zoom at the pointer; double-click a card to open its rows, the background
/// to add a note.
///
/// Cards are laid out at the zoomed size (fonts, padding, frames), not scaled
/// as a bitmap, so text stays sharp at any zoom.
/// Keyboard: arrows move the board or nudge the selection (Shift: further),
/// Command-A selects all, Escape clears, Return opens, N adds a note,
/// Delete removes selected notes, Command-= / Command-- / Command-0 zoom and fit.
struct SchemaBoard: View {
    @Environment(AppModel.self) private var model
    @FocusState private var focused: Bool
    @State private var marquee: CGRect?
    @State private var marqueeBase: Set<String> = []
    @State private var dragOrigins: [String: CGPoint] = [:]
    @State private var panStart: CGPoint?
    @State private var hovered: String?
    @State private var monitor: Any?
    @State private var frameBox = FrameBox()
    @State private var showingHelp = false

    /// The board's frame in window coordinates, read by the scroll monitor.
    final class FrameBox { var frame = CGRect.zero }

    var body: some View {
        if model.schemaLoading && model.schemaGraph == nil {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = model.schemaError {
            ContentUnavailableView {
                Label("Couldn't read the schema", systemImage: "exclamationmark.triangle")
            } description: {
                Text(verbatim: error).textSelection(.enabled)
            }
        } else if let graph = model.schemaGraph, let board = model.board, !graph.tables.isEmpty {
            canvas(graph, board: board)
        } else {
            ContentUnavailableView("No tables", systemImage: "point.3.connected.trianglepath.dotted",
                                   description: Text("This database has no tables to draw yet."))
        }
    }

    private func visible(_ graph: SchemaGraph) -> [SchemaGraph.Table] {
        guard let schema = model.boardSchema else { return graph.tables }
        return graph.tables.filter { $0.schema == schema }
    }

    // MARK: Canvas

    private func canvas(_ graph: SchemaGraph, board: BoardState) -> some View {
        let tables = visible(graph)
        let ids = Set(tables.map(\.id))
        let links = graph.links.filter { ids.contains($0.from) && ids.contains($0.to) }
        let foreignKeys = Dictionary(grouping: links, by: \.from).mapValues { Set($0.flatMap(\.fromColumns)) }
        return VStack(spacing: 0) {
            controls(graph, tables: tables, links: links, board: board)
            Divider()
            GeometryReader { geometry in
                ZStack(alignment: .topLeading) {
                    BoardBackground(pan: board.pan, zoom: board.zoom)
                    // The background takes clicks and drags that don't land on a card or note.
                    Color.clear
                        .contentShape(Rectangle())
                        .gesture(backgroundDrag(tables: tables, board: board))
                        .simultaneousGesture(SpatialTapGesture(count: 2).onEnded { tap in
                            addNote(at: board.toWorld(tap.location), board: board)
                        })
                        .simultaneousGesture(SpatialTapGesture().onEnded { _ in
                            board.editingNote = nil
                            if !NSEvent.modifierFlags.contains(.shift) { board.selection = [] }
                            focused = true
                        })
                    Canvas { context, _ in
                        for link in links { draw(link, tables: tables, foreignKeys: foreignKeys, board: board, in: &context) }
                    }
                    .allowsHitTesting(false)
                    content(tables, foreignKeys: foreignKeys, board: board)
                    if let marquee {
                        Rectangle()
                            .fill(Palette.accent.opacity(0.1))
                            .strokeBorder(Palette.accent.opacity(0.7), lineWidth: 1)
                            .frame(width: marquee.width, height: marquee.height)
                            .offset(x: marquee.minX, y: marquee.minY)  // grid: ok the marquee follows the pointer
                            .allowsHitTesting(false)
                    }
                    Minimap(tables: tables, board: board)
                        .padding(Space.md)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing)
                }
                .coordinateSpace(.named("board"))
                .clipped()
                .onAppear {
                    board.viewport = geometry.size
                    frameBox.frame = geometry.frame(in: .global)
                    if board.needsFit { board.fit(BoardLayout.bounds(tables, notes: board.notes, board: board)); board.needsFit = false }
                }
                .onChange(of: geometry.frame(in: .global)) { _, frame in
                    board.viewport = frame.size
                    frameBox.frame = frame
                }
            }
        }
        .focusable()
        .focused($focused)
        .focusEffectDisabled()
        .onKeyPress(phases: [.down, .repeat, .up]) { press in handle(press, tables: tables, board: board) }
        .onCommand(#selector(NSResponder.selectAll(_:))) {
            board.selection = Set(tables.map(\.id)).union(board.notes.map(\.selectionID))
        }
        .onAppear {
            focused = true
            installMonitor(board: board)
        }
        .onDisappear(perform: removeMonitor)
    }

    /// Cards and notes, laid out at the zoomed size and placed in screen
    /// coordinates. Cards outside the view are skipped.
    private func content(_ tables: [SchemaGraph.Table], foreignKeys: [String: Set<String>], board: BoardState) -> some View {
        let showSchema = model.boardSchema == nil && (model.schemaGraph?.schemas.count ?? 0) > 1
        let zoom = board.zoom
        let view = CGRect(origin: .zero, size: board.viewport).insetBy(dx: -Space.section, dy: -Space.section)
        let shown = tables.filter { table in
            guard let point = board.positions[table.id], board.viewport != .zero else { return true }
            let size = BoardLayout.size(table, collapsed: board.collapsed.contains(table.id))
            return view.intersects(CGRect(origin: board.toScreen(point), size: CGSize(width: size.width * zoom, height: size.height * zoom)))
        }
        return ZStack(alignment: .topLeading) {
            ForEach(shown) { table in
                let point = board.toScreen(board.positions[table.id] ?? .zero)
                TableCard(table: table, foreignKeys: foreignKeys[table.id] ?? [],
                          collapsed: board.collapsed.contains(table.id),
                          color: board.colors[table.id] ?? 0,
                          showSchema: showSchema,
                          selected: board.selection.contains(table.id),
                          highlighted: hovered == table.id,
                          zoom: zoom)
                    .offset(x: point.x, y: point.y)  // grid: ok cards sit at their board position
                    .onHover { inside in hovered = inside ? table.id : (hovered == table.id ? nil : hovered) }
                    .gesture(itemDrag(table.id, board: board))
                    .simultaneousGesture(TapGesture(count: 2).onEnded { Task { await model.openFromBoard(table) } })
                    .simultaneousGesture(TapGesture().onEnded { select(table.id, board: board) })
                    .contextMenu { cardMenu(table, board: board) }
            }
            ForEach(board.notes) { note in
                let point = board.toScreen(note.position)
                NoteView(note: note, board: board, selected: board.selection.contains(note.selectionID), zoom: zoom)
                    .offset(x: point.x, y: point.y)  // grid: ok notes sit at their board position
                    .gesture(itemDrag(note.selectionID, board: board))
                    .simultaneousGesture(TapGesture(count: 2).onEnded { board.editingNote = note.id })
                    .simultaneousGesture(TapGesture().onEnded { select(note.selectionID, board: board) })
                    .contextMenu { noteMenu(note, board: board) }
            }
        }
        .frame(width: 1, height: 1, alignment: .topLeading)
    }

    // MARK: Selection and dragging

    private func select(_ id: String, board: BoardState) {
        focused = true
        if NSEvent.modifierFlags.contains(.shift) || NSEvent.modifierFlags.contains(.command) {
            if board.selection.contains(id) { board.selection.remove(id) } else { board.selection.insert(id) }
        } else {
            board.selection = [id]
        }
    }

    /// Drags a card or note, and everything selected with it. Snaps to an 8-point
    /// grid; hold Option to move freely.
    private func itemDrag(_ id: String, board: BoardState) -> some Gesture {
        DragGesture(minimumDistance: 2, coordinateSpace: .named("board"))
            .onChanged { value in
                if dragOrigins.isEmpty {
                    focused = true
                    if !board.selection.contains(id) {
                        board.selection = NSEvent.modifierFlags.contains(.shift) ? board.selection.union([id]) : [id]
                    }
                    for selected in board.selection { if let origin = position(of: selected, board: board) { dragOrigins[selected] = origin } }
                }
                let free = NSEvent.modifierFlags.contains(.option)
                for (selected, origin) in dragOrigins {
                    var point = CGPoint(x: origin.x + value.translation.width / board.zoom,
                                        y: origin.y + value.translation.height / board.zoom)
                    if !free { point = CGPoint(x: (point.x / 8).rounded() * 8, y: (point.y / 8).rounded() * 8) }
                    move(selected, to: point, board: board)
                }
            }
            .onEnded { _ in
                dragOrigins = [:]
                board.save()
            }
    }

    private func position(of id: String, board: BoardState) -> CGPoint? {
        if id.hasPrefix("note:") { return board.notes.first { $0.selectionID == id }?.position }
        return board.positions[id]
    }

    private func move(_ id: String, to point: CGPoint, board: BoardState) {
        if id.hasPrefix("note:") {
            if let index = board.notes.firstIndex(where: { $0.selectionID == id }) { board.notes[index].position = point }
        } else {
            board.positions[id] = point
        }
    }

    /// The background: drag to move the board, Shift-drag to select with a rectangle.
    private func backgroundDrag(tables: [SchemaGraph.Table], board: BoardState) -> some Gesture {
        DragGesture(minimumDistance: 2, coordinateSpace: .named("board"))
            .onChanged { value in
                focused = true
                if panStart != nil || (marquee == nil && !NSEvent.modifierFlags.contains(.shift)) {
                    if panStart == nil { NSCursor.closedHand.push() }
                    let start = panStart ?? board.pan
                    panStart = start
                    board.pan = CGPoint(x: start.x + value.translation.width, y: start.y + value.translation.height)
                    return
                }
                if marquee == nil { marqueeBase = board.selection }
                let rect = CGRect(x: min(value.startLocation.x, value.location.x), y: min(value.startLocation.y, value.location.y),
                                  width: abs(value.location.x - value.startLocation.x), height: abs(value.location.y - value.startLocation.y))
                marquee = rect
                let topLeft = board.toWorld(rect.origin)
                let world = CGRect(x: topLeft.x, y: topLeft.y, width: rect.width / board.zoom, height: rect.height / board.zoom)
                var hit = marqueeBase
                for table in tables {
                    if let point = board.positions[table.id],
                       world.intersects(CGRect(origin: point, size: BoardLayout.size(table, collapsed: board.collapsed.contains(table.id)))) {
                        hit.insert(table.id)
                    }
                }
                for note in board.notes where world.intersects(CGRect(origin: note.position, size: BoardLayout.noteSize)) {
                    hit.insert(note.selectionID)
                }
                board.selection = hit
            }
            .onEnded { _ in
                marquee = nil
                if panStart != nil {
                    NSCursor.pop()
                    board.save()
                }
                panStart = nil
            }
    }

    // MARK: Keyboard and scroll

    private func handle(_ press: KeyPress, tables: [SchemaGraph.Table], board: BoardState) -> KeyPress.Result {
        guard board.editingNote == nil else { return .ignored }
        if press.key == .space {
            board.spaceHeld = press.phase != .up
            return .handled
        }
        guard press.phase != .up else { return .ignored }
        let shift = press.modifiers.contains(.shift)
        let arrows: [KeyEquivalent: CGVector] = [.leftArrow: CGVector(dx: -1, dy: 0), .rightArrow: CGVector(dx: 1, dy: 0),
                                                 .upArrow: CGVector(dx: 0, dy: -1), .downArrow: CGVector(dx: 0, dy: 1)]
        if let direction = arrows[press.key] {
            if board.selection.isEmpty {
                let step: CGFloat = shift ? 240 : 48
                board.pan = CGPoint(x: board.pan.x - direction.dx * step, y: board.pan.y - direction.dy * step)
            } else {
                let step: CGFloat = shift ? 40 : 8
                for id in board.selection {
                    if let point = position(of: id, board: board) {
                        move(id, to: CGPoint(x: point.x + direction.dx * step, y: point.y + direction.dy * step), board: board)
                    }
                }
            }
            board.save()
            return .handled
        }
        switch press.key {
        case .escape:
            board.selection = []
            return .handled
        case .return:
            if board.selection.count == 1, let id = board.selection.first, let table = tables.first(where: { $0.id == id }) {
                Task { await model.openFromBoard(table) }
                return .handled
            }
            return .ignored
        case .delete, .deleteForward:
            let notes = board.selection.filter { $0.hasPrefix("note:") }
            guard !notes.isEmpty else { return .ignored }
            board.notes.removeAll { notes.contains($0.selectionID) }
            board.selection.subtract(notes)
            board.save()
            return .handled
        default:
            break
        }
        if press.characters.lowercased() == "n", press.modifiers.isEmpty {
            addNote(at: board.toWorld(CGPoint(x: board.viewport.width / 2, y: board.viewport.height / 2)), board: board)
            return .handled
        }
        return .ignored
    }

    /// Scroll and pinch zoom at the pointer. A local monitor, because SwiftUI
    /// has no scroll-wheel events.
    private func installMonitor(board: BoardState) {
        removeMonitor()
        let box = frameBox
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .magnify]) { event in
            guard let content = event.window?.contentView else { return event }
            let frame = box.frame
            let point = CGPoint(x: event.locationInWindow.x - frame.minX,
                                y: content.bounds.height - event.locationInWindow.y - frame.minY)
            guard point.x >= 0, point.y >= 0, point.x <= frame.width, point.y <= frame.height else { return event }
            if event.type == .magnify {
                board.zoom(by: 1 + event.magnification, at: point)
            } else {
                let delta = event.scrollingDeltaY * (event.hasPreciseScrollingDeltas ? 0.01 : 0.08)
                guard delta != 0 else { return nil }
                board.zoom(by: exp(delta), at: point)
            }
            board.save()
            return nil
        }
    }

    private func removeMonitor() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
    }

    // MARK: Notes and menus

    private func addNote(at point: CGPoint, board: BoardState) {
        let note = BoardNote(position: CGPoint(x: (point.x / 8).rounded() * 8, y: (point.y / 8).rounded() * 8))
        board.notes.append(note)
        board.selection = [note.selectionID]
        board.editingNote = note.id
        board.save()
    }

    @ViewBuilder
    private func cardMenu(_ table: SchemaGraph.Table, board: BoardState) -> some View {
        Button("Open Rows", systemImage: "tablecells") { Task { await model.openFromBoard(table) } }
        Button(board.collapsed.contains(table.id) ? LocalizedStringKey("Show All Columns") : LocalizedStringKey("Key Columns Only"),
               systemImage: "rectangle.compress.vertical") {
            if board.collapsed.contains(table.id) { board.collapsed.remove(table.id) } else { board.collapsed.insert(table.id) }
            board.save()
        }
        colorMenu(for: board.selection.contains(table.id) ? board.selection : [table.id], board: board)
    }

    @ViewBuilder
    private func noteMenu(_ note: BoardNote, board: BoardState) -> some View {
        Button("Edit Note", systemImage: "pencil") { board.editingNote = note.id }
        colorMenu(for: [note.selectionID], board: board)
        Divider()
        Button("Delete Note", systemImage: "trash", role: .destructive) {
            board.notes.removeAll { $0.id == note.id }
            board.save()
        }
    }

    private func colorMenu(for ids: Set<String>, board: BoardState) -> some View {
        Menu("Color") {
            ForEach(BoardPalette.colors.indices, id: \.self) { index in
                Button(BoardPalette.names[index]) {
                    for id in ids {
                        if id.hasPrefix("note:") {
                            if let at = board.notes.firstIndex(where: { $0.selectionID == id }) { board.notes[at].color = index }
                        } else {
                            board.colors[id] = index == 0 ? nil : index
                        }
                    }
                    board.save()
                }
            }
        }
    }

    // MARK: Controls

    private func controls(_ graph: SchemaGraph, tables: [SchemaGraph.Table], links: [SchemaGraph.Link], board: BoardState) -> some View {
        @Bindable var model = model
        let allCollapsed = !tables.isEmpty && tables.allSatisfy { board.collapsed.contains($0.id) }
        return HStack(spacing: Space.sm) {
            if graph.schemas.count > 1 {
                Picker("Schema", selection: $model.boardSchema) {
                    Text("All Schemas").tag(String?.none)
                    ForEach(graph.schemas, id: \.self) { Text(verbatim: $0).tag(String?.some($0)) }
                }
                .fixedSize()
            }
            Text(verbatim: [tables.count == 1 ? String(localized: "1 table") : String(localized: "\(tables.count) tables"),
                            links.count == 1 ? String(localized: "1 relation") : String(localized: "\(links.count) relations")]
                .joined(separator: " · "))
                .foregroundStyle(.secondary)
                .monospacedDigit()
            if model.schemaLoading { ProgressView().controlSize(.small) }
            Spacer()
            Button("Add Note", systemImage: "note.text.badge.plus") {
                addNote(at: board.toWorld(CGPoint(x: board.viewport.width / 2, y: board.viewport.height / 2)), board: board)
            }
            .help("Add a sticky note (N, or double-click the board)")
            Button(allCollapsed ? LocalizedStringKey("Show All Columns") : LocalizedStringKey("Key Columns Only"),
                   systemImage: allCollapsed ? "rectangle.expand.vertical" : "rectangle.compress.vertical") {
                withAnimation(.snappy) {
                    if allCollapsed { board.collapsed.subtract(tables.map(\.id)) } else { board.collapsed.formUnion(tables.map(\.id)) }
                }
                board.save()
            }
            Button("Arrange", systemImage: "rectangle.3.group") {
                withAnimation(.snappy) {
                    board.arrange(tables, links: links)
                    board.fit(BoardLayout.bounds(tables, notes: board.notes, board: board))
                }
            }
            .help("Lay the cards out again automatically")
            ControlGroup {
                Button("Zoom Out", systemImage: "minus.magnifyingglass") { withAnimation(.snappy) { board.zoom(by: 1 / 1.25) } }
                    .help("Zoom out (Command-Minus)")
                Button("Fit", systemImage: "arrow.up.left.and.down.right.magnifyingglass") {
                    withAnimation(.snappy) { board.fit(BoardLayout.bounds(tables, notes: board.notes, board: board)) }
                }
                .help("Fit every table in view (Command-0)")
                Button("Zoom In", systemImage: "plus.magnifyingglass") { withAnimation(.snappy) { board.zoom(by: 1.25) } }
                    .help("Zoom in (Command-Plus)")
            }
            .fixedSize()
            Text(verbatim: "\(Int((board.zoom * 100).rounded()))%")
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(minWidth: Space.section + Space.sm, alignment: .trailing)
            Button("Shortcuts", systemImage: "keyboard") { showingHelp.toggle() }
                .labelStyle(.iconOnly)
                .help("Mouse and keyboard shortcuts for the board")
                .popover(isPresented: $showingHelp, arrowEdge: .bottom) { BoardHelp() }
        }
        .controlSize(.small)
        .padding(.horizontal, Space.md)
        .padding(.vertical, Space.sm)
    }

    // MARK: Edges

    /// A foreign key as a curve, in screen space, from the source column's side to the target column's side.
    private func draw(_ link: SchemaGraph.Link, tables: [SchemaGraph.Table], foreignKeys: [String: Set<String>],
                      board: BoardState, in context: inout GraphicsContext) {
        guard let source = tables.first(where: { $0.id == link.from }), let target = tables.first(where: { $0.id == link.to }),
              let from = board.positions[source.id], let to = board.positions[target.id] else { return }
        let width = BoardLayout.cardWidth
        func rowY(_ table: SchemaGraph.Table, _ origin: CGPoint, _ column: String?) -> CGFloat {
            let rows = BoardLayout.rows(table, collapsed: board.collapsed.contains(table.id), foreignKeys: foreignKeys[table.id] ?? [])
            guard let column, let index = rows.firstIndex(where: { $0.name == column }) else { return origin.y + BoardLayout.header / 2 }
            return origin.y + BoardLayout.header + CGFloat(index) * BoardLayout.row + BoardLayout.row / 2
        }
        let y1 = rowY(source, from, link.fromColumns.first), y2 = rowY(target, to, link.toColumns.first)
        var start: CGPoint, end: CGPoint, bend: CGFloat
        if to.x > from.x + width {
            start = CGPoint(x: from.x + width, y: y1); end = CGPoint(x: to.x, y: y2); bend = 1
        } else if to.x + width < from.x {
            start = CGPoint(x: from.x, y: y1); end = CGPoint(x: to.x + width, y: y2); bend = -1
        } else {
            start = CGPoint(x: from.x + width, y: y1); end = CGPoint(x: to.x + width, y: y2); bend = 0
        }
        start = board.toScreen(start)
        end = board.toScreen(end)
        let reach = max(abs(end.x - start.x) / 2, 48 * board.zoom)
        var path = Path()
        path.move(to: start)
        if bend == 0 {
            let out = 72 * board.zoom
            path.addCurve(to: end, control1: CGPoint(x: start.x + out, y: start.y), control2: CGPoint(x: end.x + out, y: end.y))
        } else {
            path.addCurve(to: end, control1: CGPoint(x: start.x + reach * bend, y: start.y), control2: CGPoint(x: end.x - reach * bend, y: end.y))
        }
        let lit = [source.id, target.id].contains { hovered == $0 || board.selection.contains($0) }
        let color: Color = lit ? Palette.accent : Color.secondary.opacity(0.5)
        context.stroke(path, with: .color(color), style: StrokeStyle(lineWidth: lit ? 2 : 1.25, lineCap: .round, dash: [4, 4]))
        for point in [start, end] {
            context.fill(Path(ellipseIn: CGRect(x: point.x - 3, y: point.y - 3, width: 6, height: 6)), with: .color(color))  // grid: ok edge end dots, drawn in points
        }
    }
}

// MARK: - Cards and notes

/// A table on the board: its name, then its columns (or only the key ones, folded).
private struct TableCard: View {
    let table: SchemaGraph.Table
    let foreignKeys: Set<String>
    let collapsed: Bool
    let color: Int
    let showSchema: Bool
    let selected: Bool
    let highlighted: Bool
    /// Every size is multiplied by the zoom, so the card is drawn sharp at any scale.
    let zoom: CGFloat

    var body: some View {
        let z = zoom
        let shape = RoundedRectangle(cornerRadius: 10 * z, style: .continuous)  // grid: ok card corners match the sidebar's 10 pt rows
        let rows = BoardLayout.rows(table, collapsed: collapsed, foreignKeys: foreignKeys)
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: Space.sm * z) {
                Image(systemName: table.isView ? "eye" : "tablecells")
                    .font(.system(size: 13 * z))
                    .foregroundStyle(color == 0 ? Palette.accentText : BoardPalette.colors[color])
                Text(verbatim: table.name).font(.system(size: 13 * z, weight: .semibold)).lineLimit(1)
                Spacer(minLength: 0)
                if showSchema {
                    Text(verbatim: table.schema).font(.system(size: 10 * z)).foregroundStyle(.tertiary).lineLimit(1)
                }
                if collapsed, rows.count < table.columns.count {
                    Text(verbatim: "+\(table.columns.count - rows.count)").font(.system(size: 10 * z)).foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, Space.md * z)
            .frame(height: BoardLayout.header * z)
            // A plain rectangle: the card's own clip rounds the top corners, the bottom stays square.
            .background { Rectangle().fill(color == 0 ? Color.primary.opacity(0.05) : BoardPalette.colors[color].opacity(0.22)) }
            Divider()
            ForEach(Array(rows.enumerated()), id: \.offset) { _, column in
                HStack(spacing: Space.sm * z) {
                    Group {
                        if column.isPrimaryKey {
                            Image(systemName: "key.fill").foregroundStyle(.yellow)
                        } else if foreignKeys.contains(column.name) {
                            Image(systemName: "link").foregroundStyle(Palette.accentText)
                        } else {
                            Image(systemName: column.isNullable ? "circle.dashed" : "circle").foregroundStyle(.tertiary)
                        }
                    }
                    .font(.system(size: 9 * z))
                    .frame(width: Space.md * z)
                    Text(verbatim: column.name).font(.system(size: 12 * z, design: .monospaced)).lineLimit(1)
                    Spacer(minLength: Space.sm * z)
                    Text(verbatim: column.type).font(.system(size: 11 * z, design: .monospaced)).foregroundStyle(.secondary).lineLimit(1)
                }
                .padding(.horizontal, Space.md * z)
                .frame(height: BoardLayout.row * z)
            }
        }
        .padding(.bottom, Space.xs * z)
        .frame(width: BoardLayout.cardWidth * z, alignment: .leading)
        .background(.background.secondary, in: shape)
        .overlay(shape.strokeBorder(selected ? Palette.accent : (highlighted ? Palette.accent.opacity(0.5) : Color(nsColor: .separatorColor)),
                                    lineWidth: selected ? 2 : 1))
        .clipShape(shape)
        .shadow(color: .black.opacity(selected ? 0.45 : 0.3), radius: (selected ? 14 : 8) * min(z, 1), y: 3 * min(z, 1))
    }
}

/// A sticky note: text on a color, edited in place after a double-click.
private struct NoteView: View {
    let note: BoardNote
    let board: BoardState
    let selected: Bool
    let zoom: CGFloat
    @State private var draft = ""
    @FocusState private var editing: Bool

    var body: some View {
        let z = zoom
        let shape = RoundedRectangle(cornerRadius: 8 * z, style: .continuous)
        Group {
            if board.editingNote == note.id {
                TextEditor(text: $draft)
                    .scrollContentBackground(.hidden)
                    .font(.system(size: 14 * z, weight: .medium))
                    .focused($editing)
                    .onAppear { draft = note.text; editing = true }
                    .onChange(of: editing) { _, now in if !now { commit() } }
                    .onExitCommand { commit() }
            } else {
                Text(verbatim: note.text.isEmpty ? String(localized: "Double-click to write") : note.text)
                    .font(.system(size: 14 * z, weight: .medium))
                    .foregroundStyle(note.text.isEmpty ? .secondary : .primary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
        .padding(Space.md * z)
        .frame(width: BoardLayout.noteSize.width * z, height: BoardLayout.noteSize.height * z)
        .background(BoardPalette.note(note.color), in: shape)
        .overlay(shape.strokeBorder(selected ? Palette.accent : .clear, lineWidth: 2))
        .shadow(color: .black.opacity(0.35), radius: 8 * min(z, 1), y: 3 * min(z, 1))
    }

    private func commit() {
        if let index = board.notes.firstIndex(where: { $0.id == note.id }) { board.notes[index].text = draft }
        if board.editingNote == note.id { board.editingNote = nil }
        board.save()
    }
}

/// The whole board in small, with the visible part outlined; click or drag to go there.
/// The map covers every card, note, and the visible part, centered and clipped.
private struct Minimap: View {
    let tables: [SchemaGraph.Table]
    let board: BoardState
    private let size = CGSize(width: 180, height: 120)
    private let inset: CGFloat = 8
    @State private var dragMapping: Mapping?

    /// World rect -> map rect, and back.
    private struct Mapping {
        let world: CGRect
        let scale: CGFloat
        let origin: CGPoint
        func map(_ rect: CGRect) -> CGRect {
            CGRect(x: origin.x + (rect.minX - world.minX) * scale, y: origin.y + (rect.minY - world.minY) * scale,
                   width: rect.width * scale, height: rect.height * scale)
        }
        func unmap(_ point: CGPoint) -> CGPoint {
            CGPoint(x: world.minX + (point.x - origin.x) / scale, y: world.minY + (point.y - origin.y) / scale)
        }
    }

    private var visibleRect: CGRect {
        let topLeft = board.toWorld(.zero)
        return CGRect(x: topLeft.x, y: topLeft.y, width: board.viewport.width / board.zoom, height: board.viewport.height / board.zoom)
    }

    private func mapping(_ world: CGRect) -> Mapping {
        let room = CGSize(width: size.width - inset * 2, height: size.height - inset * 2)
        let scale = min(room.width / max(world.width, 1), room.height / max(world.height, 1))
        let origin = CGPoint(x: (size.width - world.width * scale) / 2, y: (size.height - world.height * scale) / 2)
        return Mapping(world: world, scale: scale, origin: origin)
    }

    var body: some View {
        let content = BoardLayout.bounds(tables, notes: board.notes, board: board).insetBy(dx: -40, dy: -40)
        let view = visibleRect
        let mapping = mapping(content.union(view))
        Canvas { context, _ in
            for table in tables {
                guard let point = board.positions[table.id] else { continue }
                let rect = mapping.map(CGRect(origin: point, size: BoardLayout.size(table, collapsed: board.collapsed.contains(table.id))))
                let color = board.colors[table.id].map { BoardPalette.colors[$0] } ?? Color.secondary
                context.fill(Path(roundedRect: rect, cornerRadius: 2), with: .color(color.opacity(board.selection.contains(table.id) ? 0.9 : 0.45)))
            }
            for note in board.notes {
                context.fill(Path(roundedRect: mapping.map(CGRect(origin: note.position, size: BoardLayout.noteSize)), cornerRadius: 2),
                             with: .color(BoardPalette.note(note.color)))
            }
            let frame = Path(roundedRect: mapping.map(view), cornerRadius: 4)
            context.fill(frame, with: .color(Palette.accent.opacity(0.12)))
            context.stroke(frame, with: .color(Palette.accent), lineWidth: 1.5)  // grid: ok minimap outline
        }
        .frame(width: size.width, height: size.height)
        .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Color(nsColor: .separatorColor)))
        .contentShape(Rectangle())
        // The mapping is fixed for the whole drag, so the map doesn't shift under the pointer.
        .gesture(DragGesture(minimumDistance: 0)
            .onChanged { value in
                let fixed = dragMapping ?? mapping
                dragMapping = fixed
                board.center(on: fixed.unmap(value.location))
            }
            .onEnded { _ in
                dragMapping = nil
                board.save()
            })
        .help(Text("Click or drag to move around the board"))
    }
}

/// The board's mouse and keyboard controls, from the keyboard button.
private struct BoardHelp: View {
    private let items: [(LocalizedStringKey, LocalizedStringKey)] = [
        ("Drag a card", "Move it (and the rest of the selection)"),
        ("Drag the background", "Move around the board"),
        ("Shift + drag the background", "Select with a rectangle"),
        ("Scroll, or pinch", "Zoom at the pointer"),
        ("Shift-click", "Add to the selection"),
        ("Double-click a card", "Open its rows"),
        ("Double-click the board, or N", "Add a note"),
        ("Arrow keys", "Move the board, or nudge the selection (Shift: further)"),
        ("⌘A · Esc", "Select all · Clear the selection"),
        ("Return", "Open the selected table"),
        ("Delete", "Remove the selected notes"),
        ("⌘= · ⌘- · ⌘0", "Zoom in · Zoom out · Fit"),
        ("Option while dragging", "Don't snap to the grid"),
    ]

    var body: some View {
        Grid(alignment: .leading, horizontalSpacing: Space.lg, verticalSpacing: Space.sm) {
            ForEach(items.indices, id: \.self) { index in
                GridRow {
                    Text(items[index].0).fontWeight(.medium)
                    Text(items[index].1).foregroundStyle(.secondary)
                }
            }
        }
        .font(.callout)
        .padding(Space.lg)
    }
}

/// A faint dot grid that moves and scales with the board.
private struct BoardBackground: View {
    let pan: CGPoint
    let zoom: CGFloat

    var body: some View {
        Canvas { context, size in
            var step: CGFloat = 24 * zoom
            while step < 12 { step *= 2 }
            let dot = max(1, 1.5 * min(zoom, 1))
            var path = Path()
            var x = pan.x.truncatingRemainder(dividingBy: step)
            if x < 0 { x += step }
            while x <= size.width {
                var y = pan.y.truncatingRemainder(dividingBy: step)
                if y < 0 { y += step }
                while y <= size.height {
                    path.addEllipse(in: CGRect(x: x, y: y, width: dot, height: dot))
                    y += step
                }
                x += step
            }
            context.fill(path, with: .color(.secondary.opacity(0.2)))
        }
        .allowsHitTesting(false)
    }
}

// MARK: - Layout

/// Where cards go when nobody placed them: tables others point at on the left,
/// the tables pointing at them to the right, unrelated tables in a block after.
enum BoardLayout {
    static let cardWidth: CGFloat = 248
    static let header: CGFloat = 40
    static let row: CGFloat = 24
    static let gapX: CGFloat = 192
    static let gapY: CGFloat = 64
    static let margin: CGFloat = 40
    static let noteSize = CGSize(width: 200, height: 140)

    /// The columns a card shows: all, or only primary and foreign keys when folded.
    static func rows(_ table: SchemaGraph.Table, collapsed: Bool, foreignKeys: Set<String>) -> [SchemaGraph.Column] {
        guard collapsed else { return table.columns }
        return table.columns.filter { $0.isPrimaryKey || foreignKeys.contains($0.name) }
    }

    static func size(_ table: SchemaGraph.Table, collapsed: Bool) -> CGSize {
        // Folded height counts primary keys; close enough for selection and the minimap.
        let count = collapsed ? table.columns.filter(\.isPrimaryKey).count : table.columns.count
        return CGSize(width: cardWidth, height: header + CGFloat(count) * row + Space.xs + 1)
    }

    static func positions(_ tables: [SchemaGraph.Table], links: [SchemaGraph.Link]) -> [String: CGPoint] {
        let linked = Set(links.flatMap { [$0.from, $0.to] })
        let parents = Dictionary(grouping: links.filter { $0.from != $0.to }, by: \.from).mapValues { $0.map(\.to) }
        var rank: [String: Int] = [:]
        func rankOf(_ id: String, _ visiting: Set<String>) -> Int {
            if let known = rank[id] { return known }
            guard !visiting.contains(id) else { return 0 }
            let value = (parents[id] ?? []).map { rankOf($0, visiting.union([id])) + 1 }.max() ?? 0
            rank[id] = value
            return value
        }
        var columns: [Int: [SchemaGraph.Table]] = [:]
        for table in tables where linked.contains(table.id) { columns[rankOf(table.id, []), default: []].append(table) }
        let degree = Dictionary(links.flatMap { [($0.from, 1), ($0.to, 1)] }, uniquingKeysWith: +)
        var result: [String: CGPoint] = [:]
        var tallest: CGFloat = 0
        let ranks = columns.keys.sorted()
        for (index, rankValue) in ranks.enumerated() {
            var y = margin
            for table in columns[rankValue, default: []].sorted(by: { (degree[$0.id] ?? 0, $1.name) > (degree[$1.id] ?? 0, $0.name) }) {
                result[table.id] = CGPoint(x: margin + CGFloat(index) * (cardWidth + gapX), y: y)
                y += size(table, collapsed: false).height + gapY
            }
            tallest = max(tallest, y)
        }
        let loose = tables.filter { !linked.contains($0.id) }.sorted { $0.name < $1.name }
        let limit = max(tallest, 900)
        var x = margin + CGFloat(ranks.count) * (cardWidth + gapX)
        var y = margin
        for table in loose {
            let height = size(table, collapsed: false).height
            if y + height > limit, y > margin {
                x += cardWidth + gapX
                y = margin
            }
            result[table.id] = CGPoint(x: x, y: y)
            y += height + gapY
        }
        return result
    }

    /// The world rectangle holding every card and note.
    static func bounds(_ tables: [SchemaGraph.Table], notes: [BoardNote], board: BoardState) -> CGRect {
        var rect: CGRect?
        for table in tables {
            guard let point = board.positions[table.id] else { continue }
            let frame = CGRect(origin: point, size: size(table, collapsed: board.collapsed.contains(table.id)))
            rect = rect.map { $0.union(frame) } ?? frame
        }
        for note in notes {
            let frame = CGRect(origin: note.position, size: noteSize)
            rect = rect.map { $0.union(frame) } ?? frame
        }
        return rect ?? CGRect(x: 0, y: 0, width: 400, height: 300)
    }
}
