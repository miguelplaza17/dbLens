import Testing
@testable import DBLensKit

@Test func exportsCSVAndJSON() {
    let result = QueryResult(columns: [Column(name: "id"), Column(name: "note")],
                             rows: [["1", "a, \"b\"\nc"], ["2", nil]], command: "SELECT 2", totalRows: 2)
    #expect(ResultExport.csv(result) == "id,note\r\n1,\"a, \"\"b\"\"\nc\"\r\n2,\r\n")
    #expect(ResultExport.json(result) == "[\n  {\"id\": \"1\", \"note\": \"a, \\\"b\\\"\\nc\"},\n  {\"id\": \"2\", \"note\": null}\n]\n")
}
