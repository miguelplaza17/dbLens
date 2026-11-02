//! dbLens as an MCP server, so agents (Claude Code, Claude Desktop, Cursor...)
//! can read your databases through the connections saved here:
//!
//! ```text
//! claude mcp add dblens -- /path/to/dblens --mcp
//! ```
//!
//! JSON-RPC 2.0 over stdio, one message per line. Agents pick a connection by
//! name and never see its URL or password; SSH tunnels work as in the app (a
//! dropped tunnel, or a URL edited in the app, opens the session again).
//! Each tools/call runs in its own task with a deadline (`DEFAULT_TIMEOUT`,
//! `DBLENS_MCP_TIMEOUT`) and honours notifications/cancelled, so one slow
//! query never blocks ping or the other calls.
//! Reading is always read-only (the database refuses writes, see
//! `DatabaseSession::read_only`); writing needs "Let Agents Write" on the
//! connection, and adding or removing connections needs the global "Let agents
//! add and remove connections" (both kept in the store, store.rs).
//! Port of `Sources/dbLens/MCPServer.swift`. The Swift app also appends what
//! agents run to the connection's SQL history; that history lives in the
//! webview here, so it is not written.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Notify};

use crate::connector::{self, Connection};
use crate::engine::Engine;
use crate::session::{DatabaseSession, DbError, DbResult, Execution, QueryResult, Relation};
use crate::ssh::SshConfig;
use crate::store::{KeySource, SavedConnection, Store, StoreSnapshot};

const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_ROWS: usize = 1000;
const MAX_VALUE: usize = 2000;
/// How long one tools/call may run before the server stops waiting and
/// cancels its statement. `DBLENS_MCP_TIMEOUT` (seconds) changes it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the server reads the saved connections from.
#[derive(Clone)]
pub enum StoreSource {
    /// A store folder. Opened on first use; every call reloads it under the
    /// folder lock (`Store::transact`), so what the app changes meanwhile
    /// (Let Agents Write, a rename, a new password) is seen, and what the
    /// server saves is never undone by the app's next save.
    Dir(PathBuf),
    /// One store kept by the caller (`DBLENS_STORE=memory`, tests).
    Shared(Arc<Mutex<Store>>),
}

impl StoreSource {
    /// `DBLENS_STORE=memory` is an empty store in memory; otherwise the data
    /// folder (`DBLENS_STORE_DIR` moves it).
    pub fn from_env() -> DbResult<StoreSource> {
        if std::env::var("DBLENS_STORE").as_deref() == Ok("memory") {
            return Ok(StoreSource::Shared(Arc::new(
                Mutex::new(Store::in_memory()),
            )));
        }
        Ok(StoreSource::Dir(Store::default_dir()?))
    }
}

/// The store as the server holds it.
enum Stores {
    /// The folder, and the store once opened (a failed open is tried again).
    Dir(PathBuf, Mutex<Option<Store>>),
    Shared(Arc<Mutex<Store>>),
}

/// `dblens --mcp`: serves on stdin and stdout until stdin closes. Returns the
/// exit code.
pub fn run_stdio() -> i32 {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    else {
        eprintln!("dblens --mcp: can't start the async runtime");
        return 1;
    };
    runtime.block_on(async {
        let source = match StoreSource::from_env() {
            Ok(source) => source,
            Err(error) => {
                eprintln!("dblens --mcp: {error}");
                return 1;
            }
        };
        let input = tokio::io::BufReader::new(tokio::io::stdin());
        match serve(input, tokio::io::stdout(), source).await {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("dblens --mcp: {error}");
                1
            }
        }
    })
}

/// True when the process was started with `--mcp`.
pub fn requested(args: impl IntoIterator<Item = String>) -> bool {
    args.into_iter().skip(1).any(|a| a == "--mcp")
}

/// `DBLENS_MCP_TIMEOUT` in seconds, else `DEFAULT_TIMEOUT`.
fn timeout_from_env() -> Duration {
    std::env::var("DBLENS_MCP_TIMEOUT")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT)
}

/// `serve_with_timeout` with `DBLENS_MCP_TIMEOUT` or `DEFAULT_TIMEOUT`.
pub async fn serve<R, W>(input: R, output: W, source: StoreSource) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    serve_with_timeout(input, output, source, timeout_from_env()).await
}

/// Reads one JSON message per line from `input` and answers on `output`.
/// tools/call runs in its own task, so ping, other calls and
/// notifications/cancelled are served while a slow query runs; a call that
/// runs past `timeout` has its statement cancelled and answers with an error.
/// Ends when `input` does (after the calls still running answer); open
/// sessions and tunnels close.
pub async fn serve_with_timeout<R, W>(
    mut input: R,
    mut output: W,
    source: StoreSource,
    timeout: Duration,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let server = Arc::new(Server {
        stores: Arc::new(match source {
            StoreSource::Dir(dir) => Stores::Dir(dir, Mutex::new(None)),
            StoreSource::Shared(store) => Stores::Shared(store),
        }),
        sessions: Mutex::new(HashMap::new()),
        flights: Mutex::new(HashMap::new()),
        timeout,
    });
    let (send, mut replies) = mpsc::unbounded_channel::<Value>();
    let reader = {
        let server = server.clone();
        async move {
            let mut calls = tokio::task::JoinSet::new();
            let mut line = String::new();
            let result = loop {
                line.clear();
                match input.read_line(&mut line).await {
                    Ok(0) => break Ok(()),
                    Ok(_) => {}
                    Err(error) => break Err(error),
                }
                if line.trim().is_empty() {
                    continue;
                }
                match server.dispatch(line.trim()) {
                    Dispatch::Reply(reply) => {
                        let _ = send.send(reply);
                    }
                    Dispatch::Nothing => {}
                    Dispatch::Call(id, name, arguments) => {
                        // Registered before the task starts, so a cancel
                        // sent right after the call finds it.
                        let flight = server.start_flight(&id);
                        let (server, send) = (server.clone(), send.clone());
                        calls.spawn(async move {
                            if let Some(reply) = server.run_call(id, flight, name, arguments).await
                            {
                                let _ = send.send(reply);
                            }
                        });
                    }
                }
                // Forget the calls that are done.
                while calls.try_join_next().is_some() {}
            };
            // Calls still running answer before the server ends (each is
            // bounded by the timeout).
            while calls.join_next().await.is_some() {}
            drop(send);
            result
        }
    };
    let writer = async move {
        while let Some(reply) = replies.recv().await {
            let mut bytes = serde_json::to_vec(&reply).unwrap_or_default();
            bytes.push(b'\n');
            output.write_all(&bytes).await?;
            output.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let (read, written) = tokio::join!(reader, writer);
    server.close_all().await;
    read.and(written)
}

/// What one incoming line asks for.
enum Dispatch {
    Reply(Value),
    Nothing,
    /// A tools/call: id, tool name, arguments.
    Call(Value, String, Map<String, Value>),
}

/// One running tools/call.
#[derive(Default)]
struct Flight {
    cancelled: AtomicBool,
    /// Wakes the call's waiter when the client cancels it.
    stop: Notify,
    /// The connection it runs on, once opened, to cancel its statement.
    conn: Mutex<Option<Arc<Connection>>>,
}

impl Flight {
    /// Records the connection the call runs on; fails when already cancelled.
    fn uses(&self, conn: &Arc<Connection>) -> DbResult<()> {
        *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.clone());
        // Cancelled before it started: nothing of its own runs yet, so
        // nothing is interrupted (that could stop another call's statement).
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(DbError::msg("Cancelled."));
        }
        Ok(())
    }

    /// Stops the statement running for this call, if any. query, execute,
    /// test_changes and install_extension run on the pinned connection; the
    /// browsing ones (list_tables, describe_*) are left to finish.
    fn interrupt(&self) {
        if let Some(conn) = self.conn.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            conn.session.cancel_sql();
        }
    }

    /// notifications/cancelled: stop the statement; the call gets no answer.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.interrupt();
        self.stop.notify_one();
    }
}

/// A session kept for the next calls, with what it was opened with.
struct Cached {
    conn: Arc<Connection>,
    fingerprint: u64,
}

type Slot = Arc<tokio::sync::Mutex<Option<Cached>>>;

struct Server {
    stores: Arc<Stores>,
    /// One slot per connection id; its lock is held while connecting, so two
    /// calls never open the same connection twice, and others don't wait.
    sessions: Mutex<HashMap<String, Slot>>,
    /// Running tools/call by request id (as JSON text), for cancelling.
    flights: Mutex<HashMap<String, Arc<Flight>>>,
    timeout: Duration,
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn text_of(arguments: &Map<String, Value>, key: &str) -> DbResult<String> {
    match arguments.get(key).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => Ok(value.to_string()),
        _ => Err(DbError::msg(format!("Missing \"{key}\"."))),
    }
}

/// The saved connection named (any case) or with that id.
fn pick(snapshot: &StoreSnapshot, name: &str) -> DbResult<SavedConnection> {
    let matches: Vec<&SavedConnection> = snapshot
        .connections
        .iter()
        .filter(|c| c.name.to_lowercase() == name.to_lowercase() || c.id == name)
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            let names: Vec<&str> = snapshot
                .connections
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            Err(DbError::msg(format!(
                "No connection \"{name}\". Saved: {}.",
                names.join(", ")
            )))
        }
        _ => Err(DbError::msg(format!(
            "Several connections are named \"{name}\"; rename one in dbLens."
        ))),
    }
}

/// What a session was opened with: a cached one whose saved URL or SSH
/// settings changed since is opened again.
fn fingerprint(url: &str, ssh: Option<&SshConfig>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    ssh.hash(&mut hasher);
    hasher.finish()
}

fn call_result(result: DbResult<Value>) -> Value {
    match result {
        Ok(output) => {
            let text = match output {
                Value::String(text) => text,
                other => other.to_string(),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": false})
        }
        Err(error) => {
            json!({"content": [{"type": "text", "text": error.to_string()}], "isError": true})
        }
    }
}

impl Server {
    /// Runs `body` on the store, reloaded under the folder lock (blocking: the
    /// keyring may talk to D-Bus, and the lock may wait for the app).
    async fn with<T: Send + 'static>(
        &self,
        body: impl FnOnce(&mut Store) -> DbResult<T> + Send + 'static,
    ) -> DbResult<T> {
        let stores = self.stores.clone();
        tokio::task::spawn_blocking(move || match &*stores {
            Stores::Shared(store) => store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .transact(body),
            Stores::Dir(dir, opened) => {
                let mut guard = opened.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_none() {
                    *guard = Some(Store::open(dir, KeySource::Auto)?);
                }
                guard.as_mut().expect("store opened").transact(body)
            }
        })
        .await
        .map_err(|e| DbError::msg(e.to_string()))?
    }

    fn slot(&self, id: &str) -> Slot {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    async fn close_all(&self) {
        let slots: Vec<Slot> = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, slot)| slot)
            .collect();
        for slot in slots {
            if let Some(cached) = slot.lock().await.take() {
                cached.conn.session.close().await;
            }
        }
    }

    /// Answers what needs no database right away; tools/call goes to a task.
    fn dispatch(&self, line: &str) -> Dispatch {
        let Ok(Value::Object(message)) = serde_json::from_str::<Value>(line) else {
            return Dispatch::Reply(error_reply(Value::Null, -32700, "Parse error"));
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // Notifications (no id) need no answer.
        let Some(id) = message.get("id").cloned() else {
            if method == "notifications/cancelled" {
                if let Some(request) = params.get("requestId") {
                    let flight = self
                        .flights
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&request.to_string())
                        .cloned();
                    if let Some(flight) = flight {
                        flight.cancel();
                    }
                }
            }
            return Dispatch::Nothing;
        };
        let result = match method {
            "initialize" => json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "dblens", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Databases saved in dbLens. Start with list_connections, then describe_schema \
                    (or list_tables and describe_table on big databases) before writing queries. query is always \
                    read-only. execute changes data only on connections where the user turned on \"Let Agents \
                    Write\"; try changes and migrations with test_changes first, and tell the user what they will change.",
            }),
            "ping" => json!({}),
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let arguments = params
                    .get("arguments")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                return Dispatch::Call(id, name, arguments);
            }
            other => {
                return Dispatch::Reply(error_reply(
                    id,
                    -32601,
                    &format!("Method not found: {other}"),
                ))
            }
        };
        Dispatch::Reply(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn start_flight(&self, id: &Value) -> Arc<Flight> {
        let flight = Arc::new(Flight::default());
        self.flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), flight.clone());
        flight
    }

    /// One tools/call, bounded by the timeout. None when the client cancelled
    /// it (a cancelled request gets no answer).
    async fn run_call(
        self: Arc<Self>,
        id: Value,
        flight: Arc<Flight>,
        name: String,
        arguments: Map<String, Value>,
    ) -> Option<Value> {
        let key = id.to_string();
        // Its own task: a call that outlives the timeout or a cancel still
        // runs to its end (a test_changes still rolls back) instead of being
        // dropped halfway.
        let mut work = {
            let (server, flight) = (self.clone(), flight.clone());
            tokio::spawn(async move { server.call(&name, &arguments, &flight).await })
        };
        let mut finished = false;
        let outcome = tokio::select! {
            biased;
            done = &mut work => {
                finished = true;
                Some(done.unwrap_or_else(|e| Err(DbError::msg(e.to_string()))))
            }
            _ = flight.stop.notified() => None,
            _ = tokio::time::sleep(self.timeout) => Some(Err(DbError::msg(format!(
                "Stopped waiting after {} s and cancelled the running statement. \
                 Try a smaller query (a limit, a filter, an index).",
                self.timeout.as_secs()
            )))),
        };
        if !finished {
            // Interrupt until the call ends: a statement that had not started
            // yet when the first interrupt came (SQLite ignores one then) is
            // stopped too. Stops checking after a few seconds.
            let flight = flight.clone();
            tokio::spawn(async move {
                for _ in 0..30 {
                    flight.interrupt();
                    tokio::select! {
                        biased;
                        _ = &mut work => return,
                        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                    }
                }
            });
        }
        self.flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        if flight.cancelled.load(Ordering::SeqCst) {
            return None;
        }
        let result = call_result(outcome?);
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    async fn call(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
        flight: &Flight,
    ) -> DbResult<Value> {
        match name {
            "list_connections" => {
                let snapshot = self.with(|s| Ok(s.snapshot())).await?;
                Ok(Value::Array(
                    snapshot
                        .connections
                        .iter()
                        .map(|c| {
                            json!({
                                "name": c.name, "engine": c.engine.name(),
                                "via_ssh": c.ssh.is_some(), "agents_can_write": c.agents_can_write,
                            })
                        })
                        .collect(),
                ))
            }
            "create_connection" => self.create_connection(arguments).await,
            "delete_connection" => self.delete_connection(arguments).await,
            "list_tables" => {
                let (_, conn) = self.open(arguments, flight).await?;
                let relations = conn.session.relations().await?;
                Ok(Value::Array(relations.iter().map(relation_json).collect()))
            }
            "describe_schema" => {
                let (_, conn) = self.open(arguments, flight).await?;
                let schema = arguments.get("schema").and_then(Value::as_str);
                Ok(Value::String(
                    schema_text(conn.session.as_ref(), schema).await?,
                ))
            }
            "list_extensions" => {
                let (_, conn) = self.open(arguments, flight).await?;
                extensions(conn.session.as_ref()).await
            }
            "install_extension" => {
                let (saved, conn) = self.open(arguments, flight).await?;
                let extension = text_of(arguments, "name")?;
                let schema = arguments.get("schema").and_then(Value::as_str);
                install(&saved, conn.session.as_ref(), &extension, schema).await
            }
            "describe_table" => {
                let (_, conn) = self.open(arguments, flight).await?;
                describe(conn.session.as_ref(), &text_of(arguments, "table")?).await
            }
            "query" => {
                let (_, conn) = self.open(arguments, flight).await?;
                let statement = text_of(arguments, "query")?;
                let limit = arguments
                    .get("max_rows")
                    .and_then(Value::as_i64)
                    .unwrap_or(100)
                    .clamp(1, MAX_ROWS as i64) as usize;
                let execution = conn.session.read_only(&statement).await?;
                Ok(output(&execution, limit))
            }
            "test_changes" => {
                let (saved, conn) = self.open(arguments, flight).await?;
                let script = text_of(arguments, "statements")?;
                require_write(
                    &saved,
                    "Testing runs the statements (and locks what they touch) before undoing them",
                )?;
                match conn.session.rehearse(&script).await {
                    Ok(execution) => {
                        let mut out = output(&execution, 20);
                        out["rolled_back"] =
                            json!("Every statement ran and was undone: nothing changed.");
                        Ok(out)
                    }
                    Err(error) => Err(DbError::msg(format!(
                        "{error} (Rolled back: nothing changed.)"
                    ))),
                }
            }
            "execute" => {
                let (saved, conn) = self.open(arguments, flight).await?;
                let script = text_of(arguments, "statements")?;
                require_write(&saved, "Writing")?;
                let atomic = arguments.get("atomic").and_then(Value::as_bool);
                execute(conn.session.as_ref(), &script, atomic).await
            }
            other => Err(DbError::msg(format!("Unknown tool: {other}"))),
        }
    }

    /// The named connection, opened once and kept for the next calls. A kept
    /// session is opened again when its SSH tunnel dropped (sleep, a network
    /// change) or when its saved URL or SSH settings changed in the app.
    async fn open(
        &self,
        arguments: &Map<String, Value>,
        flight: &Flight,
    ) -> DbResult<(SavedConnection, Arc<Connection>)> {
        let name = text_of(arguments, "connection")?;
        let (saved, url) = self
            .with(move |s| {
                let saved = pick(&s.snapshot(), &name)?;
                let url = s.url(&saved.id).map(str::to_string);
                Ok((saved, url))
            })
            .await?;
        let url = url.ok_or_else(|| {
            DbError::msg(format!(
                "dbLens can't read the URL of \"{}\". Open dbLens and edit the connection.",
                saved.name
            ))
        })?;
        let print = fingerprint(&url, saved.ssh.as_ref());
        let slot = self.slot(&saved.id);
        let mut cached = slot.lock().await;
        if let Some(kept) = cached.as_ref() {
            let dropped = kept
                .conn
                .tunnel
                .as_ref()
                .and_then(|t| t.drop_reason())
                .is_some();
            if kept.fingerprint == print && !dropped {
                let conn = kept.conn.clone();
                drop(cached);
                flight.uses(&conn)?;
                return Ok((saved, conn));
            }
        }
        if let Some(stale) = cached.take() {
            tokio::spawn(async move { stale.conn.session.close().await });
        }
        let conn = Arc::new(connector::connect(&url, saved.ssh.as_ref()).await?);
        *cached = Some(Cached {
            conn: conn.clone(),
            fingerprint: print,
        });
        drop(cached);
        flight.uses(&conn)?;
        Ok((saved, conn))
    }

    async fn require_manage(&self) -> DbResult<()> {
        let allowed = self
            .with(|s| Ok(s.snapshot().agents_manage_connections))
            .await?;
        if allowed {
            Ok(())
        } else {
            Err(DbError::msg(
                "Adding and removing connections is off. The user can turn it on in dbLens: \
                 Settings > Agents > Let agents add and remove connections.",
            ))
        }
    }

    /// Connects first (so a wrong URL fails here, with its cause), then saves
    /// the URL encrypted, like the app's New Connection. Writing stays off.
    async fn create_connection(&self, arguments: &Map<String, Value>) -> DbResult<Value> {
        self.require_manage().await?;
        let url = text_of(arguments, "url")?;
        let Some(engine) = Engine::detect(&url) else {
            return Err(DbError::msg(
                "This doesn't look like a database URL (postgres://, mysql://, sqlite:///path, redis://, mongodb://).",
            ));
        };
        let ssh = match arguments.get("ssh_host").and_then(Value::as_str) {
            Some(host) if !host.trim().is_empty() => Some(SshConfig {
                host: host.trim().to_string(),
                port: arguments
                    .get("ssh_port")
                    .and_then(Value::as_u64)
                    .map(|p| p as u16),
                user: arguments
                    .get("ssh_user")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                key_path: arguments
                    .get("ssh_key")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }),
            _ => None,
        };
        let connection = connector::connect(&url, ssh.as_ref()).await?;
        let given = arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let name = if given.is_empty() {
            connector::summary(&url)
                .map(|s| s.suggested_name)
                .unwrap_or_else(|| engine.name().to_string())
        } else {
            given
        };
        let version = connection.session.server_version();
        let print = fingerprint(&url, ssh.as_ref());
        let (check, draft_url, draft_ssh) = (name.clone(), url, ssh);
        let saved = self
            .with(move |s| {
                let taken = s
                    .snapshot()
                    .connections
                    .iter()
                    .any(|c| c.name.to_lowercase() == check.to_lowercase());
                if taken {
                    return Err(DbError::msg(format!(
                        "A connection named \"{check}\" already exists; pass another name."
                    )));
                }
                s.save(crate::store::ConnectionDraft {
                    id: None,
                    name: check,
                    url: draft_url,
                    ssh: draft_ssh,
                    folder_id: None,
                })
            })
            .await;
        match saved {
            Ok(saved) => {
                *self.slot(&saved.id).lock().await = Some(Cached {
                    conn: Arc::new(connection),
                    fingerprint: print,
                });
                Ok(json!({
                    "name": name, "engine": engine.name(), "server_version": version,
                    "note": "Saved in dbLens, URL encrypted. Agents can read it now; writing stays off until the user turns it on.",
                }))
            }
            Err(error) => {
                connection.session.close().await;
                Err(error)
            }
        }
    }

    async fn delete_connection(&self, arguments: &Map<String, Value>) -> DbResult<Value> {
        self.require_manage().await?;
        let name = text_of(arguments, "connection")?;
        let lookup = name.clone();
        let saved = self.with(move |s| pick(&s.snapshot(), &lookup)).await?;
        let slot = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&saved.id);
        if let Some(slot) = slot {
            if let Some(cached) = slot.lock().await.take() {
                cached.conn.session.close().await;
            }
        }
        let id = saved.id.clone();
        self.with(move |s| s.delete(&id)).await?;
        Ok(json!({
            "deleted": name,
            "note": "Removed from dbLens with its saved URL. The database itself is untouched.",
        }))
    }
}

fn require_write(saved: &SavedConnection, what: &str) -> DbResult<()> {
    if saved.agents_can_write {
        Ok(())
    } else {
        Err(DbError::msg(format!(
            "{what} is off for \"{}\". The user can turn it on in dbLens: right-click the \
             connection and choose Let Agents Write. Reading with query works.",
            saved.name
        )))
    }
}

fn non_atomic_note(engine: Engine) -> &'static str {
    match engine {
        Engine::Mysql => "MySQL commits DDL as it runs: statements before a failing one stay applied. Check with describe_schema.",
        Engine::Mongodb => "MongoDB ran each statement on its own: the ones before a failure stay applied.",
        Engine::Redis => "Redis ran each command on its own: the ones before a failure stay applied.",
        _ => "Statements before a failure stay applied.",
    }
}

async fn execute(
    session: &dyn DatabaseSession,
    script: &str,
    atomic: Option<bool>,
) -> DbResult<Value> {
    let plain = |error: DbError| {
        let message = error.to_string();
        if atomic != Some(false) && matches!(session.engine(), Engine::Postgres | Engine::Sqlite) {
            DbError::msg(format!("{message} (Rolled back: nothing changed.)"))
        } else {
            DbError::msg(format!("{message} {}", non_atomic_note(session.engine())))
        }
    };
    if atomic == Some(false) {
        let mut out = output(&session.execute(script).await.map_err(plain)?, 100);
        out["atomic"] = json!(false);
        return Ok(out);
    }
    match session.execute_atomically(script).await {
        Ok(execution) => {
            let mut out = output(&execution, 100);
            out["atomic"] = json!("All statements committed together.");
            Ok(out)
        }
        Err(DbError::NotAtomic(reason)) => {
            // Asked for explicitly: say why not. Otherwise run plainly and say what that means.
            if atomic == Some(true) {
                return Err(DbError::msg(format!(
                    "{reason} Nothing ran; pass atomic: false to run it anyway."
                )));
            }
            let mut out = output(&session.execute(script).await.map_err(plain)?, 100);
            out["atomic"] = json!(false);
            out["note"] = json!(format!("{reason} {}", non_atomic_note(session.engine())));
            Ok(out)
        }
        Err(error) => Err(plain(error)),
    }
}

fn relation_json(relation: &Relation) -> Value {
    let mut item = json!({"name": relation.name, "kind": relation.kind});
    if !relation.schema.is_empty() {
        item["schema"] = json!(relation.schema);
    }
    if relation.estimate >= 0.0 {
        item["estimated_rows"] = json!(relation.estimate as i64);
    }
    if !relation.subtype.is_empty() {
        item["type"] = json!(relation.subtype);
    }
    item
}

fn find_relation<'a>(name: &str, relations: &'a [Relation]) -> Option<&'a Relation> {
    relations
        .iter()
        .find(|r| r.id() == name || (r.schema.is_empty() && r.name == name))
        .or_else(|| {
            let named: Vec<&Relation> = relations.iter().filter(|r| r.name == name).collect();
            (named.len() == 1).then(|| named[0])
        })
}

/// Columns, primary key, and foreign keys both ways; for Redis and Mongo
/// collections without a schema, the first rows' shape.
async fn describe(session: &dyn DatabaseSession, name: &str) -> DbResult<Value> {
    let relations = session.relations().await?;
    let Some(relation) = find_relation(name, &relations) else {
        return Err(DbError::msg(format!(
            "No table \"{name}\". list_tables shows what there is."
        )));
    };
    let graph = session.schema_graph().await?;
    if let Some(table) = graph
        .tables
        .iter()
        .find(|t| t.schema == relation.schema && t.name == relation.name)
    {
        let id = table.id();
        let outgoing: Vec<Value> = graph
            .links
            .iter()
            .filter(|l| l.from == id)
            .map(|l| json!({"columns": l.from_columns, "references": l.to, "referenced_columns": l.to_columns}))
            .collect();
        let incoming: Vec<Value> = graph
            .links
            .iter()
            .filter(|l| l.to == id && l.from != id)
            .map(|l| json!({"table": l.from, "columns": l.from_columns, "referenced_columns": l.to_columns}))
            .collect();
        return Ok(json!({
            "table": id, "kind": relation.kind,
            "columns": table.columns.iter().map(|c| json!({"name": c.name, "type": c.type_name, "nullable": c.is_nullable})).collect::<Vec<_>>(),
            "primary_key": table.columns.iter().filter(|c| c.is_primary_key).map(|c| c.name.clone()).collect::<Vec<_>>(),
            "foreign_keys": outgoing, "referenced_by": incoming,
        }));
    }
    let sample = session.page(relation, 5, 0, "", None).await?;
    Ok(json!({
        "table": relation.id(), "kind": relation.kind, "type": relation.subtype,
        "columns": sample.columns.iter().map(|c| json!({"name": c.name, "type": c.type_name})).collect::<Vec<_>>(),
        "sample": rows_json(&sample, 5),
    }))
}

/// Every table at once, compact enough to read in one go: columns with type,
/// PK and NOT NULL, then foreign keys.
async fn schema_text(session: &dyn DatabaseSession, schema: Option<&str>) -> DbResult<String> {
    let graph = session.schema_graph().await?;
    if graph.tables.is_empty() {
        return Err(DbError::msg(format!(
            "{} has no tables and foreign keys to describe; use list_tables.",
            session.engine().name()
        )));
    }
    let estimates: HashMap<String, f64> = session
        .relations()
        .await
        .map(|all| all.iter().map(|r| (r.id(), r.estimate)).collect())
        .unwrap_or_default();
    let wanted = schema.filter(|s| !s.is_empty());
    let tables: Vec<_> = graph
        .tables
        .iter()
        .filter(|t| wanted.map_or(true, |s| t.schema == s))
        .collect();
    if tables.is_empty() {
        return Err(DbError::msg(format!(
            "No tables in schema \"{}\". Schemas: {}.",
            wanted.unwrap_or(""),
            graph.schemas().join(", ")
        )));
    }
    let mut lines: Vec<String> = Vec::new();
    for table in tables {
        let id = table.id();
        let mut head = format!("{id} ({}", if table.is_view { "view" } else { "table" });
        if let Some(rows) = estimates.get(&id).filter(|r| **r >= 0.0) {
            head += &format!(", ~{} rows", *rows as i64);
        }
        lines.push(head + ")");
        for c in &table.columns {
            let pk = if c.is_primary_key { " PK" } else { "" };
            let not_null = if c.is_nullable || c.is_primary_key {
                ""
            } else {
                " NOT NULL"
            };
            lines.push(format!("  {} {}{pk}{not_null}", c.name, c.type_name));
        }
        for link in graph.links.iter().filter(|l| l.from == id) {
            lines.push(format!(
                "  FK ({}) -> {}({})",
                link.from_columns.join(", "),
                link.to,
                link.to_columns.join(", ")
            ));
        }
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

/// What the database adds to plain SQL, so queries only use functions that
/// exist, always with the server version.
async fn extensions(session: &dyn DatabaseSession) -> DbResult<Value> {
    async fn rows(session: &dyn DatabaseSession, sql: &str) -> DbResult<Vec<Vec<Option<String>>>> {
        Ok(session
            .read_only(sql)
            .await?
            .main()
            .map(|m| m.rows.clone())
            .unwrap_or_default())
    }
    let cell = |row: &Vec<Option<String>>, i: usize| row.get(i).cloned().flatten();
    let mut out = json!({"engine": session.engine().name(), "version": session.server_version()});
    match session.engine() {
        Engine::Postgres => {
            let all = rows(
                session,
                "select a.name, a.installed_version, a.default_version, n.nspname, a.comment \
                 from pg_available_extensions a \
                 left join pg_extension e on e.extname = a.name \
                 left join pg_namespace n on n.oid = e.extnamespace \
                 order by a.name",
            )
            .await?;
            let s = |row: &Vec<Option<String>>, i| cell(row, i).unwrap_or_default();
            out["installed"] = Value::Array(
                all.iter()
                    .filter(|r| cell(r, 1).is_some())
                    .map(|r| json!({"name": s(r, 0), "version": s(r, 1), "schema": s(r, 3), "comment": s(r, 4)}))
                    .collect(),
            );
            out["available"] = Value::Array(
                all.iter()
                    .filter(|r| cell(r, 1).is_none())
                    .map(|r| json!({"name": s(r, 0), "version": s(r, 2), "comment": s(r, 4)}))
                    .collect(),
            );
            out["note"] = json!("install_extension adds an available one (needs Let Agents Write and the right privileges).");
        }
        Engine::Mysql => {
            let plugins = rows(
                session,
                "select PLUGIN_NAME, PLUGIN_TYPE from information_schema.PLUGINS \
                 where PLUGIN_STATUS = 'ACTIVE' order by PLUGIN_TYPE, PLUGIN_NAME",
            )
            .await?;
            out["plugins"] = Value::Array(
                plugins
                    .iter()
                    .map(|r| json!({"name": cell(r, 0).unwrap_or_default(), "type": cell(r, 1).unwrap_or_default()}))
                    .collect(),
            );
        }
        Engine::Sqlite => {
            let options = rows(
                session,
                "select compile_options from pragma_compile_options",
            )
            .await?;
            out["compile_options"] = Value::Array(
                options
                    .iter()
                    .filter_map(|r| cell(r, 0))
                    .map(Value::String)
                    .collect(),
            );
        }
        Engine::Redis => {
            let modules = session
                .execute("MODULE LIST")
                .await
                .ok()
                .and_then(|e| e.main().map(|m| m.rows.clone()))
                .unwrap_or_default();
            out["modules"] = json!(modules);
        }
        Engine::Mongodb => {
            out["note"] =
                json!("MongoDB has no extensions; features depend on the server version.");
        }
    }
    Ok(out)
}

/// CREATE EXTENSION, for Postgres, where agents may write. The name must be one
/// pg_available_extensions lists.
async fn install(
    saved: &SavedConnection,
    session: &dyn DatabaseSession,
    name: &str,
    schema: Option<&str>,
) -> DbResult<Value> {
    if session.engine() != Engine::Postgres {
        return Err(DbError::msg(format!(
            "Only PostgreSQL has extensions to install; {} doesn't.",
            session.engine().name()
        )));
    }
    if !saved.agents_can_write {
        return Err(DbError::msg(format!(
            "Writing is off for \"{}\", and installing an extension is a change. The user can \
             turn it on in dbLens: right-click the connection and choose Let Agents Write.",
            saved.name
        )));
    }
    let available = session
        .read_only("select name, installed_version from pg_available_extensions")
        .await?;
    let rows = available.main().map(|m| m.rows.clone()).unwrap_or_default();
    let Some(row) = rows
        .iter()
        .find(|r| r.first().cloned().flatten().as_deref() == Some(name))
    else {
        return Err(DbError::msg(format!(
            "\"{name}\" isn't available on this server; list_extensions shows what is."
        )));
    };
    if let Some(version) = row.get(1).cloned().flatten() {
        return Ok(json!({"name": name, "version": version, "note": "Already installed."}));
    }
    let mut statement = format!("create extension if not exists {}", session.quote(name));
    if let Some(schema) = schema.filter(|s| !s.is_empty()) {
        statement += &format!(" schema {}", session.quote(schema));
    }
    session.execute(&statement).await?;
    let literal = format!("'{}'", name.replace('\'', "''"));
    let version = session
        .read_only(&format!(
            "select extversion from pg_extension where extname = {literal}"
        ))
        .await?
        .main()
        .and_then(|m| m.rows.first().and_then(|r| r.first().cloned().flatten()));
    Ok(json!({"name": name, "version": version.unwrap_or_default(), "statement": statement}))
}

fn rows_json(result: &QueryResult, max_rows: usize) -> Vec<Vec<Value>> {
    result
        .rows
        .iter()
        .take(max_rows)
        .map(|row| {
            row.iter()
                .map(|value| match value {
                    None => Value::Null,
                    Some(text) if text.chars().count() > MAX_VALUE => {
                        Value::String(text.chars().take(MAX_VALUE).collect::<String>() + "…")
                    }
                    Some(text) => Value::String(text.clone()),
                })
                .collect()
        })
        .collect()
}

/// The main result (the last with rows), and every statement's tag.
fn output(execution: &Execution, max_rows: usize) -> Value {
    let mut out = json!({
        "statements": execution.results.iter().map(|r| r.command.clone()).collect::<Vec<_>>(),
        "duration_ms": execution.duration_ms as i64,
    });
    if let Some(main) = execution.main().filter(|m| !m.columns.is_empty()) {
        out["columns"] = Value::Array(
            main.columns
                .iter()
                .map(|c| {
                    Value::String(if c.type_name.is_empty() {
                        c.name.clone()
                    } else {
                        format!("{} ({})", c.name, c.type_name)
                    })
                })
                .collect(),
        );
        out["rows"] = json!(rows_json(main, max_rows));
        out["row_count"] = json!(main.total_rows);
        if main.total_rows as usize > max_rows || main.truncated {
            out["truncated"] = json!(format!(
                "Showing {} of {} rows; add a LIMIT or raise max_rows (up to {MAX_ROWS}).",
                max_rows.min(main.rows.len()),
                main.total_rows
            ));
        }
    }
    out
}

// MARK: Tool list

fn tools() -> Value {
    let connection =
        json!({"type": "string", "description": "Connection name, from list_connections."});
    let read = json!({"readOnlyHint": true});
    json!([
        {
            "name": "list_connections",
            "description": "The database connections saved in dbLens: name, engine (PostgreSQL, MySQL, SQLite, Redis, MongoDB), whether it goes through SSH, and whether agents may write. URLs and passwords are never shown. Databases: all (PostgreSQL, MySQL/MariaDB, SQLite, Redis/Valkey, MongoDB).",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": read,
        },
        {
            "name": "create_connection",
            "description": "Saves a new connection in dbLens from its URL, after connecting to check it (a failure says why: refused, unknown host, wrong password...). The URL is stored encrypted; afterwards only the name is used. Optionally through SSH. Writing stays off on it until the user allows it. Needs the user's Settings > Agents > Let agents add and remove connections. Databases: all.",
            "inputSchema": {"type": "object", "properties": {
                "url": {"type": "string", "description": "Full connection URL: postgres://, mysql://, mariadb://, sqlite:///path, redis://, rediss://, mongodb://, mongodb+srv://."},
                "name": {"type": "string", "description": "Name to show. Default: from the URL (user @ host)."},
                "ssh_host": {"type": "string", "description": "Connect through this SSH host (a ~/.ssh/config alias works)."},
                "ssh_user": {"type": "string"},
                "ssh_port": {"type": "integer"},
                "ssh_key": {"type": "string", "description": "Path to a private key; default: ssh-agent and ~/.ssh/config."},
            }, "required": ["url"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false},
        },
        {
            "name": "delete_connection",
            "description": "Removes a saved connection from dbLens, with its encrypted URL. The database itself is not touched. Confirm with the user first. Needs the user's Settings > Agents > Let agents add and remove connections. Databases: all.",
            "inputSchema": {"type": "object", "properties": {"connection": connection}, "required": ["connection"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": true},
        },
        {
            "name": "list_tables",
            "description": "Tables and views of a connection (Redis: keys; MongoDB: collections), with their schema and estimated row count. Databases: all.",
            "inputSchema": {"type": "object", "properties": {"connection": connection}, "required": ["connection"]},
            "annotations": read,
        },
        {
            "name": "describe_schema",
            "description": "The whole relational structure in one call: every table and view with its columns (type, PK, NOT NULL), estimated rows, and foreign keys. Start here to understand a database. Databases: PostgreSQL, MySQL/MariaDB, SQLite, MongoDB (fields sampled, references guessed). Not Redis: use list_tables.",
            "inputSchema": {"type": "object", "properties": {
                "connection": connection,
                "schema": {"type": "string", "description": "Only this schema (PostgreSQL). Default: all."},
            }, "required": ["connection"]},
            "annotations": read,
        },
        {
            "name": "list_extensions",
            "description": "What the database adds to plain SQL, to write queries only with functions that exist: PostgreSQL extensions (installed with version and schema, and available to install), MySQL active plugins, SQLite compile options (JSON1, FTS5...), Redis modules. Always with the server version. Databases: all; MongoDB only reports its version.",
            "inputSchema": {"type": "object", "properties": {"connection": connection}, "required": ["connection"]},
            "annotations": read,
        },
        {
            "name": "install_extension",
            "description": "Installs an available PostgreSQL extension (CREATE EXTENSION). Only where the user turned on Let Agents Write, and the database user needs the privilege. Confirm with the user first. Databases: PostgreSQL only.",
            "inputSchema": {"type": "object", "properties": {
                "connection": connection,
                "name": {"type": "string", "description": "Extension name, from list_extensions' available list."},
                "schema": {"type": "string", "description": "Schema to put it in. Default: the server's choice."},
            }, "required": ["connection", "name"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false},
        },
        {
            "name": "describe_table",
            "description": "A table's columns with types and nullability, its primary key, its foreign keys, and the tables that reference it. Use before writing queries. Databases: all. Redis and MongoDB: the fields of the first entries, with types.",
            "inputSchema": {"type": "object", "properties": {
                "connection": connection,
                "table": {"type": "string", "description": "Table name, or schema.name when several schemas have it."},
            }, "required": ["connection", "table"]},
            "annotations": read,
        },
        {
            "name": "query",
            "description": "Runs one read-only statement and returns its rows. SQL for PostgreSQL and MySQL (inside a read-only transaction: writes fail) and SQLite (only statements SQLite itself says change nothing); mongosh syntax for MongoDB (db.users.find({...}), aggregate without $out/$merge, countDocuments, distinct); one reading command for Redis (GET, HGETALL, LRANGE, SCAN...). Databases: all.",
            "inputSchema": {"type": "object", "properties": {
                "connection": connection,
                "query": {"type": "string", "description": "One statement."},
                "max_rows": {"type": "integer", "description": "Rows to return, 1 to 1000. Default 100."},
            }, "required": ["connection", "query"]},
            "annotations": read,
        },
        {
            "name": "test_changes",
            "description": "Tries a change or a whole migration without keeping it: runs every statement inside a transaction that is always rolled back, and reports what each did (rows affected) or which one failed and why. PostgreSQL and SQLite: data and schema (DDL). MySQL: data changes only, since its DDL commits by itself. MongoDB and Redis can't undo, so they refuse. Use before execute. Leave out BEGIN/COMMIT: dbLens owns the transaction. Needs Let Agents Write, since the statements really run (and lock) before the rollback. Databases: PostgreSQL and SQLite (everything), MySQL/MariaDB (data changes only). Not MongoDB or Redis.",
            "inputSchema": {"type": "object", "properties": {
                "connection": connection,
                "statements": {"type": "string", "description": "One statement or a script, separated by semicolons."},
            }, "required": ["connection", "statements"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true},
        },
        {
            "name": "execute",
            "description": "Changes data or schema, for real: INSERT, UPDATE, DELETE, migrations (DDL), MongoDB writes, Redis commands. Only on connections where the user turned on Let Agents Write in dbLens. Scripts run all or nothing where the database allows (PostgreSQL, SQLite, MySQL without DDL); otherwise the result says so. Run test_changes first and confirm with the user. If the project has a migration tool (Prisma, Rails, Flyway...), use the tool instead, so it records the migration. Databases: all. All or nothing on PostgreSQL, SQLite, and MySQL/MariaDB without DDL.",
            "inputSchema": {"type": "object", "properties": {
                "connection": connection,
                "statements": {"type": "string", "description": "One statement or a script, separated by semicolons. Leave out BEGIN/COMMIT."},
                "atomic": {"type": "boolean", "description": "true: all or nothing, or refuse when the database can't. false: plain, each statement on its own. Default: all or nothing where possible."},
            }, "required": ["connection", "statements"]},
            "annotations": {"readOnlyHint": false, "destructiveHint": true},
        },
    ])
}
