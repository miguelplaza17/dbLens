//! The MCP server (src/mcp.rs): the JSON-RPC loop on in-memory pipes against a
//! temp SQLite file and (with DBLENS_TEST_URL) the test Postgres, and the real
//! binary with `--mcp` on a temp store folder.

use std::sync::{Arc, Mutex};

use std::time::Duration;

use dblens_lib::mcp::{serve, serve_with_timeout, StoreSource};
use dblens_lib::store::{ConnectionDraft, KeySource, Store};
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

struct Client {
    write: tokio::io::WriteHalf<DuplexStream>,
    read: BufReader<tokio::io::ReadHalf<DuplexStream>>,
    next: i64,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Client {
    fn start(source: StoreSource) -> Client {
        Client::with_server(|read, write| tokio::spawn(serve(read, write, source)))
    }

    /// A server that stops waiting for a call after `timeout`.
    fn start_with_timeout(source: StoreSource, timeout: Duration) -> Client {
        Client::with_server(|read, write| {
            tokio::spawn(serve_with_timeout(read, write, source, timeout))
        })
    }

    fn with_server(
        spawn: impl FnOnce(
            BufReader<tokio::io::ReadHalf<DuplexStream>>,
            tokio::io::WriteHalf<DuplexStream>,
        ) -> tokio::task::JoinHandle<std::io::Result<()>>,
    ) -> Client {
        let (ours, theirs) = duplex(1 << 20);
        let (their_read, their_write) = tokio::io::split(theirs);
        let server = spawn(BufReader::new(their_read), their_write);
        let (read, write) = tokio::io::split(ours);
        Client {
            write,
            read: BufReader::new(read),
            next: 0,
            server,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.write.write_all(line.as_bytes()).await.unwrap();
    }

    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.read.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let reply = self.receive().await;
        assert_eq!(reply["id"], id);
        reply
    }

    /// (isError, text) of a tools/call.
    async fn call(&mut self, tool: &str, arguments: Value) -> (bool, String) {
        let reply = self
            .request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await;
        let result = &reply["result"];
        (
            result["isError"].as_bool().unwrap(),
            result["content"][0]["text"].as_str().unwrap().to_string(),
        )
    }

    async fn call_ok(&mut self, tool: &str, arguments: Value) -> Value {
        let (error, text) = self.call(tool, arguments).await;
        assert!(!error, "{tool} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    async fn finish(mut self) {
        // Closing the write half is the server's end of input.
        self.write.shutdown().await.unwrap();
        self.server.await.unwrap().unwrap();
    }
}

async fn count(client: &mut Client) -> Value {
    client
        .call_ok(
            "query",
            json!({"connection": "Lite", "query": "select count(*) from person"}),
        )
        .await["rows"]
        .clone()
}

fn shared() -> Arc<Mutex<Store>> {
    Arc::new(Mutex::new(Store::in_memory()))
}

fn save(store: &Arc<Mutex<Store>>, name: &str, url: &str) -> String {
    store
        .lock()
        .unwrap()
        .save(ConnectionDraft {
            id: None,
            name: name.into(),
            url: url.into(),
            ssh: None,
            folder_id: None,
        })
        .unwrap()
        .id
}

#[tokio::test]
async fn handshake_and_tool_list() {
    let mut client = Client::start(StoreSource::Shared(shared()));
    let hello = client
        .request("initialize", json!({"protocolVersion": "2025-03-26"}))
        .await;
    assert_eq!(hello["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(hello["result"]["serverInfo"]["name"], "dblens");
    // A notification gets no answer: the next reply is the ping's.
    client
        .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;
    assert_eq!(client.request("ping", json!({})).await["result"], json!({}));
    let list = client.request("tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for tool in [
        "list_connections",
        "list_tables",
        "describe_schema",
        "describe_table",
        "query",
        "test_changes",
        "execute",
        "create_connection",
        "delete_connection",
        "list_extensions",
        "install_extension",
    ] {
        assert!(names.contains(&tool), "missing {tool}");
    }
    client.send(json!("not an object")).await;
    assert_eq!(client.receive().await["error"]["code"], -32700);
    let unknown = client.request("nope", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601);
    let (error, text) = client.call("nope", json!({})).await;
    assert!(error && text.contains("Unknown tool"));
    client.finish().await;
}

#[tokio::test]
async fn sqlite_read_write_gates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "create table team(id integer primary key, name text not null);
             create table person(id integer primary key, team_id integer references team(id), name text);
             insert into team values (1, 'core'); insert into person values (1, 1, 'ana');",
        )
        .unwrap();
    }
    let store = shared();
    let id = save(&store, "Lite", &format!("sqlite://{}", path.display()));
    let mut client = Client::start(StoreSource::Shared(store.clone()));

    let list = client.call_ok("list_connections", json!({})).await;
    assert_eq!(
        list,
        json!([{"name": "Lite", "engine": "SQLite", "via_ssh": false, "agents_can_write": false}])
    );
    assert!(
        !list.to_string().contains("agent.db"),
        "the URL never shows"
    );

    let tables = client
        .call_ok("list_tables", json!({"connection": "lite"}))
        .await;
    let names: Vec<&str> = tables
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["person", "team"]);

    let schema = client
        .call_ok("describe_schema", json!({"connection": "Lite"}))
        .await;
    let schema = schema.as_str().unwrap();
    assert!(schema.contains("FK (team_id) -> main.team(id)"), "{schema}");

    let table = client
        .call_ok(
            "describe_table",
            json!({"connection": "Lite", "table": "team"}),
        )
        .await;
    assert_eq!(table["primary_key"], json!(["id"]));
    assert_eq!(table["referenced_by"][0]["table"], "main.person");
    let (error, text) = client
        .call(
            "describe_table",
            json!({"connection": "Lite", "table": "ghost"}),
        )
        .await;
    assert!(error && text.contains("No table"));

    let rows = client
        .call_ok(
            "query",
            json!({"connection": "Lite", "query": "select name from person", "max_rows": 1}),
        )
        .await;
    assert_eq!(rows["rows"], json!([["ana"]]));

    // Reading is read-only: the database itself refuses.
    let (error, _) = client
        .call(
            "query",
            json!({"connection": "Lite", "query": "delete from person"}),
        )
        .await;
    assert!(error);

    // Writing is off until the user turns it on.
    for tool in ["execute", "test_changes"] {
        let (error, text) = client
            .call(
                tool,
                json!({"connection": "Lite", "statements": "delete from person"}),
            )
            .await;
        assert!(error && text.contains("Let Agents Write"), "{text}");
    }
    let (error, text) = client
        .call("create_connection", json!({"url": "sqlite:///tmp/x.db"}))
        .await;
    assert!(
        error && text.contains("Let agents add and remove"),
        "{text}"
    );
    let (error, _) = client
        .call("delete_connection", json!({"connection": "Lite"}))
        .await;
    assert!(error);

    // The app turns it on: the running server sees it on the next call.
    store
        .lock()
        .unwrap()
        .set_agents_can_write(&id, true)
        .unwrap();
    let tried = client
        .call_ok(
            "test_changes",
            json!({"connection": "Lite", "statements": "delete from person"}),
        )
        .await;
    assert!(tried["rolled_back"].is_string());

    assert_eq!(count(&mut client).await, json!([["1"]]));

    let done = client
        .call_ok(
            "execute",
            json!({"connection": "Lite", "statements": "insert into person values (2, 1, 'bo'); insert into person values (3, 1, 'cy')"}),
        )
        .await;
    assert_eq!(done["atomic"], "All statements committed together.");
    assert_eq!(count(&mut client).await, json!([["3"]]));

    // All or nothing: the second statement fails, the first is undone.
    let (error, text) = client
        .call(
            "execute",
            json!({"connection": "Lite", "statements": "insert into person values (9, 1, 'zed'); insert into nowhere values (1)"}),
        )
        .await;
    assert!(error && text.contains("Rolled back"), "{text}");
    assert_eq!(count(&mut client).await, json!([["3"]]));

    // Adding and removing connections: the global switch.
    store
        .lock()
        .unwrap()
        .set_agents_manage_connections(true)
        .unwrap();
    let extra = dir.path().join("extra.db");
    rusqlite::Connection::open(&extra).unwrap();
    let made = client
        .call_ok(
            "create_connection",
            json!({"url": format!("sqlite://{}", extra.display()), "name": "Extra"}),
        )
        .await;
    assert_eq!(made["name"], "Extra");
    let (error, text) = client
        .call(
            "create_connection",
            json!({"url": format!("sqlite://{}", extra.display()), "name": "extra"}),
        )
        .await;
    assert!(error && text.contains("already exists"));
    let (error, text) = client
        .call("create_connection", json!({"url": "hello"}))
        .await;
    assert!(error && text.contains("doesn't look like"));
    let saved = store.lock().unwrap().snapshot().connections;
    let extra_saved = saved.iter().find(|c| c.name == "Extra").unwrap();
    assert!(
        !extra_saved.agents_can_write,
        "writing stays off on new ones"
    );
    client
        .call_ok("delete_connection", json!({"connection": "Extra"}))
        .await;
    assert_eq!(store.lock().unwrap().snapshot().connections.len(), 1);
    let (error, text) = client
        .call("list_tables", json!({"connection": "Extra"}))
        .await;
    assert!(error && text.contains("No connection"));
    client.finish().await;
}

#[tokio::test]
async fn postgres_read_only_and_writes() {
    let Ok(url) = std::env::var("DBLENS_TEST_URL") else {
        return;
    };
    let store = shared();
    let id = save(&store, "pg", &url);
    let mut client = Client::start(StoreSource::Shared(store.clone()));
    let table = "mcp_test_items";
    let (error, text) = client
        .call(
            "execute",
            json!({"connection": "pg", "statements": "select 1"}),
        )
        .await;
    assert!(error && text.contains("Let Agents Write"));
    store
        .lock()
        .unwrap()
        .set_agents_can_write(&id, true)
        .unwrap();
    client
        .call_ok(
            "execute",
            json!({"connection": "pg", "statements": format!("drop table if exists {table}; create table {table}(id int primary key, n text); insert into {table} values (1,'a')")}),
        )
        .await;
    let tried = client
        .call_ok(
            "test_changes",
            json!({"connection": "pg", "statements": format!("delete from {table}")}),
        )
        .await;
    assert!(tried["rolled_back"].is_string());
    let q = |sql: String| json!({"connection": "pg", "query": sql});
    let rows = client
        .call_ok("query", q(format!("select count(*) from {table}")))
        .await;
    assert_eq!(rows["rows"], json!([["1"]]));
    let (error, _) = client
        .call("query", q(format!("delete from {table}")))
        .await;
    assert!(error, "a read-only transaction refuses writes");
    // A cancelled pg_sleep stops on the server; the next query runs at once.
    client
        .send(
            json!({"jsonrpc": "2.0", "id": "sleep", "method": "tools/call",
            "params": {"name": "query", "arguments": q("select pg_sleep(30)".into())}}),
        )
        .await;
    assert_eq!(client.request("ping", json!({})).await["result"], json!({}));
    client
        .send(
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": "sleep"}}),
        )
        .await;
    let started = std::time::Instant::now();
    let rows = client
        .call_ok("query", q(format!("select count(*) from {table}")))
        .await;
    assert_eq!(rows["rows"], json!([["1"]]));
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let described = client
        .call_ok(
            "describe_table",
            json!({"connection": "pg", "table": table}),
        )
        .await;
    assert_eq!(described["primary_key"], json!(["id"]));
    let ext = client
        .call_ok("list_extensions", json!({"connection": "pg"}))
        .await;
    assert_eq!(ext["engine"], "PostgreSQL");
    client
        .call_ok(
            "execute",
            json!({"connection": "pg", "statements": format!("drop table {table}")}),
        )
        .await;
    client.finish().await;
}

/// Never ends on its own; only an interrupt stops it.
const ENDLESS: &str =
    "with recursive c(x) as (select 1 union all select x + 1 from c) select count(*) from c";

/// A slow call doesn't hold up the others; it can be cancelled, and one that
/// runs past the timeout is stopped with an error. The connection keeps working.
#[tokio::test]
async fn slow_calls_can_be_cancelled_and_time_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slow.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("create table person(name text); insert into person values ('ana')")
        .unwrap();
    let store = shared();
    save(&store, "Lite", &format!("sqlite://{}", path.display()));
    let mut client = Client::start_with_timeout(StoreSource::Shared(store), Duration::from_secs(1));
    assert_eq!(count(&mut client).await, json!([["1"]]));

    client
        .send(
            json!({"jsonrpc": "2.0", "id": "slow", "method": "tools/call",
            "params": {"name": "query", "arguments": {"connection": "Lite", "query": ENDLESS}}}),
        )
        .await;
    // Served while the query runs.
    let pong = client.request("ping", json!({})).await;
    assert_eq!(pong["result"], json!({}));
    client
        .send(
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": "slow", "reason": "user"}}),
        )
        .await;
    // The cancelled call never answers: the next reply is this one's.
    assert_eq!(count(&mut client).await, json!([["1"]]));

    let (error, text) = client
        .call("query", json!({"connection": "Lite", "query": ENDLESS}))
        .await;
    assert!(
        error && text.contains("Stopped waiting after 1 s"),
        "{text}"
    );
    assert_eq!(count(&mut client).await, json!([["1"]]));
    client.finish().await;
}

/// A kept session is opened again when the saved URL changes in the app.
#[tokio::test]
async fn edited_urls_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let (first, second) = (dir.path().join("a.db"), dir.path().join("b.db"));
    for (path, rows) in [(&first, 1), (&second, 2)] {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("create table person(name text)").unwrap();
        for _ in 0..rows {
            db.execute_batch("insert into person values ('x')").unwrap();
        }
    }
    let store = shared();
    let id = save(&store, "Lite", &format!("sqlite://{}", first.display()));
    let mut client = Client::start(StoreSource::Shared(store.clone()));
    assert_eq!(count(&mut client).await, json!([["1"]]));
    store
        .lock()
        .unwrap()
        .save(ConnectionDraft {
            id: Some(id),
            name: "Lite".into(),
            url: format!("sqlite://{}", second.display()),
            ssh: None,
            folder_id: None,
        })
        .unwrap();
    assert_eq!(count(&mut client).await, json!([["2"]]));
    client.finish().await;
}

/// The real binary: `dblens --mcp` is headless and reads the same store folder.
#[tokio::test]
async fn binary_serves_stdio_from_the_store_folder() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let db = dir.path().join("bin.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("create table t(a); insert into t values ('x')")
        .unwrap();
    {
        let mut store = Store::open(
            &store_dir,
            KeySource::File(store_dir.join("connections.key")),
        )
        .unwrap();
        store
            .save(ConnectionDraft {
                id: None,
                name: "bin".into(),
                url: format!("sqlite://{}", db.display()),
                ssh: None,
                folder_id: None,
            })
            .unwrap();
    }
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dblens"))
        .arg("--mcp")
        .env("DBLENS_STORE_DIR", &store_dir)
        .env_remove("DBLENS_STORE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "query", "arguments": {"connection": "bin", "query": "select a from t"}}});
    stdin
        .write_all(format!("{call}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["id"], 7);
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(reply["result"]["isError"], false, "{text}");
    let out: Value = serde_json::from_str(text).unwrap();
    assert_eq!(out["rows"], json!([["x"]]));
    drop(stdin);
    assert!(child.wait().await.unwrap().success());
}
