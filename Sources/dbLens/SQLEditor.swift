import AppKit
import SwiftUI

/// A small code editor for SQL: line numbers, syntax colors, the current line
/// lit, monospaced text, and none of the smart quotes or dashes that break SQL.
struct SQLEditor: NSViewRepresentable {
    @Binding var text: String
    @Binding var selection: String
    /// SQL, Redis commands, or mongosh queries: decides the colors.
    var language = EditorLanguage.sql
    /// Tables and columns the editor suggests while you type.
    var names = EditorNames()

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = ThinScrollView()
        scroll.hasVerticalScroller = true
        let size = scroll.contentSize
        let view = NSTextView(frame: NSRect(origin: .zero, size: size))
        view.minSize = NSSize(width: 0, height: size.height)
        view.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: .greatestFiniteMagnitude)
        view.isVerticallyResizable = true
        view.isHorizontallyResizable = false
        view.autoresizingMask = [.width]
        view.textContainer?.containerSize = NSSize(width: size.width, height: .greatestFiniteMagnitude)
        view.textContainer?.widthTracksTextView = true
        scroll.documentView = view
        view.delegate = context.coordinator
        view.isRichText = false
        view.allowsUndo = true
        view.isAutomaticQuoteSubstitutionEnabled = false
        view.isAutomaticDashSubstitutionEnabled = false
        view.isAutomaticTextReplacementEnabled = false
        view.isAutomaticSpellingCorrectionEnabled = false
        view.isContinuousSpellCheckingEnabled = false
        view.isGrammarCheckingEnabled = false
        view.smartInsertDeleteEnabled = false
        view.font = CodeTheme.font
        view.textColor = CodeTheme.plain
        view.backgroundColor = CodeTheme.background
        view.insertionPointColor = CodeTheme.caret
        view.selectedTextAttributes = [.backgroundColor: CodeTheme.selection]
        view.typingAttributes = [.font: CodeTheme.font, .foregroundColor: CodeTheme.plain]
        view.textContainerInset = NSSize(width: Space.xs, height: Space.sm)
        view.setAccessibilityLabel(String(localized: "SQL Editor"))
        view.string = text
        scroll.drawsBackground = true
        scroll.backgroundColor = CodeTheme.background

        let ruler = LineNumberRuler(textView: view)
        scroll.verticalRulerView = ruler
        scroll.hasVerticalRuler = true
        scroll.rulersVisible = true
        SQLHighlighter.apply(to: view, language: language)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        let modeChanged = context.coordinator.parent.language != language
        context.coordinator.parent = self
        guard let view = scroll.documentView as? NSTextView else { return }
        if modeChanged { SQLHighlighter.apply(to: view, language: language) }
        guard view.string != text else { return }
        view.string = text
        view.setSelectedRange(NSRange(location: (text as NSString).length, length: 0))
        SQLHighlighter.apply(to: view, language: language)
        scroll.verticalRulerView?.needsDisplay = true
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var parent: SQLEditor
        /// The last edit was one typed letter (or a dot): offer completions after it.
        private var typed = false
        init(_ parent: SQLEditor) { self.parent = parent }

        func textView(_ textView: NSTextView, shouldChangeTextIn range: NSRange, replacementString text: String?) -> Bool {
            typed = text.map { $0.count == 1 && ($0 == "." || $0 == "_" || $0.first!.isLetter) } ?? false
            return true
        }

        func textDidChange(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            SQLHighlighter.apply(to: view, language: parent.language)
            view.enclosingScrollView?.verticalRulerView?.needsDisplay = true
            parent.text = view.string
            if typed, parent.language != .redis {
                typed = false
                // Only when there is something to offer: an empty popup beeps.
                let range = view.rangeForUserCompletion
                guard range.location != NSNotFound, !suggestions(in: view, for: range).isEmpty else { return }
                DispatchQueue.main.async { view.complete(nil) }
            }
        }

        func textView(_ textView: NSTextView, completions words: [String], forPartialWordRange range: NSRange,
                      indexOfSelectedItem index: UnsafeMutablePointer<Int>?) -> [String] {
            index?.pointee = -1  // nothing inserted until a suggestion is picked
            return suggestions(in: textView, for: range)
        }

        /// Tables, columns, and keywords that start with the word being typed;
        /// after "table." only that table's columns, after "db." the collections.
        private func suggestions(in textView: NSTextView, for range: NSRange) -> [String] {
            let text = textView.string as NSString
            let partial = text.substring(with: range)
            var owner: String?
            if range.location > 0, text.substring(with: NSRange(location: range.location - 1, length: 1)) == "." {
                let before = text.substring(to: range.location - 1)
                owner = before.split(whereSeparator: { !($0.isLetter || $0.isNumber || $0 == "_") }).last.map(String.init)
            }
            return parent.names.suggestions(for: partial, after: owner, language: parent.language)
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            let range = view.selectedRange()
            parent.selection = range.length > 0 ? (view.string as NSString).substring(with: range) : ""
            view.enclosingScrollView?.verticalRulerView?.needsDisplay = true
        }
    }
}

/// The editor's colors, close to Xcode's dark theme. dbLens is always dark.
enum CodeTheme {
    static let font = NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
    static let bold = NSFont.monospacedSystemFont(ofSize: 13, weight: .semibold)
    static let background = NSColor(hex: 0x1C1C21)
    static let gutter = NSColor(hex: 0x19191D)
    static let plain = NSColor(hex: 0xE4E4E8)
    static let caret = NSColor(hex: 0x7CB7FF)
    static let selection = NSColor(hex: 0x3A4A66)
    static let lineNumber = NSColor(hex: 0x5C5F66)
    static let currentLineNumber = NSColor(hex: 0xC8CAD0)
    static let keyword = NSColor(hex: 0xFC5FA3)
    static let function = NSColor(hex: 0x67B7A4)
    static let string = NSColor(hex: 0xFC6A5D)
    static let number = NSColor(hex: 0xD0BF69)
    static let comment = NSColor(hex: 0x6C7986)
    static let identifier = NSColor(hex: 0x5DD8FF)
    static let parameter = NSColor(hex: 0xA167E6)
}

/// Colors SQL by pattern: keywords, function calls, strings, numbers,
/// "quoted identifiers", $1 parameters, and comments. Rerun on every edit;
/// editor text is short, so a full pass is cheap.
enum SQLHighlighter {
    /// The keywords, for completion.
    static var keywordList: [String] { keywords.split(separator: "|").map(String.init) }

    private static let keywords = """
        select from where and or not in is null as on join left right inner outer full cross natural using \
        group by order having limit offset distinct union all intersect except insert into values update set \
        delete returning create table view index unique primary key foreign references drop alter add column \
        constraint default check if exists cascade truncate begin commit rollback with recursive case when then \
        else end asc desc nulls first last like ilike similar between true false any some lateral over partition \
        window filter grant revoke schema extension materialized refresh explain analyze vacuum cast interval \
        coalesce nullif greatest least returns function language do declare
        """.split(whereSeparator: \.isWhitespace).joined(separator: "|")

    private static let rules: [(NSRegularExpression, NSColor, Bool)] = [
        (regex("\\b[A-Za-z_][A-Za-z0-9_]*(?=\\s*\\()"), CodeTheme.function, false),
        (regex("\\b(?:\(keywords))\\b", caseInsensitive: true), CodeTheme.keyword, true),
        (regex("\\b\\d+(?:\\.\\d+)?\\b"), CodeTheme.number, false),
        (regex("\\$\\d+"), CodeTheme.parameter, false),
        (regex("\"(?:[^\"]|\"\")*\""), CodeTheme.identifier, false),
        (regex("'(?:[^']|'')*'?"), CodeTheme.string, false),
        (regex("--[^\\n]*"), CodeTheme.comment, false),
        (regex("/\\*[\\s\\S]*?(?:\\*/|$)"), CodeTheme.comment, false),
    ]

    /// Redis: the command word starting each line, quoted arguments, numbers, # comments.
    private static let commandRules: [(NSRegularExpression, NSColor, Bool)] = [
        (regex("^[ \\t]*[A-Za-z][A-Za-z.]*", multiline: true), CodeTheme.keyword, true),
        (regex("(?<=\\s)-?\\d+(?:\\.\\d+)?(?=\\s|$)", multiline: true), CodeTheme.number, false),
        (regex("\"(?:[^\"\\\\]|\\\\.)*\"?"), CodeTheme.string, false),
        (regex("'[^']*'?"), CodeTheme.string, false),
        (regex("^[ \\t]*#[^\\n]*", multiline: true), CodeTheme.comment, false),
    ]

    /// mongosh: db and method calls, keys, $operators, strings, numbers, // comments.
    private static let queryRules: [(NSRegularExpression, NSColor, Bool)] = [
        (regex("\\b(?:db|use|show)\\b"), CodeTheme.keyword, true),
        (regex("(?<=\\.)[A-Za-z_][A-Za-z0-9_]*(?=\\s*\\()"), CodeTheme.function, false),
        (regex("\\b(?:ObjectId|ISODate|Date|NumberLong|NumberInt|NumberDecimal|new|true|false|null)\\b"), CodeTheme.keyword, false),
        (regex("[A-Za-z_$][A-Za-z0-9_$.]*(?=\\s*:)"), CodeTheme.identifier, false),
        (regex("\\$[A-Za-z]+"), CodeTheme.parameter, false),
        (regex("\\b-?\\d+(?:\\.\\d+)?\\b"), CodeTheme.number, false),
        (regex("\"(?:[^\"\\\\]|\\\\.)*\"?"), CodeTheme.string, false),
        (regex("'(?:[^'\\\\]|\\\\.)*'?"), CodeTheme.string, false),
        (regex("//[^\\n]*"), CodeTheme.comment, false),
    ]

    private static func regex(_ pattern: String, caseInsensitive: Bool = false, multiline: Bool = false) -> NSRegularExpression {
        var options: NSRegularExpression.Options = []
        if caseInsensitive { options.insert(.caseInsensitive) }
        if multiline { options.insert(.anchorsMatchLines) }
        return try! NSRegularExpression(pattern: pattern, options: options)
    }

    static func apply(to view: NSTextView, language: EditorLanguage = .sql) {
        guard let storage = view.textStorage else { return }
        let all = NSRange(location: 0, length: storage.length)
        storage.beginEditing()
        storage.setAttributes([.font: CodeTheme.font, .foregroundColor: CodeTheme.plain], range: all)
        // Later rules win: a keyword inside a string or comment ends up string- or comment-colored.
        let chosen = switch language {
        case .sql: rules
        case .redis: commandRules
        case .mongo: queryRules
        }
        for (regex, color, bold) in chosen {
            regex.enumerateMatches(in: storage.string, range: all) { match, _, _ in
                guard let range = match?.range else { return }
                storage.addAttribute(.foregroundColor, value: color, range: range)
                storage.addAttribute(.font, value: bold ? CodeTheme.bold : CodeTheme.font, range: range)
            }
        }
        storage.endEditing()
    }
}

/// Line numbers in a gutter to the left of the editor, the caret's line brighter.
final class LineNumberRuler: NSRulerView {
    private let font = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular)

    init(textView: NSTextView) {
        super.init(scrollView: textView.enclosingScrollView, orientation: .verticalRuler)
        clientView = textView
        ruleThickness = 40
        clipsToBounds = true
        NotificationCenter.default.addObserver(self, selector: #selector(redraw),
                                               name: NSView.boundsDidChangeNotification,
                                               object: textView.enclosingScrollView?.contentView)
    }

    required init(coder: NSCoder) { fatalError("not used") }

    @objc private func redraw() { needsDisplay = true }

    /// Only the numbers: NSRulerView's own separator line ran up past the
    /// editor, over the panel header (views don't clip to bounds since macOS 14).
    override func draw(_ dirtyRect: NSRect) {
        drawHashMarksAndLabels(in: dirtyRect)
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        CodeTheme.gutter.setFill()
        bounds.fill()
        guard let view = clientView as? NSTextView, let layout = view.layoutManager,
              let container = view.textContainer else { return }
        let text = view.string as NSString
        let caretLine = lineNumber(at: view.selectedRange().location, in: text)
        let visible = view.visibleRect
        let glyphs = layout.glyphRange(forBoundingRect: visible, in: container)
        var index = layout.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil).location
        var line = lineNumber(at: index, in: text)

        while true {
            let lineRect: NSRect
            if index >= text.length {
                // The empty last line after a final newline, or an empty editor.
                guard index == 0 || text.character(at: text.length - 1) == 10 else { break }
                lineRect = layout.extraLineFragmentRect
            } else {
                lineRect = layout.lineFragmentRect(forGlyphAt: layout.glyphIndexForCharacter(at: index), effectiveRange: nil)
            }
            let y = convert(NSPoint(x: 0, y: lineRect.minY + view.textContainerInset.height), from: view).y
            if y > bounds.maxY { break }
            let label = "\(line)" as NSString
            let attributes: [NSAttributedString.Key: Any] = [
                .font: font,
                .foregroundColor: line == caretLine ? CodeTheme.currentLineNumber : CodeTheme.lineNumber,
            ]
            let size = label.size(withAttributes: attributes)
            label.draw(at: NSPoint(x: ruleThickness - size.width - Space.sm,
                                   y: y + (lineRect.height - size.height) / 2), withAttributes: attributes)
            if index >= text.length { break }
            index = NSMaxRange(text.lineRange(for: NSRange(location: index, length: 0)))
            line += 1
            if index >= text.length, text.character(at: text.length - 1) != 10 { break }
        }
    }

    /// 1-based line of a character position.
    private func lineNumber(at location: Int, in text: NSString) -> Int {
        var line = 1
        var index = 0
        let end = min(location, text.length)
        while index < end {
            if text.character(at: index) == 10 { line += 1 }
            index += 1
        }
        return line
    }
}

/// What the bottom panel's editor holds.
/// What the editor can suggest: the open database's tables and their columns.
struct EditorNames: Equatable {
    var tables: [String] = []
    /// Columns by table name, lowercased.
    var columns: [String: [String]] = [:]

    func suggestions(for partial: String, after owner: String?, language: EditorLanguage) -> [String] {
        let pool: [String]
        if let owner {
            let key = owner.lowercased()
            if language == .mongo, key == "db" { pool = tables }
            else { pool = columns[key] ?? (language == .sql ? Array(Set(columns.values.joined())).sorted() : []) }
        } else {
            guard partial.count >= 2 else { return [] }
            let keywords = language == .sql ? SQLHighlighter.keywordList.map { partial.first!.isUppercase ? $0.uppercased() : $0 } : []
            pool = tables + Array(Set(columns.values.joined())).sorted() + keywords
        }
        var seen = Set<String>()
        return pool.filter { name in
            name.count > partial.count && name.lowercased().hasPrefix(partial.lowercased()) && seen.insert(name).inserted
        }
        .prefix(50).map { $0 }
    }
}

enum EditorLanguage: Equatable {
    case sql, redis, mongo

    var title: String {
        switch self {
        case .sql: "SQL"
        case .redis: String(localized: "Commands")
        case .mongo: String(localized: "Queries")
        }
    }
}
