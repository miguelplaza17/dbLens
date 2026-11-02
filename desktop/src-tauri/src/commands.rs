//! The Tauri commands the frontend calls (`src/api.ts` wraps each one).
//! Open sessions live in `AppState`, keyed by a session id the frontend keeps.
//! Saved URLs never leave the backend: the frontend connects by saved id and
//! edits a masked URL.
//!
//! JS calls use camelCase argument names (`sessionId`); Tauri maps them to the
//! snake_case parameters here. Every error reaches JS as a string.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody, Response};
use tauri::State;

use crate::connector::{self, Connection, Summary};
use crate::engine::Engine;
use crate::export;
use crate::session::{
    CellValue, ColumnSort, DatabaseSession, DbError, DbResult, EditOutcome, QueryResult, Relation,
    RowKey, SchemaGraph,
};
use crate::ssh::SshConfig;
use crate::store::{
    ConnectionDraft, ConnectionFolder, KeySource, SavedConnection, Store, StoreSnapshot,
};
use crate::wire::{self, StreamSink};

/// One open connection.
struct Open {
    connection: Connection,
    /// The label of the webview that opened it; its sessions close when that
    /// page reloads or its window goes away, since the ids die with the page.
    owner: String,
    /// Set once the session is taken out to close: a stream still running on
    /// it stops (see `execute_stream`'s liveness check).
    closing: std::sync::atomic::AtomicBool,
}

impl Open {
    /// Marks the session closing and stops what runs; quick, no waiting.
    fn stop(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.connection.session.cancel_all();
    }

    /// Stops what runs, closes the session (a few seconds at most), then the tunnel.
    async fn close(&self) {
        self.stop();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            self.connection.session.close(),
        )
        .await;
        if let Some(tunnel) = &self.connection.tunnel {
            tunnel.close();
        }
    }
}

#[derive(Default)]
pub struct AppState {
    sessions: Mutex<HashMap<String, Arc<Open>>>,
    /// Opened on first use, off the async runtime (the keyring may block).
    store: tokio::sync::Mutex<Option<Store>>,
}

impl AppState {
    /// The open session. One whose SSH tunnel has dropped is taken out of the
    /// map and closed, and the error says why; later calls read "closed".
    fn session(&self, id: &str) -> DbResult<Arc<Open>> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DbError::msg("internal lock"))?;
        let open = sessions
            .get(id)
            .cloned()
            .ok_or_else(|| DbError::NoSession(id.to_string()))?;
        match open
            .connection
            .tunnel
            .as_ref()
            .and_then(|t| t.drop_reason())
        {
            Some(reason) => {
                sessions.remove(id);
                drop(sessions);
                tauri::async_runtime::spawn(async move { open.close().await });
                Err(DbError::Message(reason))
            }
            None => Ok(open),
        }
    }

    /// The local end of a session's SSH tunnel, if it has one (Open in Terminal).
    pub(crate) fn tunnel_port(&self, id: &str) -> Option<u16> {
        let sessions = self.sessions.lock().ok()?;
        sessions
            .get(id)?
            .connection
            .tunnel
            .as_ref()
            .map(|t| t.local_port)
    }

    /// Takes out the sessions `which` picks (all of them for `|_| true`).
    fn take_sessions(&self, which: impl Fn(&Open) -> bool) -> Vec<Arc<Open>> {
        let Ok(mut sessions) = self.sessions.lock() else {
            return vec![];
        };
        let ids: Vec<String> = sessions
            .iter()
            .filter(|(_, open)| which(open))
            .map(|(id, _)| id.clone())
            .collect();
        ids.iter().filter_map(|id| sessions.remove(id)).collect()
    }

    /// Closes the sessions a webview opened (its page reloaded or went away).
    pub async fn close_owned_by(&self, owner: &str) {
        for open in self.take_sessions(|open| open.owner == owner) {
            open.close().await;
        }
    }

    /// Takes out the sessions a webview opened right away (so its label is
    /// free to reuse) and closes them in the background: a window going away
    /// must not hold the UI thread while a slow server answers.
    pub fn close_owned_by_later(&self, owner: &str) {
        let taken = self.take_sessions(|open| open.owner == owner);
        if taken.is_empty() {
            return;
        }
        for open in &taken {
            open.stop();
        }
        tauri::async_runtime::spawn(async move {
            futures_util::future::join_all(taken.iter().map(|open| open.close())).await;
        });
    }

    /// Closes every session and tunnel, for the app's exit.
    pub async fn close_all(&self) {
        let all = self.take_sessions(|_| true);
        futures_util::future::join_all(all.iter().map(|open| open.close())).await;
    }

    /// Runs `body` on the store, opening it first if needed.
    /// `DBLENS_STORE=memory` keeps everything in memory (snapshots, demos).
    pub(crate) async fn with_store<T>(
        &self,
        body: impl FnOnce(&mut Store) -> DbResult<T>,
    ) -> DbResult<T> {
        let mut guard = self.store.lock().await;
        if guard.is_none() {
            let store = if std::env::var("DBLENS_STORE").as_deref() == Ok("memory") {
                Store::in_memory()
            } else {
                let dir = Store::default_dir()?;
                tokio::task::spawn_blocking(move || Store::open(&dir, KeySource::Auto))
                    .await
                    .map_err(|e| DbError::Message(e.to_string()))??
            };
            *guard = Some(store);
        }
        // Reloads under the folder lock first: `dblens --mcp` may have saved meanwhile.
        guard.as_mut().expect("store opened").transact(body)
    }
}

// MARK: - URLs

#[tauri::command]
pub fn detect_engine(text: String) -> Option<Engine> {
    Engine::detect(&text)
}

#[tauri::command]
pub fn summarize_url(text: String) -> Option<Summary> {
    connector::summary(&text)
}

/// The text with passwords replaced by the mask, safe to show.
#[tauri::command]
pub fn mask_url(text: String) -> String {
    crate::url::redact(&text)
}

// MARK: - Sessions

/// What the frontend keeps about an open connection.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectInfo {
    pub session_id: String,
    pub engine: Engine,
    pub server_version: String,
    /// The saved connection's name, or the URL's suggested name.
    pub name: String,
    pub saved_id: Option<String>,
    /// "ana@bastion:22" when connected through SSH.
    pub ssh_label: Option<String>,
}

/// Connects by saved id (its URL and SSH settings come from the store) or by
/// a pasted `url` (with optional `ssh`). Both together try an edited saved
/// connection (`url` may still show the mask) without saving it.
#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    webview: tauri::Webview,
    url: Option<String>,
    saved_id: Option<String>,
    ssh: Option<SshConfig>,
) -> DbResult<ConnectInfo> {
    // With both a saved id and a url, the url is an edit of that saved connection
    // being tried before it is saved: its masked passwords come from the store,
    // and nothing is written until the frontend saves it.
    let trial = saved_id.is_some() && url.is_some();
    let (text, ssh, name) = match &saved_id {
        Some(id) if trial => {
            let draft = url.unwrap_or_default();
            let text = state
                .with_store(|store| {
                    let saved = store
                        .url(id)
                        .ok_or_else(|| DbError::msg("This saved connection is gone."))?;
                    Ok(crate::url::unmask(&draft, saved))
                })
                .await?;
            let name = connector::summary(&text)
                .map(|s| s.suggested_name)
                .unwrap_or_default();
            (text, ssh, name)
        }
        Some(id) => {
            state
                .with_store(|store| {
                    let saved = store
                        .get(id)
                        .ok_or_else(|| DbError::msg("This saved connection is gone."))?;
                    let url = store
                        .url(id)
                        .ok_or_else(|| DbError::msg("This connection's URL is missing."))?;
                    Ok((url.to_string(), saved.ssh.clone(), saved.name.clone()))
                })
                .await?
        }
        None => {
            let url = url.ok_or_else(|| DbError::msg("Paste the database URL."))?;
            let name = connector::summary(&url)
                .map(|s| s.suggested_name)
                .unwrap_or_default();
            (url, ssh, name)
        }
    };
    let connection = connector::connect(&text, ssh.as_ref()).await?;
    let info = ConnectInfo {
        session_id: uuid::Uuid::new_v4().to_string(),
        engine: connection.session.engine(),
        server_version: connection.session.server_version(),
        name,
        saved_id: saved_id.clone(),
        ssh_label: connection.tunnel.as_ref().map(|t| t.config.label()),
    };
    state
        .sessions
        .lock()
        .map_err(|_| DbError::msg("internal lock"))?
        .insert(
            info.session_id.clone(),
            Arc::new(Open {
                connection,
                owner: webview.label().to_string(),
                closing: Default::default(),
            }),
        );
    if let Some(id) = saved_id.filter(|_| !trial) {
        // Last used is a nicety; a store error must not fail the connect.
        let _ = state.with_store(|store| store.touch(&id)).await;
    }
    Ok(info)
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, session_id: String) -> DbResult<()> {
    let open = state
        .sessions
        .lock()
        .map_err(|_| DbError::msg("internal lock"))?
        .remove(&session_id);
    if let Some(open) = open {
        open.close().await;
    }
    Ok(())
}

/// Stops the SQL panel's running query (or stream) of that session; with
/// `scope` "all", browsing queries too. Never waits for them.
#[tauri::command]
pub fn cancel(
    state: State<'_, AppState>,
    session_id: String,
    scope: Option<String>,
) -> DbResult<()> {
    let open = state.session(&session_id)?;
    if scope.as_deref() == Some("all") {
        open.connection.session.cancel_all();
    } else {
        open.connection.session.cancel_sql();
    }
    Ok(())
}

/// Whether the user's transaction is open on the SQL panel's connection.
#[tauri::command]
pub fn transaction_open(state: State<'_, AppState>, session_id: String) -> DbResult<bool> {
    Ok(state
        .session(&session_id)?
        .connection
        .session
        .transaction_open())
}

#[tauri::command]
pub async fn relations(state: State<'_, AppState>, session_id: String) -> DbResult<Vec<Relation>> {
    state
        .session(&session_id)?
        .connection
        .session
        .relations()
        .await
}

/// One page as a MessagePack result block (`wire::block`).
#[tauri::command]
pub async fn page(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    limit: u32,
    offset: u64,
    search: String,
    sort: Option<ColumnSort>,
) -> DbResult<Response> {
    let open = state.session(&session_id)?;
    let result = open
        .connection
        .session
        .page(&relation, limit, offset, &search, sort.as_ref())
        .await?;
    Ok(Response::new(wire::block(&result, offset)))
}

/// A block of rows for the grid, as MessagePack. Stable order (sort, then the
/// key); keyset pagination where the driver can (see `pool.rs`).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn page_block(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    offset: u64,
    limit: u32,
    search: String,
    sort: Option<ColumnSort>,
) -> DbResult<Response> {
    let open = state.session(&session_id)?;
    let result = open
        .connection
        .session
        .page_block(&relation, offset, limit, &search, sort.as_ref(), None)
        .await?;
    Ok(Response::new(wire::block(&result, offset)))
}

#[tauri::command]
pub async fn count(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    search: String,
) -> DbResult<Option<u64>> {
    state
        .session(&session_id)?
        .connection
        .session
        .count(&relation, &search)
        .await
}

#[tauri::command]
pub async fn execute(
    state: State<'_, AppState>,
    session_id: String,
    sql: String,
) -> DbResult<Response> {
    let execution = state
        .session(&session_id)?
        .connection
        .session
        .execute(&sql)
        .await?;
    Ok(Response::new(wire::execution(&execution)))
}

#[tauri::command]
pub async fn read_only(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
) -> DbResult<Response> {
    let execution = state
        .session(&session_id)?
        .connection
        .session
        .read_only(&text)
        .await?;
    Ok(Response::new(wire::execution(&execution)))
}

#[tauri::command]
pub async fn rehearse(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
) -> DbResult<Response> {
    let execution = state
        .session(&session_id)?
        .connection
        .session
        .rehearse(&text)
        .await?;
    Ok(Response::new(wire::execution(&execution)))
}

/// An all-or-nothing run (`{kind: "ran", execution}`), or why this database
/// can't do one (`{kind: "notAtomic", message}`; the frontend may then offer
/// to run the script plainly), as MessagePack.
#[tauri::command]
pub async fn execute_atomically(
    state: State<'_, AppState>,
    session_id: String,
    text: String,
) -> DbResult<Response> {
    match state
        .session(&session_id)?
        .connection
        .session
        .execute_atomically(&text)
        .await
    {
        Ok(execution) => Ok(Response::new(wire::atomic_ran(&execution))),
        Err(DbError::NotAtomic(message)) => Ok(Response::new(wire::atomic_refused(&message))),
        Err(other) => Err(other),
    }
}

/// Runs statements on the SQL panel's connection and streams their rows over
/// `channel` as MessagePack messages (see `wire::StreamSink`): the first block
/// after 500 rows, then bigger ones, up to `row_cap` rows per result (default
/// 100,000; more are counted). The last message is always `done`, with the
/// error if the run failed; the command itself resolves once it is sent.
#[tauri::command]
pub async fn execute_stream(
    state: State<'_, AppState>,
    webview: tauri::Webview,
    session_id: String,
    sql: String,
    channel: Channel<InvokeResponseBody>,
    row_cap: Option<usize>,
) -> DbResult<()> {
    let open = state.session(&session_id)?;
    let start = std::time::Instant::now();
    // A send can't tell that the window is gone (Tauri queues big messages
    // for a webview that left), so the sink also asks whether the webview is
    // still there (every webview here is a window's own, same label) and
    // the session not closing.
    let alive = {
        use tauri::Manager;
        let app = webview.app_handle().clone();
        let label = webview.label().to_string();
        let open = open.clone();
        std::sync::Arc::new(move || {
            !open.closing.load(std::sync::atomic::Ordering::SeqCst)
                && app.get_webview_window(&label).is_some()
        })
    };
    let mut sink = StreamSink::new(
        std::sync::Arc::new(move |bytes| channel.send(InvokeResponseBody::Raw(bytes)).is_ok()),
        row_cap.unwrap_or(wire::STREAM_ROW_CAP),
    )
    .with_alive(alive);
    let outcome = open
        .connection
        .session
        .execute_stream(&sql, &mut sink)
        .await;
    sink.done(
        start.elapsed().as_secs_f64() * 1000.0,
        outcome.as_ref().err(),
    );
    Ok(())
}

#[tauri::command]
pub async fn update_row(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    key: RowKey,
    changes: Vec<CellValue>,
) -> DbResult<EditOutcome> {
    state
        .session(&session_id)?
        .connection
        .session
        .update(&relation, &key, &changes)
        .await
}

#[tauri::command]
pub async fn insert_row(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    values: Vec<CellValue>,
) -> DbResult<EditOutcome> {
    state
        .session(&session_id)?
        .connection
        .session
        .insert(&relation, &values)
        .await
}

#[tauri::command]
pub async fn delete_row(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    key: RowKey,
) -> DbResult<EditOutcome> {
    state
        .session(&session_id)?
        .connection
        .session
        .delete(&relation, &key)
        .await
}

#[tauri::command]
pub async fn schema_graph(state: State<'_, AppState>, session_id: String) -> DbResult<SchemaGraph> {
    state
        .session(&session_id)?
        .connection
        .session
        .schema_graph()
        .await
}

/// The relation's name quoted for this engine's SQL ("public"."users", `shop`.`users`).
#[tauri::command]
pub fn qualified_name(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
) -> DbResult<String> {
    Ok(state
        .session(&session_id)?
        .connection
        .session
        .qualified(&relation))
}

/// A result as CSV or JSON text, for Copy and Save.
#[tauri::command]
pub fn export_result(result: QueryResult, format: String) -> DbResult<String> {
    match format.as_str() {
        "csv" => Ok(export::csv(&result)),
        "json" => Ok(export::json(&result)),
        other => Err(DbError::Message(format!("Unknown export format {other}."))),
    }
}

// MARK: - Saved connections

#[tauri::command]
pub async fn list_connections(state: State<'_, AppState>) -> DbResult<StoreSnapshot> {
    state.with_store(|store| Ok(store.snapshot())).await
}

#[tauri::command]
pub async fn save_connection(
    state: State<'_, AppState>,
    draft: ConnectionDraft,
) -> DbResult<SavedConnection> {
    state.with_store(|store| store.save(draft)).await
}

#[tauri::command]
pub async fn delete_connection(
    state: State<'_, AppState>,
    history: State<'_, crate::history::HistoryState>,
    id: String,
) -> DbResult<()> {
    state.with_store(|store| store.delete(&id)).await?;
    // Its SQL history goes with it (history.rs).
    history.clear(Some(&id))
}

#[tauri::command]
pub async fn rename_connection(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> DbResult<SavedConnection> {
    state.with_store(|store| store.rename(&id, &name)).await
}

/// "Let Agents Write" on one connection (the MCP server, mcp.rs).
#[tauri::command]
pub async fn set_agents_can_write(
    state: State<'_, AppState>,
    id: String,
    allowed: bool,
) -> DbResult<SavedConnection> {
    state
        .with_store(|store| store.set_agents_can_write(&id, allowed))
        .await
}

/// "Let agents add and remove connections" for the MCP server.
#[tauri::command]
pub async fn set_agents_manage_connections(
    state: State<'_, AppState>,
    allowed: bool,
) -> DbResult<()> {
    state
        .with_store(|store| store.set_agents_manage_connections(allowed))
        .await
}

/// The saved URL with its passwords masked, for the edit sheet.
#[tauri::command]
pub async fn connection_url(state: State<'_, AppState>, id: String) -> DbResult<String> {
    state
        .with_store(|store| {
            store
                .masked_url(&id)
                .ok_or_else(|| DbError::msg("No such connection."))
        })
        .await
}

#[tauri::command]
pub async fn move_connection(
    state: State<'_, AppState>,
    id: String,
    folder_id: String,
    before_id: Option<String>,
) -> DbResult<()> {
    state
        .with_store(|store| store.move_connection(&id, &folder_id, before_id.as_deref()))
        .await
}

#[tauri::command]
pub async fn add_folder(state: State<'_, AppState>, name: String) -> DbResult<ConnectionFolder> {
    state.with_store(|store| store.add_folder(&name)).await
}

#[tauri::command]
pub async fn rename_folder(state: State<'_, AppState>, id: String, name: String) -> DbResult<()> {
    state
        .with_store(|store| store.rename_folder(&id, &name))
        .await
}

#[tauri::command]
pub async fn delete_folder(state: State<'_, AppState>, id: String) -> DbResult<()> {
    state.with_store(|store| store.delete_folder(&id)).await
}

#[tauri::command]
pub async fn set_folder_expanded(
    state: State<'_, AppState>,
    id: String,
    expanded: bool,
) -> DbResult<()> {
    state
        .with_store(|store| store.set_folder_expanded(&id, expanded))
        .await
}
