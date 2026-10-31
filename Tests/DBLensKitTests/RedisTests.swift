import Foundation
import Testing
@testable import DBLensKit

// MARK: URLs (no server)

@Test func redisAcceptsFullURLs() throws {
    let url = try RedisURL("  redis://ana:s%40nha@cache.example.com:6380/3 ")
    #expect(url.raw == "redis://ana:s%40nha@cache.example.com:6380/3")
    #expect(url.host == "cache.example.com" && url.port == 6380)
    #expect(url.user == "ana" && url.password == "s@nha")
    #expect(url.database == 3 && !url.tls && url.verifyTLS)
    #expect(url.suggestedName == "db 3 @ cache.example.com")
}

@Test func redisDefaults() throws {
    let url = try RedisURL("redis://")
    #expect(url.host == "localhost" && url.port == 6379 && url.database == 0)
    #expect(url.user == nil && url.password == nil && !url.tls && url.verifyTLS)
    #expect(url.suggestedName == "db 0 @ localhost")
}

@Test("Redis formats people paste", arguments: [
    // (pasted, host, port, user, password, database, tls, verify)
    ("redis://:dblens-test@127.0.0.1:56379/0", "127.0.0.1", 56379, nil, "dblens-test", 0, false, true),
    ("redis://default:pw@cache.x.com", "cache.x.com", 6379, "default", "pw", 0, false, true),
    ("redis://cache.x.com?db=5", "cache.x.com", 6379, nil, nil, 5, false, true),
    ("redis://cache.x.com/7?db=5", "cache.x.com", 6379, nil, nil, 7, false, true),
    ("redis://cache.x.com?password=pw&username=ana", "cache.x.com", 6379, "ana", "pw", 0, false, true),
    ("rediss://:pw@cache.x.com:6380/1", "cache.x.com", 6380, nil, "pw", 1, true, true),
    ("rediss://cache.x.com?ssl_cert_reqs=none", "cache.x.com", 6379, nil, nil, 0, true, false),
    ("rediss://cache.x.com?ssl_cert_reqs=CERT_NONE", "cache.x.com", 6379, nil, nil, 0, true, true),
    ("rediss://cache.x.com?insecure=true", "cache.x.com", 6379, nil, nil, 0, true, false),
    ("rediss://cache.x.com?skip_verify=1", "cache.x.com", 6379, nil, nil, 0, true, false),
    ("REDIS_URL=redis://:pw@cache.x.com:6379/2", "cache.x.com", 6379, nil, "pw", 2, false, true),
    ("export REDIS_URL=\"rediss://ana:pw@cache.x.com\"", "cache.x.com", 6379, "ana", "pw", 0, true, true),
    ("redis://[::1]:6379/0", "::1", 6379, nil, nil, 0, false, true),
    ("REDIS://CACHE.x.com", "CACHE.x.com", 6379, nil, nil, 0, false, true),
] as [(String, String, Int, String?, String?, Int, Bool, Bool)])
func redisPastedFormats(_ text: String, host: String, port: Int, user: String?, password: String?,
                        database: Int, tls: Bool, verify: Bool) throws {
    let url = try RedisURL(text)
    #expect(url.host == host)
    #expect(url.port == port)
    #expect(url.user == user)
    #expect(url.password == password)
    #expect(url.database == database)
    #expect(url.tls == tls)
    #expect(url.verifyTLS == verify)
    #expect(Engine.detect(text) == .redis)
}

@Test func redisRejectsOtherInput() {
    for text in ["", "   ", "postgres://a@b/c", "mysql://a@b/c", "hello", "https://example.com", "redis://host/abc", "redis://host/-1"] {
        #expect(throws: DatabaseError.self, "\(text)") { try RedisURL(text) }
    }
    for text in ["redis+sentinel://h:26379/mymaster", "rediss+sentinel://h", "redis+cluster://h", "redis-sentinel://h", "redis+unix:///tmp/r.sock"] {
        do {
            _ = try RedisURL(text)
            Issue.record("accepted \(text)")
        } catch {
            #expect(error.localizedDescription.contains("not supported yet"), "\(text)")
        }
    }
}

// MARK: Protocol (no server)

/// Reads every reply from `wire`, handing the reader a few bytes at a time.
private func replies(_ wire: String, chunk: Int = 3, count: Int = 1) throws -> [RESPValue] {
    let bytes = Array(wire.utf8)
    var offset = 0
    var reader = RESPReader()
    var values: [RESPValue] = []
    let fill: () throws -> [UInt8] = {
        guard offset < bytes.count else { return [] }
        defer { offset += chunk }
        return Array(bytes[offset..<min(offset + chunk, bytes.count)])
    }
    for _ in 0..<count { values.append(try reader.next(fill: fill)) }
    return values
}

@Test func respReadsRESP2() throws {
    #expect(try replies("+OK\r\n") == [.simple("OK")])
    #expect(try replies("-ERR bad thing\r\n") == [.error("ERR bad thing")])
    #expect(try replies(":-42\r\n") == [.integer(-42)])
    #expect(try replies("$5\r\nhe\r\no\r\n", chunk: 1) == [.bulk(Array("he\r\no".utf8))])
    #expect(try replies("$0\r\n\r\n") == [.bulk([])])
    #expect(try replies("$-1\r\n") == [.null])
    #expect(try replies("*-1\r\n") == [.null])
    #expect(try replies("*3\r\n:1\r\n$1\r\na\r\n*1\r\n+x\r\n", chunk: 2)
        == [.array([.integer(1), .bulk([97]), .array([.simple("x")])])])
    // Several replies in one read (a pipeline).
    #expect(try replies("+OK\r\n:1\r\n$2\r\nhi\r\n", chunk: 100, count: 3) == [.simple("OK"), .integer(1), .bulk(Array("hi".utf8))])
}

@Test func respReadsRESP3() throws {
    #expect(try replies("_\r\n") == [.null])
    #expect(try replies("#t\r\n") == [.boolean(true)])
    #expect(try replies(",3.14\r\n") == [.double("3.14")])
    #expect(try replies("(12345678901234567890\r\n") == [.bigNumber("12345678901234567890")])
    #expect(try replies("!9\r\nERR boom!\r\n") == [.error("ERR boom!")])
    #expect(try replies("=8\r\ntxt:abcd\r\n") == [.bulk(Array("abcd".utf8))])
    #expect(try replies("%1\r\n+k\r\n:1\r\n") == [.map([.simple("k"), .integer(1)])])
    #expect(try replies("~2\r\n:1\r\n:2\r\n") == [.array([.integer(1), .integer(2)])])
    #expect(try replies("|1\r\n+ttl\r\n:3\r\n+OK\r\n") == [.simple("OK")])
    #expect(throws: RESPProtocolError.self) { try replies("?what\r\n") }
}

@Test func respEncodesCommands() {
    let data = RESP.encode([RedisSession.args(["SET", "k", "ação"]), [[0xFF]]])
    #expect(data == Data("*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$6\r\nação\r\n*1\r\n$1\r\n".utf8) + Data([0xFF, 13, 10]))
}

@Test func respDisplaysValues() {
    #expect(RESPValue.bulk(Array("ação".utf8)).text == "ação")
    #expect(RESPValue.bulk([0x00, 0xFF]).text == "0x00FF" && RESPValue.bulk([0xFF]).isBinary)
    #expect(RESPValue.bulk([UInt8](repeating: 0xC3, count: 100)).text == "0x" + String(repeating: "C3", count: 64) + "…")
    #expect(RESPValue.null.text == nil)
    #expect(RESPValue.array([.bulk(Array("a\"b".utf8)), .integer(2), .null, .array([])]).text == "[\"a\\\"b\", 2, null, []]")
    #expect(RESPValue.map([.simple("k"), .bulk([118])]).text == "{\"k\": \"v\"}")
}

@Test func redisTokenizesLikeRedisCli() throws {
    func words(_ line: String) throws -> [String] { try RedisSession.tokenize(line).map { String(decoding: $0, as: UTF8.self) } }
    #expect(try words("  set  key   value ") == ["set", "key", "value"])
    #expect(try words(#"SET "my key" "a \"quoted\" \\ value\n""#) == ["SET", "my key", "a \"quoted\" \\ value\n"])
    #expect(try words(#"SET k 'it\'s $x \n'"#) == ["SET", "k", "it's $x \\n"])
    #expect(try words(#"SET k """#) == ["SET", "k", ""])
    #expect(try RedisSession.tokenize(#"SET k "\x00\xff\t""#)[2] == [0x00, 0xFF, 9])
    #expect(try words("GET ação") == ["GET", "ação"])
    for bad in [#"SET "unclosed"#, #"SET 'unclosed"#, #"SET "a"b"#] {
        #expect(throws: DatabaseError.self, "\(bad)") { try RedisSession.tokenize(bad) }
    }
    let lines = try RedisSession.commands(in: "# a comment\n\nPING\r\n  # indented comment\nGET a\n")
    #expect(lines.map(RedisSession.name) == ["PING", "GET"])
}

@Test func redisShapesReplies() {
    func shape(_ reply: RESPValue, _ line: String) -> QueryResult {
        RedisSession.result(reply, for: (try? RedisSession.tokenize(line)) ?? [])
    }
    let keys = shape(.array([.bulk([97]), .bulk([98])]), "keys *")
    #expect(keys.columns.map(\.name) == ["value"] && keys.rows == [["a"], ["b"]] && keys.command == "KEYS 2")
    let hash = shape(.array([.bulk([102]), .bulk([118]), .bulk([103]), .bulk([119])]), "hgetall h")
    #expect(hash.columns.map(\.name) == ["field", "value"] && hash.rows == [["f", "v"], ["g", "w"]] && hash.command == "HGETALL 2")
    let zset = shape(.array([.bulk([109]), .bulk(Array("1.5".utf8))]), "ZRANGE z 0 -1 withscores")
    #expect(zset.columns.map(\.name) == ["member", "score"] && zset.rows == [["m", "1.5"]])
    #expect(shape(.array([.bulk([109]), .bulk([110])]), "ZRANGE z 0 -1").columns.map(\.name) == ["value"])
    let resp3 = shape(.array([.array([.bulk([109]), .double("2")])]), "ZRANGE z 0 -1 WITHSCORES")
    #expect(resp3.rows == [["m", "2"]])
    let config = shape(.array([.bulk(Array("maxmemory".utf8)), .bulk([48])]), "CONFIG GET maxmemory")
    #expect(config.columns.map(\.name) == ["field", "value"] && config.rows == [["maxmemory", "0"]])
    let stream = shape(.array([.array([.bulk(Array("1-0".utf8)), .array([.bulk([97]), .bulk([49]), .bulk([98]), .bulk([50])])])]), "XRANGE s - +")
    #expect(stream.columns.map(\.name) == ["id", "fields"] && stream.rows == [["1-0", "a=1, b=2"]])
    let scan = shape(.array([.bulk([48]), .array([.bulk([107])])]), "SCAN 0")
    #expect(scan.rows == [["0"], ["[\"k\"]"]] && scan.command == "SCAN 2")
    let set = shape(.simple("OK"), "set a b")
    #expect(set.rows == [["OK"]] && set.command == "SET" && set.columns.map(\.name) == ["value"])
    #expect(shape(.null, "GET nope").rows == [[nil]])
    #expect(shape(.integer(3), "INCR n").rows == [["3"]])
    #expect(shape(.map([.simple("server"), .simple("redis")]), "HELLO").columns.map(\.name) == ["field", "value"])
}

@Test func redisVersions() {
    #expect(RedisSession.version(info: "# Server\r\nredis_version:7.4.1\r\nredis_mode:standalone\r\n") == "7.4.1")
    #expect(RedisSession.version(info: "# Server\r\nredis_version:7.2.4\r\nserver_name:valkey\r\nvalkey_version:8.0.1\r\n") == "8.0.1")
}

// MARK: Servers (DBLENS_TEST_REDIS_URL, DBLENS_TEST_VALKEY_URL)

private let testServers = ["DBLENS_TEST_REDIS_URL", "DBLENS_TEST_VALKEY_URL"]
    .compactMap { ProcessInfo.processInfo.environment[$0] }

private func connect(_ text: String) async throws -> RedisSession {
    try await RedisSession.connect(try RedisURL(text), ssh: nil)
}

/// A key prefix only this test uses, so tests can run side by side.
private func prefix(_ name: String) -> String { "dblens-\(name)-\(UUID().uuidString.prefix(8))" }

private func clear(_ session: RedisSession, _ prefix: String) async {
    guard let keys = try? await session.execute("KEYS \(prefix)*").main?.rows.compactMap({ $0.first ?? nil }),
          !keys.isEmpty else { return }
    _ = try? await session.execute("DEL " + keys.map { "\"\($0)\"" }.joined(separator: " "))
}

private func key(_ session: RedisSession, _ name: String) async throws -> Relation {
    try #require(try await session.relations().first { $0.name == name })
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisConnectsAndSelects(_ server: String) async throws {
    let session = try await connect(server)
    #expect(session.engine == .redis)
    #expect(session.quote("user:42 x") == "user:42 x")
    #expect(session.keyLimit == 5000)
    #expect(session.serverVersion.first?.isNumber == true)
    print("server:", session.serverVersion)
    #expect(try await session.execute("PING").main?.rows == [["PONG"]])

    // The URL's database is selected, and a SELECT in the panel sticks.
    let name = prefix("db")
    let other = try await connect(server.replacingOccurrences(of: "/0", with: "/3"))
    _ = try await other.execute("SET \(name) three")
    #expect(try await session.execute("EXISTS \(name)").main?.rows == [["0"]])
    #expect(try await session.execute("SELECT 3\nGET \(name)").main?.rows == [["three"]])
    _ = try await session.execute("DEL \(name)\nSELECT 0")
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisListsKeysWithTypes(_ server: String) async throws {
    let session = try await connect(server)
    let p = prefix("rel")
    defer { Task { await clear(session, p) } }
    _ = try await session.execute("""
        SET \(p):s v
        HSET \(p):h f v
        RPUSH \(p):l a
        SADD \(p):set m
        ZADD \(p):z 1 m
        XADD \(p):x * a 1
        SET \(p)noprefix v
        """)
    let relations = try await session.relations().filter { $0.name.hasPrefix(p) }
    #expect(relations.count == 7)
    #expect(relations.allSatisfy { $0.kind == .key && $0.estimate == -1 })
    let types = Dictionary(uniqueKeysWithValues: relations.map { ($0.name, $0.subtype) })
    #expect(types == ["\(p):s": "string", "\(p):h": "hash", "\(p):l": "list", "\(p):set": "set",
                      "\(p):z": "zset", "\(p):x": "stream", "\(p)noprefix": "string"])
    #expect(relations.first { $0.name == "\(p)noprefix" }?.schema == "")
    #expect(relations.first { $0.name == "\(p):h" }?.schema == p)
    let all = try await session.relations()
    #expect(all == all.sorted { $0.schema == $1.schema ? $0.name < $1.name : $0.schema < $1.schema })
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisPagesEveryType(_ server: String) async throws {
    let session = try await connect(server)
    let p = prefix("page")
    defer { Task { await clear(session, p) } }
    _ = try await session.execute("""
        SET \(p):s "ação 🚀" EX 3600
        HSET \(p):h b 2 a 1 c 3
        RPUSH \(p):l x y z
        SADD \(p):set beta alpha gamma
        ZADD \(p):z 2 two 1.5 one 3 three
        XADD \(p):x 1-0 a 1 b 2
        XADD \(p):x 2-0 c 3
        """)

    let string = try await session.page(of: try await key(session, "\(p):s"), limit: 100, offset: 0)
    #expect(string.columns.map(\.name) == ["value"] && string.rows == [["ação 🚀"]])
    #expect(string.keys?.columns == ["key"] && string.keys?.values == [["\(p):s"]])
    let note = try #require(string.note)
    #expect(note.hasPrefix("string · TTL ") && note.hasSuffix(" s"), "\(note)")
    #expect(try await session.count(of: try await key(session, "\(p):s")) == 1)

    let hash = try await session.page(of: try await key(session, "\(p):h"), limit: 2, offset: 1)
    #expect(hash.columns.map(\.name) == ["field", "value"] && hash.rows == [["b", "2"], ["c", "3"]])
    #expect(hash.keys?.columns == ["field"] && hash.keys?.values == [["b"], ["c"]])
    let found = try await session.page(of: try await key(session, "\(p):h"), limit: 10, offset: 0, search: "C")
    #expect(found.rows == [["c", "3"]] && found.keys?.values == [["c"]])
    #expect(try await session.count(of: try await key(session, "\(p):h"), search: "C") == 1)
    #expect(hash.note == "hash · no expiry")
    #expect(try await session.count(of: try await key(session, "\(p):h")) == 3)

    let list = try await session.page(of: try await key(session, "\(p):l"), limit: 2, offset: 1)
    #expect(list.columns.map(\.name) == ["index", "value"] && list.columns.map(\.type) == ["integer", ""])
    #expect(list.rows == [["1", "y"], ["2", "z"]] && list.keys?.values == [["1"], ["2"]] && list.keys?.columns == ["index"])
    #expect(list.note == "list · no expiry")
    #expect(try await session.count(of: try await key(session, "\(p):l")) == 3)

    let set = try await session.page(of: try await key(session, "\(p):set"), limit: 10, offset: 0)
    #expect(set.columns.map(\.name) == ["member"] && set.rows == [["alpha"], ["beta"], ["gamma"]])
    #expect(set.keys?.columns == ["member"])
    #expect(try await session.count(of: try await key(session, "\(p):set")) == 3)

    let zset = try await session.page(of: try await key(session, "\(p):z"), limit: 2, offset: 0)
    #expect(zset.columns.map(\.name) == ["member", "score"] && zset.columns.map(\.type) == ["", "double"])
    #expect(zset.rows == [["one", "1.5"], ["two", "2"]] && zset.keys?.columns == ["member"])
    #expect(try await session.count(of: try await key(session, "\(p):z")) == 3)

    let stream = try await session.page(of: try await key(session, "\(p):x"), limit: 10, offset: 1)
    #expect(stream.columns.map(\.name) == ["id", "fields"] && stream.rows == [["2-0", "c=3"]] && stream.keys == nil)
    #expect(try await session.page(of: try await key(session, "\(p):x"), limit: 1, offset: 0).rows == [["1-0", "a=1, b=2"]])
    #expect(try await session.count(of: try await key(session, "\(p):x")) == 2)

    // A key deleted after the list was loaded.
    let gone = try await key(session, "\(p):s")
    _ = try await session.execute("DEL \(p):s")
    await #expect(throws: DatabaseError.self) { try await session.page(of: gone, limit: 10, offset: 0) }
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisEditsEveryType(_ server: String) async throws {
    let session = try await connect(server)
    let p = prefix("edit")
    defer { Task { await clear(session, p) } }
    _ = try await session.execute("""
        SET \(p):s old EX 3600
        HSET \(p):h a 1 b 2
        RPUSH \(p):l x y z
        SADD \(p):set a b
        ZADD \(p):z 1 a 2 b
        XADD \(p):x 1-0 a 1
        """)
    func page(_ name: String) async throws -> (Relation, QueryResult) {
        let relation = try await key(session, "\(p):\(name)")
        return (relation, try await session.page(of: relation, limit: 100, offset: 0))
    }
    func expectChanged(_ body: () async throws -> EditOutcome) async {
        do {
            _ = try await body()
            Issue.record("no error")
        } catch {
            #expect(error.localizedDescription.contains("changed or was deleted"), "\(error.localizedDescription)")
        }
    }

    // string: SET … KEEPTTL; no insert or row delete.
    var (relation, result) = try await page("s")
    var keys = try #require(result.keys)
    let set = try await session.update(relation, keys: keys, row: 0, changes: [("value", "new 'v'")])
    #expect(set.sql == "SET \(p):s ? KEEPTTL XX" && set.affected == 1 && set.command == "SET")
    #expect(try await session.execute("GET \(p):s").main?.rows == [["new 'v'"]])
    let ttl = try await session.execute("TTL \(p):s").main?.rows.first?.first ?? nil
    #expect((Int(ttl ?? "") ?? 0) > 3000)
    await #expect(throws: DatabaseError.self) { try await session.insert(relation, values: [("value", "x")]) }
    await #expect(throws: DatabaseError.self) { try await session.delete(relation, keys: keys, row: 0) }
    await #expect(throws: DatabaseError.self) { try await session.update(relation, keys: keys, row: 0, changes: [("value", nil)]) }

    // hash: value, rename, insert, delete.
    (relation, result) = try await page("h")
    keys = try #require(result.keys)
    #expect(try await session.update(relation, keys: keys, row: 0, changes: [("value", "10")]).sql == "HSET \(p):h ? ?")
    let rename = try await session.update(relation, keys: keys, row: 1, changes: [("field", "c")])
    #expect(rename.sql == "HSET \(p):h ? ?; HDEL \(p):h ?")
    #expect(try await session.execute("HGETALL \(p):h").main?.rows.sorted { ($0[0] ?? "") < ($1[0] ?? "") } == [["a", "10"], ["c", "2"]])
    await expectChanged { try await session.update(relation, keys: keys, row: 1, changes: [("value", "x")]) }
    await #expect(throws: DatabaseError.self) { try await session.update(relation, keys: keys, row: 0, changes: [("field", "c")]) }
    #expect(try await session.insert(relation, values: [("field", "d"), ("value", "4")]).sql == "HSETNX \(p):h ? ?")
    await #expect(throws: DatabaseError.self) { try await session.insert(relation, values: [("field", "d"), ("value", "5")]) }
    #expect(try await session.delete(relation, keys: keys, row: 0).sql == "HDEL \(p):h ?")
    await expectChanged { try await session.delete(relation, keys: keys, row: 0) }
    #expect(try await session.execute("HLEN \(p):h").main?.rows == [["2"]])

    // list: LSET only while the index still holds the shown value.
    (relation, result) = try await page("l")
    keys = try #require(result.keys)
    #expect(try await session.update(relation, keys: keys, row: 1, changes: [("value", "Y")]).sql == "LSET \(p):l 1 ?")
    _ = try await session.execute("LPUSH \(p):l first")   // every index now holds something else
    await expectChanged { try await session.update(relation, keys: keys, row: 0, changes: [("value", "X")]) }
    await expectChanged { try await session.delete(relation, keys: keys, row: 2) }
    (relation, result) = try await page("l")
    #expect(result.rows.map { $0[1] } == ["first", "x", "Y", "z"])
    keys = try #require(result.keys)
    #expect(try await session.delete(relation, keys: keys, row: 2).sql == "LSET \(p):l 2 ?; LREM \(p):l 1 ?")
    #expect(try await session.insert(relation, values: [("value", "end")]).sql == "RPUSH \(p):l ?")
    #expect(try await session.execute("LRANGE \(p):l 0 -1").main?.rows == [["first"], ["x"], ["z"], ["end"]])

    // set: a rename is SREM + SADD.
    (relation, result) = try await page("set")
    keys = try #require(result.keys)
    #expect(try await session.update(relation, keys: keys, row: 0, changes: [("member", "A")]).sql == "SREM \(p):set ?; SADD \(p):set ?")
    await expectChanged { try await session.update(relation, keys: keys, row: 0, changes: [("member", "AA")]) }
    await #expect(throws: DatabaseError.self) { try await session.update(relation, keys: keys, row: 1, changes: [("member", "A")]) }
    #expect(try await session.insert(relation, values: [("member", "c")]).command == "SADD")
    await #expect(throws: DatabaseError.self) { try await session.insert(relation, values: [("member", "c")]) }
    #expect(try await session.delete(relation, keys: keys, row: 1).sql == "SREM \(p):set ?")
    #expect(try await session.execute("SMEMBERS \(p):set").main?.rows.compactMap { $0[0] }.sorted() == ["A", "c"])

    // zset: score with ZADD XX, member rename keeps the score.
    (relation, result) = try await page("z")
    keys = try #require(result.keys)
    #expect(try await session.update(relation, keys: keys, row: 0, changes: [("score", "5")]).sql == "ZADD \(p):z XX ? ?")
    #expect(try await session.update(relation, keys: keys, row: 1, changes: [("member", "B")]).sql == "ZREM \(p):z ?; ZADD \(p):z ? ?")
    await #expect(throws: DatabaseError.self) { try await session.update(relation, keys: keys, row: 0, changes: [("score", "not a number")]) }
    #expect(try await session.insert(relation, values: [("member", "c")]).sql == "ZADD \(p):z NX ? ?")
    #expect(try await session.execute("ZRANGE \(p):z 0 -1 WITHSCORES").main?.rows == [["c", "0"], ["B", "2"], ["a", "5"]])
    #expect(try await session.delete(relation, keys: keys, row: 0).sql == "ZREM \(p):z ?")
    await expectChanged { try await session.delete(relation, keys: keys, row: 0) }

    // stream: read-only.
    (relation, result) = try await page("x")
    #expect(result.keys == nil)
    await #expect(throws: DatabaseError.self) { try await session.insert(relation, values: [("id", "*")]) }
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisRunsCommands(_ server: String) async throws {
    let session = try await connect(server)
    let p = prefix("run")
    defer { Task { await clear(session, p) } }
    let run = try await session.execute("""
        # set things up
        SET \(p):a "two words \\"quoted\\"\\n"

        set \(p):b 'single \\n literal'
        HSET \(p):h f1 v1 f2 v2
        GET \(p):a
        GET \(p):b
        GET \(p):nope
        HGETALL \(p):h
        KEYS \(p):*
        INCR \(p):n
        """)
    #expect(run.results.map(\.command) == ["SET", "SET", "HSET", "GET", "GET", "GET", "HGETALL 2", "KEYS 3", "INCR"])
    #expect(run.results[0].rows == [["OK"]])
    #expect(run.results[2].rows == [["2"]])
    #expect(run.results[3].rows == [["two words \"quoted\"\n"]])
    #expect(run.results[4].rows == [["single \\n literal"]])
    #expect(run.results[5].rows == [[nil]])
    #expect(run.results[6].columns.map(\.name) == ["field", "value"])
    #expect(Set(run.results[6].rows) == [["f1", "v1"], ["f2", "v2"]])
    #expect(run.results[7].columns.map(\.name) == ["value"] && run.results[7].rows.count == 3)
    #expect(run.main?.rows == [["1"]])

    // The server's own error text, unchanged; the run stops there.
    do {
        _ = try await session.execute("INCR \(p):n\nHGET \(p):a x\nINCR \(p):n")
        Issue.record("no error")
    } catch {
        #expect(error.localizedDescription.hasPrefix("WRONGTYPE"), "\(error.localizedDescription)")
    }
    #expect(try await session.execute("GET \(p):n").main?.rows == [["2"]])
    do {
        _ = try await session.execute("NOSUCHCOMMAND x")
        Issue.record("no error")
    } catch {
        #expect(error.localizedDescription.contains("unknown command"), "\(error.localizedDescription)")
    }
    await #expect(throws: DatabaseError.self) { try await session.execute("GET \"unclosed") }
    for refused in ["SUBSCRIBE ch", "psubscribe c*", "MONITOR", "SYNC", "PSYNC ? -1"] {
        await #expect(throws: DatabaseError.self, "\(refused)") { try await session.execute(refused) }
    }
    #expect(try await session.execute("# only a comment\n\n").results.isEmpty)
    // A transaction typed line by line.
    let multi = try await session.execute("MULTI\nINCR \(p):n\nINCR \(p):n\nEXEC")
    #expect(multi.results.map(\.command) == ["MULTI", "INCR", "INCR", "EXEC 2"] && multi.main?.rows == [["3"], ["4"]])
    #expect(try await session.execute("PING").main?.rows == [["PONG"]])
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisCancelsBlockingCommands(_ server: String) async throws {
    let session = try await connect(server)
    let p = prefix("cancel")
    let clock = ContinuousClock()
    let start = clock.now
    let running = Task { try await session.execute("BLPOP \(p):nothing 0") }
    try await Task.sleep(for: .milliseconds(500))
    session.cancel()
    do {
        _ = try await running.value
        Issue.record("not canceled")
    } catch {
        #expect(error.localizedDescription == "The command was canceled.")
    }
    #expect(clock.now - start < .seconds(2))
    // Cancel with nothing running does nothing; the session reconnects on its own.
    session.cancel()
    #expect(try await session.execute("PING").main?.rows == [["PONG"]])
    #expect(try await session.relations().count >= 0)
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisShowsBinaryAsHex(_ server: String) async throws {
    let session = try await connect(server)
    let p = prefix("bin")
    defer { Task { await clear(session, p) } }
    _ = try await session.execute("""
        SET \(p):s "\\x00\\xff\\x10"
        HSET \(p):h bin "\\xc3\\x28" text ok
        """)
    let (relation, page) = try await (key(session, "\(p):s"), session.page(of: try await key(session, "\(p):s"), limit: 10, offset: 0))
    #expect(page.rows == [["0x00FF10"]])
    do {
        _ = try await session.update(relation, keys: try #require(page.keys), row: 0, changes: [("value", "x")])
        Issue.record("edited bytes")
    } catch {
        #expect(error.localizedDescription.contains("hex"), "\(error.localizedDescription)")
    }
    #expect(try await session.execute("STRLEN \(p):s").main?.rows == [["3"]])
    let hash = try await key(session, "\(p):h")
    let rows = try await session.page(of: hash, limit: 10, offset: 0)
    #expect(rows.rows == [["bin", "0xC328"], ["text", "ok"]])
    let keys = try #require(rows.keys)
    await #expect(throws: DatabaseError.self) { try await session.update(hash, keys: keys, row: 0, changes: [("value", "x")]) }
    #expect(try await session.update(hash, keys: keys, row: 1, changes: [("value", "fine")]).affected == 1)
    #expect(try await session.execute("GET \(p):s").main?.rows == [["0x00FF10"]])
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisWrongPasswordGivesServerMessage(_ server: String) async throws {
    let url = try RedisURL(server)
    let wrong = url.password.map { server.replacingOccurrences(of: ":\($0)@", with: ":wrong-password@") }
        ?? server.replacingOccurrences(of: "redis://", with: "redis://:wrong-password@")
    do {
        _ = try await connect(wrong)
        Issue.record("connected with a wrong password")
    } catch {
        let message = error.localizedDescription
        // Redis with a password: WRONGPASS. Without one: an ERR about AUTH.
        #expect(message.contains("WRONGPASS") || message.contains("AUTH"), "\(message)")
    }
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func redisTLSFailsCleanlyOnPlainPort(_ server: String) async throws {
    let url = try RedisURL(server)
    let clock = ContinuousClock()
    let start = clock.now
    do {
        _ = try await connect(server.replacingOccurrences(of: "redis://", with: "rediss://") + "?insecure=true")
        Issue.record("TLS to a plain port connected")
    } catch {
        print("TLS to \(url.port):", error.localizedDescription)
        #expect(error is DatabaseError)
    }
    #expect(clock.now - start < .seconds(11))
}

@Test func redisRefusedPortFailsFast() async throws {
    let clock = ContinuousClock()
    let start = clock.now
    do {
        _ = try await connect("redis://127.0.0.1:1")
        Issue.record("connected to port 1")
    } catch {
        #expect(error.localizedDescription.contains("127.0.0.1:1"), "\(error.localizedDescription)")
    }
    #expect(clock.now - start < .seconds(11))
}
