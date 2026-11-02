//! SQLite driver against temp files. Always runs (no env var needed).

use dblens_lib::drivers::sqlite;
use dblens_lib::session::{
    CellValue, ColumnSort, DatabaseSession, DbError, Relation, RelationKind, RowKey, RowKeyKind,
};
use tempfile::TempDir;

struct Db {
    _dir: TempDir,
    path: String,
    session: Box<dyn DatabaseSession>,
}

async fn open_with(setup: &str) -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.db").to_string_lossy().into_owned();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(setup).unwrap();
    }
    let session = sqlite::connect(&format!("sqlite://{path}"), None)
        .await
        .expect("connect");
    Db {
        _dir: dir,
        path,
        session,
    }
}

const PEOPLE: &str = "create table people (id integer primary key, name text, note text, score real);
    insert into people values (1,'Alpha','first',1.5),(2,'beta','100% sure',null),(3,'Gamma','it''s here',3),(10,'delta',null,4);";

fn table(name: &str) -> Relation {
    Relation::new("main", name, RelationKind::Table, -1.0)
}

fn cell(column: &str, value: Option<&str>) -> CellValue {
    CellValue {
        column: column.into(),
        value: value.map(String::from),
    }
}

fn key(id: &str) -> RowKey {
    RowKey {
        columns: vec!["id".into()],
        kind: RowKeyKind::PrimaryKey,
        values: vec![Some(id.into())],
    }
}

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[tokio::test]
async fn connects_and_reports_version() {
    let db = open_with(PEOPLE).await;
    assert!(db.session.server_version().starts_with('3'));
    assert_eq!(db.session.quote("a\"b"), "\"a\"\"b\"");
    assert_eq!(
        db.session.qualified(&table("people")),
        "\"main\".\"people\""
    );
    db.session.close().await;
    assert!(db.session.relations().await.is_err());
}

#[tokio::test]
async fn accepts_plain_and_file_forms() {
    let db = open_with(PEOPLE).await;
    for text in [
        db.path.clone(),
        format!("file:{}", db.path),
        format!("sqlite3://{}", db.path),
    ] {
        let session = sqlite::connect(&text, None).await.expect(&text);
        assert_eq!(session.relations().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn fails_for_missing_and_non_database_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.db").to_string_lossy().into_owned();
    let error = sqlite::connect(&missing, None).await.err().unwrap();
    assert!(error.message().starts_with("No SQLite file at"));
    let junk = dir.path().join("junk.db");
    std::fs::write(
        &junk,
        "this is not a database, just text that is long enough to be a header",
    )
    .unwrap();
    assert!(sqlite::connect(junk.to_str().unwrap(), None).await.is_err());
    assert!(sqlite::connect("not a path", None).await.is_err());
}

#[tokio::test]
async fn runs_several_statements() {
    let db = open_with(PEOPLE).await;
    let run = db
        .session
        .execute("select 1 as a, 'x' as b; update people set name = 'z' where id = 1; -- c\n create table t (x); select * from t")
        .await
        .unwrap();
    assert_eq!(run.results.len(), 4);
    assert_eq!(run.results[0].command, "SELECT 1");
    assert_eq!(run.results[0].rows, vec![vec![s("1"), s("x")]]);
    assert_eq!(run.results[1].command, "UPDATE 1");
    assert_eq!(run.results[2].command, "CREATE");
    assert_eq!(run.results[3].command, "SELECT 0");
    assert!(run.duration_ms >= 0.0);
    assert!(db.session.execute("select nope from people").await.is_err());
}

#[tokio::test]
async fn formats_values() {
    let db = open_with("create table v (a integer, b real, c blob, d text, e);").await;
    db.session
        .execute("insert into v values (7, 2.0, x'deadbeef', 'héllo', null)")
        .await
        .unwrap();
    let r = db
        .session
        .execute("select a, b, c, d, e, zeroblob(100) from v")
        .await
        .unwrap();
    let row = &r.results[0].rows[0];
    assert_eq!(row[0], s("7"));
    assert_eq!(row[1], s("2.0"));
    assert_eq!(row[2], s("0xdeadbeef"));
    assert_eq!(row[3], s("héllo"));
    assert_eq!(row[4], None);
    assert!(row[5].as_ref().unwrap().ends_with('…'));
    assert!(r.results[0].columns[1]
        .type_name
        .eq_ignore_ascii_case("real"));
}

#[tokio::test]
async fn cuts_big_results() {
    let db = open_with("create table n (i integer primary key);").await;
    db.session
        .execute("with recursive c(x) as (select 1 union all select x+1 from c where x < 6000) insert into n select x from c")
        .await
        .unwrap();
    let r = db.session.execute("select * from n").await.unwrap();
    let result = &r.results[0];
    assert_eq!(result.rows.len(), 5000);
    assert!(result.truncated);
    assert_eq!(result.total_rows, 6000);
}

#[tokio::test]
async fn lists_tables_and_views_only() {
    let db = open_with(&format!(
        "{PEOPLE} create view v as select * from people; create table other (x);"
    ))
    .await;
    let rels = db.session.relations().await.unwrap();
    let names: Vec<_> = rels.iter().map(|r| (r.name.as_str(), r.kind)).collect();
    assert_eq!(
        names,
        vec![
            ("other", RelationKind::Table),
            ("people", RelationKind::Table),
            ("v", RelationKind::View)
        ]
    );
    assert!(rels
        .iter()
        .all(|r| r.schema == "main" && r.estimate == -1.0));
}

#[tokio::test]
async fn pages_by_primary_key_with_keys_and_types() {
    let db = open_with(PEOPLE).await;
    let p = db
        .session
        .page(&table("people"), 3, 0, "", None)
        .await
        .unwrap();
    let ids: Vec<_> = p.rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(ids, vec!["1", "2", "3"]);
    let keys = p.keys.unwrap();
    assert_eq!(keys.kind, RowKeyKind::PrimaryKey);
    assert_eq!(keys.columns, vec!["id"]);
    // Keys carry their storage class, so they bind back exactly.
    assert_eq!(keys.values[2], vec![s("i:3")]);
    assert_eq!(p.columns[0].type_name, "INTEGER");
    assert!(p.columns[1].type_name.eq_ignore_ascii_case("text"));
    let next = db
        .session
        .page(&table("people"), 3, 3, "", None)
        .await
        .unwrap();
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows[0][0], s("10"));
}

#[tokio::test]
async fn sorts_then_by_key_with_nulls_last() {
    let db = open_with(PEOPLE).await;
    let up = ColumnSort {
        column: "score".into(),
        ascending: true,
    };
    let p = db
        .session
        .page(&table("people"), 10, 0, "", Some(&up))
        .await
        .unwrap();
    let ids: Vec<_> = p.rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(ids, vec!["1", "3", "10", "2"]);
    let down = ColumnSort {
        column: "score".into(),
        ascending: false,
    };
    let p = db
        .session
        .page(&table("people"), 10, 0, "", Some(&down))
        .await
        .unwrap();
    let ids: Vec<_> = p.rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(ids, vec!["10", "3", "1", "2"]);
}

#[tokio::test]
async fn searches_every_column_case_insensitively() {
    let db = open_with(PEOPLE).await;
    let t = table("people");
    let p = db.session.page(&t, 10, 0, "ALPHA", None).await.unwrap();
    assert_eq!(p.rows.len(), 1);
    // LIKE wildcards in the search are literal.
    assert_eq!(db.session.count(&t, "100%").await.unwrap(), Some(1));
    assert_eq!(db.session.count(&t, "%").await.unwrap(), Some(1));
    assert_eq!(db.session.count(&t, "_").await.unwrap(), Some(0));
    assert_eq!(db.session.count(&t, "it's").await.unwrap(), Some(1));
    assert_eq!(db.session.count(&t, "").await.unwrap(), Some(4));
    assert_eq!(db.session.count(&t, "nomatch").await.unwrap(), Some(0));
}

#[tokio::test]
async fn composite_keys_and_rowid_tables() {
    let db = open_with(
        "create table pair (b text, a text, v text, primary key (a, b));
         insert into pair values ('b1','a1','x'),('b2','a1','y');
         create table loose (name text); insert into loose values ('one'),('two'),('three');
         create table nul (k integer primary key, v);
         create view lv as select * from loose;",
    )
    .await;
    let p = db
        .session
        .page(&table("pair"), 10, 0, "", None)
        .await
        .unwrap();
    let keys = p.keys.unwrap();
    assert_eq!(keys.columns, vec!["a", "b"]);
    assert_eq!(keys.values[0], vec![s("t:a1"), s("t:b1")]);

    let p = db
        .session
        .page(&table("loose"), 10, 0, "", None)
        .await
        .unwrap();
    assert_eq!(p.columns.len(), 1);
    assert_eq!(p.rows[0].len(), 1);
    let keys = p.keys.unwrap();
    assert_eq!(keys.kind, RowKeyKind::Rowid);
    assert_eq!(keys.columns, vec!["rowid"]);
    assert_eq!(keys.values, vec![vec![s("1")], vec![s("2")], vec![s("3")]]);

    // Edit by rowid.
    let k = RowKey {
        columns: vec!["rowid".into()],
        kind: RowKeyKind::Rowid,
        values: vec![s("2")],
    };
    let out = db
        .session
        .update(&table("loose"), &k, &[cell("name", Some("TWO"))])
        .await
        .unwrap();
    assert_eq!(out.command, "UPDATE 1");
    assert_eq!(
        db.session.count(&table("loose"), "TWO").await.unwrap(),
        Some(1)
    );

    // A view has no keys.
    let view = Relation::new("main", "lv", RelationKind::View, -1.0);
    let p = db.session.page(&view, 10, 0, "", None).await.unwrap();
    assert!(p.keys.is_none());
    assert_eq!(p.rows.len(), 3);
}

#[tokio::test]
async fn edits_one_row() {
    let db = open_with(PEOPLE).await;
    let t = table("people");
    let out = db
        .session
        .update(
            &t,
            &key("2"),
            &[cell("name", Some("Beta")), cell("note", None)],
        )
        .await
        .unwrap();
    assert_eq!(out.command, "UPDATE 1");
    assert_eq!(out.affected, 1);
    assert!(out.sql.starts_with("update \"main\".\"people\" set"));
    let r = db
        .session
        .execute("select name, note from people where id = 2")
        .await
        .unwrap();
    assert_eq!(r.results[0].rows[0], vec![s("Beta"), None]);

    let out = db
        .session
        .insert(&t, &[cell("id", Some("20")), cell("name", Some("new"))])
        .await
        .unwrap();
    assert_eq!(out.command, "INSERT 1");
    db.session.insert(&table("people"), &[]).await.unwrap();
    assert_eq!(db.session.count(&t, "").await.unwrap(), Some(6));

    let out = db.session.delete(&t, &key("20")).await.unwrap();
    assert_eq!(out.command, "DELETE 1");
    // Gone already: refused, nothing changes.
    let error = db.session.delete(&t, &key("20")).await.unwrap_err();
    assert!(error.message().contains("changed or was deleted"));
    let error = db
        .session
        .update(&t, &key("999"), &[cell("name", Some("x"))])
        .await
        .unwrap_err();
    assert!(error.message().contains("Reload"));
    // No changes is a no-op.
    assert_eq!(
        db.session
            .update(&t, &key("1"), &[])
            .await
            .unwrap()
            .affected,
        0
    );
}

#[tokio::test]
async fn an_edit_that_hits_many_rows_is_rolled_back() {
    // No primary key and a key that matches several rows: a unique-looking key that is not.
    let db = open_with(
        "create table dup (name text, n integer); insert into dup values ('a',1),('a',2);",
    )
    .await;
    let k = RowKey {
        columns: vec!["name".into()],
        kind: RowKeyKind::PrimaryKey,
        values: vec![s("a")],
    };
    let error = db
        .session
        .update(&table("dup"), &k, &[cell("n", Some("9"))])
        .await
        .unwrap_err();
    assert!(error.message().contains("affected 2 rows"));
    let r = db.session.execute("select sum(n) from dup").await.unwrap();
    assert_eq!(r.results[0].rows[0][0], s("3"));
    assert!(db.session.delete(&table("dup"), &k).await.is_err());
    assert_eq!(db.session.count(&table("dup"), "").await.unwrap(), Some(2));
}

#[tokio::test]
async fn edit_errors_roll_back_and_keep_working() {
    let db = open_with("create table u (id integer primary key, e text unique not null); insert into u values (1,'a'),(2,'b');").await;
    let t = table("u");
    assert!(db
        .session
        .update(&t, &key("2"), &[cell("e", Some("a"))])
        .await
        .is_err());
    assert!(db
        .session
        .update(&t, &key("2"), &[cell("e", None)])
        .await
        .is_err());
    // The failed edits left no open transaction.
    db.session
        .update(&t, &key("2"), &[cell("e", Some("c"))])
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_files_refuse_edits() {
    use std::os::unix::fs::PermissionsExt;
    let db = open_with(PEOPLE).await;
    let mut perms = std::fs::metadata(&db.path).unwrap().permissions();
    perms.set_mode(0o444);
    std::fs::set_permissions(&db.path, perms).unwrap();
    // Root can still write; skip when the open did not turn read-only.
    let session = sqlite::connect(&db.path, None).await.unwrap();
    let probe = session
        .update(&table("people"), &key("1"), &[cell("name", Some("x"))])
        .await;
    if let Err(e) = probe {
        assert_eq!(e.message(), "This file is read-only.");
        assert!(session.insert(&table("people"), &[]).await.is_err());
        assert!(session.delete(&table("people"), &key("1")).await.is_err());
    }
}

#[tokio::test]
async fn read_only_is_guarded_by_sqlite() {
    let db = open_with(PEOPLE).await;
    let s = &db.session;
    let ok = s.read_only("select count(*) from people; ").await.unwrap();
    assert_eq!(ok.results[0].rows[0][0], Some("4".into()));
    s.read_only("with x as (select 1) select * from x -- done")
        .await
        .unwrap();
    for sql in [
        "delete from people",
        "update people set name = 'x'",
        "insert into people (id) values (99)",
        "drop table people",
        "pragma journal_mode = wal",
        "attach database ':memory:' as m",
        "begin",
        "vacuum",
    ] {
        let e = s.read_only(sql).await.unwrap_err();
        assert!(
            e.message().contains("would change the database"),
            "{sql}: {e}"
        );
    }
    assert!(s
        .read_only("select 1; select 2")
        .await
        .unwrap_err()
        .message()
        .contains("one statement"));
    assert!(s
        .read_only("  -- nothing")
        .await
        .unwrap_err()
        .message()
        .contains("no statement"));
    // The lock is released after a read.
    s.execute("update people set name = 'ok' where id = 1")
        .await
        .unwrap();
    assert_eq!(s.count(&table("people"), "ok").await.unwrap(), Some(1));
}

#[tokio::test]
async fn rehearse_always_rolls_back() {
    let db = open_with(PEOPLE).await;
    let s = &db.session;
    let run = s.rehearse("update people set name = 'r'; create table made (x); insert into made values (1); select * from made").await.unwrap();
    assert_eq!(run.results.len(), 4);
    assert_eq!(run.results[0].command, "UPDATE 4");
    assert_eq!(run.results[3].rows.len(), 1);
    assert_eq!(s.relations().await.unwrap().len(), 1);
    let r = s
        .execute("select count(*) from people where name = 'r'")
        .await
        .unwrap();
    assert_eq!(r.results[0].rows[0][0], s_("0"));
    // Transaction control and in-transaction-impossible statements are refused.
    assert!(s
        .rehearse("insert into people (id) values (50); commit")
        .await
        .is_err());
    assert!(s
        .rehearse("vacuum")
        .await
        .unwrap_err()
        .message()
        .contains("can't run inside a transaction"));
    assert!(s.rehearse("attach ':memory:' as m").await.is_err());
    assert!(s.rehearse("  ").await.is_err());
    // A failure names the statement and still rolls back.
    let e = s
        .rehearse("update people set name = 'q'; select * from nope")
        .await
        .unwrap_err();
    assert!(e.message().starts_with("Statement 2 of 2 failed"), "{e}");
    let r = s
        .execute("select count(*) from people where name = 'q'")
        .await
        .unwrap();
    assert_eq!(r.results[0].rows[0][0], s_("0"));
}

fn s_(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[tokio::test]
async fn atomic_runs_commit_or_roll_back() {
    let db = open_with(PEOPLE).await;
    let s = &db.session;
    let run = s.execute_atomically("create table a (x); insert into a values (1),(2); update people set note = 'z' where id = 1").await.unwrap();
    assert_eq!(run.results.len(), 3);
    assert_eq!(s.count(&table("a"), "").await.unwrap(), Some(2));

    let e = s
        .execute_atomically("insert into a values (3); create table a (x)")
        .await
        .unwrap_err();
    assert!(!matches!(e, DbError::NotAtomic(_)));
    assert!(e.message().starts_with("Statement 2 of 2 failed"));
    assert_eq!(s.count(&table("a"), "").await.unwrap(), Some(2));
    assert!(s.execute_atomically("begin; select 1").await.is_err());
    // Still usable afterwards.
    s.execute("insert into a values (9)").await.unwrap();
    assert_eq!(s.count(&table("a"), "").await.unwrap(), Some(3));
}

#[tokio::test]
async fn schema_graph_has_columns_and_foreign_keys() {
    let db = open_with(
        "create table customers (id integer primary key, name text not null);
         create table orders (id integer primary key, customer_id integer references customers(id), note text);
         create table lines (order_id integer, n integer, ref_id integer references customers, primary key (order_id, n),
            foreign key (order_id) references orders (id));
         create view v as select * from orders;",
    )
    .await;
    let g = db.session.schema_graph().await.unwrap();
    let names: Vec<_> = g.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["customers", "lines", "orders", "v"]);
    assert!(g.tables[3].is_view);
    let customers = &g.tables[0];
    assert!(customers.columns[0].is_primary_key);
    assert!(customers.columns[0].is_nullable); // integer primary key without NOT NULL
    assert!(!customers.columns[1].is_nullable);
    assert_eq!(customers.schema, "main");
    assert_eq!(g.links.len(), 3);
    let orders_link = g.links.iter().find(|l| l.from == "main.orders").unwrap();
    assert_eq!(orders_link.to, "main.customers");
    assert_eq!(orders_link.from_columns, vec!["customer_id"]);
    assert_eq!(orders_link.to_columns, vec!["id"]);
    // `references customers` with no column list points at the target's primary key.
    let implicit = g
        .links
        .iter()
        .find(|l| l.from_columns == vec!["ref_id"])
        .unwrap();
    assert_eq!(implicit.to_columns, vec!["id"]);
}

#[tokio::test]
async fn cancel_stops_a_long_query() {
    let db = open_with("create table n (i integer primary key);").await;
    let session: std::sync::Arc<dyn DatabaseSession> = std::sync::Arc::from(db.session);
    let runner = std::sync::Arc::clone(&session);
    let task = tokio::spawn(async move {
        runner
            .execute("with recursive c(x) as (select 1 union all select x+1 from c where x < 2000000000) select count(*) from c")
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    session.cancel();
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .expect("cancel took too long")
        .unwrap();
    assert_eq!(outcome.unwrap_err(), DbError::Cancelled);
    // The session works again.
    session.execute("insert into n values (1)").await.unwrap();
    // Cancelling while idle does nothing harmful.
    session.cancel();
    assert_eq!(session.count(&table("n"), "").await.unwrap(), Some(1));
}

#[tokio::test]
async fn names_with_quotes_work() {
    let db = open_with("create table \"we\"\"ird\" (\"c\"\"ol\" integer primary key, v text); insert into \"we\"\"ird\" values (1,'a');").await;
    let t = table("we\"ird");
    let p = db.session.page(&t, 10, 0, "a", None).await.unwrap();
    assert_eq!(p.columns[0].name, "c\"ol");
    let k = RowKey {
        columns: vec!["c\"ol".into()],
        kind: RowKeyKind::PrimaryKey,
        values: vec![s("1")],
    };
    db.session
        .update(&t, &k, &[cell("v", Some("b"))])
        .await
        .unwrap();
    assert_eq!(db.session.count(&t, "b").await.unwrap(), Some(1));
}

#[cfg(windows)]
#[tokio::test]
async fn opens_windows_paths() {
    let db = open_with(PEOPLE).await;
    assert!(
        sqlite::connect(&format!("sqlite:///{}", db.path.replace('\\', "/")), None)
            .await
            .is_ok()
    );
    assert!(sqlite::connect(&db.path, None).await.is_ok());
}

fn row_key(page: &dblens_lib::session::QueryResult, row: usize) -> RowKey {
    let keys = page.keys.clone().expect("keys");
    RowKey {
        columns: keys.columns,
        kind: keys.kind,
        values: keys.values[row].clone(),
    }
}

#[tokio::test]
async fn edits_rows_keyed_by_untyped_and_blob_columns() {
    let db = open_with(
        "create table loose (id primary key, v);
         insert into loose values (1, 'a'), ('1', 'text one'), (2.5, 'real');
         create table blobs (id blob primary key, v);
         insert into blobs values (x'0102', 'a');",
    )
    .await;
    let s = &db.session;
    let page = s.page(&table("loose"), 50, 0, "", None).await.unwrap();
    assert_eq!(page.columns.len(), 2);
    for row in 0..page.rows.len() {
        let label = page.rows[row][1].clone().unwrap();
        s.update(
            &table("loose"),
            &row_key(&page, row),
            &[cell("v", Some(&format!("{label}!")))],
        )
        .await
        .unwrap_or_else(|e| panic!("row {row}: {e}"));
    }
    let after = s.page(&table("loose"), 50, 0, "", None).await.unwrap();
    assert!(after
        .rows
        .iter()
        .all(|r| r[1].as_deref().unwrap().ends_with('!')));

    let page = s.page(&table("blobs"), 50, 0, "", None).await.unwrap();
    s.delete(&table("blobs"), &row_key(&page, 0)).await.unwrap();
    assert_eq!(s.count(&table("blobs"), "").await.unwrap(), Some(0));
}

#[tokio::test]
async fn edits_inside_a_user_transaction_leave_the_commit_to_the_user() {
    let db = open_with(PEOPLE).await;
    let s = &db.session;
    s.execute("begin; insert into people values (20, 'user-pending', null, null)")
        .await
        .unwrap();
    let page = s.page(&table("people"), 50, 0, "", None).await.unwrap();
    s.update(
        &table("people"),
        &row_key(&page, 0),
        &[cell("name", Some("edited"))],
    )
    .await
    .unwrap();
    let gone = RowKey {
        values: vec![Some("i:999".into())],
        ..row_key(&page, 0)
    };
    assert!(s.delete(&table("people"), &gone).await.is_err());
    assert!(s.rehearse("select 1").await.is_err());
    s.execute("rollback").await.unwrap();
    assert_eq!(s.count(&table("people"), "").await.unwrap(), Some(4));
    let page = s.page(&table("people"), 50, 0, "", None).await.unwrap();
    assert_eq!(page.rows[0][1].as_deref(), Some("Alpha"));
}
