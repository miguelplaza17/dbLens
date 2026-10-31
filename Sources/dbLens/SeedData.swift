import Foundation
import SwiftData

/// Sample state for snapshots: APP_SEED_URL=<url> with APP_STORE in /tmp saves
/// that database plus two more names, opens it, picks a table, and runs a query.
/// Use a throwaway database; the seed only reads.
@MainActor
enum SeedData {
    static var url: String? {
        let value = ProcessInfo.processInfo.environment["APP_SEED_URL"] ?? ""
        return value.isEmpty ? nil : value
    }

    static func insertIfRequested(into model: AppModel) async {
        guard let url, !SelfTest.requested else { return }
        guard Store.isOverridden else {
            print("APP_SEED_URL ignored: APP_STORE must be a file in /tmp")
            return
        }
        guard ((try? model.context.fetchCount(FetchDescriptor<SavedConnection>())) ?? 0) == 0 else { return }
        for name in ["production @ db.example.com", "analytics @ server"] {
            model.context.insert(SavedConnection(name: name))
        }
        do {
            try await model.save(url: url, name: "demo @ localhost")
        } catch {
            print("APP_SEED_URL could not connect: \(error.localizedDescription)")
            return
        }
        // The demo database's customers make a fuller first screen than whatever sorts first.
        await model.showTable((model.relations.first { $0.name == "customers" } ?? model.relations.first)?.id)
        if ProcessInfo.processInfo.environment["APP_OPEN_TERMINAL"] == "1" { model.openInTerminal() }
        model.sql = """
            select o.status, count(*) as orders, sum(o.total) as revenue
            from sales.orders o
            group by o.status
            order by revenue desc;
            """
        await model.run()
    }
}
