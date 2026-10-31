import Foundation
import Network
import Security
import Synchronization

/// A Redis (or Valkey) connection, as pasted.
///   redis://:pass@cache.example.com:6379/0     (password only)
///   redis://ana:pass@cache.example.com/3       (ACL user)
///   rediss://…                                  (TLS)
///   rediss://…?ssl_cert_reqs=none               (TLS, certificate not checked)
///   redis://host?db=3&password=…
///   REDIS_URL="redis://…"                       (an .env line)
/// Sentinel and Cluster URLs are refused for now.
public struct RedisURL: Sendable, Hashable {
    /// What was pasted, without an .env name or quotes around it.
    public let raw: String
    /// "localhost" when the URL names none.
    public let host: String
    /// 6379 when the URL names none.
    public let port: Int
    /// The ACL user; nil for `redis://:pass@host` (password only).
    public let user: String?
    public let password: String?
    /// The database index, 0 by default.
    public let database: Int
    /// rediss://
    public let tls: Bool
    /// False when the URL says not to check the server's certificate.
    public let verifyTLS: Bool

    /// "db 0 @ cache.example.com" for the sidebar.
    public var suggestedName: String { "db \(database) @ \(host)" }

    public init(_ text: String) throws {
        let cleaned = ConnectionURL.clean(text)
        guard !cleaned.isEmpty else { throw DatabaseError(message: String(localized: "Paste the database URL.")) }
        let lower = cleaned.lowercased()
        if lower.range(of: "^rediss?([+-][a-z0-9_]+)://", options: .regularExpression) != nil {
            throw DatabaseError(message: String(localized: "Redis Sentinel, Cluster and socket URLs are not supported yet. Use a redis:// URL to one server."))
        }
        guard lower.hasPrefix("redis://") || lower.hasPrefix("rediss://"), let parts = URLParts(cleaned) else {
            throw DatabaseError(message: String(localized: "Unrecognized format. Use a redis://… URL (or rediss:// for TLS)"))
        }
        let query = Dictionary(parts.query.map { ($0.key.lowercased(), $0.value) }, uniquingKeysWith: { a, _ in a })
        func nonEmpty(_ value: String?) -> String? { value.flatMap { $0.isEmpty ? nil : $0 } }
        func flag(_ name: String) -> Bool? {
            guard let value = query[name]?.lowercased() else { return nil }
            if ["true", "1", "yes", "on"].contains(value) { return true }
            if ["false", "0", "no", "off"].contains(value) { return false }
            return nil
        }

        let number = nonEmpty(parts.path) ?? nonEmpty(query["db"] ?? query["database"])
        guard let database = number.map({ Int($0) }) ?? 0, database >= 0 else {
            throw DatabaseError(message: String(localized: "The Redis database must be a number, like /0."))
        }
        raw = cleaned
        host = nonEmpty(parts.hosts.first?.name) ?? "localhost"
        port = parts.hosts.first?.port ?? 6379
        user = nonEmpty(parts.user ?? query["username"] ?? query["user"])
        password = nonEmpty(parts.password ?? query["password"])
        self.database = database
        tls = lower.hasPrefix("rediss://") || flag("ssl") == true || flag("tls") == true
        let skip = query["ssl_cert_reqs"]?.lowercased() == "none" || flag("insecure") == true
            || flag("skip_verify") == true || flag("tls_skip_verify") == true
        verifyTLS = !skip
    }
}

// MARK: - RESP

/// One reply in the Redis protocol (RESP2, plus the RESP3 types a server may send).
enum RESPValue: Sendable, Equatable {
    case simple(String)
    case error(String)
    case integer(Int64)
    case bulk([UInt8])
    case null
    case array([RESPValue])
    /// A RESP3 map, flattened: key, value, key, value…
    case map([RESPValue])
    case double(String)
    case boolean(Bool)
    case bigNumber(String)

    /// The reply as one cell: text, 0x… for bytes that aren't UTF-8, nil for null,
    /// JSON-ish text for arrays and maps.
    var text: String? {
        switch self {
        case .simple(let text), .error(let text), .double(let text), .bigNumber(let text): text
        case .integer(let number): String(number)
        case .boolean(let flag): flag ? "true" : "false"
        case .bulk(let bytes): RESP.display(bytes)
        case .null: nil
        case .array, .map: RESP.jsonish(self)
        }
    }

    var integer: Int64? {
        switch self {
        case .integer(let number): number
        case .bulk, .simple: text.flatMap { Int64($0) }
        default: nil
        }
    }

    /// The bytes of a string reply.
    var bytes: [UInt8]? {
        switch self {
        case .bulk(let bytes): bytes
        case .simple(let text): Array(text.utf8)
        default: nil
        }
    }

    /// A bulk string that isn't UTF-8, shown as hex.
    var isBinary: Bool {
        guard case .bulk(let bytes) = self else { return false }
        return String(validating: bytes, as: UTF8.self) == nil
    }

    var isAggregate: Bool {
        switch self {
        case .array, .map: true
        default: false
        }
    }
}

enum RESP {
    /// The commands as RESP arrays of bulk strings, ready to send in one write.
    static func encode(_ commands: [[[UInt8]]]) -> Data {
        var data = Data()
        for command in commands {
            data.append(contentsOf: Array("*\(command.count)\r\n".utf8))
            for argument in command {
                data.append(contentsOf: Array("$\(argument.count)\r\n".utf8))
                data.append(contentsOf: argument)
                data.append(contentsOf: [13, 10])
            }
        }
        return data
    }

    /// UTF-8 text, or 0x… cut to 64 bytes when the bytes aren't text.
    static func display(_ bytes: [UInt8]) -> String {
        if let text = String(validating: bytes, as: UTF8.self) { return text }
        return bytes.withUnsafeBytes { MySQLSession.hex($0) }
    }

    /// Arrays and maps as JSON-ish text, for one cell.
    static func jsonish(_ value: RESPValue) -> String {
        switch value {
        case .array(let items): return "[" + items.map(jsonish).joined(separator: ", ") + "]"
        case .map(let flat):
            let pairs = stride(from: 0, to: flat.count - 1, by: 2).map { "\(jsonish(flat[$0])): \(jsonish(flat[$0 + 1]))" }
            return "{" + pairs.joined(separator: ", ") + "}"
        case .null: return "null"
        case .integer, .double, .bigNumber, .boolean: return value.text ?? ""
        case .error(let message): return quoted("(error) " + message)
        case .simple, .bulk: return quoted(value.text ?? "")
        }
    }

    private static func quoted(_ text: String) -> String {
        var out = "\""
        for scalar in text.unicodeScalars {
            switch scalar {
            case "\"": out += "\\\""
            case "\\": out += "\\\\"
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "\t": out += "\\t"
            case _ where scalar.value < 0x20: out += String(format: "\\u%04x", scalar.value)
            default: out.unicodeScalars.append(scalar)
            }
        }
        return out + "\""
    }
}

/// A malformed reply, or one cut short.
struct RESPProtocolError: Error {
    let message: String
}

/// Reads replies from a byte stream, asking for more bytes when a reply is incomplete.
struct RESPReader {
    private var buffer: [UInt8] = []
    private var position = 0

    /// The next reply. `fill` blocks until more bytes arrive.
    mutating func next(fill: () throws -> [UInt8]) throws -> RESPValue {
        let line = try readLine(fill)
        guard let marker = line.first else { throw RESPProtocolError(message: "empty line") }
        let rest = String(decoding: line.dropFirst(), as: UTF8.self)
        switch marker {
        case UInt8(ascii: "+"): return .simple(rest)
        case UInt8(ascii: "-"): return .error(rest)
        case UInt8(ascii: ":"):
            guard let number = Int64(rest) else { throw RESPProtocolError(message: "bad integer \(rest)") }
            return .integer(number)
        case UInt8(ascii: "$"):
            let length = try count(rest)
            return length < 0 ? .null : .bulk(try readBytes(length, fill))
        case UInt8(ascii: "*"), UInt8(ascii: "~"), UInt8(ascii: ">"):
            let length = try count(rest)
            guard length >= 0 else { return .null }
            return .array(try (0..<length).map { _ in try next(fill: fill) })
        case UInt8(ascii: "%"):
            let length = try count(rest)
            return .map(try (0..<(2 * max(length, 0))).map { _ in try next(fill: fill) })
        case UInt8(ascii: "_"): return .null
        case UInt8(ascii: "#"): return .boolean(rest == "t")
        case UInt8(ascii: ","): return .double(rest)
        case UInt8(ascii: "("): return .bigNumber(rest)
        case UInt8(ascii: "!"):
            return .error(String(decoding: try readBytes(try count(rest), fill), as: UTF8.self))
        case UInt8(ascii: "="):
            // Verbatim string: "txt:" or "mkd:" then the text.
            return .bulk(Array(try readBytes(try count(rest), fill).dropFirst(4)))
        case UInt8(ascii: "|"):
            // Attributes come before the reply they describe; skip them.
            for _ in 0..<(2 * max(try count(rest), 0)) { _ = try next(fill: fill) }
            return try next(fill: fill)
        default:
            throw RESPProtocolError(message: "unexpected reply type \(Character(UnicodeScalar(marker)))")
        }
    }

    private func count(_ text: String) throws -> Int {
        guard let value = Int(text) else { throw RESPProtocolError(message: "bad length \(text)") }
        return value
    }

    private mutating func readLine(_ fill: () throws -> [UInt8]) throws -> [UInt8] {
        var scan = position
        while true {
            if let newline = buffer[scan...].firstIndex(of: 10), newline > position, buffer[newline - 1] == 13 {
                let line = Array(buffer[position..<(newline - 1)])
                position = newline + 1
                return line
            } else if let newline = buffer[scan...].firstIndex(of: 10) {
                // A bare \n; keep looking after it.
                scan = newline + 1
                continue
            }
            let consumed = position
            scan = max(position, buffer.count - 1)
            try more(fill)
            scan -= consumed
        }
    }

    private mutating func readBytes(_ length: Int, _ fill: () throws -> [UInt8]) throws -> [UInt8] {
        guard length >= 0 else { throw RESPProtocolError(message: "bad length") }
        while buffer.count - position < length + 2 { try more(fill) }
        let bytes = Array(buffer[position..<(position + length)])
        guard buffer[position + length] == 13, buffer[position + length + 1] == 10 else {
            throw RESPProtocolError(message: "missing CRLF")
        }
        position += length + 2
        return bytes
    }

    private mutating func more(_ fill: () throws -> [UInt8]) throws {
        if position > 0 {
            buffer.removeSubrange(0..<position)
            position = 0
        }
        let chunk = try fill()
        guard !chunk.isEmpty else { throw RESPProtocolError(message: "no data") }
        buffer.append(contentsOf: chunk)
    }
}

// MARK: - Socket

/// The connection failed or was closed: the link can't be used again.
struct RedisLinkError: Error {
    let message: String
}

/// A value set once from a Network callback and read by the waiting thread.
private final class Slot<Value: Sendable>: Sendable {
    private let value = Mutex<Value?>(nil)
    func set(_ new: Value) { value.withLock { if $0 == nil { $0 = new } } }
    var current: Value? { value.withLock { $0 } }
}

/// One TCP (or TLS) connection to the server, driven synchronously: the
/// session's queue blocks on a semaphore while Network works on its own queue.
final class RedisLink: @unchecked Sendable {
    private let connection: NWConnection
    private let queue = DispatchQueue(label: "dbLens.redis.io")
    private let closed = Atomic<Bool>(false)
    /// Only used from the session's queue.
    private var reader = RESPReader()

    private init(_ connection: NWConnection) {
        self.connection = connection
    }

    deinit { close() }

    var isOpen: Bool { !closed.load(ordering: .acquiring) }

    /// Connects, or throws why not, within `timeout`.
    static func open(host: String, port: Int, tls: Bool, verify: Bool, serverName: String,
                     timeout: Duration = .seconds(10)) throws -> RedisLink {
        guard let endpointPort = NWEndpoint.Port(rawValue: UInt16(clamping: max(port, 0))), port > 0 else {
            throw RedisLinkError(message: String(localized: "The port \(port) is not valid."))
        }
        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        tcp.enableKeepalive = true
        tcp.connectionTimeout = Int(timeout.components.seconds)
        var tlsOptions: NWProtocolTLS.Options?
        if tls {
            let options = NWProtocolTLS.Options()
            let security = options.securityProtocolOptions
            sec_protocol_options_set_tls_server_name(security, serverName)
            if !verify {
                sec_protocol_options_set_verify_block(security, { _, _, complete in complete(true) },
                                                      DispatchQueue(label: "dbLens.redis.verify"))
            }
            tlsOptions = options
        }
        let parameters = NWParameters(tls: tlsOptions, tcp: tcp)
        let link = RedisLink(NWConnection(host: NWEndpoint.Host(host), port: endpointPort, using: parameters))
        try link.start(timeout: timeout, host: host, port: port)
        return link
    }

    private func start(timeout: Duration, host: String, port: Int) throws {
        // nil: ready; otherwise why it failed.
        let outcome = Slot<String?>()
        let signal = DispatchSemaphore(value: 0)
        connection.stateUpdateHandler = { state in
            switch state {
            case .ready: outcome.set(nil)
            case .failed(let error), .waiting(let error): outcome.set(Self.describe(error))
            case .cancelled: outcome.set(String(localized: "The connection was closed."))
            default: return
            }
            signal.signal()
        }
        connection.start(queue: queue)
        let seconds = Double(timeout.components.seconds)
        if signal.wait(timeout: .now() + seconds) == .timedOut {
            close()
            throw RedisLinkError(message: String(localized: "Couldn't connect to \(host):\(String(port)) within \(Int(seconds)) seconds."))
        }
        if case .some(.some(let reason)) = outcome.current {
            close()
            throw RedisLinkError(message: String(localized: "Couldn't connect to \(host):\(String(port)): \(reason)"))
        }
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed, .cancelled: self?.closed.store(true, ordering: .releasing)
            default: break
            }
        }
    }

    /// Closes the socket. Safe from any thread; a call waiting on the link fails.
    func close() {
        if !closed.exchange(true, ordering: .acquiringAndReleasing) { connection.cancel() }
    }

    /// Sends commands in one write (a pipeline).
    func send(_ commands: [[[UInt8]]]) throws {
        let data = RESP.encode(commands)
        let failure: String? = try wait { done in
            connection.send(content: data, completion: .contentProcessed { error in done(error.map(Self.describe)) })
        }
        if let failure {
            close()
            throw RedisLinkError(message: failure)
        }
    }

    /// The next reply, waiting as long as it takes (until closed).
    func read() throws -> RESPValue {
        do {
            return try reader.next { try receive() }
        } catch let error as RESPProtocolError {
            close()
            throw RedisLinkError(message: String(localized: "The server sent a reply dbLens doesn't understand (\(error.message))."))
        }
    }

    private enum Chunk: Sendable {
        case bytes([UInt8])
        case end
        case failure(String)
    }

    private func receive() throws -> [UInt8] {
        let chunk: Chunk = try wait { done in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 16) { data, _, isComplete, error in
                if let data, !data.isEmpty { done(.bytes([UInt8](data))) }
                else if let error { done(.failure(Self.describe(error))) }
                else if isComplete { done(.end) }
                else { done(.bytes([])) }
            }
        }
        switch chunk {
        case .bytes(let bytes) where !bytes.isEmpty:
            return bytes
        case .failure(let message):
            close()
            throw RedisLinkError(message: message)
        default:
            close()
            throw RedisLinkError(message: String(localized: "The server closed the connection."))
        }
    }

    /// Starts a Network call and waits for its callback, failing when the link closes.
    private func wait<Value: Sendable>(_ start: (@escaping @Sendable (Value) -> Void) -> Void) throws -> Value {
        guard isOpen else { throw RedisLinkError(message: String(localized: "The connection was closed.")) }
        let result = Slot<Value>()
        let signal = DispatchSemaphore(value: 0)
        start { value in
            result.set(value)
            signal.signal()
        }
        while signal.wait(timeout: .now() + .milliseconds(200)) == .timedOut {
            if !isOpen, result.current == nil {
                throw RedisLinkError(message: String(localized: "The connection was closed."))
            }
        }
        guard let value = result.current else { throw RedisLinkError(message: String(localized: "The connection was closed.")) }
        return value
    }

    private static func describe(_ error: NWError) -> String {
        switch error {
        case .posix(let code):
            return String(cString: strerror(code.rawValue))
        case .tls(let status):
            let text = SecCopyErrorMessageString(status, nil) as String?
            return String(localized: "TLS failed: \(text ?? String(status))")
        default:
            return error.localizedDescription
        }
    }
}

/// Where to connect and how to log in; reused to reconnect after a cancel.
struct RedisTarget: Sendable {
    let host: String
    let port: Int
    let serverName: String
    let url: RedisURL

    init(_ url: RedisURL, localPort: Int?) {
        self.url = url
        host = localPort == nil ? url.host : "127.0.0.1"
        port = localPort ?? url.port
        serverName = url.host
    }

    /// Connects, logs in, selects the database and pings, or throws the server's message.
    func open(database: Int) throws -> RedisLink {
        let link = try RedisLink.open(host: host, port: port, tls: url.tls, verify: url.verifyTLS, serverName: serverName)
        var commands: [[[UInt8]]] = []
        if let password = url.password {
            commands.append(RedisSession.args(["AUTH"] + (url.user.map { [$0] } ?? []) + [password]))
        }
        if database != 0 { commands.append(RedisSession.args(["SELECT", String(database)])) }
        commands.append(RedisSession.args(["PING"]))
        do {
            try link.send(commands)
            let replies = try commands.map { _ in try link.read() }
            for case .error(let message) in replies { throw DatabaseError(message: message) }
        } catch {
            link.close()
            throw error
        }
        return link
    }
}

/// The current link and whether a call is running, shared with `cancel()`.
private final class RedisLinkState: Sendable {
    private let state = Mutex<(link: RedisLink?, running: Bool, requested: Bool)>((nil, false, false))

    var link: RedisLink? { state.withLock { $0.link } }

    /// Keeps a new link, unless a cancel came while it connected.
    func install(_ link: RedisLink) -> Bool {
        state.withLock { state in
            guard !state.requested else { return false }
            state.link = link
            return true
        }
    }

    func drop(_ link: RedisLink) {
        link.close()
        state.withLock { if $0.link === link { $0.link = nil } }
    }

    func begin() { state.withLock { $0.running = true; $0.requested = false } }

    /// Ends a call; true when it was canceled.
    func end() -> Bool {
        state.withLock { state in
            defer { state.running = false; state.requested = false }
            return state.requested
        }
    }

    var canceled: Bool { state.withLock { $0.requested } }

    /// Closes the socket of a running call. The next call reconnects.
    func cancel() {
        let link: RedisLink? = state.withLock { state in
            guard state.running else { return nil }
            state.requested = true
            defer { state.link = nil }
            return state.link
        }
        link?.close()
    }

    func closeAll() { state.withLock { $0.link }?.close() }
}

// MARK: - Session

/// One Redis or Valkey connection. Keys are the relations; each page shows
/// what a key holds, by type. Calls block on the session's own serial queue.
public actor RedisSession: DatabaseSession {
    public nonisolated var engine: Engine { .redis }
    /// Redis has no identifier quoting: the key as it is.
    public nonisolated func quote(_ identifier: String) -> String { identifier }
    /// The most keys `relations()` lists.
    public nonisolated var keyLimit: Int { 5000 }

    private let queue = DispatchSerialQueue(label: "dbLens.redis")
    public nonisolated var unownedExecutor: UnownedSerialExecutor { queue.asUnownedSerialExecutor() }

    public nonisolated let url: RedisURL
    public nonisolated let serverVersion: String
    /// The SSH tunnel this connection goes through, if any. It closes with the session.
    public nonisolated let tunnel: SSHTunnel?
    private let target: RedisTarget
    private let state = RedisLinkState()
    /// The database in use: the URL's, or the last SELECT run in the panel.
    private var database: Int
    /// The rows of the last page of each key, to check edits against.
    private var shown: [String: ShownPage] = [:]

    /// Connects, through an SSH tunnel to the server's host and port (as the SSH
    /// server sees them) when `ssh` is given, or throws the server's own message.
    public static func connect(_ url: RedisURL, ssh: SSHTunnel.Config?) async throws -> RedisSession {
        guard let ssh else { return try await Task.detached { try RedisSession(url: url, tunnel: nil) }.value }
        let tunnel = try await SSHTunnel.open(ssh, to: url.host, port: url.port)
        do {
            return try await Task.detached { try RedisSession(url: url, tunnel: tunnel) }.value
        } catch {
            tunnel.close()
            throw error
        }
    }

    private init(url: RedisURL, tunnel: SSHTunnel?) throws {
        let target = RedisTarget(url, localPort: tunnel?.localPort)
        let link: RedisLink
        do {
            link = try target.open(database: url.database)
        } catch let error as RedisLinkError {
            throw DatabaseError(message: error.message)
        }
        do {
            try link.send([Self.args(["INFO", "server"])])
            let info = try link.read()
            if case .error(let message) = info { throw DatabaseError(message: message) }
            serverVersion = Self.version(info: info.text ?? "")
        } catch let error as RedisLinkError {
            throw DatabaseError(message: error.message)
        }
        self.url = url
        self.tunnel = tunnel
        self.target = target
        database = url.database
        _ = state.install(link)
    }

    deinit {
        state.closeAll()
        tunnel?.close()
    }

    /// "7.4.1" from INFO server; Valkey's own version when it gives one.
    static func version(info: String) -> String {
        var fields: [String: String] = [:]
        for line in info.split(whereSeparator: \.isNewline) {
            let parts = line.split(separator: ":", maxSplits: 1)
            if parts.count == 2 { fields[String(parts[0])] = parts[1].trimmingCharacters(in: .whitespaces) }
        }
        return fields["valkey_version"] ?? fields["redis_version"] ?? ""
    }

    /// Closes the socket of the running command (Redis can't cancel one on the
    /// server); the next call reconnects. Safe to call from any thread.
    public nonisolated func cancel() { state.cancel() }

    // MARK: Talking to the server

    static func args(_ parts: [String]) -> [[UInt8]] { parts.map { Array($0.utf8) } }

    /// Runs one public call: reconnects when needed and turns a closed or
    /// canceled connection into a message.
    private func guarded<T>(_ body: () throws -> T) throws -> T {
        state.begin()
        do {
            let value = try body()
            _ = state.end()
            return value
        } catch let error as RedisLinkError {
            let canceled = state.end()
            if let link = state.link, !link.isOpen { state.drop(link) }
            if canceled { throw DatabaseError(message: String(localized: "The command was canceled.")) }
            throw DatabaseError(message: error.message)
        } catch {
            _ = state.end()
            throw error
        }
    }

    private func link() throws -> RedisLink {
        if let link = state.link {
            if link.isOpen { return link }
            state.drop(link)
        }
        if state.canceled { throw RedisLinkError(message: "canceled") }
        let link = try target.open(database: database)
        guard state.install(link) else {
            link.close()
            throw RedisLinkError(message: "canceled")
        }
        return link
    }

    /// Sends commands in one write and reads every reply; error replies are returned, not thrown.
    private func pipeline(_ commands: [[[UInt8]]]) throws -> [RESPValue] {
        guard !commands.isEmpty else { return [] }
        let link = try link()
        try link.send(commands)
        return try commands.map { _ in try link.read() }
    }

    /// One command; an error reply is thrown with the server's text.
    @discardableResult
    private func command(_ parts: [String]) throws -> RESPValue {
        try raw(Self.args(parts))
    }

    @discardableResult
    private func raw(_ parts: [[UInt8]]) throws -> RESPValue {
        let reply = try pipeline([parts])[0]
        if case .error(let message) = reply { throw DatabaseError(message: message) }
        return reply
    }

    // MARK: The command panel

    /// Commands that never return, so the panel would wait forever.
    private static let streaming: Set<String> = ["SUBSCRIBE", "PSUBSCRIBE", "SSUBSCRIBE", "MONITOR", "SYNC", "PSYNC"]

    /// Runs commands, one per line, as redis-cli would. Blank lines and lines
    /// starting with # are skipped. An error stops the run with the server's text.
    public func execute(_ text: String) throws -> Execution {
        let clock = ContinuousClock()
        let start = clock.now
        let commands = try Self.commands(in: text)
        for parts in commands {
            let name = Self.name(parts)
            if Self.streaming.contains(name) {
                throw DatabaseError(message: String(localized: "\(name) never returns, so it can't run here. Use redis-cli for it."))
            }
        }
        let results = try guarded {
            try commands.map { parts in
                let reply = try raw(parts)
                switch Self.name(parts) {
                case "SELECT":
                    if reply == .simple("OK"), parts.count > 1, let index = Int(String(decoding: parts[1], as: UTF8.self)) {
                        database = index
                        shown = [:]
                    }
                case "QUIT":
                    if let link = state.link { state.drop(link) }
                default:
                    break
                }
                return Self.result(reply, for: parts)
            }
        }
        return Execution(results: results, duration: clock.now - start)
    }

    /// Every command line of the text, split into arguments.
    static func commands(in text: String) throws -> [[[UInt8]]] {
        try text.split(whereSeparator: \.isNewline).compactMap { line in
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            guard !trimmed.isEmpty, !trimmed.hasPrefix("#") else { return nil }
            let parts = try tokenize(trimmed)
            return parts.isEmpty ? nil : parts
        }
    }

    /// Splits a line like redis-cli: spaces between arguments; "double quotes"
    /// with \" \\ \n \r \t \b \a and \xHH escapes; 'single quotes' taken as
    /// written (only \' escapes a quote). A quote must end its argument.
    static func tokenize(_ line: String) throws -> [[UInt8]] {
        let bytes = Array(line.utf8)
        var parts: [[UInt8]] = []
        var index = 0
        let unbalanced = DatabaseError(message: String(localized: "Unbalanced quotes in: \(line)"))
        func isSpace(_ byte: UInt8) -> Bool { [9, 10, 11, 12, 13, 32].contains(byte) }
        func hexValue(_ byte: UInt8) -> UInt8? {
            switch byte {
            case 48...57: byte - 48
            case 65...70: byte - 55
            case 97...102: byte - 87
            default: nil
            }
        }
        while true {
            while index < bytes.count, isSpace(bytes[index]) { index += 1 }
            guard index < bytes.count else { return parts }
            var current: [UInt8] = []
            var quote: UInt8?
            while index < bytes.count {
                let byte = bytes[index]
                if let open = quote {
                    if open == UInt8(ascii: "\""), byte == UInt8(ascii: "\\"), index + 1 < bytes.count {
                        let next = bytes[index + 1]
                        if next == UInt8(ascii: "x"), index + 3 < bytes.count,
                           let high = hexValue(bytes[index + 2]), let low = hexValue(bytes[index + 3]) {
                            current.append(high << 4 | low)
                            index += 4
                            continue
                        }
                        switch next {
                        case UInt8(ascii: "n"): current.append(10)
                        case UInt8(ascii: "r"): current.append(13)
                        case UInt8(ascii: "t"): current.append(9)
                        case UInt8(ascii: "b"): current.append(8)
                        case UInt8(ascii: "a"): current.append(7)
                        default: current.append(next)
                        }
                        index += 2
                    } else if open == UInt8(ascii: "'"), byte == UInt8(ascii: "\\"), index + 1 < bytes.count,
                              bytes[index + 1] == UInt8(ascii: "'") {
                        current.append(UInt8(ascii: "'"))
                        index += 2
                    } else if byte == open {
                        index += 1
                        // The closing quote must be followed by a space or the end.
                        guard index >= bytes.count || isSpace(bytes[index]) else { throw unbalanced }
                        quote = nil
                        break
                    } else {
                        current.append(byte)
                        index += 1
                    }
                } else if isSpace(byte) {
                    break
                } else if byte == UInt8(ascii: "\""), current.isEmpty {
                    quote = byte
                    index += 1
                } else if byte == UInt8(ascii: "'"), current.isEmpty {
                    quote = byte
                    index += 1
                } else {
                    current.append(byte)
                    index += 1
                }
            }
            if quote != nil { throw unbalanced }
            parts.append(current)
        }
    }

    static func name(_ parts: [[UInt8]]) -> String {
        String(decoding: parts.first ?? [], as: UTF8.self).uppercased()
    }

    /// Commands whose flat replies alternate two things, and what to call them.
    private static func pairNames(_ parts: [[UInt8]]) -> (String, String)? {
        let words = parts.map { String(decoding: $0, as: UTF8.self).uppercased() }
        let name = words.first ?? ""
        let sub = words.count > 1 ? words[1] : ""
        switch name {
        case "HGETALL": return ("field", "value")
        case "CONFIG" where sub == "GET": return ("field", "value")
        case "HRANDFIELD" where words.contains("WITHVALUES"): return ("field", "value")
        case "ZPOPMIN", "ZPOPMAX": return ("member", "score")
        case "ZRANGE", "ZRANGEBYSCORE", "ZREVRANGE", "ZREVRANGEBYSCORE", "ZRANDMEMBER",
             "ZUNION", "ZINTER", "ZDIFF":
            return words.contains("WITHSCORES") ? ("member", "score") : nil
        default: return nil
        }
    }

    /// A reply as a grid: arrays become rows, pairs two columns, the rest one cell.
    static func result(_ reply: RESPValue, for parts: [[UInt8]]) -> QueryResult {
        let name = Self.name(parts)
        var columns = ["value"]
        var rows: [[String?]]
        var counted = true
        switch reply {
        case .array(let items):
            if ["XRANGE", "XREVRANGE"].contains(name), let entries = streamEntries(items) {
                columns = ["id", "fields"]
                rows = entries
            } else if let names = pairNames(parts), let pairRows = Self.pairs(items) {
                columns = [names.0, names.1]
                rows = pairRows
            } else {
                rows = items.map { [$0.text] }
            }
        case .map(let flat):
            columns = ["field", "value"]
            rows = stride(from: 0, to: flat.count - 1, by: 2).map { [flat[$0].text, flat[$0 + 1].text] }
        default:
            rows = [[reply.text]]
            counted = false
        }
        let total = rows.count
        let kept = Array(rows.prefix(QueryResult.rowLimit))
        return QueryResult(columns: columns.map { Column(name: $0) }, rows: kept,
                           command: counted ? "\(name) \(total)" : name,
                           truncated: total > kept.count, totalRows: total)
    }

    /// A flat array of scalars of even length, or an array of 2-element arrays, as pairs.
    private static func pairs(_ items: [RESPValue]) -> [[String?]]? {
        if items.allSatisfy({ !$0.isAggregate }), items.count.isMultiple(of: 2) {
            return stride(from: 0, to: items.count, by: 2).map { [items[$0].text, items[$0 + 1].text] }
        }
        var rows: [[String?]] = []
        for item in items {
            guard case .array(let pair) = item, pair.count == 2, !pair[0].isAggregate, !pair[1].isAggregate else { return nil }
            rows.append([pair[0].text, pair[1].text])
        }
        return rows
    }

    /// XRANGE entries: [id, [field, value, …]] as id and "field=value, …".
    private static func streamEntries(_ items: [RESPValue]) -> [[String?]]? {
        var rows: [[String?]] = []
        for item in items {
            guard case .array(let entry) = item, entry.count == 2 else { return nil }
            let fields: [RESPValue]
            switch entry[1] {
            case .array(let flat), .map(let flat): fields = flat
            case .null: fields = []
            default: return nil
            }
            rows.append([entry[0].text, fieldText(fields)])
        }
        return rows
    }

    private static func fieldText(_ flat: [RESPValue]) -> String {
        stride(from: 0, to: flat.count - 1, by: 2)
            .map { "\(flat[$0].text ?? "")=\(flat[$0 + 1].text ?? "")" }
            .joined(separator: ", ")
    }

    // MARK: Browsing

    /// The database's keys, found with SCAN (at most `keyLimit`), grouped by
    /// the prefix before the first ":".
    public func relations() throws -> [Relation] {
        try guarded {
            var cursor = "0"
            var seen = Set<[UInt8]>()
            var keys: [[UInt8]] = []
            repeat {
                let reply = try command(["SCAN", cursor, "COUNT", "1000"])
                let (next, batch) = try Self.scanReply(reply)
                for key in batch {
                    guard let bytes = key.bytes, seen.insert(bytes).inserted else { continue }
                    keys.append(bytes)
                }
                cursor = next
            } while cursor != "0" && keys.count < keyLimit
            keys = Array(keys.prefix(keyLimit))
            let types = try pipeline(keys.map { [Array("TYPE".utf8), $0] })
            var relations: [Relation] = []
            for (key, type) in zip(keys, types) {
                if case .error = type { continue }
                guard let subtype = type.text, subtype != "none" else { continue }
                let name = RESP.display(key)
                let schema = name.firstIndex(of: ":").map { String(name[..<$0]) } ?? ""
                relations.append(Relation(schema: schema, name: name, kind: .key, estimate: -1, subtype: subtype))
            }
            return relations.sorted { a, b in a.schema == b.schema ? a.name < b.name : a.schema < b.schema }
        }
    }

    private static func scanReply(_ reply: RESPValue) throws -> (String, [RESPValue]) {
        guard case .array(let parts) = reply, parts.count == 2, let cursor = parts[0].text,
              case .array(let batch) = parts[1] else {
            throw DatabaseError(message: String(localized: "The server's SCAN reply wasn't understood."))
        }
        return (cursor, batch)
    }

    /// One page of what a key holds, by type, with a note on its TTL.
    public func page(of relation: Relation, limit: Int, offset: Int, search: String = "") throws -> QueryResult {
        try guarded {
            let key = relation.name
            let head = try pipeline([Self.args(["TYPE", key]), Self.args(["TTL", key])])
            if case .error(let message) = head[0] { throw DatabaseError(message: message) }
            let type = head[0].text ?? "none"
            guard type != "none" else { throw Self.gone }
            let ttl = head[1].integer ?? -1
            let page = search.isEmpty
                ? try read(key, type: type, limit: max(limit, 0), offset: max(offset, 0))
                : Self.filtered(try read(key, type: type, limit: QueryResult.rowLimit, offset: 0), search,
                                limit: max(limit, 0), offset: max(offset, 0))
            shown[relation.id] = ShownPage(keys: page.keys?.values ?? [], values: page.rows.map { $0.last ?? nil },
                                           binary: page.binary)
            var result = QueryResult(columns: page.columns, rows: page.rows, command: page.command,
                                     truncated: page.truncated, totalRows: page.rows.count, keys: page.keys)
            result.note = ttl >= 0 ? String(localized: "\(type) · TTL \(ttl) s") : String(localized: "\(type) · no expiry")
            return result
        }
    }

    /// The rows of a whole key that contain `search` (case-insensitive), then one page of them.
    private static func filtered(_ page: Page, _ search: String, limit: Int, offset: Int) -> Page {
        let hits = page.rows.indices.filter { page.rows[$0].contains { $0?.localizedCaseInsensitiveContains(search) == true } }
        let slice = Array(hits.dropFirst(offset).prefix(limit))
        var result = page
        result.rows = slice.map { page.rows[$0] }
        result.binary = slice.map { page.binary.indices.contains($0) ? page.binary[$0] : false }
        if let keys = page.keys, keys.values.count == page.rows.count {
            result.keys = RowKeys(columns: keys.columns, kind: keys.kind, values: slice.map { keys.values[$0] })
        }
        return result
    }

    private static var gone: DatabaseError {
        DatabaseError(message: String(localized: "This key no longer exists. Refresh the list of keys."))
    }

    /// A page before it becomes a QueryResult, with which rows hold bytes that aren't text.
    private struct Page {
        var columns: [Column]
        var rows: [[String?]] = []
        var binary: [Bool] = []
        var keys: RowKeys?
        var command: String
        var truncated = false
    }

    /// What a page showed, to check edits against.
    private struct ShownPage {
        var keys: [[String?]]
        /// The last column of each row: the value a list index held.
        var values: [String?]
        var binary: [Bool]
    }

    private func read(_ key: String, type: String, limit: Int, offset: Int) throws -> Page {
        let keyRow = RowKeys(columns: ["key"], kind: .primaryKey, values: [[key]])
        switch type {
        case "string":
            let value = try command(["GET", key])
            return Page(columns: [Column(name: "value")], rows: [[value.text]], binary: [value.isBinary],
                        keys: keyRow, command: "GET")
        case "hash":
            let (flat, truncated) = try scanAll("HSCAN", key, pairs: true)
            var byField: [[UInt8]: (RESPValue, RESPValue)] = [:]
            for index in stride(from: 0, to: flat.count - 1, by: 2) {
                byField[flat[index].bytes ?? []] = (flat[index], flat[index + 1])
            }
            let sorted = byField.values.sorted { ($0.0.text ?? "") < ($1.0.text ?? "") }
            let capped = Array(sorted.prefix(QueryResult.rowLimit))
            let slice = Array(capped.dropFirst(offset).prefix(limit))
            return Page(columns: [Column(name: "field"), Column(name: "value")],
                        rows: slice.map { [$0.0.text, $0.1.text] },
                        binary: slice.map { $0.0.isBinary || $0.1.isBinary },
                        keys: RowKeys(columns: ["field"], kind: .primaryKey, values: slice.map { [$0.0.text] }),
                        command: "HSCAN \(slice.count)", truncated: truncated || sorted.count > capped.count)
        case "list":
            var items: [RESPValue] = []
            if limit > 0 { items = Self.items(try command(["LRANGE", key, String(offset), String(offset + limit - 1)])) }
            return Page(columns: [Column(name: "index", type: "integer"), Column(name: "value")],
                        rows: items.enumerated().map { [String(offset + $0.offset), $0.element.text] },
                        binary: items.map(\.isBinary),
                        keys: RowKeys(columns: ["index"], kind: .primaryKey,
                                      values: items.indices.map { [String(offset + $0)] }),
                        command: "LRANGE \(items.count)")
        case "set":
            let (flat, truncated) = try scanAll("SSCAN", key, pairs: false)
            var byMember: [[UInt8]: RESPValue] = [:]
            for member in flat { byMember[member.bytes ?? []] = member }
            let sorted = byMember.values.sorted { ($0.text ?? "") < ($1.text ?? "") }
            let capped = Array(sorted.prefix(QueryResult.rowLimit))
            let slice = Array(capped.dropFirst(offset).prefix(limit))
            return Page(columns: [Column(name: "member")], rows: slice.map { [$0.text] }, binary: slice.map(\.isBinary),
                        keys: RowKeys(columns: ["member"], kind: .primaryKey, values: slice.map { [$0.text] }),
                        command: "SSCAN \(slice.count)", truncated: truncated || sorted.count > capped.count)
        case "zset":
            var flat: [RESPValue] = []
            if limit > 0 { flat = Self.items(try command(["ZRANGE", key, String(offset), String(offset + limit - 1), "WITHSCORES"])) }
            let pairs: [(RESPValue, RESPValue)] = flat.allSatisfy({ !$0.isAggregate })
                ? stride(from: 0, to: flat.count - 1, by: 2).map { (flat[$0], flat[$0 + 1]) }
                : flat.compactMap { item in
                    guard case .array(let pair) = item, pair.count == 2 else { return nil }
                    return (pair[0], pair[1])
                }
            return Page(columns: [Column(name: "member"), Column(name: "score", type: "double")],
                        rows: pairs.map { [$0.0.text, $0.1.text] }, binary: pairs.map { $0.0.isBinary },
                        keys: RowKeys(columns: ["member"], kind: .primaryKey, values: pairs.map { [$0.0.text] }),
                        command: "ZRANGE \(pairs.count)")
        case "stream":
            var entries: [RESPValue] = []
            if limit > 0 { entries = Self.items(try command(["XRANGE", key, "-", "+", "COUNT", String(offset + limit)])) }
            let rows = Array((Self.streamEntries(entries) ?? []).dropFirst(offset))
            return Page(columns: [Column(name: "id"), Column(name: "fields")], rows: rows,
                        binary: rows.map { _ in false }, keys: nil, command: "XRANGE \(rows.count)")
        case "ReJSON-RL":
            let reply = try pipeline([Self.args(["JSON.GET", key, "INDENT", "  ", "NEWLINE", "\n", "SPACE", " "])])[0]
            return Page(columns: [Column(name: "value")], rows: [[reply.text]], binary: [reply.isBinary],
                        keys: keyRow, command: "JSON.GET")
        default:
            return Page(columns: [Column(name: "value")],
                        rows: [[String(localized: "dbLens can't show \(type) keys yet. Use the command panel.")]],
                        binary: [false], keys: nil, command: "TYPE")
        }
    }

    private static func items(_ reply: RESPValue) -> [RESPValue] {
        if case .array(let items) = reply { return items }
        return []
    }

    /// Every element HSCAN or SSCAN returns, stopping a little past the row limit.
    private func scanAll(_ name: String, _ key: String, pairs: Bool) throws -> ([RESPValue], Bool) {
        var cursor = "0"
        var all: [RESPValue] = []
        let cap = QueryResult.rowLimit * (pairs ? 2 : 1)
        repeat {
            let (next, batch) = try Self.scanReply(try command([name, key, cursor, "COUNT", "1000"]))
            all += batch
            cursor = next
        } while cursor != "0" && all.count <= cap
        return (all, cursor != "0")
    }

    /// The number of rows a key's page has: 1 for a string, else its length.
    public func count(of relation: Relation, search: String = "") throws -> Int? {
        if !search.isEmpty {
            return try page(of: relation, limit: QueryResult.rowLimit, offset: 0, search: search).rows.count
        }
        return try guarded {
            let type = try command(["TYPE", relation.name]).text ?? "none"
            let length: String
            switch type {
            case "string", "ReJSON-RL": return 1
            case "hash": length = "HLEN"
            case "list": length = "LLEN"
            case "set": length = "SCARD"
            case "zset": length = "ZCARD"
            case "stream": length = "XLEN"
            default: return nil
            }
            return try command([length, relation.name]).integer.map { Int($0) }
        }
    }

    private func currentType(_ key: String) throws -> String {
        let type = try command(["TYPE", key]).text ?? "none"
        guard type != "none" else { throw Self.rowChanged }
        return type
    }

    // MARK: Editing

    private static var rowChanged: DatabaseError {
        DatabaseError(message: String(localized: "The row changed or was deleted since it was loaded. Reload the table and try again."))
    }

    private static var noNull: DatabaseError {
        DatabaseError(message: String(localized: "Redis has no NULL. Enter a value instead."))
    }

    private static func readOnly(_ type: String) -> DatabaseError {
        DatabaseError(message: String(localized: "\(type) keys can't be edited here. Use the command panel."))
    }

    private static func notEditable(_ column: String) -> DatabaseError {
        DatabaseError(message: String(localized: "The \(column) column can't be changed."))
    }

    /// The new value for a column, if the edit changes it. NULL is refused.
    private static func change(_ pairs: [(String, String?)], _ column: String) throws -> String? {
        guard let pair = pairs.last(where: { $0.0 == column }) else { return nil }
        guard let value = pair.1 else { throw noNull }
        return value
    }

    /// The row as it was shown; refuses rows whose values were shown as hex.
    private func shownRow(_ relation: Relation, keys: RowKeys, row: Int) throws -> (key: String?, value: String?) {
        guard row >= 0, row < keys.values.count else { throw Self.rowChanged }
        let key = keys.values[row].first ?? nil
        guard let page = shown[relation.id], row < page.keys.count, page.keys[row] == keys.values[row] else {
            return (key, nil)
        }
        if page.binary[row] {
            throw DatabaseError(message: String(localized: "This row holds bytes that aren't text (shown as hex), so it can't be edited here. Use the command panel."))
        }
        return (key, page.values[row])
    }

    /// WATCH the key, check it, then run the commands in MULTI/EXEC. A failed
    /// check or a key changed meanwhile gives the "row changed" message.
    private func transaction(_ key: String, check: () throws -> Bool, _ commands: () -> [[String]]) throws -> [RESPValue] {
        try command(["WATCH", key])
        let passed: Bool
        do {
            passed = try check()
        } catch {
            _ = try? command(["UNWATCH"])
            throw error
        }
        guard passed else {
            try command(["UNWATCH"])
            throw Self.rowChanged
        }
        let replies = try pipeline([Self.args(["MULTI"])] + commands().map(Self.args) + [Self.args(["EXEC"])])
        for case .error(let message) in replies.dropLast() { throw DatabaseError(message: message) }
        switch replies.last {
        case .error(let message): throw DatabaseError(message: message)
        case .array(let results):
            for case .error(let message) in results { throw DatabaseError(message: message) }
            return results
        default:
            throw Self.rowChanged
        }
    }

    private func outcome(_ sql: String) -> EditOutcome {
        EditOutcome(sql: sql, command: String(sql.prefix { $0 != " " }), affected: 1)
    }

    /// Changes one row of a key's page. `changes` are (column, value) pairs.
    public func update(_ relation: Relation, keys: RowKeys, row: Int, changes: [(String, String?)]) throws -> EditOutcome {
        guard !changes.isEmpty else { return EditOutcome(sql: "", command: "UPDATE 0", affected: 0) }
        return try guarded {
            let key = relation.name
            let type = try currentType(key)
            let shownRow = try shownRow(relation, keys: keys, row: row)
            let allowed: Set<String> = switch type {
            case "string", "ReJSON-RL", "list": ["value"]
            case "hash": ["field", "value"]
            case "set": ["member"]
            case "zset": ["member", "score"]
            default: []
            }
            guard !allowed.isEmpty else { throw Self.readOnly(type) }
            if let column = changes.first(where: { !allowed.contains($0.0) })?.0 { throw Self.notEditable(column) }
            switch type {
            case "string":
                guard let value = try Self.change(changes, "value") else { return EditOutcome(sql: "", command: "SET", affected: 0) }
                guard try command(["SET", key, value, "KEEPTTL", "XX"]) != .null else { throw Self.rowChanged }
                return outcome("SET \(key) ? KEEPTTL XX")
            case "ReJSON-RL":
                guard let value = try Self.change(changes, "value") else { return EditOutcome(sql: "", command: "JSON.SET", affected: 0) }
                guard try command(["JSON.SET", key, "$", value, "XX"]) != .null else { throw Self.rowChanged }
                return outcome("JSON.SET \(key) $ ? XX")
            case "hash":
                return try updateHash(key, field: shownRow.key, changes: changes)
            case "list":
                guard let value = try Self.change(changes, "value") else { return EditOutcome(sql: "", command: "LSET", affected: 0) }
                guard let index = shownRow.key, let old = shownRow.value else { throw Self.rowChanged }
                _ = try transaction(key, check: {
                    try command(["LINDEX", key, index]).bytes == Array(old.utf8)
                }, { [["LSET", key, index, value]] })
                return outcome("LSET \(key) \(index) ?")
            case "set":
                guard let member = try Self.change(changes, "member"), let old = shownRow.key else {
                    return EditOutcome(sql: "", command: "SADD", affected: 0)
                }
                guard member != old else { return outcome("SADD \(key) ?") }
                if try command(["SISMEMBER", key, member]).integer == 1 { throw Self.exists(member) }
                let results = try transaction(key, check: { try command(["SISMEMBER", key, old]).integer == 1 },
                                              { [["SREM", key, old], ["SADD", key, member]] })
                guard results.allSatisfy({ $0.integer == 1 }) else { throw Self.rowChanged }
                return outcome("SREM \(key) ?; SADD \(key) ?")
            default:
                return try updateSortedSet(key, member: shownRow.key, changes: changes)
            }
        }
    }

    private static func exists(_ name: String) -> DatabaseError {
        DatabaseError(message: String(localized: "“\(name)” is already there. Pick another name."))
    }

    private func updateHash(_ key: String, field: String?, changes: [(String, String?)]) throws -> EditOutcome {
        guard let field else { throw Self.rowChanged }
        let newField = try Self.change(changes, "field") ?? field
        let newValue = try Self.change(changes, "value")
        if newField == field {
            guard let newValue else { return EditOutcome(sql: "", command: "HSET", affected: 0) }
            _ = try transaction(key, check: { try command(["HEXISTS", key, field]).integer == 1 },
                                { [["HSET", key, field, newValue]] })
            return outcome("HSET \(key) ? ?")
        }
        // A new field name: HSET the new one and HDEL the old one together.
        if try command(["HEXISTS", key, newField]).integer == 1 { throw Self.exists(newField) }
        var value = newValue
        let results = try transaction(key, check: {
            guard try command(["HEXISTS", key, field]).integer == 1 else { return false }
            if value == nil {
                let old = try command(["HGET", key, field])
                guard !old.isBinary, let text = old.text else { return false }
                value = text
            }
            return true
        }, { [["HSET", key, newField, value ?? ""], ["HDEL", key, field]] })
        guard results.count == 2, results[1].integer == 1 else { throw Self.rowChanged }
        return outcome("HSET \(key) ? ?; HDEL \(key) ?")
    }

    private func updateSortedSet(_ key: String, member: String?, changes: [(String, String?)]) throws -> EditOutcome {
        guard let member else { throw Self.rowChanged }
        let newMember = try Self.change(changes, "member") ?? member
        let newScore = try Self.change(changes, "score")
        if newMember == member {
            guard let newScore else { return EditOutcome(sql: "", command: "ZADD", affected: 0) }
            _ = try transaction(key, check: { try command(["ZSCORE", key, member]) != .null },
                                { [["ZADD", key, "XX", newScore, member]] })
            return outcome("ZADD \(key) XX ? ?")
        }
        if try command(["ZSCORE", key, newMember]) != .null { throw Self.exists(newMember) }
        var score = newScore
        let results = try transaction(key, check: {
            let old = try command(["ZSCORE", key, member])
            guard let text = old.text else { return false }
            score = score ?? text
            return true
        }, { [["ZREM", key, member], ["ZADD", key, score ?? "0", newMember]] })
        guard results.allSatisfy({ $0.integer == 1 }) else { throw Self.rowChanged }
        return outcome("ZREM \(key) ?; ZADD \(key) ? ?")
    }

    /// Adds a field, item or member to a key. Columns left out are empty (score 0).
    public func insert(_ relation: Relation, values: [(String, String?)]) throws -> EditOutcome {
        try guarded {
            let key = relation.name
            let type = try currentType(key)
            func value(_ column: String) throws -> String { try Self.change(values, column) ?? "" }
            switch type {
            case "hash":
                let field = try value("field")
                guard !field.isEmpty else { throw Self.fill("field") }
                guard try command(["HSETNX", key, field, try value("value")]).integer == 1 else { throw Self.exists(field) }
                return outcome("HSETNX \(key) ? ?")
            case "list":
                try command(["RPUSH", key, try value("value")])
                return outcome("RPUSH \(key) ?")
            case "set":
                let member = try value("member")
                guard try command(["SADD", key, member]).integer == 1 else { throw Self.exists(member) }
                return outcome("SADD \(key) ?")
            case "zset":
                let member = try value("member")
                let score = try value("score")
                guard try command(["ZADD", key, "NX", score.isEmpty ? "0" : score, member]).integer == 1 else {
                    throw Self.exists(member)
                }
                return outcome("ZADD \(key) NX ? ?")
            case "string", "ReJSON-RL":
                throw DatabaseError(message: String(localized: "A \(type) key holds one value. Edit it instead of adding a row."))
            default:
                throw Self.readOnly(type)
            }
        }
    }

    private static func fill(_ column: String) -> DatabaseError {
        DatabaseError(message: String(localized: "Enter a \(column)."))
    }

    /// Removes one field, item or member. Whole keys are deleted with DEL in the command panel.
    public func delete(_ relation: Relation, keys: RowKeys, row: Int) throws -> EditOutcome {
        try guarded {
            let key = relation.name
            let type = try currentType(key)
            let shownRow = try shownRow(relation, keys: keys, row: row)
            guard let name = shownRow.key else { throw Self.rowChanged }
            let removal: String
            switch type {
            case "hash": removal = "HDEL"
            case "set": removal = "SREM"
            case "zset": removal = "ZREM"
            case "list":
                guard let old = shownRow.value else { throw Self.rowChanged }
                // LREM removes by value, so the item becomes a value nothing else holds first.
                let sentinel = "dblens:deleted:" + UUID().uuidString
                let results = try transaction(key, check: {
                    try command(["LINDEX", key, name]).bytes == Array(old.utf8)
                }, { [["LSET", key, name, sentinel], ["LREM", key, "1", sentinel]] })
                guard results.last?.integer == 1 else { throw Self.rowChanged }
                return outcome("LSET \(key) \(name) ?; LREM \(key) 1 ?")
            case "string", "ReJSON-RL":
                throw DatabaseError(message: String(localized: "To delete the whole key, run DEL in the command panel."))
            default:
                throw Self.readOnly(type)
            }
            guard try command([removal, key, name]).integer == 1 else { throw Self.rowChanged }
            return outcome("\(removal) \(key) ?")
        }
    }
}
