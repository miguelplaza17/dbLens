import Foundation

/// A SQL script as its statements, for running them one at a time inside a
/// transaction dbLens controls (tests and all-or-nothing runs for agents).
public enum SQLScript {
    public enum Dialect: Sendable { case postgres, mysql, sqlite }

    /// The script cut at semicolons that are not inside quotes, `identifiers`,
    /// $tag$ bodies (Postgres), or comments. Empty statements are dropped.
    public static func statements(_ sql: String, dialect: Dialect) -> [String] {
        let chars = Array(sql)
        var statements: [String] = []
        var start = 0
        var index = 0
        func skip(past terminator: [Character]) {
            while index < chars.count, !chars[index...].starts(with: terminator) {
                // A backslash escapes the next character in MySQL strings.
                if dialect == .mysql, chars[index] == "\\", terminator.count == 1, terminator != ["\n"] { index += 1 }
                index += 1
            }
            index = min(chars.count, index + terminator.count)
        }
        while index < chars.count {
            let char = chars[index]
            let next: Character? = index + 1 < chars.count ? chars[index + 1] : nil
            switch char {
            case "'", "\"":
                index += 1
                skip(past: [char])
            case "`" where dialect != .postgres:
                index += 1
                skip(past: ["`"])
            case "-" where next == "-":
                skip(past: ["\n"])
            case "#" where dialect == .mysql:
                skip(past: ["\n"])
            case "/" where next == "*":
                index += 2
                skip(past: ["*", "/"])
            case "$" where dialect == .postgres:
                // $$ or $tag$ … $tag$ (a tag is letters, digits, underscores).
                var end = index + 1
                while end < chars.count, chars[end].isLetter || chars[end].isNumber || chars[end] == "_" { end += 1 }
                if end < chars.count, chars[end] == "$", !(index > 0 && (chars[index - 1].isLetter || chars[index - 1].isNumber)) {
                    let tag = Array(chars[index...end])
                    index = end + 1
                    skip(past: tag)
                } else {
                    index += 1
                }
            case ";":
                statements.append(String(chars[start..<index]))
                index += 1
                start = index
            default:
                index += 1
            }
        }
        statements.append(String(chars[start..<min(index, chars.count)]))
        return statements.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !verb($0).isEmpty }
    }

    /// The statement's first word(s), uppercased, after comments: "SELECT",
    /// "CREATE TEMPORARY", "START TRANSACTION". Empty for a comment alone.
    public static func verb(_ statement: String, words: Int = 1) -> String {
        var rest = Substring(statement)
        var found: [String] = []
        while found.count < words {
            rest = rest.drop(while: { $0.isWhitespace || $0 == "(" })
            if rest.hasPrefix("--") || rest.hasPrefix("#") {
                rest = rest.drop(while: { $0 != "\n" })
            } else if rest.hasPrefix("/*") {
                guard let end = rest.range(of: "*/") else { break }
                rest = rest[end.upperBound...]
            } else {
                let word = rest.prefix(while: { $0.isLetter || $0 == "_" })
                guard !word.isEmpty else { break }
                found.append(word.uppercased())
                rest = rest.dropFirst(word.count)
            }
        }
        return found.joined(separator: " ")
    }

    /// Statements that end or open a transaction: refused in scripts whose
    /// transaction dbLens owns, since a COMMIT inside would make a test permanent.
    static let transactionVerbs: Set<String> = ["BEGIN", "START", "COMMIT", "END", "ROLLBACK", "ABORT", "SAVEPOINT", "RELEASE"]

    /// The statements, or an error naming the first one that controls the transaction.
    public static func checked(_ sql: String, dialect: Dialect) throws -> [String] {
        let statements = statements(sql, dialect: dialect)
        guard !statements.isEmpty else { throw DatabaseError(message: String(localized: "There is no statement to run.")) }
        for (index, statement) in statements.enumerated() {
            let first = verb(statement)
            let two = verb(statement, words: 2)
            if transactionVerbs.contains(first) || two == "PREPARE TRANSACTION" || two == "SET AUTOCOMMIT" {
                throw DatabaseError(message: String(localized: "Statement \(index + 1) (\(first)) controls the transaction; dbLens runs the script in its own. Leave out BEGIN, COMMIT, and ROLLBACK."))
            }
        }
        return statements
    }

    /// Runs each statement, naming the one that fails.
    public static func run(_ statements: [String], _ body: (String) throws -> QueryResult) throws -> [QueryResult] {
        var results: [QueryResult] = []
        for (index, statement) in statements.enumerated() {
            do {
                results.append(try body(statement))
            } catch {
                let message = (error as? DatabaseError)?.message ?? error.localizedDescription
                let shown = statement.count > 80 ? String(statement.prefix(80)) + "…" : statement
                throw DatabaseError(message: String(localized: "Statement \(index + 1) of \(statements.count) failed (\(shown)): \(message)"))
            }
        }
        return results
    }
}

/// A script that can't run all-or-nothing on this database (MySQL DDL, MongoDB,
/// Redis). The caller may run it plainly instead.
public struct NotAtomic: Error, Sendable {
    public let message: String
    public init(_ message: String) { self.message = message }
}
