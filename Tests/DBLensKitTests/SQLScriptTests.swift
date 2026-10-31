import Testing
@testable import DBLensKit

@Test func splitsScriptsAtTheRightSemicolons() {
    let postgres = """
        create function f() returns int as $$ begin return 1; end $$ language plpgsql;
        insert into t values ('a;b', "c;d"); -- a comment; with a semicolon
        /* block; comment */ update t set x = $tag$;$tag$;
        ;
        """
    #expect(SQLScript.statements(postgres, dialect: .postgres).count == 3)
    #expect(SQLScript.statements("insert into t values ('it''s; fine'); select 1", dialect: .sqlite).count == 2)
    #expect(SQLScript.statements("insert into t values ('a\\';b'); # note; here\nselect `x;y` from t", dialect: .mysql).count == 2)
    #expect(SQLScript.verb("  -- hi\n /* x */ CREATE temporary table t(a int)", words: 2) == "CREATE TEMPORARY")
}

@Test func refusesTransactionControlInScripts() {
    for script in ["delete from t; commit", "begin; delete from t", "START TRANSACTION; select 1", "savepoint a", "set autocommit = 1"] {
        #expect(throws: DatabaseError.self, "\(script)") { try SQLScript.checked(script, dialect: .mysql) }
    }
    #expect((try? SQLScript.checked("alter table t add c int; update t set c = 1", dialect: .postgres))?.count == 2)
}
