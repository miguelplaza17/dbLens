import Foundation
import SQLite3
import Testing
@testable import DBLensKit

private let home = NSHomeDirectory()

// MARK: Location

@Test("SQLite locations people paste", arguments: [
    ("sqlite:///tmp/data/app.db", "/tmp/data/app.db"),
    ("sqlite3:///tmp/data/app.db", "/tmp/data/app.db"),
    ("sqlite://~/x.db", home + "/x.db"),
    ("sqlite:relative/app.db", home + "/relative/app.db"),
    ("sqlite3:app.db", home + "/app.db"),
    ("file:/tmp/a.db", "/tmp/a.db"),
    ("file:///tmp/a.db?mode=ro", "/tmp/a.db"),
    ("file:///tmp/My%20Data.sqlite?mode=rwc&cache=shared", "/tmp/My Data.sqlite"),
    ("/tmp/a.sqlite3", "/tmp/a.sqlite3"),
    ("/tmp/a.DB3", "/tmp/a.DB3"),
    ("~/Documents/a.sqlite", home + "/Documents/a.sqlite"),
    ("./local.db", home + "/local.db"),
    ("DATABASE_URL=\"sqlite:///tmp/e.db\"", "/tmp/e.db"),
    ("export DB=sqlite://~/e.db", home + "/e.db"),
    ("DB_PATH=/tmp/e.sqlite", "/tmp/e.sqlite"),
    ("DB_PATH='~/e.db'", home + "/e.db"),
    ("  '/tmp/quoted.db'  ", "/tmp/quoted.db"),
] as [(String, String)])
func parsesLocations(_ text: String, path: String) throws {
    #expect(try SQLiteLocation(text).path == path)
}

@Test func locationNames() throws {
    let location = try SQLiteLocation("sqlite:///tmp/data/app.db")
    #expect(location.suggestedName == "app.db")
    #expect(location.raw == "sqlite:///tmp/data/app.db")
}

@Test("Text that is not a SQLite file", arguments: [
    "", "   ", "postgres://a@b/c", "/tmp/notes.txt", "app.db", "hello", "sqlite:", "sqlite:///", "file:///",
])
func rejectsLocations(_ text: String) {
    #expect(throws: DatabaseError.self) { try SQLiteLocation(text) }
}

// MARK: Helpers

/// A new database file with `sql` already run on it.
private func makeDatabase(_ sql: String = "") throws -> SQLiteLocation {
    let path = NSTemporaryDirectory() + "dblens-\(UUID().uuidString).db"
    var db: OpaquePointer?
    #expect(sqlite3_open_v2(path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK)
    defer { sqlite3_close(db) }
    if !sql.isEmpty { #expect(sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK) }
    return try SQLiteLocation(path)
}

private func openSession(_ sql: String = "") async throws -> SQLiteSession {
    try await SQLiteSession.open(makeDatabase(sql))
}

private let shop = """
    create table customers (id integer primary key, name text not null, note text);
    insert into customers values (1, 'Ana', 'first'), (2, 'Bia', null), (3, 'ação 🚀', 'x');
    create table log (message text, level integer);
    insert into log values ('a', 1), ('b', 2), ('c', 3);
    create table codes (code text primary key, label text) without rowid;
    insert into codes values ('b', 'Beta'), ('a', 'Alpha');
    create view named as select name from customers;
    create table pair (a integer, b integer, v text, primary key (b, a));
    insert into pair values (1, 20, 'x'), (2, 10, 'y');
    """

private func relation(_ session: SQLiteSession, _ name: String) async throws -> Relation {
    try #require(try await session.relations().first { $0.name == name })
}

// MARK: Opening

@Test func missingFileIsAnError() async throws {
    let location = try SQLiteLocation("/tmp/dblens-missing-\(UUID().uuidString).db")
    let error = try await #require(throws: DatabaseError.self) { try await SQLiteSession.open(location) }
    #expect(error.message == "No SQLite file at \(location.path)")
}

@Test func notADatabaseFails() async throws {
    let path = NSTemporaryDirectory() + "dblens-\(UUID().uuidString).db"
    try Data("this is plain text, not a database file at all, long enough to be read".utf8).write(to: URL(fileURLWithPath: path))
    await #expect(throws: DatabaseError.self) { try await SQLiteSession.open(try SQLiteLocation(path)) }
}

@Test func sessionBasics() async throws {
    let session = try await openSession()
    #expect(session.engine == .sqlite)
    #expect(session.tunnel == nil)
    #expect(session.serverVersion.split(separator: ".").count >= 3)
    #expect(!session.isReadOnly)
    #expect(session.quote("we\"ird") == "\"we\"\"ird\"")
}

// MARK: Executing

@Test func executesSeveralStatements() async throws {
    let session = try await openSession()
    let run = try await session.execute("""
        -- a comment first
        create table t (id integer primary key, n text);
        insert into t (n) values ('a'), ('b'), ('c');
        update t set n = 'z' where id > 1;
        ;
        delete from t where id = 1;
        select id, n from t order by id; /* trailing */
        """)
    #expect(run.results.map(\.command) == ["CREATE", "INSERT 3", "UPDATE 2", "DELETE 1", "SELECT 2"])
    #expect(run.main?.rows == [["2", "z"], ["3", "z"]])
    #expect(run.main?.columns.map(\.name) == ["id", "n"])
    #expect(run.main?.columns.map(\.type) == ["INTEGER", "TEXT"])
}

@Test func emptyAndWhitespaceInput() async throws {
    let session = try await openSession()
    #expect(try await session.execute("  ; -- nothing\n").results.isEmpty)
    #expect(try await session.execute("").results.isEmpty)
}

@Test func selectWithNoRowsKeepsColumns() async throws {
    let session = try await openSession("create table t (a int, b text)")
    let result = try #require(try await session.execute("select * from t").main)
    #expect(result.command == "SELECT 0")
    #expect(result.columns.map(\.name) == ["a", "b"])
    #expect(result.hasRows)
}

@Test func errorsPassThrough() async throws {
    let session = try await openSession()
    let error = try await #require(throws: DatabaseError.self) { try await session.execute("select * from nope") }
    #expect(error.message == "no such table: nope")
    // Statements before the failing one already ran.
    await #expect(throws: DatabaseError.self) { try await session.execute("create table ok (a); select * from nope") }
    #expect(try await session.relations().map(\.name) == ["ok"])
}

@Test func nullsBlobsAndUnicode() async throws {
    let session = try await openSession()
    let long = String(repeating: "ab", count: 80)
    let run = try await session.execute("""
        select null, x'00ff10', x'', zeroblob(0), x'\(long)', 'ação 🚀', 42, 1.5, 'a' || char(0) || 'b';
        """)
    let row = try #require(run.main?.rows.first)
    #expect(row[0] == nil)
    #expect(row[1] == "0x00ff10")
    #expect(row[2] == "0x")
    #expect(row[3] == "0x")
    #expect(row[4] == "0x" + String(repeating: "ab", count: 64) + "…")
    #expect(row[5] == "ação 🚀")
    #expect(row[6] == "42")
    #expect(row[7] == "1.5")
    #expect(row[8] == "a\u{0}b")
}

@Test func rowLimitCutsResult() async throws {
    let session = try await openSession()
    let result = try #require(try await session.execute("""
        with recursive c(x) as (select 1 union all select x + 1 from c limit 6000) select x from c
        """).main)
    #expect(result.rows.count == QueryResult.rowLimit)
    #expect(result.totalRows == 6000)
    #expect(result.truncated)
}

@Test func foreignKeysAreOn() async throws {
    let session = try await openSession("""
        create table p (id integer primary key);
        create table c (pid integer references p(id));
        """)
    await #expect(throws: DatabaseError.self) { try await session.execute("insert into c values (99)") }
}

@Test func cancelInterruptsLongQuery() async throws {
    let session = try await openSession()
    let query = Task {
        try await session.execute("with recursive c(x) as (select 1 union all select x + 1 from c) select count(*) from c")
    }
    try await Task.sleep(for: .milliseconds(300))
    session.cancel()
    let error = try await #require(throws: DatabaseError.self) { try await query.value }
    #expect(error.message.lowercased().contains("interrupt"))
    // The session works afterwards.
    #expect(try await session.execute("select 1").main?.rows == [["1"]])
}

// MARK: Browsing

@Test func listsTablesAndViews() async throws {
    let session = try await openSession(shop + "create table sqlitelike (a); create index i on log(level);")
    let relations = try await session.relations()
    #expect(relations.map(\.name) == ["codes", "customers", "log", "named", "pair", "sqlitelike"])
    #expect(relations.first { $0.name == "named" }?.kind == .view)
    #expect(relations.first { $0.name == "log" }?.kind == .table)
    #expect(relations.allSatisfy { $0.schema == "main" && $0.estimate == -1 })
}

@Test func pageOfPrimaryKeyTable() async throws {
    let session = try await openSession(shop)
    let page = try await session.page(of: relation(session, "customers"), limit: 2, offset: 1)
    #expect(page.rows == [["2", "Bia", nil], ["3", "ação 🚀", "x"]])
    #expect(page.columns.map(\.type) == ["INTEGER", "TEXT", "TEXT"])
    #expect(page.keys?.kind == .primaryKey)
    #expect(page.keys?.columns == ["id"])
    #expect(page.keys?.values == [["2"], ["3"]])
}

@Test func searchMatchesAnyColumn() async throws {
    let session = try await openSession(shop)
    let customers = try await relation(session, "customers")
    #expect(try await session.page(of: customers, limit: 10, offset: 0, search: "bia").rows == [["2", "Bia", nil]])
    #expect(try await session.count(of: customers, search: "bia") == 1)
    // Read-only queries: SQLite refuses every way to write.
    #expect(try await session.readOnly("select count(*) from customers").main?.rows.count == 1)
    let before = try await session.count(of: customers)
    for attempt in ["insert into customers(name) values ('x')", "select 1; delete from customers", "delete from customers",
                    "pragma query_only = off", "attach database '/tmp/dblens-ro.db' as x", "begin", "vacuum"] {
        await #expect(throws: (any Error).self, "\(attempt)") { try await session.readOnly(attempt) }
    }
    #expect(try await session.count(of: customers) == before)
    _ = try await session.execute("update customers set name = name where id = 1")
    #expect(try await session.page(of: customers, limit: 10, offset: 0, search: "_").rows.isEmpty)
    let byName = try await session.page(of: customers, limit: 10, offset: 0, sort: ColumnSort(column: "name", ascending: false))
    let upward = try await session.page(of: customers, limit: 10, offset: 0, sort: ColumnSort(column: "name", ascending: true))
    #expect(byName.rows.map { $0[1] } == Array(upward.rows.map { $0[1] }.reversed()) && byName.rows.count > 1)
    #expect(byName.keys?.values.count == byName.rows.count)
}

@Test func pageOfRowidTable() async throws {
    let session = try await openSession(shop)
    let page = try await session.page(of: relation(session, "log"), limit: 10, offset: 0)
    #expect(page.columns.map(\.name) == ["message", "level"])
    #expect(page.columns.map(\.type) == ["TEXT", "INTEGER"])
    #expect(page.rows == [["a", "1"], ["b", "2"], ["c", "3"]])
    #expect(page.keys?.kind == .rowid)
    #expect(page.keys?.columns == ["rowid"])
    #expect(page.keys?.values == [["1"], ["2"], ["3"]])
}

@Test func pageOfWithoutRowidTable() async throws {
    let session = try await openSession(shop)
    let page = try await session.page(of: relation(session, "codes"), limit: 10, offset: 0)
    #expect(page.rows == [["a", "Alpha"], ["b", "Beta"]])
    #expect(page.keys?.kind == .primaryKey)
    #expect(page.keys?.columns == ["code"])
}

@Test func compositePrimaryKeyFollowsKeyOrder() async throws {
    let session = try await openSession(shop)
    let page = try await session.page(of: relation(session, "pair"), limit: 10, offset: 0)
    #expect(page.keys?.columns == ["b", "a"])
    #expect(page.rows.map { $0[1] } == ["10", "20"])
    #expect(page.keys?.values == [["10", "2"], ["20", "1"]])
}

@Test func pageOfViewHasNoKeys() async throws {
    let session = try await openSession(shop)
    let page = try await session.page(of: relation(session, "named"), limit: 10, offset: 0)
    #expect(page.keys == nil)
    #expect(page.rows.count == 3)
}

@Test func countsRows() async throws {
    let session = try await openSession(shop)
    #expect(try await session.count(of: relation(session, "log")) == 3)
}

// MARK: Editing

@Test func updatesByPrimaryKey() async throws {
    let session = try await openSession(shop)
    let table = try await relation(session, "customers")
    let keys = try #require(try await session.page(of: table, limit: 10, offset: 0).keys)
    let outcome = try await session.update(table, keys: keys, row: 1, changes: [("name", "Beatriz"), ("note", nil)])
    #expect(outcome.sql == "update \"main\".\"customers\" set \"name\" = ?, \"note\" = ? where \"id\" = ?")
    #expect(outcome.command == "UPDATE 1")
    #expect(outcome.affected == 1)
    #expect(try await session.execute("select name, note from customers where id = 2").main?.rows == [["Beatriz", nil]])
}

@Test func updatesInsertsAndDeletesByRowid() async throws {
    let session = try await openSession(shop)
    let table = try await relation(session, "log")
    var keys = try #require(try await session.page(of: table, limit: 10, offset: 0).keys)

    _ = try await session.update(table, keys: keys, row: 0, changes: [("message", "changed")])
    let added = try await session.insert(table, values: [("message", "d"), ("level", "4")])
    #expect(added.sql == "insert into \"main\".\"log\" (\"message\", \"level\") values (?, ?)")
    #expect(added.command == "INSERT 1")
    _ = try await session.delete(table, keys: keys, row: 2)

    #expect(try await session.execute("select message, level from log order by rowid").main?.rows
        == [["changed", "1"], ["b", "2"], ["d", "4"]])

    keys = try #require(try await session.page(of: table, limit: 10, offset: 0).keys)
    #expect(keys.values.count == 3)
}

@Test func insertWithDefaults() async throws {
    let session = try await openSession("create table t (id integer primary key, n text default 'x')")
    let table = try await relation(session, "t")
    let outcome = try await session.insert(table, values: [])
    #expect(outcome.sql == "insert into \"main\".\"t\" default values")
    #expect(try await session.execute("select id, n from t").main?.rows == [["1", "x"]])
}

@Test func staleRowidIsRefused() async throws {
    let session = try await openSession(shop)
    let table = try await relation(session, "log")
    let keys = try #require(try await session.page(of: table, limit: 10, offset: 0).keys)
    _ = try await session.execute("delete from log where rowid = 2")
    let expected = "The row changed or was deleted since it was loaded. Reload the table and try again."
    var error = try await #require(throws: DatabaseError.self) {
        try await session.update(table, keys: keys, row: 1, changes: [("message", "zzz")])
    }
    #expect(error.message == expected)
    error = try await #require(throws: DatabaseError.self) { try await session.delete(table, keys: keys, row: 1) }
    #expect(error.message == expected)
    // The failed edit left no open transaction behind.
    _ = try await session.execute("begin; commit")
}

@Test func editTouchingManyRowsRollsBack() async throws {
    let session = try await openSession(shop)
    let table = try await relation(session, "log")
    // A key that matches more than one row: all three share rowid-less NULL-free level? Use a fake key column.
    let keys = RowKeys(columns: ["level > 0 or rowid"], kind: .rowid, values: [["1"]])
    let error = try await #require(throws: DatabaseError.self) {
        try await session.update(table, keys: keys, row: 0, changes: [("message", "boom")])
    }
    #expect(error.message == "The change would have affected 3 rows and was rolled back.")
    #expect(try await session.execute("select message from log order by rowid").main?.rows == [["a"], ["b"], ["c"]])
}

@Test func nullKeyValueMatchesNull() async throws {
    let session = try await openSession("create table t (k text primary key, v text); insert into t values (null, 'n'), ('a', 'x')")
    let table = try await relation(session, "t")
    let keys = RowKeys(columns: ["k"], kind: .primaryKey, values: [[nil]])
    _ = try await session.update(table, keys: keys, row: 0, changes: [("v", "changed")])
    #expect(try await session.execute("select v from t where k is null").main?.rows == [["changed"]])
}

@Test func constraintFailureRollsBack() async throws {
    let session = try await openSession(shop)
    let table = try await relation(session, "customers")
    let keys = try #require(try await session.page(of: table, limit: 10, offset: 0).keys)
    await #expect(throws: DatabaseError.self) {
        try await session.update(table, keys: keys, row: 0, changes: [("name", nil)])
    }
    #expect(try await session.execute("select name from customers where id = 1").main?.rows == [["Ana"]])
    _ = try await session.execute("begin; commit")
}

@Test func valuesAreBoundNotInlined() async throws {
    let session = try await openSession(shop)
    let table = try await relation(session, "log")
    let nasty = "x'); drop table log; --"
    let outcome = try await session.insert(table, values: [("message", nasty)])
    #expect(!outcome.sql.contains("drop"))
    #expect(try await session.count(of: table) == 4)
    #expect(try await session.execute("select message from log where rowid = 4").main?.rows == [[nasty]])
}

@Test func readOnlyFileRefusesEdits() async throws {
    let location = try makeDatabase(shop)
    try FileManager.default.setAttributes([.posixPermissions: 0o444], ofItemAtPath: location.path)
    defer { try? FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: location.path) }
    let session = try await SQLiteSession.open(location)
    #expect(session.isReadOnly)

    let table = try await relation(session, "log")
    let keys = try #require(try await session.page(of: table, limit: 10, offset: 0).keys)
    let refused = "This file is read-only."
    var error = try await #require(throws: DatabaseError.self) {
        try await session.update(table, keys: keys, row: 0, changes: [("message", "no")])
    }
    #expect(error.message == refused)
    error = try await #require(throws: DatabaseError.self) { try await session.insert(table, values: [("message", "no")]) }
    #expect(error.message == refused)
    error = try await #require(throws: DatabaseError.self) { try await session.delete(table, keys: keys, row: 0) }
    #expect(error.message == refused)
    // Reading still works.
    #expect(try await session.count(of: table) == 3)
}
