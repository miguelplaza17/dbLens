import Foundation
import Testing
@testable import DBLensKit

@Test func acceptsFullURLs() throws {
    let url = try ConnectionURL("  postgresql://ana:s%40nha@db.example.com:6543/loja?sslmode=require ")
    #expect(url.host == "db.example.com")
    #expect(url.port == 6543)
    #expect(url.user == "ana")
    #expect(url.password == "s@nha")
    #expect(url.database == "loja")
    #expect(url.suggestedName == "loja @ db.example.com")
    #expect(!url.redacted.contains("s%40nha"))
    #expect(url.forLibpq == "host='db.example.com' port='6543' dbname='loja' user='ana' password='s@nha' sslmode='require' connect_timeout='10'")
}

@Test func rejectsOtherInput() {
    #expect(throws: ConnectionURL.Problem.empty) { try ConnectionURL("   ") }
    #expect(throws: ConnectionURL.Problem.notPostgres) { try ConnectionURL("mysql://a@b/c") }
    #expect(throws: ConnectionURL.Problem.notPostgres) { try ConnectionURL("só texto") }
}

@Test func databaseDefaultsToUser() throws {
    #expect(try ConnectionURL("postgres://ana@localhost").database == "ana")
}

@Test("Formats people paste", arguments: [
    // (pasted, host, port, database, user, sslmode)
    ("jdbc:postgresql://db.x.com:5433/loja?user=ana&password=pw&ssl=true", "db.x.com", 5433, "loja", "ana", "require"),
    ("postgresql+psycopg2://ana:pw@db.x.com/loja", "db.x.com", nil, "loja", "ana", nil),
    ("postgres+asyncpg://ana@db.x.com/loja", "db.x.com", nil, "loja", "ana", nil),
    ("DATABASE_URL=\"postgresql://ana:pw@db.x.com:5432/loja?schema=public\"", "db.x.com", 5432, "loja", "ana", nil),
    ("export DATABASE_URL='postgres://ana@db.x.com/loja?sslmode=no-verify'", "db.x.com", nil, "loja", "ana", "require"),
    ("postgresql://ana@h1.x.com:5432,h2.x.com:5433/loja?target_session_attrs=read-write", "h1.x.com", 5432, "loja", "ana", nil),
    ("postgresql:///loja?host=/var/run/postgresql", "localhost", nil, "loja", nil, nil),
    ("postgresql://%2Fvar%2Frun%2Fpostgresql/loja", "localhost", nil, "loja", nil, nil),
    ("postgresql://ana@[::1]:5432/loja", "::1", 5432, "loja", "ana", nil),
    ("postgres://ana:pw@db.x.com/loja?connection_limit=5&pgbouncer=true", "db.x.com", nil, "loja", "ana", nil),
    ("host=db.x.com port=5432 dbname=loja user=ana password='it\\'s secret' sslmode=require", "db.x.com", 5432, "loja", "ana", "require"),
    ("postgresql://ana:pw@db.x.com/loja?username=outra&database=outro", "db.x.com", nil, "loja", "ana", nil),
] as [(String, String, Int?, String, String?, String?)])
func pastedFormats(_ text: String, host: String, port: Int?, database: String, user: String?, sslmode: String?) throws {
    let url = try ConnectionURL(text)
    #expect(url.host == host)
    #expect(url.port == port)
    #expect(url.database == database)
    #expect(url.user == user)
    #expect(url.sslmode == sslmode)
    // libpq refuses settings it doesn't know; none get through.
    for bad in ["schema", "connection_limit", "pgbouncer", "ssl=", "username", "database="] {
        #expect(!url.forLibpq.contains(bad), "\(bad) in \(url.forLibpq)")
    }
}

@Test func driverOptionsBecomeLibpqOnes() throws {
    let prisma = try ConnectionURL("postgresql://ana@db.x.com/loja?schema=vendas")
    #expect(prisma.settings["options"] == "-c search_path=vendas")
    let keyValue = try ConnectionURL("host=db.x.com dbname=loja password='it\\'s secret'")
    #expect(keyValue.password == "it's secret")
    #expect(keyValue.forLibpq.contains("password='it\\'s secret'"))
    #expect(!keyValue.redacted.contains("secret"))
    let hosts = try ConnectionURL("postgresql://h1:5432,h2:5433/loja")
    #expect(hosts.settings["host"] == "h1,h2" && hosts.settings["port"] == "5432,5433")
}

@Test func composesURLsFromFields() throws {
    let text = ConnectionURL.compose(host: "db.x.com", port: "5433", database: "minha loja", user: "ana",
                                     password: "p@ss:w/rd", sslmode: "require")
    #expect(text == "postgresql://ana:p%40ss%3Aw%2Frd@db.x.com:5433/minha%20loja?sslmode=require")
    let url = try ConnectionURL(text)
    #expect(url.password == "p@ss:w/rd" && url.database == "minha loja" && url.port == 5433)
}

@Test func quotesIdentifiers() {
    #expect(quoteIdent("my\"table") == "\"my\"\"table\"")
}

/// Runs only with DBLENS_TEST_URL set to a database you can write to.
@Test(.enabled(if: ProcessInfo.processInfo.environment["DBLENS_TEST_URL"] != nil))
func talksToPostgres() async throws {
    let url = try ConnectionURL(ProcessInfo.processInfo.environment["DBLENS_TEST_URL"]!)
    let session = try await PostgresSession.connect(url)

    let run = try await session.execute("create temp table t(a int, b text); insert into t values (1,'x'),(2,null); select * from t order by a")
    #expect(run.results.count == 3)
    let main = try #require(run.main)
    #expect(main.columns.map(\.name) == ["a", "b"])
    #expect(main.rows == [["1", "x"], ["2", nil]])

    await #expect(throws: PostgresError.self) { try await session.execute("select * from nao_existe") }
    // The connection still works after an error.
    #expect(try await session.execute("select 1").main?.rows == [["1"]])

    let relations = try await session.relations()
    if let first = relations.first {
        let page = try await session.page(of: first, limit: 5, offset: 0)
        #expect(page.rows.count <= 5)
        #expect(page.columns.allSatisfy { !$0.type.isEmpty })
    }
}

@Test func badHostFailsWithMessage() async {
    await #expect(throws: PostgresError.self) {
        try await PostgresSession.connect(try ConnectionURL("postgres://x@127.0.0.1:1/db"))
    }
}

/// libpq's translated messages keep their accents ("não", not "n~ao").
@Test func errorMessagesKeepAccents() async {
    do {
        _ = try await PostgresSession.connect(try ConnectionURL("postgres://x@host-que-nao-existe.invalid/db"))
        Issue.record("connected to a host that does not exist")
    } catch {
        print("libpq says:", error.localizedDescription)
        #expect(!error.localizedDescription.contains("~"))
    }
}

/// Edits through primary keys and ctid. Needs DBLENS_TEST_URL.
@Test(.enabled(if: ProcessInfo.processInfo.environment["DBLENS_TEST_URL"] != nil))
func editsRows() async throws {
    let session = try await PostgresSession.connect(try ConnectionURL(ProcessInfo.processInfo.environment["DBLENS_TEST_URL"]!))
    _ = try await session.execute("""
        drop table if exists dblens_kit_pk, dblens_kit_nopk;
        create table dblens_kit_pk(id int primary key, nome text, n int default 7);
        create table dblens_kit_nopk(nome text, n int);
        insert into dblens_kit_pk values (1,'a',1),(2,'b',2);
        insert into dblens_kit_nopk values ('x',1),('x',1);
        """)
    defer { Task { _ = try? await session.execute("drop table if exists dblens_kit_pk, dblens_kit_nopk") } }
    let relations = try await session.relations()
    let pk = try #require(relations.first { $0.name == "dblens_kit_pk" })
    let nopk = try #require(relations.first { $0.name == "dblens_kit_nopk" })
    #expect(try await session.page(of: pk, limit: 10, offset: 0, search: "B").rows == [["2", "b", "2"]])
    #expect(try await session.count(of: pk, search: "B") == 1)
    #expect(try await session.page(of: pk, limit: 10, offset: 0, search: "%").rows.isEmpty)

    var page = try await session.page(of: pk, limit: 10, offset: 0)
    let keys = try #require(page.keys)
    #expect(keys.columns == ["id"] && !keys.usesCtid)
    _ = try await session.update(pk, keys: keys, row: 1, changes: [("nome", "bê"), ("n", nil)])
    _ = try await session.insert(pk, values: [("id", "3"), ("nome", "c")])
    page = try await session.page(of: pk, limit: 10, offset: 0)
    #expect(page.rows == [["1", "a", "1"], ["2", "bê", nil], ["3", "c", "7"]])
    await #expect(throws: PostgresError.self) { try await session.update(pk, keys: page.keys!, row: 0, changes: [("n", "não é número")]) }

    page = try await session.page(of: nopk, limit: 10, offset: 0)
    #expect(page.columns.map(\.name) == ["nome", "n"])
    let ctids = try #require(page.keys)
    #expect(ctids.usesCtid)
    // Identical rows: ctid still edits only one.
    _ = try await session.update(nopk, keys: ctids, row: 0, changes: [("n", "2")])
    _ = try await session.delete(nopk, keys: ctids, row: 1)
    #expect(try await session.execute("select n from dblens_kit_nopk").main?.rows == [["2"]])
    // Another session changes the row after it was loaded: the old ctid must not match anything.
    page = try await session.page(of: nopk, limit: 10, offset: 0)
    _ = try await session.execute("update dblens_kit_nopk set n = n")
    await #expect(throws: PostgresError.self) { try await session.update(nopk, keys: page.keys!, row: 0, changes: [("n", "9")]) }
    #expect(try await session.execute("select n from dblens_kit_nopk").main?.rows == [["2"]])
}

@Test func tunnelArgumentsAndLocalPort() throws {
    let config = SSHTunnel.Config(host: "bastion.x.com", port: 2222, user: "ana", keyPath: "~/.ssh/id_ed25519")
    let args = SSHTunnel.arguments(config, localPort: 61000, remoteHost: "localhost", remotePort: 5433)
    #expect(args.contains("127.0.0.1:61000:localhost:5433"))
    #expect(args.suffix(4) == ["-l", "ana", "--", "bastion.x.com"])
    #expect(args.contains("BatchMode=yes") && args.contains("2222"))
    #expect(!args.contains("~/.ssh/id_ed25519"))
    #expect(SSHTunnel.arguments(.init(host: "bastion"), localPort: 1, remoteHost: "::1", remotePort: 5432)
        .contains("127.0.0.1:1:[::1]:5432"))
    let url = try ConnectionURL("postgres://ana:pw@localhost:5433/loja?sslmode=disable")
    #expect(url.forLibpq(throughLocalPort: 61000) == "host='127.0.0.1' port='61000' dbname='loja' user='ana' password='pw' sslmode='disable' connect_timeout='10'")
}

@Test func tunnelReportsSSHErrors() async {
    await #expect(throws: PostgresError.self) {
        _ = try await SSHTunnel.open(.init(host: "127.0.0.1", port: 1), to: "localhost", port: 5432)
    }
}

/// End to end through a real sshd. Needs DBLENS_TEST_SSH ("user@host:port|keyfile")
/// and DBLENS_TEST_URL as the server sees the database.
@Test(.enabled(if: ProcessInfo.processInfo.environment["DBLENS_TEST_SSH"] != nil))
func connectsThroughSSH() async throws {
    let env = ProcessInfo.processInfo.environment
    let parts = env["DBLENS_TEST_SSH"]!.split(separator: "|").map(String.init)
    let target = parts[0].split(separator: ":").map(String.init)
    let config = SSHTunnel.Config(host: target[0], port: target.count > 1 ? Int(target[1]) : nil,
                                  keyPath: parts.count > 1 ? parts[1] : "")
    let session = try await PostgresSession.connect(try ConnectionURL(env["DBLENS_TEST_URL"]!), via: config)
    #expect(session.tunnel?.isRunning == true)
    #expect(try await session.execute("select 1").main?.rows == [["1"]])
}

@Test func secretBoxRoundTripsAndRefusesTampering() throws {
    let dir = FileManager.default.temporaryDirectory.appending(path: "dblens-box-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: dir) }
    let keyFile = dir.appending(path: "connections.key")
    let box = try SecretBox(keyFile: keyFile)
    let sealed = try box.seal(Data("postgres://ana:segredo@db/loja".utf8))
    #expect(!String(decoding: sealed, as: UTF8.self).contains("segredo"))
    // A second box from the same key file opens it; the key file holds no plain key.
    let again = try SecretBox(keyFile: keyFile)
    #expect(String(decoding: try again.open(sealed), as: UTF8.self) == "postgres://ana:segredo@db/loja")
    let attributes = try FileManager.default.attributesOfItem(atPath: keyFile.path)
    #expect((attributes[.posixPermissions] as? Int) == 0o600)
    var tampered = sealed
    tampered[tampered.count - 1] ^= 0xFF
    #expect(throws: SecretBox.Failure.self) { try again.open(tampered) }
    // A damaged key file is refused, never replaced by a new key.
    let broken = dir.appending(path: "broken.key")
    try Data("not a key".utf8).write(to: broken)
    #expect(throws: SecretBox.Failure.self) { try SecretBox(keyFile: broken) }
    #expect(try Data(contentsOf: broken) == Data("not a key".utf8))
    #expect(throws: SecretBox.Failure.self) { try SecretBox(keyFile: dir.appending(path: "absent.key"), create: false) }
    // Another key file (another Mac) can't open it.
    let other = try SecretBox(keyFile: dir.appending(path: "other.key"))
    #expect(throws: SecretBox.Failure.self) { try other.open(sealed) }
}

@Test("Masked passwords come back on save", arguments: [
    "postgresql://ana:p%40ss@db.x.com:5432/loja?sslmode=require",
    "postgres://ana@db.x.com/loja?password=p%40ss",
    "host=db.x.com dbname=loja user=ana password='p@ss'",
])
func unmasksPasswords(_ original: String) throws {
    let saved = try ConnectionURL(original)
    #expect(!saved.redacted.contains("p@ss") && !saved.redacted.contains("p%40ss"))
    // Unchanged, or with another part edited: the real password returns.
    #expect(try ConnectionURL(ConnectionURL.unmask(saved.redacted, original: saved)).password == "p@ss")
    let edited = saved.redacted.replacingOccurrences(of: "loja", with: "outra")
    let back = try ConnectionURL(ConnectionURL.unmask(edited, original: saved))
    #expect(back.password == "p@ss" && back.database == "outra")
    // A new password typed over the mask wins.
    let typed = edited.replacingOccurrences(of: ConnectionURL.mask, with: "nova")
    #expect(try ConnectionURL(ConnectionURL.unmask(typed, original: saved)).password == "nova")
}

@Test("SSH values that could run commands are refused", arguments: [
    SSHTunnel.Config(host: "-oProxyCommand=open -a Calculator"),
    SSHTunnel.Config(host: "a;touch /tmp/x"),
    SSHTunnel.Config(host: "bastion", user: "-oProxyCommand=x"),
    SSHTunnel.Config(host: "bastion", user: "ana bob"),
    SSHTunnel.Config(host: "bastion", keyPath: "-oX"),
])
func refusesDangerousSSHValues(_ config: SSHTunnel.Config) async {
    #expect(SSHTunnel.problem(config, remoteHost: "localhost") != nil)
    await #expect(throws: PostgresError.self) { _ = try await SSHTunnel.open(config, to: "localhost", port: 5432) }
}

@Test func acceptsNormalSSHValues() {
    for host in ["bastion", "ana@db.x.com", "100.64.0.1", "[::1]", "my-host_1.example.com"] {
        #expect(SSHTunnel.problem(.init(host: host, user: "ana.b-1", keyPath: "~/.ssh/id_ed25519"), remoteHost: "db-service-internal") == nil, "\(host)")
    }
    #expect(SSHTunnel.problem(.init(host: "bastion"), remoteHost: "db;rm") != nil)
}

@Test("Passwords never show, in any form", arguments: [
    ("postgres://u:p@ss@db.x.com/loja", "p@ss"),
    ("postgres://u@db.x.com/loja?PASSWORD=segredo", "segredo"),
    ("postgres://u:pw@db.x.com/loja?sslpassword=chave&sslmode=require", "chave"),
    ("host=db user=u Password=segredo dbname=loja", "segredo"),
])
func redactsEverySecret(_ text: String, secret: String) throws {
    let url = try ConnectionURL(text)
    #expect(!url.redacted.contains(secret), "\(url.redacted)")
    #expect(try ConnectionURL(ConnectionURL.unmask(url.redacted, original: url)).settings == url.settings)
}

@Test("The engine comes from what was pasted", arguments: [
    ("postgres://u:p@db/x", Engine.postgres),
    ("jdbc:postgresql://db:5432/x?user=u", .postgres),
    ("DATABASE_URL=\"postgresql+psycopg2://u@db/x\"", .postgres),
    ("host=db dbname=x user=u", .postgres),
    ("mysql://root:pw@127.0.0.1:3306/shop", .mysql),
    ("mariadb://u@db/x", .mysql),
    ("jdbc:mysql://db:3306/x", .mysql),
    ("mysql+pymysql://u:p@db/x", .mysql),
    ("sqlite:///Users/me/app.db", .sqlite),
    ("~/Projects/app/data.sqlite3", .sqlite),
    ("file:/tmp/test.db", .sqlite),
    ("redis://:secret@cache:6379/0", .redis),
    ("rediss://default:pw@redis.example.com:6380", .redis),
    ("REDIS_URL=redis://localhost", .redis),
    ("mongodb://u:p@mongo:27017/app?authSource=admin", .mongodb),
    ("mongodb+srv://u:p@cluster0.abcd.mongodb.net/app", .mongodb),
])
func detectsEngines(_ text: String, engine: Engine) {
    #expect(Engine.detect(text) == engine)
}

@Test func detectsNothingForOtherText() {
    for text in ["", "hello", "https://example.com", "oracle://db/x"] {
        #expect(Engine.detect(text) == nil, "\(text)")
    }
}

/// The schema board's graph from every SQL engine: tables, primary keys, and a composite foreign key.
@Test func schemaGraphFromSQLite() async throws {
    let file = FileManager.default.temporaryDirectory.appending(path: "graph-\(UUID().uuidString).sqlite")
    FileManager.default.createFile(atPath: file.path, contents: nil)
    defer { try? FileManager.default.removeItem(at: file) }
    let session = try await SQLiteSession.open(try SQLiteLocation(file.path))
    _ = try await session.execute("""
        create table customers(id integer primary key, name text not null);
        create table orders(id integer primary key, customer_id integer references customers, total real);
        create table lines(order_id integer, n integer, sku text, primary key(order_id, n), foreign key(order_id) references orders(id));
        """)
    let graph = try await session.schemaGraph()
    #expect(graph.tables.map(\.name) == ["customers", "lines", "orders"])
    #expect(graph.tables.first { $0.name == "lines" }?.columns.filter(\.isPrimaryKey).map(\.name) == ["order_id", "n"])
    #expect(Set(graph.links.map { "\($0.from)->\($0.to) \($0.fromColumns)=\($0.toColumns)" }) == [
        "main.orders->main.customers [\"customer_id\"]=[\"id\"]",
        "main.lines->main.orders [\"order_id\"]=[\"id\"]",
    ])
}

@Test(arguments: ["DBLENS_TEST_URL", "DBLENS_TEST_MYSQL_URL"])
func schemaGraphFromServers(_ variable: String) async throws {
    guard let text = ProcessInfo.processInfo.environment[variable] else { return }
    let session = try await Connector.connect(text, ssh: nil)
    _ = try await session.execute("""
        drop table if exists dbl_lines; drop table if exists dbl_orders; drop table if exists dbl_customers;
        create table dbl_customers(id int primary key, name varchar(40) not null);
        create table dbl_orders(id int primary key, customer_id int references dbl_customers(id), total numeric(10,2));
        create table dbl_lines(order_id int, n int, sku varchar(20), primary key(order_id, n),
                               foreign key(order_id) references dbl_orders(id));
        """)
    defer { Task { _ = try? await session.execute("drop table dbl_lines; drop table dbl_orders; drop table dbl_customers;") } }
    let graph = try await session.schemaGraph()
    let lines = try #require(graph.tables.first { $0.name == "dbl_lines" })
    #expect(lines.columns.filter(\.isPrimaryKey).map(\.name) == ["order_id", "n"])
    #expect(lines.columns.first { $0.name == "sku" }?.isNullable == true)
    let names = Set(graph.links.filter { $0.from.hasSuffix("dbl_lines") || $0.from.hasSuffix("dbl_orders") }
        .map { "\($0.from.split(separator: ".").last!)->\($0.to.split(separator: ".").last!)" })
    #expect(names.contains("dbl_lines->dbl_orders"))
    // MySQL ignores inline REFERENCES on a column; only Postgres has the orders link from it.
    if variable == "DBLENS_TEST_URL" { #expect(names.contains("dbl_orders->dbl_customers")) }
}

@Test func clientCommandsKeepPasswordsOutOfArguments() throws {
    let pg = try ClientCommand.make(for: "postgres://ana:s3cr%27t@db.x.com:5433/loja?sslmode=require")
    #expect(pg.tools == ["psql"] && pg.environment == ["PGPASSWORD": "s3cr't"])
    #expect(pg.arguments == ["host='db.x.com' port='5433' dbname='loja' user='ana' sslmode='require'"])
    #expect(try ClientCommand.make(for: "postgres://ana:pw@db.x.com/loja", tunnelPort: 61000).arguments.first?.contains("host='127.0.0.1' port='61000'") == true)
    let my = try ClientCommand.make(for: "mysql://root:pw@db.x.com:3307/shop?ssl-mode=REQUIRED")
    #expect(my.arguments == ["--host=db.x.com", "--port=3307", "--protocol=TCP", "--user=root", "--ssl-mode=REQUIRED", "shop"])
    #expect(my.environment == ["MYSQL_PWD": "pw"])
    let redis = try ClientCommand.make(for: "rediss://default:pw@cache.x.com:6380/2")
    #expect(redis.arguments == ["-h", "cache.x.com", "-p", "6380", "-n", "2", "--user", "default", "--tls"])
    #expect(redis.environment == ["REDISCLI_AUTH": "pw"])
    let lite = try ClientCommand.make(for: "sqlite:///tmp/app.db")
    #expect(lite.arguments == ["/tmp/app.db"] && lite.environment.isEmpty)
    for command in [pg, my, redis] {
        #expect(!command.arguments.joined().contains("s3cr") && !command.arguments.joined().contains("pw@"))
    }
    let mongo = try ClientCommand.make(for: "mongodb://app:pw@db.x.com:27017/shop?authSource=admin", tunnelPort: 61001)
    #expect(mongo.arguments == ["mongodb://app:pw@127.0.0.1:61001/shop?authSource=admin&directConnection=true"])
    let script = pg.script(executable: "/opt/homebrew/bin/psql")
    #expect(script.contains("export PGPASSWORD='s3cr'\\''t'") && script.hasPrefix("#!/bin/zsh\nrm -f -- \"$0\"\n"))
}

/// Runs the scripts the app would open, with the installed clients and the test servers.
@Test func clientScriptsReallyConnect() async throws {
    let env = ProcessInfo.processInfo.environment
    let cases: [(String?, String)] = [
        (env["DBLENS_TEST_URL"], "select 41 + 1"),
        (env["DBLENS_TEST_REDIS_URL"], "PING"),
    ]
    for (url, query) in cases {
        guard let url, let command = try? ClientCommand.make(for: url), let tool = command.resolve() else { continue }
        // Instead of an interactive session, run one query and check the answer.
        let extra = tool.hasSuffix("psql") ? ["-At", "-c", query] : [query]
        let script = command.script(executable: tool).replacingOccurrences(of: "clear\n", with: "")
            .replacingOccurrences(of: "\n", with: " " + extra.map { "'\($0)'" }.joined(separator: " ") + "\n",
                                  options: .backwards, range: nil)
        let file = FileManager.default.temporaryDirectory.appending(path: "dblens-test-\(UUID().uuidString).command")
        try script.write(to: file, atomically: true, encoding: .utf8)
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/zsh")
        process.arguments = [file.path]
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output
        try process.run()
        process.waitUntilExit()
        let text = String(decoding: output.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        #expect(text.contains(tool.hasSuffix("psql") ? "42" : "PONG"), "\(tool): \(text)")
        #expect(!FileManager.default.fileExists(atPath: file.path), "the script removes itself")
    }
}
