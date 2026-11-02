//! What the frontend receives: MessagePack for table data (`wire.rs`), JSON
//! for the rest. `src/api.ts` mirrors these keys and enum spellings; when this
//! test changes, change api.ts with it.

use std::sync::{Arc, Mutex};

use dblens_lib::commands::ConnectInfo;
use dblens_lib::connector::Summary;
use dblens_lib::engine::Engine;
use dblens_lib::session::{
    Column, DbError, EditOutcome, Execution, QueryResult, Relation, RelationKind, RowKeyKind,
    RowKeys, SchemaColumn, SchemaGraph, SchemaLink, SchemaTable,
};
use dblens_lib::ssh::SshConfig;
use dblens_lib::store::{ConnectionFolder, SavedConnection, StoreSnapshot};
use dblens_lib::wire::{self, StreamSink};
use serde_json::{json, Value};

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    names.sort();
    names
}

fn result() -> QueryResult {
    QueryResult {
        columns: vec![Column::new("id", "int4")],
        rows: vec![vec![Some("1".into())], vec![None]],
        command: "SELECT 2".into(),
        truncated: false,
        total_rows: 2,
        keys: Some(RowKeys {
            columns: vec!["id".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![vec![Some("1".into())]],
        }),
        note: None,
    }
}

/// MessagePack bytes as a JSON value, the way @msgpack/msgpack decodes them.
fn decode(bytes: &[u8]) -> Value {
    rmp_serde::from_slice(bytes).expect("valid MessagePack")
}

#[test]
fn result_blocks_are_messagepack_and_column_major() {
    // `page`, `page_block`: a block (api.ts `ResultBlock`).
    let v = decode(&wire::block(&result(), 400));
    assert_eq!(
        keys(&v),
        sorted(&[
            "columns",
            "data",
            "rowCount",
            "offset",
            "command",
            "truncated",
            "totalRows",
            "keys",
            "note"
        ])
    );
    assert_eq!(v["columns"][0], json!({"name": "id", "type": "int4"}));
    // One array per column; integers that JS holds exactly arrive as numbers.
    assert_eq!(v["data"], json!([[1, null]]));
    assert_eq!(v["rowCount"], 2);
    assert_eq!(v["offset"], 400);
    assert_eq!(v["totalRows"], 2);
    assert_eq!(v["note"], Value::Null);
    // Keys: column-major too, always strings (sent back exactly as they came).
    assert_eq!(
        v["keys"],
        json!({"columns": ["id"], "kind": "primaryKey", "data": [["1"]]})
    );
    // Text that only looks like a number stays text.
    let mut text = result();
    text.rows = vec![
        vec![Some("007".into())],
        vec![Some("9007199254740993".into())],
    ];
    assert_eq!(
        decode(&wire::block(&text, 0))["data"],
        json!([["007", "9007199254740993"]])
    );

    // `execute`, `read_only`, `rehearse`: an execution of blocks.
    let e = decode(&wire::execution(&Execution {
        results: vec![result(), QueryResult::command("UPDATE 1", 1)],
        duration_ms: 1.5,
    }));
    assert_eq!(keys(&e), sorted(&["results", "durationMs"]));
    assert_eq!(e["durationMs"], 1.5);
    assert_eq!(e["results"][1]["columns"], json!([]));
    assert_eq!(e["results"][1]["data"], json!([]));
    assert_eq!(e["results"][1]["totalRows"], 1);
    assert_eq!(e["results"][1]["keys"], Value::Null);

    // `execute_atomically`.
    let ran = decode(&wire::atomic_ran(&Execution {
        results: vec![],
        duration_ms: 0.0,
    }));
    assert_eq!(ran["kind"], "ran");
    assert_eq!(keys(&ran["execution"]), sorted(&["results", "durationMs"]));
    assert_eq!(
        decode(&wire::atomic_refused("no")),
        json!({"kind": "notAtomic", "message": "no"})
    );

    // `export_result` still takes a QueryResult as JSON (row-major).
    let j = serde_json::to_value(result()).unwrap();
    assert_eq!(
        keys(&j),
        sorted(&[
            "columns",
            "rows",
            "command",
            "truncated",
            "totalRows",
            "keys",
            "note"
        ])
    );
    assert_eq!(j["rows"][1][0], Value::Null);
    for (kind, text) in [(RowKeyKind::Ctid, "ctid"), (RowKeyKind::Rowid, "rowid")] {
        assert_eq!(serde_json::to_value(kind).unwrap(), text);
    }
}

#[test]
fn stream_messages() {
    let messages = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let kept = messages.clone();
    let mut sink = StreamSink::new(
        Arc::new(move |m| {
            kept.lock().unwrap().push(m);
            true
        }),
        wire::STREAM_ROW_CAP,
    );
    sink.send_result(&result());
    sink.command("UPDATE 3", 3);
    sink.done(2.5, Some(&DbError::Cancelled));
    let all: Vec<Value> = messages.lock().unwrap().iter().map(|m| decode(m)).collect();
    assert_eq!(keys(&all[0]), sorted(&["type", "result", "columns"]));
    assert_eq!(all[0]["type"], "columns");
    assert_eq!(all[0]["columns"][0], json!({"name": "id", "type": "int4"}));
    assert_eq!(
        keys(&all[1]),
        sorted(&["type", "result", "offset", "rowCount", "data"])
    );
    assert_eq!(all[1]["type"], "rows");
    assert_eq!(all[1]["data"], json!([[1, null]]));
    assert_eq!(
        keys(&all[2]),
        sorted(&["type", "result", "command", "totalRows", "truncated"])
    );
    assert_eq!(all[2]["type"], "end");
    assert_eq!(all[2]["result"], 0);
    assert_eq!(all[3]["type"], "end");
    assert_eq!(all[3]["result"], 1);
    assert_eq!(all[3]["totalRows"], 3);
    assert_eq!(
        keys(&all[4]),
        sorted(&["type", "durationMs", "error", "cancelled"])
    );
    assert_eq!(all[4]["error"], "The query was canceled.");
    assert_eq!(all[4]["cancelled"], true);
}

#[test]
fn relations_and_enums() {
    let r = serde_json::to_value(Relation {
        schema: "public".into(),
        name: "users".into(),
        kind: RelationKind::Materialized,
        estimate: -1.0,
        subtype: String::new(),
    })
    .unwrap();
    assert_eq!(
        keys(&r),
        sorted(&["schema", "name", "kind", "estimate", "subtype"])
    );
    assert_eq!(r["kind"], "materialized");
    for (kind, text) in [
        (RelationKind::Table, "table"),
        (RelationKind::Partitioned, "partitioned"),
        (RelationKind::View, "view"),
        (RelationKind::Foreign, "foreign"),
        (RelationKind::Key, "key"),
    ] {
        assert_eq!(serde_json::to_value(kind).unwrap(), text);
    }
    for (engine, text) in [
        (Engine::Postgres, "postgres"),
        (Engine::Mysql, "mysql"),
        (Engine::Sqlite, "sqlite"),
        (Engine::Redis, "redis"),
        (Engine::Mongodb, "mongodb"),
    ] {
        assert_eq!(serde_json::to_value(engine).unwrap(), text);
    }
    // Errors reach the frontend as plain strings.
    assert_eq!(
        serde_json::to_value(DbError::Message("boom".into())).unwrap(),
        "boom"
    );
    let edit = serde_json::to_value(EditOutcome {
        sql: "x".into(),
        command: "UPDATE 1".into(),
        affected: 1,
    })
    .unwrap();
    assert_eq!(keys(&edit), sorted(&["sql", "command", "affected"]));
}

#[test]
fn schema_graph() {
    let g = serde_json::to_value(SchemaGraph {
        tables: vec![SchemaTable {
            schema: "public".into(),
            name: "a".into(),
            is_view: false,
            columns: vec![SchemaColumn {
                name: "id".into(),
                type_name: "int4".into(),
                is_primary_key: true,
                is_nullable: false,
            }],
        }],
        links: vec![SchemaLink {
            name: "fk".into(),
            from: "public.b".into(),
            from_columns: vec!["a_id".into()],
            to: "public.a".into(),
            to_columns: vec!["id".into()],
        }],
    })
    .unwrap();
    assert_eq!(
        keys(&g["tables"][0]),
        sorted(&["schema", "name", "isView", "columns"])
    );
    assert_eq!(
        keys(&g["tables"][0]["columns"][0]),
        sorted(&["name", "type", "isPrimaryKey", "isNullable"])
    );
    assert_eq!(
        keys(&g["links"][0]),
        sorted(&["name", "from", "fromColumns", "to", "toColumns"])
    );
}

#[test]
fn connections() {
    let ssh = SshConfig {
        host: "bastion".into(),
        port: Some(22),
        user: "ana".into(),
        key_path: String::new(),
    };
    let s = serde_json::to_value(&ssh).unwrap();
    assert_eq!(keys(&s), sorted(&["host", "port", "user", "keyPath"]));

    let info = serde_json::to_value(ConnectInfo {
        session_id: "1".into(),
        engine: Engine::Postgres,
        server_version: "16".into(),
        name: "n".into(),
        saved_id: None,
        ssh_label: None,
    })
    .unwrap();
    assert_eq!(
        keys(&info),
        sorted(&[
            "sessionId",
            "engine",
            "serverVersion",
            "name",
            "savedId",
            "sshLabel"
        ])
    );

    let summary = serde_json::to_value(Summary {
        engine: Engine::Redis,
        database: "db 3".into(),
        host: None,
        port: None,
        user: None,
        tls: None,
        suggested_name: "x".into(),
    })
    .unwrap();
    assert_eq!(
        keys(&summary),
        sorted(&[
            "engine",
            "database",
            "host",
            "port",
            "user",
            "tls",
            "suggestedName"
        ])
    );

    let snap = serde_json::to_value(StoreSnapshot {
        connections: vec![SavedConnection {
            id: "c".into(),
            name: "n".into(),
            engine: Engine::Sqlite,
            created_at: "2026-01-01T00:00:00Z".into(),
            last_used: None,
            folder_id: String::new(),
            sort_index: 0,
            ssh: Some(ssh),
        }],
        folders: vec![ConnectionFolder {
            id: "f".into(),
            name: "F".into(),
            expanded: true,
        }],
        key_in_file: true,
    })
    .unwrap();
    assert_eq!(snap["keyInFile"], true);
    assert_eq!(
        keys(&snap["connections"][0]),
        sorted(&[
            "id",
            "name",
            "engine",
            "createdAt",
            "lastUsed",
            "folderId",
            "sortIndex",
            "ssh"
        ])
    );
    assert_eq!(
        keys(&snap["folders"][0]),
        sorted(&["id", "name", "expanded"])
    );
}

/// With `DBLENS_WIRE_FIXTURES=<dir>`, writes the bytes the front end receives
/// (a block, an execution, a stream) so `@msgpack/msgpack` can be checked
/// against them from Node (desktop/docs/PERF.md, "Contract check").
#[test]
fn writes_fixtures_for_the_front_end() {
    let Some(dir) = std::env::var_os("DBLENS_WIRE_FIXTURES") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut wide = result();
    wide.columns.push(Column::new("t", "text"));
    for row in &mut wide.rows {
        row.push(Some("ünï \"q\" 007".into()));
    }
    wide.rows.push(vec![Some("9007199254740993".into()), None]);
    wide.rows
        .push(vec![Some("-42".into()), Some(String::new())]);
    wide.total_rows = 4;
    wide.keys = None;
    std::fs::write(dir.join("block.msgpack"), wire::block(&wide, 400)).unwrap();
    std::fs::write(
        dir.join("execution.msgpack"),
        wire::execution(&Execution {
            results: vec![wide.clone(), QueryResult::command("UPDATE 1", 1)],
            duration_ms: 1.5,
        }),
    )
    .unwrap();
    let messages = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let kept = messages.clone();
    let mut sink = StreamSink::new(
        Arc::new(move |m| {
            kept.lock().unwrap().push(m);
            true
        }),
        wire::STREAM_ROW_CAP,
    );
    sink.send_result(&wide);
    sink.command("UPDATE 3", 3);
    sink.done(2.5, None);
    for (i, m) in messages.lock().unwrap().iter().enumerate() {
        std::fs::write(dir.join(format!("stream-{i}.msgpack")), m).unwrap();
    }
}
