import Foundation

/// A result as a file: CSV (RFC 4180) or JSON (an array of objects).
public enum ResultExport {
    /// Header line, then one line per row. Values with a comma, quote, or line
    /// break go in quotes; NULL is an empty field.
    public static func csv(_ result: QueryResult) -> String {
        func field(_ value: String?) -> String {
            guard let value else { return "" }
            guard value.contains(where: { $0 == "," || $0 == "\"" || $0 == "\n" || $0 == "\r" }) else { return value }
            return "\"" + value.replacingOccurrences(of: "\"", with: "\"\"") + "\""
        }
        var lines = [result.columns.map { field($0.name) }.joined(separator: ",")]
        lines += result.rows.map { row in row.map(field).joined(separator: ",") }
        return lines.joined(separator: "\r\n") + "\r\n"
    }

    /// One object per row, keyed by column name, in column order. Values stay
    /// text as the grid shows them; NULL is null.
    public static func json(_ result: QueryResult) -> String {
        func string(_ text: String) -> String {
            let data = (try? JSONSerialization.data(withJSONObject: text, options: [.fragmentsAllowed, .withoutEscapingSlashes])) ?? Data()
            return String(decoding: data, as: UTF8.self)
        }
        let names = result.columns.map { string($0.name) }
        let objects = result.rows.map { row in
            "  {" + zip(names, row).map { "\($0): \($1.map(string) ?? "null")" }.joined(separator: ", ") + "}"
        }
        return objects.isEmpty ? "[]\n" : "[\n" + objects.joined(separator: ",\n") + "\n]\n"
    }
}
