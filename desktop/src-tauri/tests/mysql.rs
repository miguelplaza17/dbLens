//! MySQL/MariaDB driver against a throwaway server. Skips when
//! DBLENS_TEST_MYSQL_URL is unset. Each test makes and drops its own tables.

use dblens_lib::drivers::mysql;
use dblens_lib::session::{
    CellValue, ColumnSort, DatabaseSession, DbError, Relation, RelationKind, RowKey, RowKeyKind,
};

async fn open() -> Option<Box<dyn DatabaseSession>> {
    let url = std::env::var("DBLENS_TEST_MYSQL_URL").ok()?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    Some(mysql::connect(&url, None).await.expect("connect"))
}

fn cell(column: &str, value: Option<&str>) -> CellValue {
    CellValue { column: column.into(), value: value.map(String::from) }
}

fn key(id: &str) -> RowKey {
    RowKey { columns: vec!["id".into()], kind: RowKeyKind::PrimaryKey, values: vec![Some(id.into())] }
}

/// Makes `name` fresh with a few rows; the database name comes from the server.
async fn fixture(s: &dyn DatabaseSession, name: &str) -> Relation {
    s.execute(&format!("drop table if exists `{name}`")).await.unwrap();
    s.execute(&format!(
        "create table `{name}` (id int primary key, title varchar(40), note text, price decimal(8,2) null)"
    ))
    .await
    .unwrap();
    s.execute(&format!(
        "insert into `{name}` values (1,'Alpha','first',1.50),(2,'beta','100% sure',null),(3,'Gamma','it''s here',3),(10,'delta',null,4)"
    ))
    .await
    .unwrap();
    let db = s.execute("select database()").await.unwrap().results[0].rows[0][0].clone().unwrap();
    Relation::new(db, name, RelationKind::Table, -1.0)
}

async fn drop_table(s: &dyn DatabaseSession, name: &str) {
    let _ = s.execute(&format!("drop table if exists `{name}`")).await;
}

#[tokio::test]
async fn connects_and_runs_multiple_statements() {
    let Some(s) = open().await else { return };
    assert!(!s.server_version().is_empty());
    let run = s.execute("select 1 as a, 'x' as b; update_nothing_here_is_skipped").await;
    assert!(run.is_err());
    let run = s.execute("select 1 as a, 'x' as b; select 2; set @x = 1").await.unwrap();
    assert_eq!(run.results.len(), 3);
    assert_eq!(run.results[0].columns[0].name, "a");
    assert_eq!(run.results[0].columns[0].type_name, "int");
    assert_eq!(run.results[0].rows, vec![vec![Some("1".to_string()), Some("x".to_string())]]);
    assert_eq!(run.results[0].command, "SELECT 1");
    assert_eq!(run.results[2].command, "SET");
    let err = s.execute("selec 1").await.unwrap_err();
    assert!(matches!(err, DbError::Message(_)));
    s.close().await;
}

#[tokio::test]
async fn shows_binary_and_null_values() {
    let Some(s) = open().await else { return };
    let run = s.execute("select x'DEAD01' as b, null as n, 'é' as t").await.unwrap();
    let row = &run.results[0].rows[0];
    assert_eq!(row[0].as_deref(), Some("0xDEAD01"));
    assert_eq!(row[1], None);
    assert_eq!(row[2].as_deref(), Some("é"));
}

#[tokio::test]
async fn lists_relations_and_pages_by_primary_key() {
    let Some(s) = open().await else { return };
    let rel = fixture(&*s, "dbl_page").await;
    let all = s.relations().await.unwrap();
    let found = all.iter().find(|r| r.name == "dbl_page").expect("listed");
    assert_eq!(found.kind, RelationKind::Table);
    assert_eq!(found.schema, rel.schema);
    // The URL's database sorts first.
    assert_eq!(all[0].schema, rel.schema);

    let page = s.page(&rel, 3, 0, "", None).await.unwrap();
    let ids: Vec<_> = page.rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(ids, ["1", "2", "3"]);
    let keys = page.keys.as_ref().expect("keys");
    assert_eq!(keys.columns, ["id"]);
    assert_eq!(keys.row(1).unwrap().values, vec![Some("2".to_string())]);
    assert_eq!(page.columns[1].type_name, "varchar(40)");
    assert_eq!(page.columns[3].type_name, "decimal(8,2)");

    let next = s.page(&rel, 3, 3, "", None).await.unwrap();
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows[0][0].as_deref(), Some("10"));
    assert_eq!(s.count(&rel, "").await.unwrap(), Some(4));
    drop_table(&*s, "dbl_page").await;
}

#[tokio::test]
async fn searches_and_sorts() {
    let Some(s) = open().await else { return };
    let rel = fixture(&*s, "dbl_search").await;
    // Case-insensitive, across columns.
    let hit = s.page(&rel, 10, 0, "ALPHA", None).await.unwrap();
    assert_eq!(hit.rows.len(), 1);
    assert_eq!(s.count(&rel, "alpha").await.unwrap(), Some(1));
    // LIKE wildcards and quotes in the search text are literal.
    assert_eq!(s.count(&rel, "100%").await.unwrap(), Some(1));
    assert_eq!(s.count(&rel, "%").await.unwrap(), Some(1));
    assert_eq!(s.count(&rel, "_").await.unwrap(), Some(0));
    assert_eq!(s.count(&rel, "it's").await.unwrap(), Some(1));
    assert_eq!(s.count(&rel, "nothing like this").await.unwrap(), Some(0));

    // Sort by a column, NULL last in both directions, then by key.
    let asc = ColumnSort { column: "price".into(), ascending: true };
    let rows = s.page(&rel, 10, 0, "", Some(&asc)).await.unwrap().rows;
    let ids: Vec<_> = rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(ids, ["1", "3", "10", "2"]);
    let desc = ColumnSort { column: "price".into(), ascending: false };
    let rows = s.page(&rel, 10, 0, "", Some(&desc)).await.unwrap().rows;
    let ids: Vec<_> = rows.iter().map(|r| r[0].clone().unwrap()).collect();
    assert_eq!(ids, ["10", "3", "1", "2"]);
    drop_table(&*s, "dbl_search").await;
}

#[tokio::test]
async fn edits_exactly_one_row() {
    let Some(s) = open().await else { return };
    let rel = fixture(&*s, "dbl_edit").await;

    let out = s.update(&rel, &key("1"), &[cell("title", Some("A'\\ \"x\"")), cell("note", None)]).await.unwrap();
    assert_eq!(out.affected, 1);
    assert!(out.sql.contains("`title` = ?"));
    assert!(!out.sql.contains("A'"));
    let page = s.page(&rel, 10, 0, "", None).await.unwrap();
    assert_eq!(page.rows[0][1].as_deref(), Some("A'\\ \"x\""));
    assert_eq!(page.rows[0][2], None);

    // Setting the same value still counts as the one matched row.
    assert_eq!(s.update(&rel, &key("1"), &[cell("title", Some("A'\\ \"x\""))]).await.unwrap().affected, 1);

    // A row that is gone is refused.
    let err = s.update(&rel, &key("99"), &[cell("title", Some("z"))]).await.unwrap_err();
    assert!(err.message().contains("changed or was deleted"));

    // A key that matches several rows is rolled back.
    let wide = RowKey { columns: vec!["note".into()], kind: RowKeyKind::PrimaryKey, values: vec![None] };
    s.execute("insert into dbl_edit (id, title) values (20, 'x'), (21, 'y')").await.unwrap();
    let err = s.update(&rel, &wide, &[cell("title", Some("all"))]).await.unwrap_err();
    assert!(err.message().contains("rolled back"), "{}", err.message());
    let titles = s.execute("select count(*) from dbl_edit where title = 'all'").await.unwrap();
    assert_eq!(titles.results[0].rows[0][0].as_deref(), Some("0"));
    let err = s.delete(&rel, &wide).await.unwrap_err();
    assert!(err.message().contains("rolled back"));

    // Insert (omitted columns take their default) and delete.
    let out = s.insert(&rel, &[cell("id", Some("30")), cell("title", Some("new"))]).await.unwrap();
    assert_eq!(out.affected, 1);
    assert!(out.command.starts_with("INSERT"));
    assert_eq!(s.delete(&rel, &key("30")).await.unwrap().affected, 1);
    assert_eq!(s.update(&rel, &key("1"), &[]).await.unwrap().affected, 0);
    drop_table(&*s, "dbl_edit").await;
}

#[tokio::test]
async fn read_only_is_refused_by_the_database() {
    let Some(s) = open().await else { return };
    let rel = fixture(&*s, "dbl_ro").await;
    let ok = s.read_only("select count(*) from dbl_ro").await.unwrap();
    assert_eq!(ok.results[0].rows[0][0].as_deref(), Some("4"));
    assert!(s.read_only("delete from dbl_ro").await.is_err());
    assert!(s.read_only("drop table dbl_ro").await.is_err());
    assert!(s.read_only("select 1; select 2").await.is_err());
    // The session is writable again afterwards, and nothing changed.
    assert_eq!(s.count(&rel, "").await.unwrap(), Some(4));
    s.execute("update dbl_ro set title = 'ok' where id = 1").await.unwrap();
    drop_table(&*s, "dbl_ro").await;
}

#[tokio::test]
async fn rehearse_rolls_back_and_atomic_commits_or_not_at_all() {
    let Some(s) = open().await else { return };
    let rel = fixture(&*s, "dbl_tx").await;
    let sizes = || async { s.count(&rel, "").await.unwrap().unwrap() };

    let rehearsal = s.rehearse("delete from dbl_tx where id = 1; delete from dbl_tx where id = 2").await.unwrap();
    assert_eq!(rehearsal.results.len(), 2);
    assert_eq!(rehearsal.results[0].command, "DELETE 1");
    assert_eq!(sizes().await, 4);

    // DDL commits by itself: not rehearsable, not atomic.
    let err = s.rehearse("alter table dbl_tx add column z int").await.unwrap_err();
    assert!(err.message().contains("commits by itself"));
    let err = s.execute_atomically("delete from dbl_tx; drop table dbl_tx").await.unwrap_err();
    assert!(matches!(err, DbError::NotAtomic(_)), "{err:?}");
    assert_eq!(sizes().await, 4);

    // A failing statement undoes the ones before it and is named.
    let err = s.execute_atomically("delete from dbl_tx where id = 1; insert into dbl_tx (id) values (2)").await.unwrap_err();
    assert!(err.message().contains("Statement 2 of 2 failed"), "{}", err.message());
    assert_eq!(sizes().await, 4);

    // Transaction control is the app's.
    assert!(s.execute_atomically("delete from dbl_tx; commit").await.is_err());

    let done = s.execute_atomically("delete from dbl_tx where id = 1; delete from dbl_tx where id = 2").await.unwrap();
    assert_eq!(done.results.len(), 2);
    assert_eq!(sizes().await, 2);
    drop_table(&*s, "dbl_tx").await;
}

#[tokio::test]
async fn cancel_stops_a_running_query() {
    let Some(s) = open().await else { return };
    let s: std::sync::Arc<dyn DatabaseSession> = s.into();
    let runner = s.clone();
    let task = tokio::spawn(async move { runner.execute("select sleep(30)").await });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let started = std::time::Instant::now();
    s.cancel();
    let outcome = task.await.unwrap();
    assert!(matches!(outcome, Err(DbError::Cancelled)), "{outcome:?}");
    assert!(started.elapsed().as_secs() < 10);
    // The connection still works.
    assert_eq!(s.execute("select 1").await.unwrap().results.len(), 1);
    // Cancel with nothing running does nothing.
    s.cancel();
    assert_eq!(s.execute("select 2").await.unwrap().results.len(), 1);
}

#[tokio::test]
async fn schema_graph_has_foreign_keys() {
    let Some(s) = open().await else { return };
    s.execute("drop table if exists dbl_child").await.unwrap();
    s.execute("drop table if exists dbl_parent").await.unwrap();
    s.execute("create table dbl_parent (a int, b int, name varchar(10), primary key (a, b))").await.unwrap();
    s.execute(
        "create table dbl_child (id int primary key, pa int not null, pb int null, \
         constraint fk_child_parent foreign key (pa, pb) references dbl_parent (a, b))",
    )
    .await
    .unwrap();
    let graph = s.schema_graph().await.unwrap();
    let child = graph.tables.iter().find(|t| t.name == "dbl_child").expect("child");
    assert!(child.columns.iter().any(|c| c.name == "id" && c.is_primary_key && !c.is_nullable));
    assert!(child.columns.iter().any(|c| c.name == "pb" && c.is_nullable));
    assert!(!child.is_view);
    let link = graph.links.iter().find(|l| l.name == "fk_child_parent").expect("link");
    assert!(link.from.ends_with(".dbl_child") && link.to.ends_with(".dbl_parent"));
    assert_eq!(link.from_columns, ["pa", "pb"]);
    assert_eq!(link.to_columns, ["a", "b"]);
    s.execute("drop table dbl_child").await.unwrap();
    s.execute("drop table dbl_parent").await.unwrap();
}

#[tokio::test]
async fn views_are_read_only_and_calls_return_their_sets() {
    let Some(s) = open().await else { return };
    let rel = fixture(&*s, "dbl_view_src").await;
    s.execute("drop view if exists dbl_view").await.unwrap();
    s.execute("create view dbl_view as select id, title from dbl_view_src").await.unwrap();
    let all = s.relations().await.unwrap();
    let view = all.iter().find(|r| r.name == "dbl_view").expect("view listed").clone();
    assert_eq!(view.kind, RelationKind::View);
    let page = s.page(&view, 10, 0, "", None).await.unwrap();
    assert_eq!(page.rows.len(), 4);
    assert!(page.keys.is_none());
    // A procedure's result sets come before its own status.
    s.execute("drop procedure if exists dbl_proc").await.unwrap();
    s.execute("create procedure dbl_proc() begin select 1 as one; select 2 as two; end").await.unwrap();
    let run = s.execute("call dbl_proc()").await.unwrap();
    assert!(run.results.len() >= 2);
    assert_eq!(run.results[0].columns[0].name, "one");
    assert_eq!(run.results[1].columns[0].name, "two");
    assert_eq!(run.main().unwrap().columns[0].name, "two");
    s.execute("drop procedure dbl_proc").await.unwrap();
    s.execute("drop view dbl_view").await.unwrap();
    let _ = rel;
    drop_table(&*s, "dbl_view_src").await;
}

#[tokio::test]
async fn bad_logins_carry_the_servers_message() {
    let Ok(url) = std::env::var("DBLENS_TEST_MYSQL_URL") else { return };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bad = url.replacen("dblens:dblens@", "dblens:wrong-password@", 1);
    let Err(error) = mysql::connect(&bad, None).await else { panic!("login should fail") };
    assert!(error.message().to_lowercase().contains("access denied"), "{}", error.message());
}
