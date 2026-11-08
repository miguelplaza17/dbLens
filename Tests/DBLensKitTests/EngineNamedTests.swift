import Testing
@testable import DBLensKit

@Suite struct EngineNamedTests {
    @Test func readsTheNamesPeopleUse() {
        #expect(Engine.named("Redis") == .redis)
        #expect(Engine.named(" PostgreSQL ") == .postgres)
        #expect(Engine.named("postgres") == .postgres)
        #expect(Engine.named("MariaDB") == .mysql)
        #expect(Engine.named("Mongo") == .mongodb)
        #expect(Engine.named("oracle") == nil)
        for engine in Engine.allCases { #expect(Engine.named(engine.name) == engine) }
    }
}
