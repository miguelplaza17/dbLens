import CMongoC
import Foundation

/// mongosh-style text, as typed in the command panel: how it splits into
/// statements, what each statement asks for, and the object literals its
/// arguments are written in, turned into the extended JSON libbson reads.
public enum MongoShell {
    /// What one statement asks for. Arguments stay as typed, to be read with `extendedJSON`.
    public enum Statement: Sendable, Equatable {
        /// show dbs, show databases
        case showDatabases
        /// show collections, show tables
        case showCollections
        /// use shop
        case use(String)
        /// db.runCommand({…}), db.getCollectionNames(), …
        case database(method: String, arguments: [String])
        /// db.users.find({…}).sort({…}).limit(5), db.getCollection("x").drop(), …
        case collection(name: String, calls: [Call])
    }

    /// One method call of a chain: `find({a: 1})` is ("find", ["{a: 1}"]).
    public struct Call: Sendable, Equatable {
        public let name: String
        public let arguments: [String]

        public init(name: String, arguments: [String]) {
            self.name = name
            self.arguments = arguments
        }
    }

    /// The error for a statement dbLens doesn't know.
    static func unsupported(_ statement: String) -> DatabaseError {
        let line = statement.split(separator: "\n").first.map(String.init) ?? statement
        return DatabaseError(message: String(localized: "dbLens can't run this yet: \(line.trimmingCharacters(in: .whitespaces))"))
    }

    // MARK: Statements

    /// Splits the panel's text into statements: at `;` and at line ends, but not
    /// while a bracket is open or when the next line goes on with `.method()`.
    /// Comments (`//`, `/* */`) are dropped; strings and regexes are kept as typed.
    public static func statements(_ text: String) -> [String] {
        let s = Array(text.unicodeScalars)
        var out: [String] = []
        var current = String.UnicodeScalarView()
        var depth = 0
        var last: Unicode.Scalar?
        var i = 0
        func flush() {
            let statement = String(current).trimmingCharacters(in: .whitespacesAndNewlines)
            if !statement.isEmpty { out.append(statement) }
            current = String.UnicodeScalarView()
            last = nil
            depth = 0
        }
        while i < s.count {
            let c = s[i]
            if let end = Lexer.commentEnd(s, at: i) {
                current.append(" ")
                i = end
                continue
            }
            if c == "\"" || c == "'" || c == "`" {
                let end = Lexer.stringEnd(s, at: i)
                current.append(contentsOf: s[i..<end])
                last = c
                i = end
                continue
            }
            if c == "/", Lexer.regexMayStart(after: last) {
                let end = Lexer.regexEnd(s, at: i)
                current.append(contentsOf: s[i..<end])
                last = "/"
                i = end
                continue
            }
            if "([{".unicodeScalars.contains(c) { depth += 1 }
            if ")]}".unicodeScalars.contains(c) { depth = max(0, depth - 1) }
            if depth == 0, c == ";" {
                flush()
                i += 1
                continue
            }
            if depth == 0, c == "\n" {
                if last == "." || Lexer.nextIsChainedCall(s, from: i + 1) {
                    current.append(" ")
                } else {
                    flush()
                }
                i += 1
                continue
            }
            current.append(c)
            if !c.properties.isWhitespace { last = c }
            i += 1
        }
        flush()
        return out
    }

    /// What one statement (as `statements` returns it) asks for, or the
    /// "can't run this yet" error.
    public static func parse(_ statement: String) throws -> Statement {
        let trimmed = statement.trimmingCharacters(in: CharacterSet.whitespacesAndNewlines.union(CharacterSet(charactersIn: ";")))
        let words = trimmed.split(whereSeparator: \.isWhitespace).map(String.init)
        if words.first == "show", words.count == 2 {
            switch words[1].lowercased() {
            case "dbs", "databases": return .showDatabases
            case "collections", "tables": return .showCollections
            default: throw unsupported(statement)
            }
        }
        if words.first == "use", words.count == 2 {
            let name = words[1].trimmingCharacters(in: CharacterSet(charactersIn: "\"'`"))
            guard !name.isEmpty else { throw unsupported(statement) }
            return .use(name)
        }
        var reader = Lexer(trimmed)
        guard (try? parseChain(&reader)) != nil, let result = reader.statement else { throw unsupported(statement) }
        return result
    }

    /// db, then a collection (`.name`, `["name"]`, `.getCollection("name")`) and its
    /// method calls, or a database method call.
    private static func parseChain(_ reader: inout Lexer) throws {
        reader.skipSpace()
        guard reader.identifier() == "db" else { throw Lexer.Failure() }
        var parts: [String] = []
        var collection: String?
        var calls: [Call] = []
        while true {
            reader.skipSpace()
            if reader.atEnd { break }
            if collection == nil, parts.isEmpty, reader.consume("[") {
                let args = try reader.arguments(until: "]")
                guard args.count == 1, let name = try? stringArgument(args[0]) else { throw Lexer.Failure() }
                collection = name
                continue
            }
            guard reader.consume(".") else { throw Lexer.Failure() }
            reader.skipSpace()
            guard let name = reader.identifier() else { throw Lexer.Failure() }
            reader.skipSpace()
            guard reader.peek == "(" else {
                guard collection == nil, calls.isEmpty else { throw Lexer.Failure() }
                parts.append(name)
                continue
            }
            reader.advance()
            let args = try reader.arguments(until: ")")
            if collection == nil, parts.isEmpty {
                if name == "getCollection" {
                    guard args.count == 1, let target = try? stringArgument(args[0]) else { throw Lexer.Failure() }
                    collection = target
                    continue
                }
                reader.skipSpace()
                guard reader.atEnd else { throw Lexer.Failure() }
                reader.statement = .database(method: name, arguments: args)
                return
            }
            if collection == nil { collection = parts.joined(separator: ".") }
            calls.append(Call(name: name, arguments: args))
        }
        guard let collection, !collection.isEmpty, !calls.isEmpty else { throw Lexer.Failure() }
        reader.statement = .collection(name: collection, calls: calls)
    }

    /// A string argument ("name" or 'name'), as its text.
    static func stringArgument(_ argument: String) throws -> String {
        var reader = Lexer(argument)
        reader.skipSpace()
        guard reader.peek == "\"" || reader.peek == "'" || reader.peek == "`" else { throw Lexer.Failure() }
        let text = try reader.string()
        reader.skipSpace()
        guard reader.atEnd else { throw Lexer.Failure() }
        return text
    }

    // MARK: Literals

    /// A mongosh value literal as extended JSON (the form libbson's
    /// `bson_new_from_json` reads): unquoted keys and 'single quotes' become
    /// JSON strings, trailing commas go, and the shell's helpers become their
    /// extended JSON: ObjectId("…") → {"$oid": …}, ISODate("…") and new Date("…")
    /// → {"$date": …}, NumberLong, NumberInt, NumberDecimal, UUID, Timestamp,
    /// BinData, MinKey, MaxKey, and /regex/flags → {"$regularExpression": …}.
    /// Plain JSON and extended JSON pass through.
    public static func extendedJSON(_ literal: String) throws -> String {
        var reader = Lexer(literal)
        do {
            let value = try reader.value()
            reader.skipSpace()
            guard reader.atEnd else { throw Lexer.Failure(at: reader.position) }
            return value.json
        } catch let failure as Lexer.Failure {
            if let message = failure.message { throw DatabaseError(message: message) }
            let snippet = literal.trimmingCharacters(in: .whitespacesAndNewlines)
            let short = snippet.count > 80 ? String(snippet.prefix(80)) + "…" : snippet
            throw DatabaseError(message: String(localized: "This isn't a value dbLens can read: \(short)"))
        }
    }

    /// A JSON string literal: "a \"b\"".
    public static func jsonString(_ text: String) -> String {
        var out = "\""
        for scalar in text.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case "\u{08}": out += "\\b"
            case "\u{0C}": out += "\\f"
            default:
                if scalar.value < 0x20 {
                    out += String(format: "\\u%04x", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        return out + "\""
    }

    // MARK: Dates

    /// Milliseconds since 1970 for an ISO 8601 date, read the way mongosh's ISODate
    /// does: "2024-05-01", "2024-05-01T10:30", "2024-05-01T10:30:00.250Z",
    /// "2024-05-01 10:30:00+02:00". Without a zone, UTC. nil when it isn't one.
    public static func isoDateMilliseconds(_ text: String) -> Int64? {
        let pattern = #"^(\d{4})-(\d{2})-(\d{2})(?:[Tt ](\d{2}):(\d{2})(?::(\d{2})(?:[.,](\d{1,9}))?)?)?\s*(Z|z|[+-]\d{2}(?::?\d{2})?)?$"#
        let value = text.trimmingCharacters(in: .whitespaces)
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let match = regex.firstMatch(in: value, range: NSRange(value.startIndex..., in: value)) else { return nil }
        func group(_ index: Int) -> String? {
            guard let range = Range(match.range(at: index), in: value) else { return nil }
            return String(value[range])
        }
        guard let year = Int64(group(1) ?? ""), let month = Int64(group(2) ?? ""), let day = Int64(group(3) ?? "") else { return nil }
        let hour = Int64(group(4) ?? "0") ?? 0, minute = Int64(group(5) ?? "0") ?? 0, second = Int64(group(6) ?? "0") ?? 0
        guard (1...12).contains(month), day >= 1, day <= daysIn(month: month, year: year),
              hour <= 23, minute <= 59, second <= 59 else { return nil }
        let fraction = (group(7) ?? "") + "000"
        let millis = Int64(fraction.prefix(3)) ?? 0
        var offset: Int64 = 0
        if let zone = group(8), zone.uppercased() != "Z" {
            let sign: Int64 = zone.hasPrefix("-") ? -1 : 1
            let digits = zone.dropFirst().replacingOccurrences(of: ":", with: "")
            let hours = Int64(digits.prefix(2)) ?? 0
            let minutes = digits.count > 2 ? Int64(digits.dropFirst(2)) ?? 0 : 0
            guard hours <= 23, minutes <= 59 else { return nil }
            offset = sign * (hours * 3600 + minutes * 60)
        }
        let seconds = daysFromCivil(year: year, month: month, day: day) * 86_400 + hour * 3600 + minute * 60 + second - offset
        return seconds * 1000 + millis
    }

    /// "2024-05-01T10:30:00.250Z" for milliseconds since 1970 (UTC), or nil
    /// outside the years 0–9999.
    public static func isoDate(milliseconds: Int64) -> String? {
        let (seconds, millis) = milliseconds.quotientAndRemainder(dividingBy: 1000)
        let wholeSeconds = millis < 0 ? seconds - 1 : seconds
        let ms = millis < 0 ? millis + 1000 : millis
        var days = wholeSeconds / 86_400
        var rest = wholeSeconds % 86_400
        if rest < 0 { rest += 86_400; days -= 1 }
        let (year, month, day) = civilFromDays(days)
        guard (0...9999).contains(year) else { return nil }
        return String(format: "%04lld-%02lld-%02lldT%02lld:%02lld:%02lld.%03lldZ",
                      year, month, day, rest / 3600, rest % 3600 / 60, rest % 60, ms)
    }

    private static func daysIn(month: Int64, year: Int64) -> Int64 {
        switch month {
        case 2: (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 ? 29 : 28
        case 4, 6, 9, 11: 30
        default: 31
        }
    }

    /// Days since 1970-01-01 (Howard Hinnant's algorithm).
    private static func daysFromCivil(year: Int64, month: Int64, day: Int64) -> Int64 {
        let y = month <= 2 ? year - 1 : year
        let era = (y >= 0 ? y : y - 399) / 400
        let yoe = y - era * 400
        let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
        return era * 146_097 + doe - 719_468
    }

    private static func civilFromDays(_ days: Int64) -> (Int64, Int64, Int64) {
        let z = days + 719_468
        let era = (z >= 0 ? z : z - 146_096) / 146_097
        let doe = z - era * 146_097
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        let mp = (5 * doy + 2) / 153
        let day = doy - (153 * mp + 2) / 5 + 1
        let month = mp < 10 ? mp + 3 : mp - 9
        return (yoe + era * 400 + (month <= 2 ? 1 : 0), month, day)
    }

    static func dateJSON(_ milliseconds: Int64) -> String { #"{"$date":{"$numberLong":"\#(milliseconds)"}}"# }
}

// MARK: Lexer

/// Reads mongosh text one Unicode scalar at a time.
struct Lexer {
    struct Failure: Error {
        var at = 0
        /// A message to show as is, for helpers with a bad argument.
        var message: String?
    }

    /// A literal read so far: strings and numbers are kept apart so the
    /// shell's helpers can take them as arguments.
    enum Value {
        case string(String)
        case number(String)
        case json(String)

        var json: String {
            switch self {
            case .string(let text): MongoShell.jsonString(text)
            case .number(let text): text
            case .json(let text): text
            }
        }
    }

    let s: [Unicode.Scalar]
    var position = 0
    /// What `MongoShell.parse` found.
    var statement: MongoShell.Statement?

    init(_ text: String) { s = Array(text.unicodeScalars) }

    var atEnd: Bool { position >= s.count }
    var peek: Unicode.Scalar? { position < s.count ? s[position] : nil }
    mutating func advance() { position += 1 }

    mutating func consume(_ scalar: Unicode.Scalar) -> Bool {
        guard peek == scalar else { return false }
        position += 1
        return true
    }

    mutating func skipSpace() {
        while position < s.count {
            if s[position].properties.isWhitespace {
                position += 1
            } else if let end = Self.commentEnd(s, at: position) {
                position = end
            } else {
                return
            }
        }
    }

    static func isIdentifierStart(_ c: Unicode.Scalar) -> Bool { c == "_" || c == "$" || c.properties.isAlphabetic }
    static func isIdentifierPart(_ c: Unicode.Scalar) -> Bool {
        isIdentifierStart(c) || ("0"..."9").contains(c)
    }

    mutating func identifier() -> String? {
        guard let first = peek, Self.isIdentifierStart(first) else { return nil }
        let start = position
        while let c = peek, Self.isIdentifierPart(c) { position += 1 }
        return String(String.UnicodeScalarView(s[start..<position]))
    }

    /// The arguments up to the closing bracket, split at top-level commas, as typed.
    mutating func arguments(until close: Unicode.Scalar) throws -> [String] {
        var args: [String] = []
        var current = String.UnicodeScalarView()
        var depth = 0
        var last: Unicode.Scalar? = "("
        while position < s.count {
            let c = s[position]
            if let end = Self.commentEnd(s, at: position) {
                current.append(" ")
                position = end
                continue
            }
            if c == "\"" || c == "'" || c == "`" {
                let end = Self.stringEnd(s, at: position)
                current.append(contentsOf: s[position..<end])
                position = end
                last = c
                continue
            }
            if c == "/", Self.regexMayStart(after: last) {
                let end = Self.regexEnd(s, at: position)
                current.append(contentsOf: s[position..<end])
                position = end
                last = "/"
                continue
            }
            position += 1
            if depth == 0, c == close {
                let text = String(current).trimmingCharacters(in: .whitespacesAndNewlines)
                if !text.isEmpty || !args.isEmpty { args.append(text) }
                // A trailing comma: f(a, b,)
                if args.last == "" { args.removeLast() }
                return args
            }
            if "([{".unicodeScalars.contains(c) { depth += 1 }
            if ")]}".unicodeScalars.contains(c) { depth -= 1 }
            if depth < 0 { throw Failure(at: position) }
            if depth == 0, c == "," {
                args.append(String(current).trimmingCharacters(in: .whitespacesAndNewlines))
                current = String.UnicodeScalarView()
                last = c
                continue
            }
            current.append(c)
            if !c.properties.isWhitespace { last = c }
        }
        throw Failure(at: position)
    }

    // MARK: Values

    mutating func value() throws -> Value {
        skipSpace()
        guard let c = peek else { throw Failure(at: position) }
        switch c {
        case "{": return .json(try object())
        case "[": return .json(try array())
        case "\"", "'", "`": return .string(try string())
        case "/": return .json(try regex())
        case "-", "+", ".", "0"..."9": return try number()
        default:
            guard Self.isIdentifierStart(c) else { throw Failure(at: position) }
            return try word()
        }
    }

    private mutating func object() throws -> String {
        position += 1
        var members: [String] = []
        while true {
            skipSpace()
            if consume("}") { break }
            let key = try key()
            skipSpace()
            guard consume(":") else { throw Failure(at: position) }
            members.append(MongoShell.jsonString(key) + ":" + (try value()).json)
            skipSpace()
            if consume(",") { continue }
            guard consume("}") else { throw Failure(at: position) }
            break
        }
        return "{" + members.joined(separator: ",") + "}"
    }

    private mutating func key() throws -> String {
        guard let c = peek else { throw Failure(at: position) }
        if c == "\"" || c == "'" || c == "`" { return try string() }
        let start = position
        // Lenient: a.b and numbers are taken as they are.
        while let c = peek, Self.isIdentifierPart(c) || c == "." || c == "-" { position += 1 }
        guard position > start else { throw Failure(at: position) }
        return String(String.UnicodeScalarView(s[start..<position]))
    }

    private mutating func array() throws -> String {
        position += 1
        var items: [String] = []
        while true {
            skipSpace()
            if consume("]") { break }
            items.append((try value()).json)
            skipSpace()
            if consume(",") { continue }
            guard consume("]") else { throw Failure(at: position) }
            break
        }
        return "[" + items.joined(separator: ",") + "]"
    }

    /// A quoted string, with JavaScript's escapes read.
    mutating func string() throws -> String {
        guard let quote = peek else { throw Failure(at: position) }
        position += 1
        var out = String.UnicodeScalarView()
        var pendingHigh: UInt32?
        func append(_ value: UInt32) {
            if (0xD800...0xDBFF).contains(value) { pendingHigh = value; return }
            if let high = pendingHigh, (0xDC00...0xDFFF).contains(value) {
                pendingHigh = nil
                out.append(Unicode.Scalar(0x10000 + ((high - 0xD800) << 10) + (value - 0xDC00)) ?? "\u{FFFD}")
                return
            }
            if pendingHigh != nil { out.append("\u{FFFD}"); pendingHigh = nil }
            out.append(Unicode.Scalar(value) ?? "\u{FFFD}")
        }
        while position < s.count {
            let c = s[position]
            position += 1
            if c == quote {
                if pendingHigh != nil { out.append("\u{FFFD}") }
                return String(out)
            }
            guard c == "\\" else {
                if c == "\n", quote != "`" { throw Failure(at: position) }
                append(c.value)
                continue
            }
            guard position < s.count else { break }
            let e = s[position]
            position += 1
            switch e {
            case "n": append(0x0A)
            case "t": append(0x09)
            case "r": append(0x0D)
            case "b": append(0x08)
            case "f": append(0x0C)
            case "v": append(0x0B)
            case "0": append(0)
            case "\n": continue
            case "x":
                guard let value = hex(count: 2) else { throw Failure(at: position) }
                append(value)
            case "u":
                if consume("{") {
                    let start = position
                    while let c = peek, c != "}" { position += 1 }
                    guard consume("}"), let value = UInt32(String(String.UnicodeScalarView(s[start..<position - 1])), radix: 16)
                    else { throw Failure(at: position) }
                    append(value)
                } else {
                    guard let value = hex(count: 4) else { throw Failure(at: position) }
                    append(value)
                }
            default: append(e.value)
            }
        }
        throw Failure(at: position)
    }

    private mutating func hex(count: Int) -> UInt32? {
        guard position + count <= s.count else { return nil }
        let text = String(String.UnicodeScalarView(s[position..<position + count]))
        guard let value = UInt32(text, radix: 16) else { return nil }
        position += count
        return value
    }

    /// /pattern/flags. MongoDB keeps the flags i, m, s, x, u and l, sorted.
    private mutating func regex() throws -> String {
        let end = Self.regexEnd(s, at: position)
        let text = s[position..<end]
        guard let close = text.lastIndex(of: "/"), close > position else { throw Failure(at: position) }
        let pattern = String(String.UnicodeScalarView(s[(position + 1)..<close]))
        let flags = String(String.UnicodeScalarView(s[(close + 1)..<end]))
        position = end
        return Self.regexJSON(pattern: pattern, flags: flags)
    }

    static func regexJSON(pattern: String, flags: String) -> String {
        let options = String(Set(flags.filter { "imsxul".contains($0) }).sorted())
        return #"{"$regularExpression":{"pattern":\#(MongoShell.jsonString(pattern)),"options":"\#(options)"}}"#
    }

    private mutating func number() throws -> Value {
        var sign = ""
        if consume("-") { sign = "-" } else { _ = consume("+") }
        skipSpace()
        if let c = peek, Self.isIdentifierStart(c) {
            guard let word = identifier(), word == "Infinity" else { throw Failure(at: position) }
            return .json(#"{"$numberDouble":"\#(sign)Infinity"}"#)
        }
        if peek == "0", position + 1 < s.count, s[position + 1] == "x" || s[position + 1] == "X" {
            position += 2
            let start = position
            while let c = peek, c.properties.isASCIIHexDigit { position += 1 }
            guard let value = Int64(String(String.UnicodeScalarView(s[start..<position])), radix: 16) else { throw Failure(at: position) }
            return .number(sign + String(value))
        }
        func digits() -> String {
            let start = position
            while let c = peek, ("0"..."9").contains(c) || c == "_" { position += 1 }
            return String(String.UnicodeScalarView(s[start..<position])).replacingOccurrences(of: "_", with: "")
        }
        var whole = digits()
        var fraction: String?
        if peek == "." {
            position += 1
            fraction = digits()
        }
        var exponent = ""
        if peek == "e" || peek == "E" {
            position += 1
            var expSign = ""
            if consume("-") { expSign = "-" } else { _ = consume("+") }
            let expDigits = digits()
            guard !expDigits.isEmpty else { throw Failure(at: position) }
            exponent = "e" + expSign + expDigits
        }
        guard !whole.isEmpty || !(fraction ?? "").isEmpty else { throw Failure(at: position) }
        // JSON has no leading zeros (007) and no bare points (.5, 5.).
        whole = String(whole.drop { $0 == "0" })
        if whole.isEmpty { whole = "0" }
        var text = sign + whole
        if let fraction { text += "." + (fraction.isEmpty ? "0" : fraction) }
        return .number(text + exponent)
    }

    /// true, null, ObjectId("…"), new Date(), NumberLong(5), …
    private mutating func word() throws -> Value {
        guard var name = identifier() else { throw Failure(at: position) }
        skipSpace()
        if name == "new" {
            guard let next = identifier() else { throw Failure(at: position) }
            name = next
            skipSpace()
        }
        var args: [Value]?
        if consume("(") {
            var values: [Value] = []
            while true {
                skipSpace()
                if consume(")") { break }
                values.append(try value())
                skipSpace()
                if consume(",") { continue }
                guard consume(")") else { throw Failure(at: position) }
                break
            }
            args = values
        }
        return try helper(name, args)
    }

    private func bad(_ name: String, _ args: [Value]?) -> Failure {
        let shown = (args ?? []).map(\.json).joined(separator: ", ")
        return Failure(at: position, message: String(localized: "This isn't a valid \(name)(\(shown))."))
    }

    private func helper(_ name: String, _ args: [Value]?) throws -> Value {
        let list = args ?? []
        func text(_ index: Int) -> String? {
            guard index < list.count else { return nil }
            switch list[index] {
            case .string(let value), .number(let value): return value
            case .json: return nil
            }
        }
        switch (name, args == nil) {
        case ("true", true), ("false", true), ("null", true): return .json(name)
        case ("undefined", true): return .json(#"{"$undefined":true}"#)
        case ("Infinity", true): return .json(#"{"$numberDouble":"Infinity"}"#)
        case ("NaN", true): return .json(#"{"$numberDouble":"NaN"}"#)
        case ("MinKey", _): return .json(#"{"$minKey":1}"#)
        case ("MaxKey", _): return .json(#"{"$maxKey":1}"#)
        default: break
        }
        guard args != nil else {
            throw Failure(at: position, message: String(localized: "dbLens doesn't know \(name) here. Write the value itself, like \"text\", 42 or {a: 1}."))
        }
        switch name {
        case "ObjectId", "ObjectID":
            guard let hex = text(0) else {
                guard list.isEmpty else { throw bad(name, args) }
                return .json(#"{"$oid":"\#(BSON.newObjectId())"}"#)
            }
            guard hex.count == 24, hex.allSatisfy(\.isHexDigit) else { throw bad(name, args) }
            return .json(#"{"$oid":"\#(hex.lowercased())"}"#)
        case "ISODate", "Date":
            if list.isEmpty { return .json(MongoShell.dateJSON(Int64(Date().timeIntervalSince1970 * 1000))) }
            if case .number(let ms) = list[0], let value = Int64(ms) ?? Double(ms).map({ Int64($0) }) {
                return .json(MongoShell.dateJSON(value))
            }
            guard let value = text(0), let ms = MongoShell.isoDateMilliseconds(value) else { throw bad(name, args) }
            return .json(MongoShell.dateJSON(ms))
        case "NumberLong", "Long":
            guard let value = text(0), let number = Int64(value) ?? Double(value).flatMap({ Int64(exactly: $0) }) else { throw bad(name, args) }
            return .json(#"{"$numberLong":"\#(number)"}"#)
        case "NumberInt", "Int32":
            guard let value = text(0), let number = Int32(value) ?? Double(value).flatMap({ Int32(exactly: $0) }) else { throw bad(name, args) }
            return .json(#"{"$numberInt":"\#(number)"}"#)
        case "NumberDecimal", "Decimal128":
            guard let value = text(0), BSON.isDecimal(value) else { throw bad(name, args) }
            return .json(#"{"$numberDecimal":"\#(value)"}"#)
        case "Double":
            guard let value = text(0), let number = Double(value) else { throw bad(name, args) }
            return .json(#"{"$numberDouble":"\#(BSON.doubleText(number))"}"#)
        case "UUID":
            let hex = list.isEmpty ? UUID().uuidString : (text(0) ?? "")
            let digits = hex.replacingOccurrences(of: "-", with: "")
            guard digits.count == 32, digits.allSatisfy(\.isHexDigit) else { throw bad(name, args) }
            var bytes: [UInt8] = []
            var index = digits.startIndex
            while index < digits.endIndex {
                let next = digits.index(index, offsetBy: 2)
                bytes.append(UInt8(digits[index..<next], radix: 16) ?? 0)
                index = next
            }
            return .json(#"{"$binary":{"base64":"\#(Data(bytes).base64EncodedString())","subType":"04"}}"#)
        case "BinData":
            guard list.count == 2, let subtype = text(0).flatMap({ Int($0) }), (0...255).contains(subtype), let base64 = text(1),
                  Data(base64Encoded: base64) != nil else { throw bad(name, args) }
            return .json(#"{"$binary":{"base64":"\#(base64)","subType":"\#(String(format: "%02x", subtype))"}}"#)
        case "Timestamp":
            if list.count == 2, let t = text(0).flatMap({ UInt32($0) }), let i = text(1).flatMap({ UInt32($0) }) {
                return .json(#"{"$timestamp":{"t":\#(t),"i":\#(i)}}"#)
            }
            if list.count == 1, case .json(let object) = list[0], object.hasPrefix("{") {
                return .json(#"{"$timestamp":\#(object)}"#)
            }
            throw bad(name, args)
        case "RegExp":
            guard let pattern = text(0) else { throw bad(name, args) }
            return .json(Self.regexJSON(pattern: pattern, flags: text(1) ?? ""))
        default:
            throw Failure(at: position, message: String(localized: "dbLens can't read \(name)(…) here."))
        }
    }

    // MARK: Scanning

    /// Where a comment that starts at `index` ends, or nil when none starts there.
    static func commentEnd(_ s: [Unicode.Scalar], at index: Int) -> Int? {
        guard s[index] == "/", index + 1 < s.count else { return nil }
        if s[index + 1] == "/" {
            var end = index + 2
            while end < s.count, s[end] != "\n" { end += 1 }
            return end
        }
        if s[index + 1] == "*" {
            var end = index + 2
            while end + 1 < s.count, !(s[end] == "*" && s[end + 1] == "/") { end += 1 }
            return min(end + 2, s.count)
        }
        return nil
    }

    /// Just past the closing quote of the string that starts at `index`.
    static func stringEnd(_ s: [Unicode.Scalar], at index: Int) -> Int {
        let quote = s[index]
        var end = index + 1
        while end < s.count {
            let c = s[end]
            if c == "\\" { end += 2; continue }
            if c == quote { return end + 1 }
            if c == "\n", quote != "`" { return end }
            end += 1
        }
        return s.count
    }

    /// Just past the flags of the regex literal that starts at `index`.
    static func regexEnd(_ s: [Unicode.Scalar], at index: Int) -> Int {
        var end = index + 1
        var inClass = false
        while end < s.count {
            let c = s[end]
            if c == "\\" { end += 2; continue }
            if c == "\n" { return end }
            if c == "[" { inClass = true }
            if c == "]" { inClass = false }
            if c == "/", !inClass {
                end += 1
                while end < s.count, s[end].properties.isAlphabetic { end += 1 }
                return end
            }
            end += 1
        }
        return s.count
    }

    /// A `/` starts a regex after these (or at the start); after a value it divides.
    static func regexMayStart(after last: Unicode.Scalar?) -> Bool {
        guard let last else { return true }
        return "(,=:[!&|?{};+-*%<>~^".unicodeScalars.contains(last)
    }

    /// True when the next line (past spaces and comments) starts with `.name`.
    static func nextIsChainedCall(_ s: [Unicode.Scalar], from start: Int) -> Bool {
        var index = start
        while index < s.count {
            if s[index].properties.isWhitespace { index += 1; continue }
            if let end = commentEnd(s, at: index) { index = end; continue }
            break
        }
        guard index < s.count, s[index] == "." else { return false }
        return index + 1 < s.count && isIdentifierStart(s[index + 1])
    }
}
