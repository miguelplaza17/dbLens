import Testing
@testable import DBLensKit

@Suite struct RowReplaceTests {
    private let columns = [Column(name: "id", type: "text"), Column(name: "nome", type: "varchar(20)"),
                           Column(name: "n", type: "integer"), Column(name: "obs", type: "TEXT")]

    @Test func replacesInTextColumnsIgnoringCase() {
        let changes = RowReplace.changes(columns: columns, row: ["x", "Ana ANA", "2", nil], keyColumns: ["id"], find: "ana", with: "Bia")
        #expect(changes.map(\.0) == ["nome"])
        #expect(changes.first?.1 == "Bia Bia")
    }

    @Test func leavesKeysAndNumbersAlone() {
        #expect(RowReplace.changes(columns: columns, row: ["a2", "b", "2", "c"], keyColumns: ["id"], find: "2", with: "3").isEmpty)
    }

    @Test func emptySearchChangesNothing() {
        #expect(RowReplace.changes(columns: columns, row: ["a", "b", "1", "c"], keyColumns: [], find: "", with: "x").isEmpty)
    }

    @Test func treatsTheSearchAsPlainText() {
        let changes = RowReplace.changes(columns: columns, row: ["a", "1.5 e 1x5", "1", nil], keyColumns: ["id"], find: "1.5", with: "2")
        #expect(changes.first?.1 == "2 e 1x5")
    }
}
