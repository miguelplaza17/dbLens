import CMongoC
import Foundation
import Testing
@testable import DBLensKit

// MARK: URLs (no server)

@Test func mongoAcceptsFullURLs() throws {
    let url = try MongoURL("  mongodb://ana:s%40nha@db.example.com:27018/shop?authSource=admin&tls=true ")
    #expect(url.raw == "mongodb://ana:s%40nha@db.example.com:27018/shop?authSource=admin&tls=true")
    #expect(!url.isSRV)
    #expect(url.hosts == ["db.example.com:27018"])
    #expect(url.user == "ana" && url.password == "s@nha")
    #expect(url.database == "shop")
    #expect(url.suggestedName == "shop @ db.example.com")
    #expect(Engine.detect(url.raw) == .mongodb)
}

@Test("MongoDB formats people paste", arguments: [
    // (pasted, srv, hosts, user, password, database, name)
    ("mongodb://localhost", false, ["localhost:27017"], nil, nil, nil, "mongodb @ localhost"),
    ("mongodb://localhost:27017/", false, ["localhost:27017"], nil, nil, nil, "mongodb @ localhost"),
    ("mongodb://root:dblens-test@127.0.0.1:57017/shop?authSource=admin", false, ["127.0.0.1:57017"], "root", "dblens-test", "shop", "shop @ 127.0.0.1"),
    ("mongodb://h1:27017,h2:27018,h3/app?replicaSet=rs0&readPreference=secondaryPreferred", false,
     ["h1:27017", "h2:27018", "h3:27017"], nil, nil, "app", "app @ h1"),
    ("mongodb+srv://ana:pw@cluster0.example.mongodb.net/shop?retryWrites=true&w=majority", true,
     ["cluster0.example.mongodb.net"], "ana", "pw", "shop", "shop @ cluster0.example.mongodb.net"),
    ("mongodb+srv://cluster0.example.mongodb.net/?appName=x", true, ["cluster0.example.mongodb.net"], nil, nil, nil,
     "mongodb @ cluster0.example.mongodb.net"),
    ("MONGO_URL=mongodb://ana:pw@db.x.com/shop", false, ["db.x.com:27017"], "ana", "pw", "shop", "shop @ db.x.com"),
    ("export MONGODB_URI=\"mongodb+srv://ana:pw@c.x.net/shop\"", true, ["c.x.net"], "ana", "pw", "shop", "shop @ c.x.net"),
    ("MONGODB_URI='mongodb://[::1]:27017/local'", false, ["[::1]:27017"], nil, nil, "local", "local @ ::1"),
    ("MONGODB://Host.X.com/db", false, ["Host.X.com:27017"], nil, nil, "db", "db @ Host.X.com"),
    ("mongodb://ana@db.x.com/shop?authSource=admin&serverSelectionTimeoutMS=2000", false, ["db.x.com:27017"],
     "ana", nil, "shop", "shop @ db.x.com"),
    ("mongodb://ana:pw@db.x.com/shop?authMechanism=SCRAM-SHA-256&tls=true&tlsAllowInvalidCertificates=true", false,
     ["db.x.com:27017"], "ana", "pw", "shop", "shop @ db.x.com"),
] as [(String, Bool, [String], String?, String?, String?, String)])
func mongoPastedFormats(_ text: String, srv: Bool, hosts: [String], user: String?, password: String?,
                        database: String?, name: String) throws {
    let url = try MongoURL(text)
    #expect(url.isSRV == srv)
    #expect(url.hosts == hosts)
    #expect(url.user == user)
    #expect(url.password == password)
    #expect(url.database == database)
    #expect(url.suggestedName == name)
    #expect(Engine.detect(text) == .mongodb)
}

@Test func mongoRejectsOtherInput() {
    for text in ["", "   ", "postgres://a@b/c", "redis://h", "hello", "https://example.com", "mongodb://",
                 "mongodb+srv://h1,h2/db", "mongodb+srv://host:27017/db", "mongodb://h/db?replicaSet=", "mongo://h"] {
        #expect(throws: DatabaseError.self, "\(text)") { try MongoURL(text) }
    }
    #expect(throws: DatabaseError.self) { try MongoURL("") }
    do {
        _ = try MongoURL("https://example.com")
    } catch {
        #expect(error.localizedDescription == "Unrecognized format. Use a mongodb://… URL (or mongodb+srv://)")
    }
}

@Test func mongoQueryOptionsPassThrough() throws {
    let url = try MongoURL("mongodb://ana:pw@db.x.com/shop?authSource=admin&tls=true&retryWrites=false")
    #expect(url.connectionText == url.raw)
    #expect(try MongoURL("MONGODB://h/db?w=1").connectionText == "mongodb://h/db?w=1")
}

@Test func mongoURLThroughTunnel() throws {
    let url = try MongoURL("mongodb://ana:p%40w@db.internal:27018/shop?authSource=admin&directConnection=false")
    #expect(url.text(throughLocalPort: 61000) == "mongodb://ana:p%40w@127.0.0.1:61000/shop?authSource=admin&directConnection=true")
    #expect(try MongoURL("mongodb://db.internal").text(throughLocalPort: 5) == "mongodb://127.0.0.1:5/?directConnection=true")
    #expect(try MongoURL("mongodb://db.internal?tls=true").text(throughLocalPort: 5) == "mongodb://127.0.0.1:5/?tls=true&directConnection=true")
}

@Test func mongoRedactsPasswords() {
    #expect(ConnectionURL.redact("mongodb+srv://ana:secret@c.x.net/shop") == "mongodb+srv://ana:•••@c.x.net/shop")
}

@Test func mongoSSHNeedsOneHost() async {
    let ssh = SSHTunnel.Config(host: "bastion.example.com")
    for text in ["mongodb+srv://c.x.net/shop", "mongodb://h1,h2/shop"] {
        do {
            _ = try await MongoSession.connect(try MongoURL(text), ssh: ssh)
            Issue.record("connected \(text)")
        } catch {
            #expect(error.localizedDescription.contains("SSH tunnel goes to one MongoDB server"))
        }
    }
}

// MARK: Statements (no server)

@Test func mongoSplitsStatements() {
    let text = """
        // comment line
        use shop
        db.users.find({name: "a;b"});  db.users.countDocuments()
        db.users.find({
          age: {$gt: 3},   // trailing comment
        })
          .sort({age: -1})
          .limit(2)
        db.users.find({name: /a\\/b;c/i}) /* block ; comment */
        show collections
        """
    #expect(MongoShell.statements(text) == [
        "use shop",
        #"db.users.find({name: "a;b"})"#,
        "db.users.countDocuments()",
        "db.users.find({\n  age: {$gt: 3},    \n})   .sort({age: -1})   .limit(2)",
        #"db.users.find({name: /a\/b;c/i})"#,
        "show collections",
    ])
    #expect(MongoShell.statements("  \n // nothing\n ;; ").isEmpty)
    #expect(MongoShell.statements("db.a.find().\nlimit(1)") == ["db.a.find(). limit(1)"])
}

@Test func mongoParsesStatements() throws {
    typealias C = MongoShell.Call
    #expect(try MongoShell.parse("show dbs") == .showDatabases)
    #expect(try MongoShell.parse("show databases;") == .showDatabases)
    #expect(try MongoShell.parse("show collections") == .showCollections)
    #expect(try MongoShell.parse("use other") == .use("other"))
    #expect(try MongoShell.parse("use 'other'") == .use("other"))
    #expect(try MongoShell.parse("db.users.find()") == .collection(name: "users", calls: [C(name: "find", arguments: [])]))
    #expect(try MongoShell.parse("db.users.find({a: 1, b: [1, 2]}, {a: 1}).sort({a: -1}).limit(5).skip(2)")
        == .collection(name: "users", calls: [C(name: "find", arguments: ["{a: 1, b: [1, 2]}", "{a: 1}"]),
                                              C(name: "sort", arguments: ["{a: -1}"]),
                                              C(name: "limit", arguments: ["5"]), C(name: "skip", arguments: ["2"])]))
    #expect(try MongoShell.parse(#"db.getCollection("my-things").drop()"#)
        == .collection(name: "my-things", calls: [C(name: "drop", arguments: [])]))
    #expect(try MongoShell.parse(#"db["odd name"].findOne({})"#)
        == .collection(name: "odd name", calls: [C(name: "findOne", arguments: ["{}"])]))
    #expect(try MongoShell.parse("db.system.users.find()")
        == .collection(name: "system.users", calls: [C(name: "find", arguments: [])]))
    #expect(try MongoShell.parse("db.runCommand({ping: 1})") == .database(method: "runCommand", arguments: ["{ping: 1}"]))
    #expect(try MongoShell.parse("db.getCollectionNames()") == .database(method: "getCollectionNames", arguments: []))
    #expect(try MongoShell.parse(#"db.users.distinct("city", {a: ")"})"#)
        == .collection(name: "users", calls: [C(name: "distinct", arguments: [#""city""#, #"{a: ")"}"#])]))
    #expect(try MongoShell.parse("db.users.insertMany([{a: 1}, {a: 2},])")
        == .collection(name: "users", calls: [C(name: "insertMany", arguments: ["[{a: 1}, {a: 2},]"])]))
    #expect(try MongoShell.parse("db.users.find({a: /x)/})")
        == .collection(name: "users", calls: [C(name: "find", arguments: ["{a: /x)/}"])]))
    for text in ["SELECT 1", "db", "db.users", "db.users.find(", "show", "show users x", "db.users.find() extra",
                 "users.find()", "db.runCommand({}).foo()", "print('x')"] {
        #expect(throws: DatabaseError.self, "\(text)") { try MongoShell.parse(text) }
    }
    do {
        _ = try MongoShell.parse("print('x')")
    } catch {
        #expect(error.localizedDescription == "dbLens can't run this yet: print('x')")
    }
}

// MARK: Literals (no server)

@Test("mongosh literals as extended JSON", arguments: [
    ("{}", "{}"),
    ("[]", "[]"),
    ("{a: 1}", #"{"a":1}"#),
    ("{ 'a b': 'c' , $set : {x:true}, }", #"{"a b":"c","$set":{"x":true}}"#),
    (#"{"a": "b", "n": null}"#, #"{"a":"b","n":null}"#),
    ("{a: [1, 2.5, -3, 1e3, .5, 5., +7, 0x1F, 007,]}", #"{"a":[1,2.5,-3,1e3,0.5,5.0,7,31,7]}"#),
    (#"{s: 'it\'s', d: "say \"hi\"", t: `tpl`}"#, #"{"s":"it's","d":"say \"hi\"","t":"tpl"}"#),
    (#"{u: 'é\x41\n\t\\'}"#, #"{"u":"éA\n\t\\"}"#),
    ("{e: 'café ☕'}", #"{"e":"café ☕"}"#),
    (#"{p: '😀'}"#, #"{"p":"😀"}"#),
    (#"{_id: ObjectId("65A1b2c3d4e5f60718293a4b")}"#, #"{"_id":{"$oid":"65a1b2c3d4e5f60718293a4b"}}"#),
    (#"{d: ISODate("2024-05-01T10:30:00.250Z")}"#, #"{"d":{"$date":{"$numberLong":"1714559400250"}}}"#),
    (#"{d: new Date("2024-05-01")}"#, #"{"d":{"$date":{"$numberLong":"1714521600000"}}}"#),
    (#"{d: new Date(0)}"#, #"{"d":{"$date":{"$numberLong":"0"}}}"#),
    (#"{d: Date("1969-12-31T23:59:59.999Z")}"#, #"{"d":{"$date":{"$numberLong":"-1"}}}"#),
    ("{n: NumberLong(5)}", #"{"n":{"$numberLong":"5"}}"#),
    (#"{n: NumberLong("9007199254740993")}"#, #"{"n":{"$numberLong":"9007199254740993"}}"#),
    ("{n: NumberInt(42)}", #"{"n":{"$numberInt":"42"}}"#),
    (#"{n: NumberDecimal("9.99")}"#, #"{"n":{"$numberDecimal":"9.99"}}"#),
    (#"{n: Decimal128("1E+3")}"#, #"{"n":{"$numberDecimal":"1E+3"}}"#),
    ("{a: true, b: false, c: null, d: undefined}", #"{"a":true,"b":false,"c":null,"d":{"$undefined":true}}"#),
    ("{a: Infinity, b: -Infinity, c: NaN}", #"{"a":{"$numberDouble":"Infinity"},"b":{"$numberDouble":"-Infinity"},"c":{"$numberDouble":"NaN"}}"#),
    ("{name: /^an.a$/i}", #"{"name":{"$regularExpression":{"pattern":"^an.a$","options":"i"}}}"#),
    (#"{name: /a\/b[/]c/gmi}"#, #"{"name":{"$regularExpression":{"pattern":"a\\/b[/]c","options":"im"}}}"#),
    (#"{r: RegExp("x+", "s")}"#, #"{"r":{"$regularExpression":{"pattern":"x+","options":"s"}}}"#),
    ("{k: MinKey, m: MaxKey()}", #"{"k":{"$minKey":1},"m":{"$maxKey":1}}"#),
    ("{t: Timestamp(1700000000, 1)}", #"{"t":{"$timestamp":{"t":1700000000,"i":1}}}"#),
    (#"{b: BinData(0, "AQID")}"#, #"{"b":{"$binary":{"base64":"AQID","subType":"00"}}}"#),
    (#"{u: UUID("0e4f7a3c-1b2d-4e5f-8a9b-0c1d2e3f4a5b")}"#, #"{"u":{"$binary":{"base64":"Dk96PBstTl+Kmwwc0uP0pb","subType":"04"}}}"#),
    (#"{x: {"$oid": "65a1b2c3d4e5f60718293a4b"}}"#, #"{"x":{"$oid":"65a1b2c3d4e5f60718293a4b"}}"#),
    (#"{"a.b": 1, c.d: 2}"#, #"{"a.b":1,"c.d":2}"#),
    ("{a: 1, // one\n b: /* two */ 2}", #"{"a":1,"b":2}"#),
    ("[{$match: {a: {$in: [1,2]}}}, {$group: {_id: '$a', n: {$sum: 1}}}]",
     #"[{"$match":{"a":{"$in":[1,2]}}},{"$group":{"_id":"$a","n":{"$sum":1}}}]"#),
    ("'plain'", #""plain""#),
    ("42", "42"),
    ("-0.5", "-0.5"),
] as [(String, String)])
func mongoLiterals(_ literal: String, json: String) throws {
    if json.contains("Dk96PBstTl") {
        // UUID bytes: check by decoding instead of a hand-made base64.
        let converted = try MongoShell.extendedJSON(literal)
        let base64 = try #require(converted.split(separator: "\"").dropFirst(7).first)
        #expect(Data(base64Encoded: String(base64))?.map { String(format: "%02x", $0) }.joined() == "0e4f7a3c1b2d4e5f8a9b0c1d2e3f4a5b")
        #expect(converted.hasSuffix(#""subType":"04"}}}"#))
        return
    }
    #expect(try MongoShell.extendedJSON(literal) == json)
}

@Test func mongoLiteralHelpersMakeValues() throws {
    let id = try MongoShell.extendedJSON("ObjectId()")
    #expect(id.range(of: #"^\{"\$oid":"[0-9a-f]{24}"\}$"#, options: .regularExpression) != nil)
    let now = try MongoShell.extendedJSON("new Date()")
    #expect(now.hasPrefix(#"{"$date":{"$numberLong":""#))
    let uuid = try MongoShell.extendedJSON("UUID()")
    #expect(uuid.hasSuffix(#""subType":"04"}}"#))
}

@Test func mongoLiteralErrors() {
    for literal in ["{a: }", "{a 1}", "{a: 1", "[1, 2", "{a: foo}", "{a: ObjectId('xyz')}", "{a: ISODate('yesterday')}",
                    "{a: NumberInt('1.5')}", "{a: NumberInt(3000000000)}", "{a: NumberDecimal('abc')}", "{a: Foo(1)}",
                    "{a: 'unterminated}", "{a: 1} extra", "", "{a: 1,,}"] {
        #expect(throws: DatabaseError.self, "\(literal)") { try MongoShell.extendedJSON(literal) }
    }
    do {
        _ = try MongoShell.extendedJSON("{a: ObjectId('xyz')}")
    } catch {
        #expect(error.localizedDescription == #"This isn't a valid ObjectId("xyz")."#)
    }
}

@Test func mongoReadsTypeOperators() throws {
    // libbson alone reads {"$type": "date"} as legacy binary and fails.
    for literal in ["{joined: {$type: 'date'}}", "{a: {$type: 2}}", "{a: {$type: ['string', 'double']}}",
                    "{$or: [{a: {$type: 'decimal'}}, {b: {$not: {$type: 'objectId'}}}]}", "{s: '{\"$type\":\"x\"}'}"] {
        let json = try MongoShell.extendedJSON(literal)
        let doc = try BSONDocument(json: json)
        var length = 0
        let back = try #require(bson_as_relaxed_extended_json(doc.pointer, &length))
        defer { bson_free(back) }
        let compact = BSON.compactJSON(UnsafeRawBufferPointer(start: back, count: length))
        #expect(compact == json.replacingOccurrences(of: #"{"$numberInt":"2"}"#, with: "2"), "\(literal)")
    }
    // Legacy binary still reads.
    #expect(BSON.typeOfValue(#"{"$binary":"AQID","$type":"00"}"#) == "binData")
}

@Test func mongoISODates() {
    #expect(MongoShell.isoDateMilliseconds("1970-01-01T00:00:00Z") == 0)
    #expect(MongoShell.isoDateMilliseconds("2024-05-01T10:30:00.250Z") == 1_714_559_400_250)
    #expect(MongoShell.isoDateMilliseconds("2024-05-01T12:30:00.250+02:00") == 1_714_559_400_250)
    #expect(MongoShell.isoDateMilliseconds("2024-05-01 10:30:00.250") == 1_714_559_400_250)
    #expect(MongoShell.isoDateMilliseconds("2024-05-01T10:30Z") == 1_714_559_400_000)
    #expect(MongoShell.isoDateMilliseconds("2024-02-29") == 1_709_164_800_000)
    #expect(MongoShell.isoDateMilliseconds("2023-02-29") == nil)
    #expect(MongoShell.isoDateMilliseconds("2024-13-01") == nil)
    #expect(MongoShell.isoDateMilliseconds("May 1") == nil)
    #expect(MongoShell.isoDate(milliseconds: 1_714_559_400_250) == "2024-05-01T10:30:00.250Z")
    #expect(MongoShell.isoDate(milliseconds: -1) == "1969-12-31T23:59:59.999Z")
    #expect(MongoShell.isoDate(milliseconds: 0) == "1970-01-01T00:00:00.000Z")
    #expect(MongoShell.isoDate(milliseconds: 253_402_300_800_000) == nil)
}

// MARK: Grid values (no server)

@Test("Values typed in the grid keep the field's type", arguments: [
    // (text, original type, extended JSON)
    ("42", "string", #""42""#),
    (" padded ", "string", #"" padded ""#),
    ("42", "int", #"{"$numberInt":"42"}"#),
    ("NumberInt(7)", "int", #"{"$numberInt":"7"}"#),
    ("9007199254740993", "long", #"{"$numberLong":"9007199254740993"}"#),
    ("1.5", "double", #"{"$numberDouble":"1.5"}"#),
    ("3", "double", #"{"$numberDouble":"3.0"}"#),
    ("-Infinity", "double", #"{"$numberDouble":"-Infinity"}"#),
    ("9.99", "decimal", #"{"$numberDecimal":"9.99"}"#),
    ("TRUE", "bool", "true"),
    ("false", "bool", "false"),
    ("65A1B2C3D4E5F60718293A4B", "objectId", #"{"$oid":"65a1b2c3d4e5f60718293a4b"}"#),
    (#"ObjectId("65a1b2c3d4e5f60718293a4b")"#, "objectId", #"{"$oid":"65a1b2c3d4e5f60718293a4b"}"#),
    ("2024-05-01T10:30:00.250Z", "date", #"{"$date":{"$numberLong":"1714559400250"}}"#),
    (#"{"$date": "2024-05-01T10:30:00Z"}"#, "date", #"{"$date":"2024-05-01T10:30:00Z"}"#),
    ("{city: 'Lisbon'}", "object", #"{"city":"Lisbon"}"#),
    (#"["a", 1]"#, "array", #"["a",1]"#),
    ("42", "", "42"),
    ("4.5", "", "4.5"),
    ("true", "null", "true"),
    ("hello world", "", #""hello world""#),
    (#""quoted""#, "", #""quoted""#),
    ("{a: 1}", "", #"{"a":1}"#),
    (#"{"$binary": {"base64": "AQID", "subType": "00"}}"#, "binData", #"{"$binary":{"base64":"AQID","subType":"00"}}"#),
] as [(String, String, String)])
func mongoGridValues(_ text: String, type: String, json: String) throws {
    #expect(try MongoSession.extendedJSON(from: text, like: type) == json)
}

@Test func mongoGridNullAndErrors() {
    #expect((try? MongoSession.extendedJSON(from: nil, like: "string")) == "null")
    #expect((try? MongoSession.extendedJSON(from: nil, like: "int")) == "null")
    for (text, type) in [("abc", "int"), ("1.5", "int"), ("3000000000", "int"), ("x", "long"), ("1,5", "double"),
                         ("abc", "decimal"), ("yes", "bool"), ("12345", "objectId"), ("tomorrow", "date"),
                         ("[1]", "object"), ("{a: 1}", "array"), ("not base64", "binData")] {
        do {
            _ = try MongoSession.extendedJSON(from: text, like: type)
            Issue.record("accepted \(text) as \(type)")
        } catch {
            #expect(error.localizedDescription == "This isn't a valid \(type).")
        }
    }
}

@Test func mongoGuessesReferences() {
    #expect(MongoSession.referencedName("user_id") == "user")
    #expect(MongoSession.referencedName("customerId") == "customer")
    #expect(MongoSession.referencedName("_id") == nil)
    #expect(MongoSession.referencedName("ID") == nil)
    #expect(MongoSession.referencedName("paid") == nil)
}

// MARK: Server (DBLENS_TEST_MONGO_URL)

private let testServer = ProcessInfo.processInfo.environment["DBLENS_TEST_MONGO_URL"]
private let hasServer = testServer != nil

private func connect(_ text: String? = testServer) async throws -> MongoSession {
    try await MongoSession.connect(try MongoURL(try #require(text)), ssh: nil)
}

/// A collection name only this test uses, so tests can run side by side.
private func collectionName(_ name: String) -> String { "dblens_\(name)_\(UUID().uuidString.prefix(8).lowercased())" }

private func collection(_ session: MongoSession, _ name: String) async throws -> Relation {
    try #require(try await session.relations().first { $0.name == name })
}

@Test(.enabled(if: hasServer))
func mongoConnects() async throws {
    let session = try await connect()
    #expect(session.engine == .mongodb)
    #expect(session.quote("my things") == "my things")
    #expect(session.serverVersion.first?.isNumber == true)
    print("server:", session.serverVersion)
    #expect(await session.currentDatabase == "shop")
    let ping = try #require(try await session.execute("db.runCommand({ping: 1})").main)
    #expect(ping.columns.map(\.name) == ["ok"])
    #expect(ping.rows == [["1.0"]] || ping.rows == [["1"]])

    let wrong = try #require(testServer).replacingOccurrences(of: ":dblens-test@", with: ":wrong@")
    do {
        _ = try await connect(wrong)
        Issue.record("connected with a wrong password")
    } catch {
        #expect(error.localizedDescription.contains("Authentication failed"))
    }
    do {
        _ = try await connect("mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=500")
        Issue.record("connected to a closed port")
    } catch {
        #expect(!error.localizedDescription.isEmpty)
    }
}

@Test(.enabled(if: hasServer))
func mongoListsCollectionsAndViews() async throws {
    let session = try await connect()
    let name = collectionName("rel")
    _ = try await session.execute("""
        db.\(name).insertMany([{a: 1}, {a: 2}, {a: 3}])
        db.runCommand({create: "\(name)_view", viewOn: "\(name)", pipeline: [{$match: {a: {$gt: 1}}}]})
        """)
    let relations = try await session.relations()
    let table = try #require(relations.first { $0.name == name })
    #expect(table.schema == "shop" && table.kind == .table && table.subtype == "collection" && table.estimate == 3)
    let view = try #require(relations.first { $0.name == "\(name)_view" })
    #expect(view.kind == .view && view.subtype == "view" && view.estimate == -1)
    #expect(!relations.contains { $0.name.hasPrefix("system.") })
    #expect(try await session.count(of: table) == 3)
    #expect(try await session.count(of: view) == 2)
    let viewPage = try await session.page(of: view, limit: 10, offset: 0)
    #expect(viewPage.rows.count == 2 && viewPage.keys == nil)

    // Without a database in the URL: every database but admin, local and config.
    let all = try await connect(try #require(testServer).replacingOccurrences(of: "/shop?", with: "/?"))
    let everywhere = try await all.relations()
    #expect(everywhere.contains { $0.schema == "shop" && $0.name == name })
    #expect(!everywhere.contains { ["admin", "local", "config"].contains($0.schema) })
    _ = try await session.execute("db.\(name)_view.drop(); db.\(name).drop()")
}

private func seed(_ session: MongoSession, _ name: String) async throws {
    _ = try await session.execute("""
        db.\(name).insertMany([
          {_id: ObjectId("650000000000000000000002"), name: "Bia", score: 1.5, big: NumberLong("9007199254740993"),
           price: NumberDecimal("9.99"), code: "42", note: null},
          {_id: ObjectId("650000000000000000000001"), name: "Ana", age: 30, tags: ["a", "b"], address: {city: "Lisbon", zip: "1000"},
           joined: ISODate("2024-05-01T10:30:00.250Z"), active: true},
          {_id: ObjectId("650000000000000000000003"), age: NumberInt(7), bin: BinData(0, "AQID"), note: "here"}
        ])
        """)
}

@Test(.enabled(if: hasServer))
func mongoSearchMatchesFieldsAndIDs() async throws {
    let session = try await connect()
    let name = collectionName("search")
    try await seed(session, name)
    let relation = try await collection(session, name)
    let ana = try await session.page(of: relation, limit: 10, offset: 0, search: "ana")
    #expect(ana.rows.count == 1 && ana.rows[0][1] == "Ana")
    #expect(try await session.count(of: relation, search: "ana") == 1)
    #expect(try await session.page(of: relation, limit: 10, offset: 0, search: "000003").rows.count == 1)
    let byAge = try await session.page(of: relation, limit: 10, offset: 0, sort: ColumnSort(column: "age", ascending: false))
    let ages = byAge.rows.compactMap { $0[2] }.compactMap(Double.init)
    #expect(ages == ages.sorted(by: >) && !ages.isEmpty)
}

@Test(.enabled(if: hasServer))
func mongoPagesMixedDocuments() async throws {
    let session = try await connect()
    let name = collectionName("page")
    try await seed(session, name)
    let relation = try await collection(session, name)
    let page = try await session.page(of: relation, limit: 10, offset: 0)
    #expect(page.columns.map(\.name) == ["_id", "name", "age", "tags", "address", "joined", "active",
                                         "score", "big", "price", "code", "note", "bin"])
    #expect(page.columns.map(\.type) == ["objectId", "string", "int", "array", "object", "date", "bool",
                                         "double", "long", "decimal", "string", "string", "binData"])
    #expect(page.rows[0] == ["650000000000000000000001", "Ana", "30", #"["a","b"]"#, #"{"city":"Lisbon","zip":"1000"}"#,
                             "2024-05-01T10:30:00.250Z", "true", nil, nil, nil, nil, nil, nil])
    #expect(page.rows[1] == ["650000000000000000000002", "Bia", nil, nil, nil, nil, nil,
                             "1.5", "9007199254740993", "9.99", "42", nil, nil])
    #expect(page.rows[2] == ["650000000000000000000003", nil, "7", nil, nil, nil, nil, nil, nil, nil, nil, "here",
                             #"{"$binary":{"base64":"AQID","subType":"00"}}"#])
    let keys = try #require(page.keys)
    #expect(keys.columns == ["_id"] && keys.kind == .primaryKey)
    #expect(keys.values[0] == [#"{"$oid":"650000000000000000000001"}"#])
    #expect(page.note == "3 documents")

    let second = try await session.page(of: relation, limit: 2, offset: 2)
    #expect(second.rows.map { $0[0] } == ["650000000000000000000003"])
    _ = try await session.execute("db.\(name).drop()")
}

@Test(.enabled(if: hasServer))
func mongoEditsDocuments() async throws {
    let session = try await connect()
    let name = collectionName("edit")
    try await seed(session, name)
    let relation = try await collection(session, name)
    var page = try await session.page(of: relation, limit: 10, offset: 0)
    var keys = try #require(page.keys)

    // A string field stays a string, even when the text looks like a number.
    let outcome = try await session.update(relation, keys: keys, row: 1, changes: [("code", "43"), ("name", "Bia Maria")])
    #expect(outcome.command == "UPDATE 1" && outcome.affected == 1)
    #expect(outcome.sql == "db.\(name).updateOne({_id: ?}, {$set: {code: ?, name: ?}})")
    let typed = try await session.execute(#"db.\#(name).countDocuments({code: {$type: "string"}, name: "Bia Maria"})"#)
    #expect(typed.main?.rows == [["1"]])

    // Numbers, dates and new fields.
    _ = try await session.update(relation, keys: keys, row: 2, changes: [("age", "8")])
    _ = try await session.update(relation, keys: keys, row: 0, changes: [("joined", "2025-01-02T03:04:05Z"), ("extra", "{n: 1}"), ("note", nil)])
    page = try await session.page(of: relation, limit: 10, offset: 0)
    let column = { (field: String) in try #require(page.columns.firstIndex { $0.name == field }) }
    #expect(page.rows[2][try column("age")] == "8")
    #expect(page.rows[0][try column("joined")] == "2025-01-02T03:04:05.000Z")
    #expect(page.rows[0][try column("extra")] == #"{"n":1}"#)
    let checks = try await session.execute("""
        db.\(name).countDocuments({age: {$type: "int"}})
        db.\(name).countDocuments({joined: {$type: "date"}})
        db.\(name).countDocuments({note: null, _id: ObjectId("650000000000000000000001")})
        """)
    #expect(checks.results.map(\.rows) == [[["2"]], [["1"]], [["1"]]])

    // Wrong types, the _id, and a document that's gone.
    await #expect(throws: DatabaseError.self) { try await session.update(relation, keys: keys, row: 2, changes: [("age", "eight")]) }
    do {
        _ = try await session.update(relation, keys: keys, row: 2, changes: [("age", "eight")])
    } catch {
        #expect(error.localizedDescription == "This isn't a valid int.")
    }
    await #expect(throws: DatabaseError.self) { try await session.update(relation, keys: keys, row: 0, changes: [("_id", "x")]) }
    _ = try await session.execute(#"db.\#(name).deleteOne({_id: ObjectId("650000000000000000000003")})"#)
    for attempt in 0..<2 {
        do {
            if attempt == 0 {
                _ = try await session.update(relation, keys: keys, row: 2, changes: [("age", "9")])
            } else {
                _ = try await session.delete(relation, keys: keys, row: 2)
            }
            Issue.record("edited a deleted document")
        } catch {
            #expect(error.localizedDescription == "The row changed or was deleted since it was loaded. Reload the table and try again.")
        }
    }

    // Insert: an empty _id becomes an ObjectId; types come from the page.
    let inserted = try await session.insert(relation, values: [("_id", ""), ("name", "Novo"), ("age", "5"), ("code", "7")])
    #expect(inserted.command == "INSERT 1" && inserted.sql == "db.\(name).insertOne({name: ?, age: ?, code: ?})")
    let found = try await session.execute(#"db.\#(name).find({name: "Novo", age: {$type: "int"}, code: "7"})"#)
    #expect(found.main?.rows.count == 1)
    #expect(found.main?.columns.first?.type == "objectId")
    _ = try await session.insert(relation, values: [("_id", "own-id"), ("name", "Own")])
    #expect(try await session.execute(#"db.\#(name).countDocuments({_id: "own-id"})"#).main?.rows == [["1"]])

    // Delete exactly the row.
    page = try await session.page(of: relation, limit: 10, offset: 0)
    keys = try #require(page.keys)
    let row = try #require(page.rows.firstIndex { $0[0] == "own-id" })
    let deleted = try await session.delete(relation, keys: keys, row: row)
    #expect(deleted.command == "DELETE 1" && deleted.sql == "db.\(name).deleteOne({_id: ?})")
    #expect(try await session.count(of: relation) == 3)
    _ = try await session.execute("db.\(name).drop()")
}

@Test(.enabled(if: hasServer))
func mongoRunsCommands() async throws {
    let session = try await connect()
    let name = collectionName("exec")
    let other = collectionName("db")
    let run = { (text: String) in try await session.execute(text) }

    #expect(try await run("db.\(name).insertOne({_id: 1, name: 'Ana', city: 'Lisbon', age: 30})").main?.command == "INSERT 1")
    let many = try await run("""
        db.\(name).insertMany([
          {_id: 2, name: 'Bia', city: 'Porto', age: 25},
          {_id: 3, name: 'Caio', city: 'Lisbon', age: 41, tags: ['x']},
          {_id: 4, name: 'Duda', city: 'Faro'},
        ])
        """)
    #expect(many.main?.command == "INSERT 3")

    let found = try #require(try await run("db.\(name).find({city: 'Lisbon'}, {name: 1, age: 1}).sort({age: -1})").main)
    #expect(found.columns.map(\.name) == ["_id", "name", "age"])
    #expect(found.rows == [["3", "Caio", "41"], ["1", "Ana", "30"]])
    #expect(found.command == "FIND 2")
    let chained = try await run("db.getCollection('\(name)').find()\n  .sort({_id: 1})\n  .skip(1)\n  .limit(2)").main
    #expect(chained?.rows.map { $0[0] } == ["2", "3"])
    #expect(try await run("db.\(name).find({name: /^d/i}).toArray()").main?.rows.first?[1] == "Duda")
    #expect(try await run("db.\(name).findOne({_id: {$gt: 1}})").main?.rows.count == 1)
    #expect(try await run("db.\(name).find({}).count()").main?.rows == [["4"]])
    #expect(try await run("db.\(name).countDocuments({age: {$exists: true}})").main?.rows == [["3"]])
    #expect(try await run("db.\(name).estimatedDocumentCount()").main?.rows == [["4"]])
    let distinct = try #require(try await run("db.\(name).distinct('city', {age: {$gte: 25}})").main)
    #expect(distinct.columns.map(\.name) == ["value"] && Set(distinct.rows.compactMap { $0[0] }) == ["Lisbon", "Porto"])

    let grouped = try #require(try await run("""
        db.\(name).aggregate([
          {$group: {_id: '$city', total: {$sum: 1}, ages: {$sum: '$age'}}},
          {$sort: {total: -1, _id: 1}},
        ])
        """).main)
    #expect(grouped.columns.map(\.name) == ["_id", "total", "ages"])
    #expect(grouped.rows == [["Lisbon", "2", "71"], ["Faro", "1", "0"], ["Porto", "1", "25"]])
    #expect(grouped.command == "AGGREGATE 3")

    #expect(try await run("db.\(name).updateOne({_id: 1}, {$set: {age: 31}})").main?.command == "UPDATE 1")
    #expect(try await run("db.\(name).updateMany({city: 'Lisbon'}, {$inc: {age: 1}})").main?.command == "UPDATE 2")
    #expect(try await run("db.\(name).updateMany({}, [{$set: {upper: {$toUpper: '$name'}}}])").main?.command == "UPDATE 4")
    #expect(try await run("db.\(name).replaceOne({_id: 4}, {name: 'Duda', city: 'Braga'})").main?.command == "UPDATE 1")
    #expect(try await run("db.\(name).findOne({_id: 4})").main?.rows == [["4", "Duda", "Braga"]])
    #expect(try await run("db.\(name).deleteOne({_id: 4})").main?.command == "DELETE 1")
    #expect(try await run("db.\(name).deleteMany({age: {$lt: 100}})").main?.command == "DELETE 3")

    let index = try #require(try await run("db.\(name).createIndex({city: 1, age: -1})").main)
    #expect(index.rows == [["city_1_age_-1"]] && index.command == "CREATE INDEX")
    #expect(try await run("db.\(name).createIndex({name: 1}, {unique: true, name: 'by_name'})").main?.rows == [["by_name"]])
    let indexes = try #require(try await run("db.\(name).getIndexes()").main)
    #expect(Set(indexes.rows.compactMap { $0[indexes.columns.firstIndex { $0.name == "name" }!] }) == ["_id_", "city_1_age_-1", "by_name"])

    let names = try #require(try await run("db.getCollectionNames()").main)
    #expect(names.rows.contains([name]))
    #expect(try await run("show collections").main?.rows.contains([name]) == true)
    let dbs = try #require(try await run("show dbs").main)
    #expect(dbs.columns.first?.name == "name" && dbs.rows.contains { $0.first == "shop" })
    #expect(try await run("db.getName()").main?.rows == [["shop"]])
    #expect(try await run("db.runCommand('ping')").main?.rows.first?.first != nil)

    // use switches the database for later statements, also in later runs.
    let used = try await run("use \(other)\ndb.things.insertOne({a: 1})\ndb.getName()")
    #expect(used.results.map(\.command) == ["USE \(other)", "INSERT 1", "FIND 1"])
    #expect(used.main?.rows == [[other]])
    #expect(await session.currentDatabase == other)
    #expect(try await run("db.things.countDocuments()").main?.rows == [["1"]])
    #expect(try await run("db.dropDatabase()").main?.command == "DROP DATABASE")
    _ = try await run("use shop")

    #expect(try await run("db.\(name).drop()").main?.command == "DROP")
    #expect(try await run("db.\(name).drop()").main?.command == "DROP")

    // Errors: the server's own words, and what dbLens doesn't run.
    do {
        _ = try await run("db.runCommand({noSuchCommand: 1})")
        Issue.record("ran an unknown command")
    } catch {
        #expect(error.localizedDescription.contains("noSuchCommand"))
    }
    do {
        _ = try await run("db.\(name).mapReduce(1, 2)")
        Issue.record("ran mapReduce")
    } catch {
        #expect(error.localizedDescription == "dbLens can't run this yet: db.\(name).mapReduce(1, 2)")
    }
    await #expect(throws: DatabaseError.self) { try await run("db.\(name).insertOne({a: })") }
    await #expect(throws: DatabaseError.self) { try await run("db.\(name).find({$bad: 1})") }
    #expect(try await run("// only a comment").results.isEmpty)
}

@Test(.enabled(if: hasServer))
func mongoCutsBigResults() async throws {
    let session = try await connect()
    let name = collectionName("big")
    _ = try await session.execute("""
        db.\(name)_seed.insertOne({})
        db.\(name)_seed.aggregate([{$project: {_id: 0, n: {$range: [0, 6000]}}}, {$unwind: '$n'}, {$out: '\(name)'}])
        """)
    let result = try #require(try await session.execute("db.\(name).find()").main)
    #expect(result.rows.count == QueryResult.rowLimit && result.truncated)
    let small = try #require(try await session.execute("db.\(name).find().limit(10)").main)
    #expect(small.rows.count == 10 && !small.truncated)
    let relation = try await collection(session, name)
    #expect(try await session.count(of: relation) == 6000)
    _ = try await session.execute("db.\(name).drop(); db.\(name)_seed.drop()")
}

@Test(.enabled(if: hasServer))
func mongoGuessesLinksOnTheBoard() async throws {
    let session = try await connect()
    let users = collectionName("users")
    let owner = collectionName("owner")
    let orders = collectionName("orders")
    _ = try await session.execute("""
        db.\(users)s.insertOne({_id: ObjectId("650000000000000000000001"), name: 'Ana'})
        db.\(owner).insertOne({_id: ObjectId("650000000000000000000009")})
        db.\(orders).insertMany([
          {\(users)_id: ObjectId("650000000000000000000001"), \(owner)Id: ObjectId("650000000000000000000009"), total: 10},
          {\(users)_id: ObjectId("650000000000000000000001"), total: 5.5, coupon: null},
          {\(users)_id: ObjectId("650000000000000000000001"), total: 7},
        ])
        """)
    let graph = try await session.schemaGraph()
    let table = try #require(graph.tables.first { $0.name == orders })
    #expect(table.schema == "shop" && !table.isView)
    let columns = Dictionary(uniqueKeysWithValues: table.columns.map { ($0.name, $0) })
    #expect(table.columns.first?.name == "_id" && columns["_id"]?.isPrimaryKey == true && columns["_id"]?.isNullable == false)
    #expect(columns["\(users)_id"]?.type == "objectId" && columns["\(users)_id"]?.isNullable == false)
    #expect(columns["total"]?.type == "int" && columns["total"]?.isNullable == false)
    #expect(columns["coupon"]?.type == "null" && columns["coupon"]?.isNullable == true)
    #expect(columns["\(owner)Id"]?.isNullable == true)
    let links = graph.links.filter { $0.from == "shop.\(orders)" }.sorted { $0.name < $1.name }
    #expect(links.map(\.to) == ["shop.\(owner)", "shop.\(users)s"])
    #expect(links.map(\.name) == ["guessed (\(owner)Id)", "guessed (\(users)_id)"])
    #expect(links.allSatisfy { $0.toColumns == ["_id"] })
    _ = try await session.execute("db.\(users)s.drop(); db.\(owner).drop(); db.\(orders).drop()")

    // One guessed link in a collection is named just "guessed".
    let solo = collectionName("solo")
    _ = try await session.execute("db.\(owner).insertOne({}); db.\(solo).insertOne({\(owner)_id: ObjectId()})")
    let soloLinks = try await session.schemaGraph().links.filter { $0.from == "shop.\(solo)" }
    #expect(soloLinks.map(\.name) == ["guessed"])
    _ = try await session.execute("db.\(owner).drop(); db.\(solo).drop()")
}

@Test(.enabled(if: hasServer))
func mongoCancelsSlowFinds() async throws {
    let session = try await connect()
    let name = collectionName("slow")
    _ = try await session.execute("""
        db.\(name)_seed.insertOne({})
        db.\(name)_seed.aggregate([{$project: {_id: 0, n: {$range: [0, 200000]}}}, {$unwind: '$n'}, {$out: '\(name)'}])
        """)
    // Matches nothing, and works hard on every document: one long server operation.
    let slow = "db.\(name).find({$expr: {$lt: [{$size: {$range: [{$mod: ['$n', 7]}, 3000]}}, 0]}})"
    let clock = ContinuousClock()
    let start = clock.now
    let running = Task { try await session.execute(slow) }
    try await Task.sleep(for: .milliseconds(500))
    session.cancel()
    do {
        _ = try await running.value
        Issue.record("not canceled")
    } catch {
        #expect(error.localizedDescription == "The query was canceled.")
    }
    #expect(clock.now - start < .seconds(2.5))
    print("canceled after", clock.now - start)
    // Cancel with nothing running does nothing; the session goes on.
    session.cancel()
    #expect(try await session.execute("db.\(name).countDocuments()").main?.rows == [["200000"]])
    _ = try await session.execute("db.\(name).drop(); db.\(name)_seed.drop()")
}
