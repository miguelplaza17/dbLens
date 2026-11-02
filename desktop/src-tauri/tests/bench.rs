//! The performance bench (ignored by default). Run it with:
//!
//! ```sh
//! DBLENS_TEST_URL=… DBLENS_TEST_MYSQL_URL=… cargo test --release --test bench -- --ignored bench --nocapture
//! ```
//!
//! It makes `bench_rows` (1,000,000 rows: id, int, text, timestamp, numeric,
//! bool, json) in each test database it has a URL for, measures connecting,
//! blocks, counting, streaming and encoding, prints a table, and drops the
//! table unless DBLENS_BENCH_KEEP=1 (a kept table is reused next time).
//! Results go in desktop/docs/PERF.md.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dblens_lib::connector;
use dblens_lib::drivers::{mysql, postgres};
use dblens_lib::pool::PooledSession;
use dblens_lib::session::{DatabaseSession, Execution, Relation, RelationKind};
use dblens_lib::wire::{self, StreamSink};

const ROWS: u64 = 1_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Postgres,
    Mysql,
}

async fn connect(kind: Kind, url: &str) -> Box<dyn DatabaseSession> {
    match kind {
        Kind::Postgres => postgres::connect(url, None).await.expect("connect"),
        Kind::Mysql => mysql::connect(url, None).await.expect("connect"),
    }
}

async fn pool(kind: Kind, url: &str) -> PooledSession {
    let pool = connector::pooled(Arc::from(connect(kind, url).await), url.to_string(), None);
    pool.warm_up();
    pool
}

fn ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.2} ms")
    } else {
        format!("{ms:.0} ms")
    }
}

/// The median of `n` runs of `work`.
async fn median<F, Fut, T>(n: usize, mut work: F) -> Duration
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let mut times = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        let _ = work().await;
        times.push(start.elapsed());
    }
    times.sort();
    times[n / 2]
}

async fn rows_in(session: &dyn DatabaseSession, name: &str) -> u64 {
    session
        .execute(&format!("select count(*) from {name}"))
        .await
        .ok()
        .and_then(|e| e.results.last()?.rows.first()?.first()?.clone())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

async fn make_table(kind: Kind, session: &dyn DatabaseSession) {
    if rows_in(session, "bench_rows").await == ROWS {
        return;
    }
    let run = |sql: String| async move {
        session
            .execute(&sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    };
    run("drop table if exists bench_rows".into()).await;
    match kind {
        Kind::Postgres => {
            run(
                "create table bench_rows (id bigint primary key, n int not null, t text, \
                 ts timestamp, num numeric(12,2), flag bool, doc jsonb)"
                    .into(),
            )
            .await;
            run(format!(
                "insert into bench_rows select i, (i::bigint * 7919) % 100000, 'row number ' || i, \
                 timestamp '2026-01-01' + i * interval '1 second', (i % 100000) / 100.0, i % 3 = 0, \
                 jsonb_build_object('i', i, 'tag', 'x' || (i % 10)) from generate_series(1, {ROWS}) i"
            ))
            .await;
            run("vacuum analyze bench_rows".into()).await;
        }
        Kind::Mysql => {
            run(
                "create table bench_rows (id bigint primary key, n int not null, t text, \
                 ts datetime, num decimal(12,2), flag bool, doc json)"
                    .into(),
            )
            .await;
            let digits = "(select 0 d union all select 1 union all select 2 union all select 3 \
                 union all select 4 union all select 5 union all select 6 union all select 7 \
                 union all select 8 union all select 9)";
            run(format!(
                "insert into bench_rows \
                 select i, (i * 7919) % 100000, concat('row number ', i), \
                 timestamp '2026-01-01 00:00:00' + interval i second, (i % 100000) / 100, i % 3 = 0, \
                 json_object('i', i, 'tag', concat('x', i % 10)) \
                 from (select 1 + a.d + 10*b.d + 100*c.d + 1000*e.d + 10000*f.d + 100000*g.d as i \
                       from {digits} a, {digits} b, {digits} c, {digits} e, {digits} f, {digits} g) s"
            ))
            .await;
            run("analyze table bench_rows".into()).await;
        }
    }
}

struct Stream {
    first: Duration,
    total: Duration,
    bytes: usize,
    messages: usize,
}

async fn stream(pool: &PooledSession, sql: &str) -> Stream {
    let bytes = Arc::new(AtomicUsize::new(0));
    let counted = bytes.clone();
    let mut sink = StreamSink::new(
        Arc::new(move |m: Vec<u8>| {
            counted.fetch_add(m.len(), Ordering::Relaxed);
            true
        }),
        wire::STREAM_ROW_CAP,
    );
    let start = Instant::now();
    pool.execute_stream(sql, &mut sink).await.expect(sql);
    let total = start.elapsed();
    sink.done(0.0, None);
    Stream {
        first: sink.first_rows_at.map(|t| t - start).unwrap_or_default(),
        total,
        bytes: bytes.load(Ordering::Relaxed),
        messages: sink.messages,
    }
}

async fn bench_engine(kind: Kind, url: &str, lines: &mut Vec<(String, String, String)>) {
    let mut add = |what: &str, value: String| {
        lines.push((format!("{kind:?}"), what.to_string(), value));
    };
    let setup = connect(kind, url).await;
    let start = Instant::now();
    make_table(kind, setup.as_ref()).await;
    add("make 1M-row table (or reuse)", ms(start.elapsed()));
    let schema = match kind {
        Kind::Postgres => "public".to_string(),
        Kind::Mysql => connector::summary(url).unwrap().database,
    };
    let table = Relation::new(schema, "bench_rows", RelationKind::Table, -1.0);

    add(
        "connect (one connection)",
        ms(median(5, || async {
            let s = connect(kind, url).await;
            s.close().await;
        })
        .await),
    );

    let pool = pool(kind, url).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    add(
        "first block, 500 rows (page_block 0)",
        ms(median(7, || pool.page_block(&table, 0, 500, "", None, None)).await),
    );
    let mut next = Vec::new();
    for _ in 0..7 {
        pool.page_block(&table, 0, 500, "", None, None)
            .await
            .unwrap();
        let start = Instant::now();
        pool.page_block(&table, 500, 500, "", None, None)
            .await
            .unwrap();
        next.push(start.elapsed());
    }
    next.sort();
    add("next block, 500 rows (keyset from previous)", ms(next[3]));
    // Deep blocks: plain OFFSET (the old way), a cold jump (key-only seek), and
    // the next block after it (keyset from the remembered key).
    add(
        "block at 900k, plain OFFSET (old page)",
        ms(median(3, || pool.pinned().page(&table, 500, 900_000, "", None)).await),
    );
    // A cold jump: the view starts over at 0, then the scrollbar lands at 900k
    // (no remembered key near it: one key-only seek, then the block).
    let mut cold_times = Vec::new();
    for _ in 0..5 {
        pool.page_block(&table, 0, 500, "", None, None)
            .await
            .unwrap();
        let start = Instant::now();
        let block = pool
            .page_block(&table, 900_000, 500, "", None, None)
            .await
            .unwrap();
        cold_times.push(start.elapsed());
        assert_eq!(block.rows[0][0].as_deref(), Some("900001"));
    }
    cold_times.sort();
    add("block at 900k, cold jump (key seek)", ms(cold_times[2]));
    pool.page_block(&table, 900_000, 500, "", None, None)
        .await
        .unwrap();
    add(
        "block at 900.5k after 900k (keyset)",
        ms(median(7, || pool.page_block(&table, 900_500, 500, "", None, None)).await),
    );
    add(
        "count (exact)",
        ms(median(3, || pool.count(&table, "")).await),
    );

    let s = stream(&pool, "select * from bench_rows limit 100000").await;
    add("execute_stream 100k rows: first block", ms(s.first));
    add("execute_stream 100k rows: total", ms(s.total));
    add(
        "execute_stream 100k rows: bytes / messages",
        format!("{:.1} MB / {}", s.bytes as f64 / 1e6, s.messages),
    );
    let s = stream(&pool, "select * from bench_rows").await;
    add(
        "execute_stream select * (1M seen, 100k kept): first block",
        ms(s.first),
    );
    add(
        "execute_stream select * (1M seen, 100k kept): total",
        ms(s.total),
    );
    let start = Instant::now();
    let old = pool
        .execute("select * from bench_rows limit 5000")
        .await
        .unwrap();
    add("execute 5k rows (whole, old path)", ms(start.elapsed()));

    // Encoding: JSON (the old IPC) vs MessagePack, per 500-row block.
    let block = pool
        .page_block(&table, 0, 500, "", None, None)
        .await
        .unwrap();
    let json = serde_json::to_vec(&block).unwrap();
    let pack = wire::block(&block, 0);
    add(
        "500-row block bytes: JSON vs MessagePack",
        format!(
            "{:.1} KB vs {:.1} KB ({:.0}%)",
            json.len() as f64 / 1e3,
            pack.len() as f64 / 1e3,
            pack.len() as f64 * 100.0 / json.len() as f64
        ),
    );
    let n = 200;
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(serde_json::to_vec(&block).unwrap());
    }
    let json_time = start.elapsed() / n;
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(wire::block(&block, 0));
    }
    let pack_time = start.elapsed() / n;
    add(
        "500-row block encode: JSON vs MessagePack",
        format!("{} vs {}", ms(json_time), ms(pack_time)),
    );
    let execution = Execution {
        results: old.results,
        duration_ms: 0.0,
    };
    let json = serde_json::to_vec(&execution).unwrap();
    let pack = wire::execution(&execution);
    add(
        "5k-row execution bytes: JSON vs MessagePack",
        format!(
            "{:.0} KB vs {:.0} KB",
            json.len() as f64 / 1e3,
            pack.len() as f64 / 1e3
        ),
    );
    if std::env::var("DBLENS_BENCH_KEEP").as_deref() != Ok("1") {
        let _ = pool.execute("drop table bench_rows").await;
    }
    pool.close().await;
    setup.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut lines: Vec<(String, String, String)> = Vec::new();
    for (kind, var) in [
        (Kind::Postgres, "DBLENS_TEST_URL"),
        (Kind::Mysql, "DBLENS_TEST_MYSQL_URL"),
    ] {
        let Some(url) = std::env::var(var).ok().filter(|u| !u.is_empty()) else {
            continue;
        };
        bench_engine(kind, &connector::clean(&url), &mut lines).await;
    }
    let width = lines.iter().map(|l| l.1.len()).max().unwrap_or(10);
    println!(
        "\n| Engine | Measure{} | Result |",
        " ".repeat(width.saturating_sub(7))
    );
    println!("|---|{}|---|", "-".repeat(width + 2));
    for (engine, what, value) in &lines {
        println!("| {engine} | {what:width$} | {value} |");
    }
}
