//! Streamed runs (`execute_stream`) on MongoDB and Redis against throwaway
//! servers. Each test skips when its DBLENS_TEST_MONGO_URL or
//! DBLENS_TEST_REDIS_URL is unset, uses its own collection or key prefix, and
//! cleans up.

use std::sync::{Arc, Mutex};

use dblens_lib::drivers::{mongo, redis};
use dblens_lib::session::{DatabaseSession, DbError};
use dblens_lib::wire::StreamSink;
use serde_json::Value;

type Messages = Arc<Mutex<Vec<Value>>>;

fn collecting(cap: usize) -> (StreamSink, Messages) {
    let messages: Messages = Arc::new(Mutex::new(Vec::new()));
    let kept = messages.clone();
    let sink = StreamSink::new(
        Arc::new(move |bytes: Vec<u8>| {
            kept.lock()
                .unwrap()
                .push(rmp_serde::from_slice(&bytes).expect("MessagePack"));
            true
        }),
        cap,
    );
    (sink, messages)
}

/// What the front end's StreamCollector builds from the messages of one run.
#[derive(Default, Debug)]
struct Block {
    columns: Vec<String>,
    data: Vec<Vec<Value>>,
    rows: usize,
    end: Option<Value>,
}

fn collect(messages: &Messages) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for m in messages.lock().unwrap().iter() {
        let at = m["result"].as_u64().unwrap_or(0) as usize;
        match m["type"].as_str().unwrap() {
            "columns" => {
                let names: Vec<String> = m["columns"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c["name"].as_str().unwrap().to_string())
                    .collect();
                if m.get("append").is_some() {
                    let block = &mut blocks[at];
                    block.columns.extend(names.iter().cloned());
                    for _ in &names {
                        block.data.push(vec![Value::Null; block.rows]);
                    }
                } else {
                    while blocks.len() <= at {
                        blocks.push(Block::default());
                    }
                    blocks[at].data = names.iter().map(|_| vec![]).collect();
                    blocks[at].columns = names;
                }
            }
            "rows" => {
                let block = &mut blocks[at];
                let count = m["rowCount"].as_u64().unwrap() as usize;
                for (c, column) in block.data.iter_mut().enumerate() {
                    for r in 0..count {
                        column.push(m["data"][c][r].clone());
                    }
                }
                block.rows += count;
            }
            "end" => {
                while blocks.len() <= at {
                    blocks.push(Block::default());
                }
                blocks[at].end = Some(m.clone());
            }
            _ => {}
        }
    }
    blocks
}

fn kinds(messages: &Messages) -> Vec<String> {
    messages
        .lock()
        .unwrap()
        .iter()
        .map(|m| m["type"].as_str().unwrap().to_string())
        .collect()
}

// MARK: MongoDB

async fn mongo_session() -> Option<mongo::MongoSession> {
    let url = std::env::var("DBLENS_TEST_MONGO_URL")
        .ok()
        .filter(|u| !u.is_empty())?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    Some(mongo::open(&url, None).await.expect("connect"))
}

async fn mongo_run(session: &mongo::MongoSession, text: &str) {
    session
        .execute(text)
        .await
        .unwrap_or_else(|e| panic!("{text}: {e}"));
}

#[tokio::test]
async fn mongo_streams_20k_documents_with_late_keys() {
    let Some(session) = mongo_session().await else {
        return;
    };
    let name = format!(
        "dblens_stream_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    // 20,000 documents; only the last 5,000 have "late".
    mongo_run(
        &session,
        &format!(
            "db.{name}_seed.insertOne({{}})\n\
             db.{name}_seed.aggregate([{{$project: {{_id: 0, n: {{$range: [0, 20000]}}}}}}, {{$unwind: '$n'}}, \
             {{$set: {{late: {{$cond: [{{$gte: ['$n', 15000]}}, 'x', '$$REMOVE']}}}}}}, {{$out: '{name}'}}])"
        ),
    )
    .await;

    // find(): the first block after 500 documents, `_id` first, `late` appended.
    let (mut sink, messages) = collecting(100_000);
    session
        .execute_stream(&format!("db.{name}.find()"), &mut sink)
        .await
        .unwrap();
    sink.done(1.0, None);
    let first_kinds = kinds(&messages);
    assert_eq!(first_kinds[0], "columns");
    assert_eq!(first_kinds[1], "rows");
    assert_eq!(messages.lock().unwrap()[1]["rowCount"], 500);
    assert!(
        messages
            .lock()
            .unwrap()
            .iter()
            .any(|m| m["type"] == "columns" && m["append"] == true),
        "the late key arrives as an appended column"
    );
    assert_eq!(first_kinds.last().unwrap(), "done");
    let blocks = collect(&messages);
    assert_eq!(blocks.len(), 1);
    let block = &blocks[0];
    assert_eq!(block.columns, vec!["_id", "n", "late"]);
    assert_eq!(block.rows, 20_000, "past the old 5,000 cut");
    assert!(block.data.iter().all(|c| c.len() == 20_000));
    let end = block.end.as_ref().unwrap();
    assert_eq!(end["totalRows"], 20_000);
    assert_eq!(end["truncated"], false);
    assert_eq!(end["command"], "FIND 20000");
    let late = &block.data[2];
    assert_eq!(late[0], Value::Null);
    assert_eq!(late[14_999], Value::Null);
    assert_eq!(late[19_999], "x");
    assert_eq!(late.iter().filter(|v| !v.is_null()).count(), 5_000);
    // Every _id is a distinct 24-hex string, in the first column.
    assert_eq!(block.data[0][0].as_str().unwrap().len(), 24);

    // A row cap cuts the result: kept rows up to it, then marked truncated.
    let (mut sink, messages) = collecting(1_000);
    session
        .execute_stream(
            &format!("db.{name}.find({{}}, {{_id: 0}}).sort({{n: 1}})"),
            &mut sink,
        )
        .await
        .unwrap();
    sink.done(1.0, None);
    let blocks = collect(&messages);
    assert_eq!(blocks[0].rows, 1_000);
    assert_eq!(blocks[0].columns[0], "n");
    assert_eq!(blocks[0].data[0][999], 999);
    let end = blocks[0].end.as_ref().unwrap();
    assert_eq!(end["truncated"], true);
    assert!(end["command"].as_str().unwrap().ends_with('+'));

    // aggregate streams too, next to a statement that has no rows to stream.
    let (mut sink, messages) = collecting(100_000);
    session
        .execute_stream(
            &format!(
                "db.{name}.countDocuments()\n\
                 db.{name}.aggregate([{{$match: {{n: {{$lt: 1200}}}}}}, {{$project: {{_id: 0, n: 1}}}}])"
            ),
            &mut sink,
        )
        .await
        .unwrap();
    sink.done(1.0, None);
    let blocks = collect(&messages);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].data[0][0], 20_000);
    assert_eq!(blocks[1].rows, 1_200);
    assert_eq!(blocks[1].end.as_ref().unwrap()["command"], "AGGREGATE 1200");

    // Nothing found: only the count, as `execute` shows it.
    let (mut sink, messages) = collecting(100);
    session
        .execute_stream(&format!("db.{name}.find({{n: -1}})"), &mut sink)
        .await
        .unwrap();
    sink.done(1.0, None);
    assert_eq!(kinds(&messages), vec!["end", "done"]);

    // A window that goes away stops the run.
    let (sink, _) = collecting(100_000);
    let mut sink = sink.with_alive(Arc::new(|| false));
    let outcome = session
        .execute_stream(&format!("db.{name}.find()"), &mut sink)
        .await;
    assert!(matches!(outcome, Err(DbError::Cancelled)), "{outcome:?}");

    // A bad statement fails the run after the earlier results went out.
    let (mut sink, messages) = collecting(100);
    let outcome = session
        .execute_stream(
            &format!("db.{name}.countDocuments()\ndb.{name}.nonsense()"),
            &mut sink,
        )
        .await;
    assert!(outcome.is_err());
    assert!(!messages.lock().unwrap().is_empty());

    mongo_run(
        &session,
        &format!("db.{name}.drop(); db.{name}_seed.drop()"),
    )
    .await;
}

// MARK: Redis

async fn redis_session() -> Option<Box<dyn DatabaseSession>> {
    let url = std::env::var("DBLENS_TEST_REDIS_URL").ok()?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    Some(redis::connect(&url, None).await.expect("connect"))
}

async fn streamed(
    session: &dyn DatabaseSession,
    text: &str,
    cap: usize,
) -> (Vec<Block>, Messages, Result<(), DbError>) {
    let (mut sink, messages) = collecting(cap);
    let outcome = session.execute_stream(text, &mut sink).await;
    sink.done(1.0, outcome.as_ref().err());
    (collect(&messages), messages, outcome)
}

#[tokio::test]
async fn redis_streams_big_arrays_and_scans() {
    let Some(session) = redis_session().await else {
        return;
    };
    let p = format!(
        "dblens:stream:{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let n = 20_000;
    let words = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<Vec<_>>().join(" ");
    let setup = [
        format!("RPUSH {p}:list {}", words(&|i| format!("v{i}"))),
        format!("HSET {p}:hash {}", words(&|i| format!("f{i} v{i}"))),
        format!("ZADD {p}:zset {}", words(&|i| format!("{i} m{i}"))),
        format!("SADD {p}:set {}", words(&|i| format!("s{i}"))),
        format!("SET {p}:text hello"),
    ];
    for line in &setup {
        session.execute(line).await.unwrap();
    }

    // LRANGE: one column, the first block after 500 rows, every element.
    let (blocks, messages, outcome) =
        streamed(session.as_ref(), &format!("LRANGE {p}:list 0 -1"), 100_000).await;
    outcome.unwrap();
    assert_eq!(kinds(&messages)[..2], ["columns", "rows"]);
    assert_eq!(messages.lock().unwrap()[1]["rowCount"], 500);
    assert_eq!(blocks[0].columns, vec!["value"]);
    assert_eq!(blocks[0].rows, n, "past the old 5,000 cut");
    assert_eq!(blocks[0].data[0][0], "v0");
    assert_eq!(blocks[0].data[0][n - 1], format!("v{}", n - 1));
    let end = blocks[0].end.as_ref().unwrap();
    assert_eq!(end["totalRows"], n);
    assert_eq!(end["truncated"], false);
    assert_eq!(end["command"], format!("LRANGE {n}"));

    // A row cap keeps the first rows and counts the rest.
    let (blocks, _, outcome) =
        streamed(session.as_ref(), &format!("LRANGE {p}:list 0 -1"), 700).await;
    outcome.unwrap();
    assert_eq!(blocks[0].rows, 700);
    let end = blocks[0].end.as_ref().unwrap();
    assert_eq!(end["totalRows"], n);
    assert_eq!(end["truncated"], true);

    // HGETALL: field and value; ZRANGE WITHSCORES: member and score.
    let (blocks, _, outcome) =
        streamed(session.as_ref(), &format!("HGETALL {p}:hash"), 100_000).await;
    outcome.unwrap();
    assert_eq!(blocks[0].columns, vec!["field", "value"]);
    assert_eq!(blocks[0].rows, n);
    // A hash has no order: each field still sits next to its own value.
    for r in [0, 5, n - 1] {
        let field = blocks[0].data[0][r].as_str().unwrap();
        assert_eq!(
            blocks[0].data[1][r].as_str().unwrap(),
            field.replace('f', "v")
        );
    }
    let (blocks, _, outcome) = streamed(
        session.as_ref(),
        &format!("ZRANGE {p}:zset 0 -1 WITHSCORES"),
        100_000,
    )
    .await;
    outcome.unwrap();
    assert_eq!(blocks[0].columns, vec!["member", "score"]);
    assert_eq!(blocks[0].rows, n);
    assert_eq!(blocks[0].data[0][19_999], "m19999");
    assert_eq!(blocks[0].data[1][19_999], 19_999);

    // The streamed grid is the one `execute` builds.
    let small = format!("HGETALL {p}:hash");
    let whole = session.execute(&small).await.unwrap();
    let whole = whole.main().unwrap();
    assert_eq!(whole.rows.len(), 5_000);
    assert!(whole.truncated);

    // SCAN loops to the end; SSCAN, HSCAN and ZSCAN too.
    let (blocks, _, outcome) =
        streamed(session.as_ref(), &format!("SCAN 0 MATCH {p}:*"), 100_000).await;
    outcome.unwrap();
    assert_eq!(blocks[0].columns, vec!["key"]);
    let mut keys: Vec<&str> = blocks[0].data[0]
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), 5, "{keys:?}");
    for (command, columns) in [
        (format!("SSCAN {p}:set 0"), vec!["member"]),
        (
            format!("HSCAN {p}:hash 0 COUNT 100"),
            vec!["field", "value"],
        ),
        (format!("ZSCAN {p}:zset 0"), vec!["member", "score"]),
    ] {
        let (blocks, _, outcome) = streamed(session.as_ref(), &command, 100_000).await;
        outcome.unwrap();
        assert_eq!(blocks[0].columns, columns, "{command}");
        let mut firsts: Vec<&str> = blocks[0].data[0]
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        firsts.sort_unstable();
        firsts.dedup();
        assert_eq!(firsts.len(), n, "{command}");
    }
    // A cap stops a SCAN early and marks it cut.
    let (blocks, _, outcome) =
        streamed(session.as_ref(), &format!("SSCAN {p}:set 0 COUNT 100"), 300).await;
    outcome.unwrap();
    assert_eq!(blocks[0].rows, 300);
    let end = blocks[0].end.as_ref().unwrap();
    assert_eq!(end["truncated"], true);
    assert!(end["command"].as_str().unwrap().ends_with('+'));

    // Scalars, several commands, and errors.
    let (blocks, _, outcome) = streamed(
        session.as_ref(),
        &format!("GET {p}:text\nLRANGE {p}:list 0 2\nSET {p}:text bye\nLRANGE {p}:nothing 0 -1"),
        100,
    )
    .await;
    outcome.unwrap();
    assert_eq!(blocks[0].data[0][0], "hello");
    assert_eq!(blocks[1].rows, 3);
    assert_eq!(blocks[2].data[0][0], "OK");
    assert_eq!(blocks[3].columns, vec!["value"]);
    assert_eq!(blocks[3].rows, 0);
    let (_, messages, outcome) =
        streamed(session.as_ref(), &format!("HGETALL {p}:list"), 100).await;
    let error = outcome.unwrap_err().to_string();
    assert!(error.contains("WRONGTYPE"), "{error}");
    assert_eq!(kinds(&messages).last().unwrap(), "done");
    // The connection still works after the error.
    let (blocks, _, outcome) = streamed(session.as_ref(), &format!("GET {p}:text"), 100).await;
    outcome.unwrap();
    assert_eq!(blocks[0].data[0][0], "bye");

    // A window that goes away stops the run and the session recovers.
    let (sink, _) = collecting(100_000);
    let mut sink = sink.with_alive(Arc::new(|| false));
    let outcome = session
        .execute_stream(&format!("LRANGE {p}:list 0 -1"), &mut sink)
        .await;
    assert!(matches!(outcome, Err(DbError::Cancelled)), "{outcome:?}");
    let (blocks, _, outcome) = streamed(session.as_ref(), &format!("GET {p}:text"), 100).await;
    outcome.unwrap();
    assert_eq!(blocks[0].data[0][0], "bye");

    session
        .execute(&format!("DEL {p}:list {p}:hash {p}:zset {p}:set {p}:text"))
        .await
        .unwrap();
}
