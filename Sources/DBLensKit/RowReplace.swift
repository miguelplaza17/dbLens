import Foundation

/// Find and replace in table rows: which cells of a row change, and to what.
/// Only text columns, and never the columns that identify the row: a search also
/// matches numbers and dates, and rewriting those (or a key) is rarely what's meant.
public enum RowReplace {
    /// "text", "varchar(20)", "character varying", "CLOB", "citext"…
    public static func isText(_ type: String) -> Bool {
        let name = type.lowercased()
        return name.contains("char") || name.contains("text") || name.contains("clob")
    }

    /// The columns to write for this row, with their new values. Case-insensitive, like the search.
    public static func changes(columns: [Column], row: [String?], keyColumns: [String],
                               find: String, with replacement: String) -> [(String, String?)] {
        guard !find.isEmpty else { return [] }
        var changes: [(String, String?)] = []
        for (column, value) in zip(columns, row) where isText(column.type) && !keyColumns.contains(column.name) {
            guard let value else { continue }
            let replaced = value.replacingOccurrences(of: find, with: replacement, options: [.caseInsensitive, .literal])
            if replaced != value { changes.append((column.name, replaced)) }
        }
        return changes
    }
}
