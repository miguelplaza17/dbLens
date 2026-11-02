//! The pool (pool.rs), block pages with keyset pagination, and streamed SQL
//! runs, on Postgres, MySQL/MariaDB (both skip without their DBLENS_TEST_*
//! URL) and SQLite (a temp file, always).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dblens_lib::connector;
use dblens_lib::drivers::{mysql, postgres, sqlite};
use dblens_lib::pool::PooledSession;
use dblens_lib::session::{
    CellValue, ColumnSort, DatabaseSession, QueryResult, Relation, RelationKind,
};
use dblens_lib::wire::StreamSink;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Postgres,
    Mysql,
    Sqlite,
}

struct Db {
    kind: Kind,
    url: String,
    pool: PooledSession,
    _dir: Option<tempfile::TempDir>,
}

async fn open(kind: Kind) -> Option<Db> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (url, dir) = match kind {
        Kind::Postgres => (std::env::var("DBLENS_TEST_URL").ok()?, None),
        Kind::Mysql => (std::env::var("DBLENS_TEST_MYSQL_URL").ok()?, None),
        Kind::Sqlite => {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("pool.db");
            rusqlite_create(&path);
            (format!("sqlite://{}", path.display()), Some(dir))
        }
    };
    if url.is_empty() {
        return None;
    }
    let url = connector::clean(&url);
    let pinned: Box<dyn DatabaseSession> = match kind {
        Kind::Postgres => postgres::connect(&url, None).await.expect("connect"),
        Kind::Mysql => mysql::connect(&url, None).await.expect("connect"),
        Kind::Sqlite => sqlite::connect(&url, None).await.expect("connect"),
    };
    let pool = connector::pooled(Arc::from(pinned), url.clone(), None);
    Some(Db {
        kind,
        url,
        pool,
        _dir: dir,
    })
}

/// SQLite opens existing files only: make an empty database.
fn rusqlite_create(path: &std::path::Path) {
    std::fs::write(path, b"").unwrap();
}

impl Db {
    fn relation(&self, name: &str) -> Relation {
        let schema = match self.kind {
            Kind::Postgres => "public".to_string(),
            Kind::Sqlite => "main".to_string(),
            Kind::Mysql => connector::summary(&self.url).unwrap().database,
        };
        Relation::new(schema, name, RelationKind::Table, -1.0)
    }

    async fn run(&self, sql: &str) {
        self.pool
            .execute(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    /// A table `name` (a int, b int, v text, primary key per `key`) with ids 1…n.
    async fn numbers(&self, name: &str, n: usize, key: &str) {
        self.run(&format!("drop table if exists {name}")).await;
        self.run(&format!(
            "create table {name} (a int not null, b int not null, v text, primary key ({key}))"
        ))
        .await;
        for start in (1..=n).step_by(1000) {
            let values: Vec<String> = (start..(start + 1000).min(n + 1))
                .map(|i| format!("({i}, {}, 'v{i}')", i % 7))
                .collect();
            self.run(&format!(
                "insert into {name} (a, b, v) values {}",
                values.join(", ")
            ))
            .await;
        }
    }
}

fn first(result: &QueryResult) -> i64 {
    result.rows[0][0].as_deref().unwrap().parse().unwrap()
}

fn column(result: &QueryResult, c: usize) -> Vec<Option<String>> {
    result.rows.iter().map(|r| r[c].clone()).collect()
}

async fn keyset_blocks(kind: Kind) {
    let Some(db) = open(kind).await else { return };
    let name = format!("dblens_pool_blocks_{}", format!("{kind:?}").to_lowercase());
    let n = 25_000;
    db.numbers(&name, n, "a").await;
    let t = db.relation(&name);

    // Blocks one after another: each starts after the last key of the one before.
    for offset in [0u64, 500, 1000, 1500] {
        let block = db
            .pool
            .page_block(&t, offset, 500, "", None, None)
            .await
            .unwrap();
        assert_eq!(block.rows.len(), 500);
        assert_eq!(first(&block), offset as i64 + 1, "block at {offset}");
        assert!(block.keys.is_some());
    }
    // A deep jump: a key-only seek from the last block's key.
    let deep = db
        .pool
        .page_block(&t, 20_000, 300, "", None, None)
        .await
        .unwrap();
    assert_eq!(first(&deep), 20_001);
    // The block right after it (pure keyset), and the end of the table.
    let next = db
        .pool
        .page_block(&t, 20_300, 300, "", None, None)
        .await
        .unwrap();
    assert_eq!(first(&next), 20_301);
    let last = db
        .pool
        .page_block(&t, (n - 10) as u64, 300, "", None, None)
        .await
        .unwrap();
    assert_eq!(last.rows.len(), 10);
    assert_eq!(first(&last), (n - 9) as i64);
    // Past the end: nothing.
    let none = db
        .pool
        .page_block(&t, n as u64 + 50_000, 300, "", None, None)
        .await
        .unwrap();
    assert!(none.rows.is_empty());

    // Sorted by the key, descending; a cold jump first (no hint), then on.
    let down = ColumnSort {
        column: "a".into(),
        ascending: false,
    };
    for offset in [15_000u64, 15_200, 0, 200] {
        let block = db
            .pool
            .page_block(&t, offset, 200, "", Some(&down), None)
            .await
            .unwrap();
        assert_eq!(
            first(&block),
            (n as u64 - offset) as i64,
            "desc at {offset}"
        );
    }

    // Sorted by another column: OFFSET as before, same rows as `page`.
    let by_b = ColumnSort {
        column: "b".into(),
        ascending: true,
    };
    let block = db
        .pool
        .page_block(&t, 12_000, 100, "", Some(&by_b), None)
        .await
        .unwrap();
    let page = db
        .pool
        .pinned()
        .page(&t, 100, 12_000, "", Some(&by_b))
        .await
        .unwrap();
    assert_eq!(block.rows, page.rows);

    // A search keeps keyset blocks consistent with plain OFFSET pages.
    for offset in [0u64, 100, 200] {
        let block = db
            .pool
            .page_block(&t, offset, 100, "v2", None, None)
            .await
            .unwrap();
        let page = db
            .pool
            .pinned()
            .page(&t, 100, offset, "v2", None)
            .await
            .unwrap();
        assert_eq!(column(&block, 0), column(&page, 0), "search at {offset}");
    }

    // A composite key compares as a row value.
    let pair = format!("{name}_pair");
    db.numbers(&pair, 3_000, "b, a").await;
    let p = db.relation(&pair);
    for offset in [0u64, 1000, 2000, 2500] {
        let block = db
            .pool
            .page_block(&p, offset, 500, "", None, None)
            .await
            .unwrap();
        let page = db
            .pool
            .pinned()
            .page(&p, 500, offset, "", None)
            .await
            .unwrap();
        assert_eq!(block.rows, page.rows, "pair at {offset}");
    }
    db.run(&format!("drop table {name}")).await;
    db.run(&format!("drop table {pair}")).await;
    db.pool.close().await;
}

#[tokio::test]
async fn postgres_blocks_use_keyset_pagination() {
    keyset_blocks(Kind::Postgres).await;
}

#[tokio::test]
async fn mysql_blocks_use_keyset_pagination() {
    keyset_blocks(Kind::Mysql).await;
}

#[tokio::test]
async fn sqlite_blocks_use_keyset_pagination() {
    keyset_blocks(Kind::Sqlite).await;
}

async fn transactions_stay_pinned(kind: Kind) {
    let Some(db) = open(kind).await else { return };
    let name = format!("dblens_pool_tx_{}", format!("{kind:?}").to_lowercase());
    db.numbers(&name, 3, "a").await;
    let t = db.relation(&name);
    let begin = if kind == Kind::Mysql {
        "start transaction"
    } else {
        "begin"
    };
    assert!(!db.pool.transaction_open());
    db.run(begin).await;
    assert!(db.pool.transaction_open(), "{kind:?}: BEGIN opens it");
    db.run(&format!("insert into {name} (a, b, v) values (4, 0, 'v4')"))
        .await;
    // Browsing while it is open sees the transaction's own row.
    let page = db.pool.page_block(&t, 0, 50, "", None, None).await.unwrap();
    assert_eq!(page.rows.len(), 4);
    // An edit joins the transaction (behind a savepoint).
    let key = page.keys.as_ref().unwrap().row(3).unwrap();
    db.pool
        .update(
            &t,
            &key,
            &[CellValue {
                column: "v".into(),
                value: Some("edited".into()),
            }],
        )
        .await
        .unwrap();
    db.run("rollback").await;
    assert!(!db.pool.transaction_open(), "{kind:?}: ROLLBACK ends it");
    let page = db.pool.page_block(&t, 0, 50, "", None, None).await.unwrap();
    assert_eq!(page.rows.len(), 3, "the rollback took the row and the edit");

    // Across separate runs, a transaction stays on one connection.
    db.run(begin).await;
    db.run(&format!("update {name} set v = 'x' where a = 1"))
        .await;
    db.run("commit").await;
    let page = db.pool.page_block(&t, 0, 50, "", None, None).await.unwrap();
    assert_eq!(page.rows[0][2].as_deref(), Some("x"));
    db.run(&format!("drop table {name}")).await;
    db.pool.close().await;
}

#[tokio::test]
async fn postgres_transactions_stay_pinned() {
    transactions_stay_pinned(Kind::Postgres).await;
}

#[tokio::test]
async fn mysql_transactions_stay_pinned() {
    transactions_stay_pinned(Kind::Mysql).await;
}

#[tokio::test]
async fn sqlite_transactions_stay_pinned() {
    transactions_stay_pinned(Kind::Sqlite).await;
}

/// Browsing doesn't wait for a slow statement in the SQL panel.
async fn browsing_runs_beside_sql(kind: Kind, sleep: &str) {
    let Some(db) = open(kind).await else { return };
    let name = format!("dblens_pool_side_{}", format!("{kind:?}").to_lowercase());
    db.numbers(&name, 10, "a").await;
    let t = db.relation(&name);
    let pool = Arc::new(db.pool);
    let slow = {
        let pool = pool.clone();
        let sleep = sleep.to_string();
        tokio::spawn(async move { pool.execute(&sleep).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let start = Instant::now();
    let page = pool.page_block(&t, 0, 50, "", None, None).await.unwrap();
    let count = pool.count(&t, "").await.unwrap();
    assert_eq!(page.rows.len(), 10);
    assert_eq!(count, Some(10));
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "{kind:?}: browsing waited {:?}",
        start.elapsed()
    );
    // Cancel stops the SQL panel's run.
    pool.cancel_sql();
    let outcome = slow.await.unwrap();
    assert!(outcome.is_err() || kind == Kind::Mysql, "{outcome:?}");
    pool.execute(&format!("drop table {name}")).await.unwrap();
    pool.close().await;
}

#[tokio::test]
async fn postgres_browsing_runs_beside_sql() {
    browsing_runs_beside_sql(Kind::Postgres, "select pg_sleep(3)").await;
}

#[tokio::test]
async fn mysql_browsing_runs_beside_sql() {
    browsing_runs_beside_sql(Kind::Mysql, "select sleep(3)").await;
}

fn collecting(cap: usize) -> (StreamSink, Arc<Mutex<Vec<Value>>>) {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let kept = messages.clone();
    let sink = StreamSink::new(
        Arc::new(move |bytes: Vec<u8>| {
            let value: Value = rmp_serde::from_slice(&bytes).expect("MessagePack");
            kept.lock().unwrap().push(value);
            true
        }),
        cap,
    );
    (sink, messages)
}

async fn streams(kind: Kind) {
    let Some(db) = open(kind).await else { return };
    let name = format!("dblens_pool_stream_{}", format!("{kind:?}").to_lowercase());
    db.numbers(&name, 7_000, "a").await;
    let (mut sink, messages) = collecting(6_000);
    db.pool
        .execute_stream(
            &format!("update {name} set b = 1 where a = 1; select a, v from {name} order by a"),
            &mut sink,
        )
        .await
        .unwrap();
    sink.done(1.0, None);
    let all = messages.lock().unwrap().clone();
    let kinds: Vec<&str> = all.iter().map(|m| m["type"].as_str().unwrap()).collect();
    assert_eq!(kinds.first(), Some(&"end"), "{kinds:?}");
    assert_eq!(all[0]["totalRows"], 1, "the update");
    assert_eq!(all[1]["type"], "columns");
    assert_eq!(all[1]["columns"][0]["name"], "a");
    assert_eq!(all[2]["type"], "rows");
    assert_eq!(all[2]["rowCount"], 500, "the first block is small");
    assert_eq!(all[2]["data"][0][0], 1);
    let rows: u64 = all
        .iter()
        .filter(|m| m["type"] == "rows")
        .map(|m| m["rowCount"].as_u64().unwrap())
        .sum();
    assert_eq!(rows, 6_000, "kept up to the cap");
    let end = all.iter().rfind(|m| m["type"] == "end").unwrap();
    assert_eq!(end["totalRows"], 7_000);
    assert_eq!(end["truncated"], true);
    assert_eq!(end["result"], 1);
    assert_eq!(all.last().unwrap()["type"], "done");

    // An error in a later statement fails the run after the earlier rows went out.
    let (mut sink, messages) = collecting(100);
    let outcome = db
        .pool
        .execute_stream("select 1; select * from dblens_no_such_table", &mut sink)
        .await;
    assert!(outcome.is_err());
    assert!(!messages.lock().unwrap().is_empty());
    db.run(&format!("drop table {name}")).await;
    db.pool.close().await;
}

#[tokio::test]
async fn postgres_streams() {
    streams(Kind::Postgres).await;
}

#[tokio::test]
async fn mysql_streams() {
    streams(Kind::Mysql).await;
}

#[tokio::test]
async fn sqlite_streams() {
    streams(Kind::Sqlite).await;
}

#[tokio::test]
async fn sqlite_stream_on_a_closed_session_still_ends() {
    let Some(db) = open(Kind::Sqlite).await else {
        return;
    };
    db.pool.close().await;
    let (mut sink, messages) = collecting(100);
    let outcome = db.pool.execute_stream("select 1", &mut sink).await;
    assert!(outcome.is_err());
    sink.done(1.0, outcome.as_ref().err());
    let all = messages.lock().unwrap().clone();
    let last = all.last().expect("a done message");
    assert_eq!(last["type"], "done");
    assert!(last["error"].is_string());
}

fn view(name: &str) -> Relation {
    Relation::new("public", name, RelationKind::View, -1.0)
}

/// The browse connection's backend, read through the pool.
async fn browse_pid(db: &Db) -> String {
    let page = db
        .pool
        .page(&view("dblens_pool_pid"), 1, 0, "", None)
        .await
        .expect("page");
    page.rows[0][0].clone().unwrap()
}

#[tokio::test]
async fn postgres_replaces_a_dead_browse_connection() {
    let Some(db) = open(Kind::Postgres).await else {
        return;
    };
    db.run("create or replace view dblens_pool_pid as select pg_backend_pid()::text as pid")
        .await;
    let pid = browse_pid(&db).await;
    db.run(&format!("select pg_terminate_backend({pid})")).await;
    // The connection task notices the closed socket in a moment.
    tokio::time::sleep(Duration::from_millis(200)).await;
    for _ in 0..3 {
        db.pool.relations().await.expect("relations on a fresh connection");
    }
    assert_ne!(browse_pid(&db).await, pid);

    // Killed without the pool noticing first: the read is tried again.
    let pid = browse_pid(&db).await;
    db.run(&format!("select pg_terminate_backend({pid})")).await;
    db.pool.relations().await.expect("retried on a fresh connection");
    db.run("drop view dblens_pool_pid").await;
    db.pool.close().await;
}

#[tokio::test]
async fn postgres_browsing_follows_the_panels_scope() {
    let Some(db) = open(Kind::Postgres).await else {
        return;
    };
    db.run("create or replace view dblens_pool_scope as select pg_backend_pid()::text as pid")
        .await;
    let relation = view("dblens_pool_scope");
    let pinned: String = {
        let run = db.pool.execute("select pg_backend_pid()::text").await.unwrap();
        run.results[0].rows[0][0].clone().unwrap()
    };
    let shown = |r: QueryResult| r.rows[0][0].clone().unwrap();
    let before = shown(db.pool.page(&relation, 1, 0, "", None).await.unwrap());
    assert_ne!(before, pinned, "a browse connection");
    db.run("set search_path to pg_catalog, public").await;
    assert!(db.pool.scope_changed());
    let during = shown(db.pool.page(&relation, 1, 0, "", None).await.unwrap());
    assert_eq!(during, pinned, "browsing follows the panel");
    db.run("reset search_path").await;
    assert!(!db.pool.scope_changed());
    db.run("drop view dblens_pool_scope").await;
    db.pool.close().await;
}

#[tokio::test]
async fn redis_browsing_follows_select() {
    let Ok(url) = std::env::var("DBLENS_TEST_REDIS_URL") else {
        return;
    };
    if !url.ends_with("/15") {
        return;
    }
    let connection = connector::connect(&url, None).await.expect("connect");
    let pool = connection.session.clone();
    pool.execute("SET dblens_pool_scope:k 1").await.unwrap();
    let has = |relations: Vec<Relation>| relations.iter().any(|r| r.name == "dblens_pool_scope:k");
    assert!(has(pool.relations().await.unwrap()));
    pool.execute("SELECT 14").await.unwrap();
    assert!(pool.scope_changed());
    assert!(!has(pool.relations().await.unwrap()), "the sidebar shows db 14");
    pool.execute("SELECT 15\nDEL dblens_pool_scope:k").await.unwrap();
    assert!(!pool.scope_changed());
    assert!(!has(pool.relations().await.unwrap()));
    pool.close().await;
}
