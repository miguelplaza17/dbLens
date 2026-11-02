//! The JSON the frontend receives. `src/api.ts` mirrors these keys and enum
//! spellings; when this test changes, change api.ts with it.

use dblens_lib::commands::{AtomicOutcome, ConnectInfo};
use dblens_lib::connector::Summary;
use dblens_lib::engine::Engine;
use dblens_lib::session::{
    Column, DbError, EditOutcome, Execution, QueryResult, Relation, RelationKind, RowKeyKind,
    RowKeys, SchemaColumn, SchemaGraph, SchemaLink, SchemaTable,
};
use dblens_lib::ssh::SshConfig;
use dblens_lib::store::{ConnectionFolder, SavedConnection, StoreSnapshot};
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

#[test]
fn query_result_and_execution() {
    let v = serde_json::to_value(result()).unwrap();
    assert_eq!(
        keys(&v),
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
    assert_eq!(v["columns"][0], json!({"name": "id", "type": "int4"}));
    assert_eq!(v["rows"][1][0], Value::Null);
    assert_eq!(v["keys"]["kind"], "primaryKey");
    for (kind, text) in [(RowKeyKind::Ctid, "ctid"), (RowKeyKind::Rowid, "rowid")] {
        assert_eq!(serde_json::to_value(kind).unwrap(), text);
    }

    let e = serde_json::to_value(Execution {
        results: vec![result()],
        duration_ms: 1.5,
    })
    .unwrap();
    assert_eq!(keys(&e), sorted(&["results", "durationMs"]));

    let ran = serde_json::to_value(AtomicOutcome::Ran {
        execution: Execution {
            results: vec![],
            duration_ms: 0.0,
        },
    })
    .unwrap();
    assert_eq!(ran["kind"], "ran");
    assert!(ran.get("execution").is_some());
    let not = serde_json::to_value(AtomicOutcome::NotAtomic {
        message: "no".into(),
    })
    .unwrap();
    assert_eq!(not, json!({"kind": "notAtomic", "message": "no"}));
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
