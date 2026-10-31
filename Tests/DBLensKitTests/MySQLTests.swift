import Foundation
import Testing
@testable import DBLensKit

// MARK: URLs (no server)

@Test func mysqlAcceptsFullURLs() throws {
    let url = try MySQLURL("  mysql://ana:s%40nha@db.example.com:3307/shop?ssl-mode=REQUIRED ")
    #expect(url.raw == "mysql://ana:s%40nha@db.example.com:3307/shop?ssl-mode=REQUIRED")
    #expect(url.host == "db.example.com")
    #expect(url.port == 3307)
    #expect(url.user == "ana")
    #expect(url.password == "s@nha")
    #expect(url.database == "shop")
    #expect(url.sslMode == "REQUIRED")
    #expect(url.socket == nil)
    #expect(url.suggestedName == "shop @ db.example.com")
}

@Test func mysqlDefaults() throws {
    let url = try MySQLURL("mysql://")
    #expect(url.host == "localhost")
    #expect(url.port == nil && url.user == nil && url.password == nil && url.database == nil)
    #expect(url.sslMode == nil && url.socket == nil)
    #expect(url.suggestedName == "mysql @ localhost")
    #expect(try MySQLURL("mysql://root@db.x.com").suggestedName == "mysql @ db.x.com")
}

@Test("MySQL formats people paste", arguments: [
    // (pasted, host, port, database, user, password, sslMode)
    ("mariadb://ana:pw@db.x.com/shop", "db.x.com", nil, "shop", "ana", "pw", nil),
    ("mysqlx://ana@db.x.com/shop", "db.x.com", nil, "shop", "ana", nil, nil),
    ("mysqlx://ana@db.x.com:33060/shop", "db.x.com", 33060, "shop", "ana", nil, nil),
    ("jdbc:mysql://db.x.com:3306/shop?user=ana&password=pw&useSSL=true", "db.x.com", 3306, "shop", "ana", "pw", "REQUIRED"),
    ("jdbc:mysql://db.x.com/shop?useSSL=false&serverTimezone=UTC", "db.x.com", nil, "shop", nil, nil, "DISABLED"),
    ("jdbc:mysql://db.x.com/shop?useSSL=true&verifyServerCertificate=true", "db.x.com", nil, "shop", nil, nil, "VERIFY_CA"),
    ("jdbc:mariadb://db.x.com/shop?user=ana&sslMode=verify-full", "db.x.com", nil, "shop", "ana", nil, "VERIFY_IDENTITY"),
    ("jdbc:mariadb://db.x.com/shop?sslMode=trust", "db.x.com", nil, "shop", nil, nil, "REQUIRED"),
    ("mysql+pymysql://ana:pw@db.x.com/shop?charset=utf8mb4", "db.x.com", nil, "shop", "ana", "pw", nil),
    ("mysql+mysqldb://ana:pw@db.x.com:3310/shop", "db.x.com", 3310, "shop", "ana", "pw", nil),
    ("DATABASE_URL=\"mysql://ana:pw@db.x.com/shop?sslmode=require\"", "db.x.com", nil, "shop", "ana", "pw", "REQUIRED"),
    ("export DATABASE_URL='mysql://ana@db.x.com/shop?ssl=true'", "db.x.com", nil, "shop", "ana", nil, "REQUIRED"),
    ("mysql://ana@db.x.com/shop?ssl-mode=disabled", "db.x.com", nil, "shop", "ana", nil, "DISABLED"),
    ("mysql://ana@db.x.com/shop?ssl-mode=PREFERRED", "db.x.com", nil, "shop", "ana", nil, "PREFERRED"),
    ("mysql://ana@db.x.com/shop?ssl-mode=VERIFY_CA", "db.x.com", nil, "shop", "ana", nil, "VERIFY_CA"),
    ("mysql://ana@db.x.com/shop?ssl-mode=verify_identity", "db.x.com", nil, "shop", "ana", nil, "VERIFY_IDENTITY"),
    ("mysql://ana@db.x.com/shop?unknown=1&connectTimeout=5", "db.x.com", nil, "shop", "ana", nil, nil),
    ("mysql://ana@[::1]:3306/shop", "::1", 3306, "shop", "ana", nil, nil),
    ("MYSQL_URL=mysql://root:dblens-test@127.0.0.1:53306", "127.0.0.1", 53306, nil, "root", "dblens-test", nil),
] as [(String, String, Int?, String?, String?, String?, String?)])
func mysqlPastedFormats(_ text: String, host: String, port: Int?, database: String?, user: String?,
                        password: String?, sslMode: String?) throws {
    let url = try MySQLURL(text)
    #expect(url.host == host)
    #expect(url.port == port)
    #expect(url.database == database)
    #expect(url.user == user)
    #expect(url.password == password)
    #expect(url.sslMode == sslMode)
    #expect(Engine.detect(text) == .mysql)
}

@Test func mysqlSocketsAndCharset() throws {
    let query = try MySQLURL("mysql://ana@localhost/shop?socket=/tmp/mysql.sock&charset=latin1")
    #expect(query.socket == "/tmp/mysql.sock" && query.host == "localhost" && query.charset == "latin1")
    let encoded = try MySQLURL("mysql://ana@%2Ftmp%2Fmysql.sock/shop")
    #expect(encoded.socket == "/tmp/mysql.sock" && encoded.host == "localhost" && encoded.database == "shop")
}

@Test func mysqlRejectsOtherInput() {
    for text in ["", "   ", "postgres://a@b/c", "hello", "redis://localhost", "https://example.com"] {
        #expect(throws: DatabaseError.self, "\(text)") { try MySQLURL(text) }
    }
}

@Test func mysqlVersions() {
    #expect(MySQLSession.version("11.4.2-MariaDB-ubu2404") == "11.4.2-MariaDB")
    #expect(MySQLSession.version("8.4.3") == "8.4.3")
}

@Test func mysqlSplitsStatements() {
    #expect(MySQLSession.statementVerbs("") == [])
    #expect(MySQLSession.statementVerbs("insert into t values (1); select * from t") == ["INSERT", "SELECT"])
    #expect(MySQLSession.statementVerbs("""
        -- a comment; still a comment
        /* block; comment */ update t set a = 'x;y', b = "it\\"s;" where `we;ird` = 1;
        # hash; comment
        delete from t;  ;
        """) == ["UPDATE", "DELETE"])
    #expect(MySQLSession.statementVerbs("create table t(a int)\n;drop table t;") == ["CREATE", "DROP"])
    #expect(MySQLSession.statementVerbs("select 'it''s; fine'") == ["SELECT"])
}

@Test func mysqlHex() {
    let short: [UInt8] = [0x00, 0xFF, 0x10]
    #expect(short.withUnsafeBytes { MySQLSession.hex($0) } == "0x00FF10")
    let long = [UInt8](repeating: 0xAB, count: 100)
    let text = long.withUnsafeBytes { MySQLSession.hex($0) }
    #expect(text == "0x" + String(repeating: "AB", count: 64) + "…")
}

// MARK: Servers (DBLENS_TEST_MYSQL_URL, DBLENS_TEST_MARIADB_URL)

private let testServers = ["DBLENS_TEST_MYSQL_URL", "DBLENS_TEST_MARIADB_URL"]
    .compactMap { ProcessInfo.processInfo.environment[$0] }

private func connect(_ text: String, ssl: String? = nil) async throws -> MySQLSession {
    var text = text
    if let ssl { text += (text.contains("?") ? "&" : "?") + "ssl-mode=" + ssl }
    return try await MySQLSession.connect(try MySQLURL(text), ssh: nil)
}

private func relation(_ session: MySQLSession, _ name: String) async throws -> Relation {
    try #require(try await session.relations().first { $0.name == name })
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlRunsStatements(_ server: String) async throws {
    let session = try await connect(server)
    #expect(session.engine == .mysql)
    #expect(session.quote("we`ird") == "`we``ird`")
    #expect(!session.serverVersion.isEmpty)
    print("server:", session.serverVersion)

    let run = try await session.execute("""
        drop table if exists dblens_run;
        create table dblens_run(a int primary key, b varchar(50), j json, d datetime);
        insert into dblens_run values (1, 'ação 🚀', '{"k": 1}', '2026-10-30 12:00:00'), (2, null, null, null);
        update dblens_run set b = b where a = 1;
        select * from dblens_run order by a
        """)
    defer { Task { _ = try? await session.execute("drop table if exists dblens_run") } }
    #expect(run.results.map(\.command) == ["DROP", "CREATE", "INSERT 2", "UPDATE 1", "SELECT 2"])
    let main = try #require(run.main)
    #expect(main.columns.map(\.name) == ["a", "b", "j", "d"])
    #expect(main.columns.map(\.type) == ["int", "varchar", "json", "datetime"]
        || main.columns.map(\.type) == ["int", "varchar", "text", "datetime"])  // MariaDB's JSON is LONGTEXT
    #expect(main.rows[0][0] == "1" && main.rows[0][1] == "ação 🚀" && main.rows[0][3] == "2026-10-30 12:00:00")
    #expect(main.rows[0][2]?.contains("\"k\"") == true)
    #expect(main.rows[1] == ["2", nil, nil, nil])

    // The server's own message comes through, and the connection still works.
    do {
        _ = try await session.execute("select * from nao_existe")
        Issue.record("no error")
    } catch {
        #expect(error.localizedDescription.contains("nao_existe"))
    }
    #expect(try await session.execute("select 1").main?.rows == [["1"]])
    // An error in the middle stops the run.
    await #expect(throws: DatabaseError.self) { try await session.execute("select 1; select * from nao_existe; select 2") }
    #expect(try await session.execute("select 3 as x").main?.rows == [["3"]])
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlCutsBigResults(_ server: String) async throws {
    let session = try await connect(server)
    let digits = "(select 0 n union all select 1 union all select 2 union all select 3 union all select 4 union all select 5 union all select 6 union all select 7 union all select 8 union all select 9)"
    let sql = "select a.n + 10 * b.n + 100 * c.n + 1000 * d.n as i from \(digits) a, \(digits) b, \(digits) c, \(digits) d where d.n < 6"
    let result = try #require(try await session.execute(sql).main)
    #expect(result.rows.count == QueryResult.rowLimit && result.truncated && result.totalRows == 6000)
    #expect(result.command == "SELECT 6000")
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlListsRelations(_ server: String) async throws {
    let session = try await connect(server)
    _ = try await session.execute("""
        create database if not exists dblens_other;
        create table if not exists dblens_other.aaa(id int primary key);
        drop table if exists dblens_rel;
        create table dblens_rel(id int primary key);
        create or replace view dblens_rel_view as select id from dblens_rel;
        """)
    defer { Task { _ = try? await session.execute("drop view if exists dblens_rel_view; drop table if exists dblens_rel; drop database if exists dblens_other") } }
    let relations = try await session.relations()
    let database = try #require(session.url.database)
    let schemas = relations.map(\.schema)
    #expect(!schemas.contains { ["mysql", "information_schema", "performance_schema", "sys"].contains($0) })
    #expect(relations.first?.schema == database)
    // The URL's database first, then the others alphabetically.
    let firstOther = try #require(schemas.firstIndex { $0 != database })
    #expect(schemas[firstOther...].allSatisfy { $0 != database })
    #expect(Array(schemas[firstOther...]) == schemas[firstOther...].sorted())
    #expect(relations.first { $0.name == "dblens_rel" }?.kind == .table)
    #expect(relations.first { $0.name == "dblens_rel_view" }?.kind == .view)
    #expect(relations.contains { $0.schema == "dblens_other" && $0.name == "aaa" })
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlPagesAndEdits(_ server: String) async throws {
    let session = try await connect(server)
    _ = try await session.execute("""
        drop table if exists dblens_pk, dblens_nopk;
        create table dblens_pk(id int primary key, nome varchar(255), n int default 7) engine=InnoDB;
        create table dblens_nopk(nome varchar(20), n int);
        insert into dblens_pk values (2,'b',2),(1,'a',1);
        insert into dblens_nopk values ('x',1),('x',1);
        """)
    defer { Task { _ = try? await session.execute("drop table if exists dblens_pk, dblens_nopk") } }
    let pk = try await relation(session, "dblens_pk")
    let nopk = try await relation(session, "dblens_nopk")

    var page = try await session.page(of: pk, limit: 10, offset: 0)
    #expect(page.rows == [["1", "a", "1"], ["2", "b", "2"]])
    #expect(page.columns.map(\.name) == ["id", "nome", "n"])
    #expect(page.columns[1].type == "varchar(255)")
    #expect(page.columns[0].type.hasPrefix("int"))
    var keys = try #require(page.keys)
    #expect(keys.columns == ["id"] && keys.kind == .primaryKey && keys.values == [["1"], ["2"]])
    #expect(try await session.count(of: pk) == 2)
    #expect(try await session.page(of: pk, limit: 1, offset: 1).rows == [["2", "b", "2"]])
    #expect(try await session.page(of: pk, limit: 10, offset: 0, search: "B").rows == [["2", "b", "2"]])
    #expect(try await session.count(of: pk, search: "B") == 1)
    // Read-only queries: the database refuses every way to write.
    #expect(try await session.readOnly("select count(*) from dblens_pk").main?.rows == [["2"]])
    for attempt in ["insert into dblens_pk values (9,'x',9)", "select 1; delete from dblens_pk",
                    "commit; delete from dblens_pk", "create table dblens_ro(a int)", "drop table dblens_pk"] {
        await #expect(throws: (any Error).self, "\(attempt)") { try await session.readOnly(attempt) }
    }
    _ = try? await session.readOnly("set session transaction read write")
    await #expect(throws: (any Error).self) { try await session.readOnly("delete from dblens_pk") }
    #expect(try await session.count(of: pk) == 2)
    // The session writes again afterwards (the app's own edits use it).
    _ = try await session.execute("update dblens_pk set n = n where id = 1")
    #expect(try await session.page(of: pk, limit: 10, offset: 0, search: "%").rows.isEmpty)
    #expect(try await session.page(of: pk, limit: 10, offset: 0, sort: ColumnSort(column: "nome", ascending: false)).rows.map { $0[1] } == ["b", "a"])

    let updated = try await session.update(pk, keys: keys, row: 1, changes: [("nome", "bê 'quoted' \\ 🚀"), ("n", nil)])
    #expect(updated.command == "UPDATE 1" && updated.affected == 1)
    #expect(updated.sql == "update `shop`.`dblens_pk` set `nome` = ?, `n` = ? where `id` = ?".replacingOccurrences(of: "shop", with: pk.schema))
    #expect(!updated.sql.contains("bê"))
    // The same value again still counts as the row found.
    #expect(try await session.update(pk, keys: keys, row: 0, changes: [("nome", "a")]).affected == 1)
    let inserted = try await session.insert(pk, values: [("id", "3"), ("nome", "c")])
    #expect(inserted.command == "INSERT 1" && !inserted.sql.contains("'c'") && inserted.sql.hasSuffix("values (?, ?)"))
    page = try await session.page(of: pk, limit: 10, offset: 0)
    #expect(page.rows == [["1", "a", "1"], ["2", "bê 'quoted' \\ 🚀", nil], ["3", "c", "7"]])
    keys = try #require(page.keys)
    // The server's message for a bad value comes through, and nothing changes.
    await #expect(throws: DatabaseError.self) { try await session.update(pk, keys: keys, row: 0, changes: [("id", "2")]) }

    // A key that matches two rows: rolled back.
    let loose = RowKeys(columns: ["n"], kind: .primaryKey, values: [["7"]])
    _ = try await session.execute("update dblens_pk set n = 7")
    do {
        _ = try await session.update(pk, keys: loose, row: 0, changes: [("nome", "z")])
        Issue.record("updated several rows")
    } catch {
        #expect(error.localizedDescription.contains("3 rows"))
    }
    #expect(try await session.execute("select count(*) from dblens_pk where nome = 'z'").main?.rows == [["0"]])

    // The row was deleted after it was loaded.
    _ = try await session.execute("delete from dblens_pk where id = 3")
    do {
        _ = try await session.update(pk, keys: keys, row: 2, changes: [("nome", "w")])
        Issue.record("updated a deleted row")
    } catch {
        #expect(error.localizedDescription.contains("changed or was deleted"))
    }
    await #expect(throws: DatabaseError.self) { try await session.delete(pk, keys: keys, row: 2) }

    let deleted = try await session.delete(pk, keys: keys, row: 0)
    #expect(deleted.command == "DELETE 1" && deleted.sql.hasSuffix("where `id` = ?"))
    #expect(try await session.execute("select id from dblens_pk order by id").main?.rows == [["2"]])
    // Every column left out: their defaults.
    #expect(try await session.insert(pk, values: [("id", "9")]).command == "INSERT 1")
    #expect(try await session.execute("select n from dblens_pk where id = 9").main?.rows == [["7"]])

    // Without a primary key: read-only.
    page = try await session.page(of: nopk, limit: 10, offset: 0)
    #expect(page.rows == [["x", "1"], ["x", "1"]])
    #expect(page.keys == nil)
    #expect(page.columns.map(\.type) == ["varchar(20)", page.columns[1].type] && page.columns[1].type.hasPrefix("int"))
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlShowsBinaryAsHex(_ server: String) async throws {
    let session = try await connect(server)
    _ = try await session.execute("""
        drop table if exists dblens_bin;
        create table dblens_bin(id int primary key, b varbinary(10), l blob, t text, bits bit(8));
        insert into dblens_bin values (1, 0x00FF10, repeat(0xAB, 100), 'ação', b'00000101'), (2, null, null, null, null);
        """)
    defer { Task { _ = try? await session.execute("drop table if exists dblens_bin") } }
    let page = try await session.page(of: try await relation(session, "dblens_bin"), limit: 10, offset: 0)
    #expect(page.rows[0] == ["1", "0x00FF10", "0x" + String(repeating: "AB", count: 64) + "…", "ação", "0x05"])
    #expect(page.rows[1] == ["2", nil, nil, nil, nil])
    #expect(page.columns.map(\.type) == ["int", "varbinary(10)", "blob", "text", "bit(8)"]
        || page.columns.map(\.type) == ["int(11)", "varbinary(10)", "blob", "text", "bit(8)"])
    let types = try #require(try await session.execute("select b, l, t from dblens_bin").main).columns.map(\.type)
    #expect(types == ["varbinary", "blob", "text"])
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlCancelsQueries(_ server: String) async throws {
    let session = try await connect(server)
    let clock = ContinuousClock()
    let start = clock.now
    let running = Task { try await session.execute("select sleep(10)") }
    try await Task.sleep(for: .milliseconds(500))
    session.cancel()
    do {
        _ = try await running.value
        Issue.record("not canceled")
    } catch {
        print("canceled:", error.localizedDescription)
    }
    #expect(clock.now - start < .seconds(2.5))
    // Cancel with nothing running does nothing, and the session still works.
    session.cancel()
    #expect(try await session.execute("select 1").main?.rows == [["1"]])
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlWrongPasswordGivesServerMessage(_ server: String) async throws {
    let url = try MySQLURL(server)
    let wrong = server.replacingOccurrences(of: ":\(url.password ?? "")@", with: ":wrong-password@")
    do {
        _ = try await connect(wrong)
        Issue.record("connected with a wrong password")
    } catch {
        #expect(error.localizedDescription.contains("Access denied"), "\(error.localizedDescription)")
    }
}

@Test(.enabled(if: !testServers.isEmpty), arguments: testServers)
func mysqlTLSModes(_ server: String) async throws {
    func cipher(_ ssl: String?) async throws -> String {
        let session = try await connect(server, ssl: ssl)
        return try await session.execute("show session status like 'Ssl_cipher'").main?.rows.first?[1] ?? ""
    }
    #expect(try await cipher("DISABLED") == "")
    let required = try await cipher("REQUIRED")
    let preferred = try await cipher(nil)
    print("TLS: required", required, "default", preferred)
    #expect(!required.isEmpty)
    #expect(!preferred.isEmpty)
}
