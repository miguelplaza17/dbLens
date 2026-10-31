import AppKit
import SwiftData
import DBLensKit

/// Self-test for agents: runs the app's real functions on a test store and a
/// real Postgres, prints PASS or FAIL per check, and quits with exit code 0 or 1.
///   APP_STORE=/tmp/dbLens-self.store APP_SEED_URL=postgres://… dbLens --selftest
/// (tools/selftest.sh does this.) Use a throwaway database: it creates and drops a table.
@MainActor
enum SelfTest {
    static var requested: Bool { CommandLine.arguments.contains("--selftest") }
    /// Read by AppDelegate.applicationWillTerminate.
    static var exitCode: Int32 = 0
    private static var failures = 0
    private static var count = 0

    static func runIfRequested(model: AppModel) {
        guard requested else { return }
        // Print each line at once, so a crash keeps the lines before it.
        setvbuf(stdout, nil, _IOLBF, 0)
        guard Store.isOverridden, let url = SeedData.url else {
            print("SELFTEST refused: set APP_STORE to a file in /tmp and APP_SEED_URL to a test database")
            finish(code: 1)
            return
        }
        Task {
            try? await Task.sleep(for: .seconds(0.3))
            @MainActor func connections() -> [SavedConnection] { (try? model.context.fetch(FetchDescriptor<SavedConnection>())) ?? [] }

            do {
                _ = try await model.save(url: "mysql://x@y/z", name: "")
                check("a non-Postgres URL is refused", false)
            } catch {
                check("a non-Postgres URL is refused", connections().isEmpty)
            }
            do {
                _ = try await model.save(url: "postgres://x@127.0.0.1:1/nada", name: "")
                check("a URL that does not connect is not saved", false)
            } catch {
                check("a URL that does not connect is not saved", connections().isEmpty, error.localizedDescription)
            }

            let saved = try? await model.save(url: url, name: "")
            check("a working URL is saved and opened", saved != nil && model.session != nil && connections().count == 1)
            check("the name comes from the URL", saved?.name == Connector.summary(url)?.suggestedName)
            check("the URL is kept for the connection", saved?.url == url)
            // Let Agents Write: what the switch saves must reach the file another process (the MCP server) reads.
            if let saved {
                saved.agentsCanWrite = true
                var saveError = ""
                do { try model.context.save() } catch { saveError = "\(error)" }
                let fresh = try? ModelContainer(for: SavedConnection.self, configurations: ModelConfiguration(url: Store.url))
                let stored = (try? fresh.map { try ModelContext($0).fetch(FetchDescriptor<SavedConnection>()) })??.first { $0.uuid == saved.uuid }
                check("Let Agents Write is saved for other processes", stored?.agentsCanWrite == true, saveError)
                saved.agentsCanWrite = false
                try? model.context.save()
            }
            let boxDir = FileManager.default.temporaryDirectory.appending(path: "dbLens-box-\(UUID().uuidString)")
            try? FileManager.default.createDirectory(at: boxDir, withIntermediateDirectories: true)
            let box = try? SecretBox(keyFile: boxDir.appending(path: "k"))
            let sealed = try? box?.seal(Data("segredo".utf8))
            check("saved URLs are encrypted with a key bound to this Mac", box?.hardwareBound == true
                  && sealed.flatMap { try? box?.open($0) } == Data("segredo".utf8))
            try? FileManager.default.removeItem(at: boxDir)
            check("tables are listed", !model.relations.isEmpty, "\(model.relations.count)")

            if let first = model.relations.first {
                await model.showTable(first.id)
                check("selecting a table loads its rows", model.tableResult != nil && model.tableError == nil, model.tableError ?? "")
                check("the grid has the column types", model.tableResult?.columns.allSatisfy { !$0.type.isEmpty } == true)
                if model.canGoForward {
                    await model.nextPage()
                    check("next page moves the offset", model.offset == model.pageSize)
                    await model.previousPage()
                    check("previous page goes back", model.offset == 0)
                }
                if model.relations.count > 1 {
                    let second = model.relations[1]
                    await model.showTable(second.id)
                    await model.showSchema()
                    try? await Task.sleep(for: .milliseconds(50))
                    await model.goBack()
                    let backToSecond = model.shown == .table && model.selectedRelation == second.id
                    await model.goBack()
                    let backToFirst = model.selectedRelation == first.id
                    await model.goForward()
                    check("Back and Forward walk the pages", backToSecond && backToFirst && model.selectedRelation == second.id,
                          "\(backToSecond) \(backToFirst)")
                }
            }

            model.sql = "select 1 as um, null::text as nada"
            await model.run()
            check("SQL runs and shows the result in the grid", model.shown == .query && model.shownResult?.rows == [["1", nil]])
            check("the run is logged", model.lastLog?.isError == false && model.lastLog?.sql.contains("select 1") == true)
            try? await Task.sleep(for: .milliseconds(50))
            await model.goBack()
            let leftQuery = model.shown == .table
            await model.goForward()
            check("Back and Forward include query results", leftQuery && model.shown == .query && model.shownResult?.rows == [["1", nil]])
            check("runs go to the history, newest first", model.sqlHistory.first?.sql.contains("select 1") == true)
            let exported = URL(fileURLWithPath: "/tmp/dbLens-selftest-export.csv")
            await model.export(.csv, to: exported)
            check("Export writes the result as CSV", (try? String(contentsOf: exported, encoding: .utf8)) == "um,nada\r\n1,\r\n")
            try? FileManager.default.removeItem(at: exported)
            await model.loadSchema()
            if let table = model.relations.first {
                let names = model.editorNames
                check("the editor suggests table names", names.suggestions(for: String(table.name.prefix(2)), after: nil, language: .sql).contains(table.name))
                let column = names.columns[table.name.lowercased()]?.first ?? ""
                check("after table. the editor suggests its columns", names.suggestions(for: "", after: table.name, language: .sql).contains(column))
            }

            model.sql = "select * from tabela_que_nao_existe"
            await model.run()
            check("a SQL error goes to the log", model.lastLog?.isError == true && model.lastLog?.message.contains("tabela_que_nao_existe") == true, model.lastLog?.message ?? "")

            model.sql = "select 1; select 2"
            model.sqlSelection = "select 2"
            await model.run()
            check("Run uses the selection", model.execution?.main?.rows == [["2"]])
            model.sqlSelection = ""

            let before = model.relations.count
            model.sql = "create table dblens_selftest(id serial primary key, nome text, n int default 5); insert into dblens_selftest(nome) values ('a'), ('b')"
            await model.run()
            check("CREATE refreshes the table list", model.relations.count == before + 1)

            if let table = model.relations.first(where: { $0.name == "dblens_selftest" }) {
                await model.showTable(table.id)
                check("picking a table after a query shows the table again", model.shown == .table && model.tableResult?.rows.count == 2)
                check("a table with a primary key can be edited", model.canEditRows && model.canInsertRows)
                try? await model.saveRow(0, values: [("nome", "editado"), ("n", nil)])
                check("editing a row saves it", model.tableResult?.rows.first == ["1", "editado", nil], "\(model.tableResult?.rows.first ?? [])")
                check("the edit is logged", model.lastLog?.message == "UPDATE 1")
                try? await model.saveRow(nil, values: [("nome", "novo")])
                check("a new row gets the defaults", model.tableResult?.rows.last == ["3", "novo", "5"], "\(model.tableResult?.rows.last ?? [])")
                do {
                    try await model.saveRow(0, values: [("n", "abc")])
                    check("a bad value is refused", false)
                } catch {
                    check("a bad value is refused", model.lastLog?.isError == true)
                }
                try? await model.deleteRow(1)
                check("deleting a row removes it", model.tableResult?.rows.map { $0[1] } == ["editado", "novo"])
                let copied = ResultGrid.tsv(model.tableResult!, header: true)
                check("Copy All copies every row with the header", copied == "id\tnome\tn\n1\teditado\t\n3\tnovo\t5", copied)
            }
            model.sql = "drop table dblens_selftest"
            await model.run()
            check("DROP refreshes the table list", model.relations.count == before)

            // SQLite: a file on this Mac, opened from its path.
            let sqliteFile = FileManager.default.temporaryDirectory.appending(path: "dbLens-self-\(UUID().uuidString).sqlite")
            FileManager.default.createFile(atPath: sqliteFile.path, contents: nil)
            if let file = try? await model.save(url: sqliteFile.path, name: "") {
                check("a SQLite file opens from its path", model.session?.engine == .sqlite && file.engine == "sqlite"
                      && file.name == sqliteFile.lastPathComponent, file.name)
                model.sql = "create table notes(id integer primary key, body text); insert into notes(body) values ('um'), ('dois')"
                await model.run()
                if let notes = model.relations.first(where: { $0.name == "notes" }) {
                    await model.showTable(notes.id)
                    try? await model.updateCell(row: 1, column: 1, value: "ação 🚀")
                    check("SQLite rows are listed and edited in place", model.tableResult?.rows == [["1", "um"], ["2", "ação 🚀"]],
                          "\(model.tableResult?.rows ?? [])")
                } else {
                    check("SQLite rows are listed and edited in place", false, "no notes table")
                }
                model.delete(file)
            } else {
                check("a SQLite file opens from its path", false)
            }
            try? FileManager.default.removeItem(at: sqliteFile)

            // MySQL and MariaDB, when DBLENS_TEST_MYSQL_URL / DBLENS_TEST_MARIADB_URL point at throwaway servers.
            for variable in ["DBLENS_TEST_MYSQL_URL", "DBLENS_TEST_MARIADB_URL"] {
                guard let mysqlURL = ProcessInfo.processInfo.environment[variable], !mysqlURL.isEmpty else { continue }
                guard let saved = try? await model.save(url: mysqlURL, name: "") else {
                    check("\(variable): connects", false)
                    continue
                }
                check("\(variable): connects", model.session?.engine == .mysql && saved.engine == "mysql")
                model.sql = "drop table if exists dblens_self; create table dblens_self(id int primary key, body text); insert into dblens_self values (1, 'um'), (2, 'dois')"
                await model.run()
                if let table = model.relations.first(where: { $0.name == "dblens_self" }) {
                    await model.showTable(table.id)
                    try? await model.updateCell(row: 1, column: 1, value: "ação 🚀")
                    check("\(variable): rows are listed and edited in place", model.tableResult?.rows == [["1", "um"], ["2", "ação 🚀"]],
                          "\(model.tableResult?.rows ?? [])")
                } else {
                    check("\(variable): rows are listed and edited in place", false, "no table")
                }
                model.sql = "drop table dblens_self"
                await model.run()
                model.delete(saved)
            }

            // Redis and Valkey, when DBLENS_TEST_REDIS_URL / DBLENS_TEST_VALKEY_URL point at throwaway servers.
            for variable in ["DBLENS_TEST_REDIS_URL", "DBLENS_TEST_VALKEY_URL"] {
                guard let redisURL = ProcessInfo.processInfo.environment[variable], !redisURL.isEmpty else { continue }
                guard let saved = try? await model.save(url: redisURL, name: "") else {
                    check("\(variable): connects", false)
                    continue
                }
                check("\(variable): connects, with the command panel", model.session?.engine == .redis && model.usesCommands)
                model.sql = "DEL dblens:self\nHSET dblens:self name \"Ada Lovelace\" city London"
                await model.run()
                if let key = model.relations.first(where: { $0.name == "dblens:self" }) {
                    check("\(variable): keys group by prefix", key.schema == "dblens" && key.subtype == "hash")
                    await model.showTable(key.id)
                    try? await model.updateCell(row: 0, column: 1, value: "Lisboa")
                    check("\(variable): hash fields are edited in place", model.tableResult?.rows == [["city", "Lisboa"], ["name", "Ada Lovelace"]],
                          "\(model.tableResult?.rows ?? [])")
                    await model.deleteKey(key)
                    check("\(variable): a key is deleted from the sidebar", !model.relations.contains { $0.name == "dblens:self" })
                } else {
                    check("\(variable): keys group by prefix", false, "no key")
                }
                model.delete(saved)
            }

                        // MongoDB, when DBLENS_TEST_MONGO_URL points at a throwaway server.
            if let mongoURL = ProcessInfo.processInfo.environment["DBLENS_TEST_MONGO_URL"], !mongoURL.isEmpty {
                if let saved = try? await model.save(url: mongoURL, name: "") {
                    check("MongoDB connects, with the query panel", model.session?.engine == .mongodb && model.usesQueries)
                    model.sql = "db.dblens_self.drop()\ndb.dblens_self.insertOne({_id: 1, name: 'Ada', tags: ['a', 'b']})"
                    await model.run()
                    if let collection = model.relations.first(where: { $0.name == "dblens_self" }) {
                        await model.showTable(collection.id)
                        let column = model.tableResult?.columns.firstIndex { $0.name == "name" } ?? 1
                        try? await model.updateCell(row: 0, column: column, value: "ação 🚀")
                        check("MongoDB documents are listed and edited in place",
                              model.tableResult?.rows.first?.contains("ação 🚀") == true, "\(model.tableResult?.rows ?? [])")
                    } else {
                        check("MongoDB documents are listed and edited in place", false, "no collection")
                    }
                    model.sql = "db.dblens_self.drop()"
                    await model.run()
                    model.delete(saved)
                } else {
                    check("MongoDB connects, with the query panel", false)
                }
            }

                        // Through SSH, when DBLENS_TEST_SSH is "host:port|keyfile" for a test sshd.
            if let spec = ProcessInfo.processInfo.environment["DBLENS_TEST_SSH"], !spec.isEmpty {
                let parts = spec.split(separator: "|").map(String.init)
                let target = parts[0].split(separator: ":").map(String.init)
                let ssh = SSHTunnel.Config(host: target[0], port: target.count > 1 ? Int(target[1]) : nil,
                                           keyPath: parts.count > 1 ? parts[1] : "")
                let viaSSH = try? await model.save(url: url, name: "via ssh", ssh: ssh)
                let first = model.session?.tunnel
                check("connecting through SSH opens a tunnel", first?.isRunning == true && viaSSH?.sshHost == target[0])
                model.sql = "select 1"
                await model.run()
                check("queries go through the tunnel", model.execution?.main?.rows == [["1"]])
                if let first {
                    kill(first.processIdentifier, SIGTERM)
                    for _ in 0..<40 where model.session?.tunnel === first || model.session?.tunnel == nil {
                        try? await Task.sleep(for: .milliseconds(250))
                    }
                    let second = model.session?.tunnel
                    check("a dropped tunnel reconnects by itself", second != nil && second !== first && second?.isRunning == true)
                    await model.run()
                    check("queries work after the reconnect", model.execution?.main?.rows == [["1"]])
                    model.disconnect()
                    try? await Task.sleep(for: .milliseconds(500))
                    check("disconnecting closes the tunnel", second?.isRunning == false)
                }
                if let viaSSH { model.delete(viaSSH) }
            }

            if let saved {
                model.delete(saved)
                check("deleting a connection closes it and removes the URL", model.session == nil && connections().isEmpty && Secrets.url(for: saved.uuid) == nil)
            }

            print(failures == 0 ? "SELFTEST PASS \(count) checks" : "SELFTEST FAIL \(failures) of \(count) checks")
            finish(code: failures == 0 ? 0 : 1)
        }
    }

    private static func check(_ name: String, _ ok: Bool, _ detail: String = "") {
        count += 1
        if !ok { failures += 1 }
        print(ok ? "PASS \(name)" : "FAIL \(name)\(detail.isEmpty ? "" : ": \(detail)")")
    }

    /// Quits through NSApp.terminate, never exit(), and closes sheets first:
    /// an open sheet refuses terminate, and the run would wait forever.
    static func finish(code: Int32) {
        exitCode = code
        for window in NSApp.windows { if let sheet = window.attachedSheet { window.endSheet(sheet) } }
        NSApp.terminate(nil)
    }
}
