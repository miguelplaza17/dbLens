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
use tauri::State;

use crate::connector::{self, Connection, Summary};
use crate::engine::Engine;
use crate::export;
use crate::session::{
    CellValue, ColumnSort, DbError, DbResult, EditOutcome, Execution, QueryResult, Relation, RowKey, SchemaGraph,
};
use crate::ssh::SshConfig;
use crate::store::{ConnectionDraft, ConnectionFolder, KeySource, SavedConnection, Store, StoreSnapshot};

/// One open connection.
struct Open {
    connection: Connection,
}

#[derive(Default)]
pub struct AppState {
    sessions: Mutex<HashMap<String, Arc<Open>>>,
    /// Opened on first use, off the async runtime (the keyring may block).
    store: tokio::sync::Mutex<Option<Store>>,
}

impl AppState {
    fn session(&self, id: &str) -> DbResult<Arc<Open>> {
        self.sessions
            .lock()
            .map_err(|_| DbError::msg("internal lock"))?
            .get(id)
            .cloned()
            .ok_or_else(|| DbError::NoSession(id.to_string()))
            .and_then(|open| match open.connection.tunnel.as_ref().and_then(|t| t.drop_reason()) {
                Some(reason) => Err(DbError::Message(reason)),
                None => Ok(open),
            })
    }

    /// Runs `body` on the store, opening it first if needed.
    /// `DBLENS_STORE=memory` keeps everything in memory (snapshots, demos).
    async fn with_store<T>(&self, body: impl FnOnce(&mut Store) -> DbResult<T>) -> DbResult<T> {
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
        body(guard.as_mut().expect("store opened"))
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
/// a pasted `url` (with optional `ssh`).
#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    url: Option<String>,
    saved_id: Option<String>,
    ssh: Option<SshConfig>,
) -> DbResult<ConnectInfo> {
    let (text, ssh, name) = match &saved_id {
        Some(id) => {
            state
                .with_store(|store| {
                    let saved = store.get(id).ok_or_else(|| DbError::msg("This saved connection is gone."))?;
                    let url = store.url(id).ok_or_else(|| DbError::msg("This connection's URL is missing."))?;
                    Ok((url.to_string(), saved.ssh.clone(), saved.name.clone()))
                })
                .await?
        }
        None => {
            let url = url.ok_or_else(|| DbError::msg("Paste the database URL."))?;
            let name = connector::summary(&url).map(|s| s.suggested_name).unwrap_or_default();
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
        .insert(info.session_id.clone(), Arc::new(Open { connection }));
    if let Some(id) = saved_id {
        // Last used is a nicety; a store error must not fail the connect.
        let _ = state.with_store(|store| store.touch(&id)).await;
    }
    Ok(info)
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, session_id: String) -> DbResult<()> {
    let open = state.sessions.lock().map_err(|_| DbError::msg("internal lock"))?.remove(&session_id);
    if let Some(open) = open {
        open.connection.session.cancel();
        open.connection.session.close().await;
        if let Some(tunnel) = &open.connection.tunnel {
            tunnel.close();
        }
    }
    Ok(())
}

/// Stops the running query of that session. Never waits for it.
#[tauri::command]
pub fn cancel(state: State<'_, AppState>, session_id: String) -> DbResult<()> {
    state.session(&session_id)?.connection.session.cancel();
    Ok(())
}

#[tauri::command]
pub async fn relations(state: State<'_, AppState>, session_id: String) -> DbResult<Vec<Relation>> {
    state.session(&session_id)?.connection.session.relations().await
}

#[tauri::command]
pub async fn page(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    limit: u32,
    offset: u64,
    search: String,
    sort: Option<ColumnSort>,
) -> DbResult<QueryResult> {
    let open = state.session(&session_id)?;
    open.connection.session.page(&relation, limit, offset, &search, sort.as_ref()).await
}

#[tauri::command]
pub async fn count(state: State<'_, AppState>, session_id: String, relation: Relation, search: String) -> DbResult<Option<u64>> {
    state.session(&session_id)?.connection.session.count(&relation, &search).await
}

#[tauri::command]
pub async fn execute(state: State<'_, AppState>, session_id: String, sql: String) -> DbResult<Execution> {
    state.session(&session_id)?.connection.session.execute(&sql).await
}

#[tauri::command]
pub async fn read_only(state: State<'_, AppState>, session_id: String, text: String) -> DbResult<Execution> {
    state.session(&session_id)?.connection.session.read_only(&text).await
}

#[tauri::command]
pub async fn rehearse(state: State<'_, AppState>, session_id: String, text: String) -> DbResult<Execution> {
    state.session(&session_id)?.connection.session.rehearse(&text).await
}

/// An all-or-nothing run, or why this database can't do one (the frontend may
/// then offer to run the script plainly with `execute`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AtomicOutcome {
    Ran { execution: Execution },
    NotAtomic { message: String },
}

#[tauri::command]
pub async fn execute_atomically(state: State<'_, AppState>, session_id: String, text: String) -> DbResult<AtomicOutcome> {
    match state.session(&session_id)?.connection.session.execute_atomically(&text).await {
        Ok(execution) => Ok(AtomicOutcome::Ran { execution }),
        Err(DbError::NotAtomic(message)) => Ok(AtomicOutcome::NotAtomic { message }),
        Err(other) => Err(other),
    }
}

#[tauri::command]
pub async fn update_row(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    key: RowKey,
    changes: Vec<CellValue>,
) -> DbResult<EditOutcome> {
    state.session(&session_id)?.connection.session.update(&relation, &key, &changes).await
}

#[tauri::command]
pub async fn insert_row(
    state: State<'_, AppState>,
    session_id: String,
    relation: Relation,
    values: Vec<CellValue>,
) -> DbResult<EditOutcome> {
    state.session(&session_id)?.connection.session.insert(&relation, &values).await
}

#[tauri::command]
pub async fn delete_row(state: State<'_, AppState>, session_id: String, relation: Relation, key: RowKey) -> DbResult<EditOutcome> {
    state.session(&session_id)?.connection.session.delete(&relation, &key).await
}

#[tauri::command]
pub async fn schema_graph(state: State<'_, AppState>, session_id: String) -> DbResult<SchemaGraph> {
    state.session(&session_id)?.connection.session.schema_graph().await
}

/// The relation's name quoted for this engine's SQL ("public"."users", `shop`.`users`).
#[tauri::command]
pub fn qualified_name(state: State<'_, AppState>, session_id: String, relation: Relation) -> DbResult<String> {
    Ok(state.session(&session_id)?.connection.session.qualified(&relation))
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
pub async fn save_connection(state: State<'_, AppState>, draft: ConnectionDraft) -> DbResult<SavedConnection> {
    state.with_store(|store| store.save(draft)).await
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, id: String) -> DbResult<()> {
    state.with_store(|store| store.delete(&id)).await
}

#[tauri::command]
pub async fn rename_connection(state: State<'_, AppState>, id: String, name: String) -> DbResult<SavedConnection> {
    state.with_store(|store| store.rename(&id, &name)).await
}

/// The saved URL with its passwords masked, for the edit sheet.
#[tauri::command]
pub async fn connection_url(state: State<'_, AppState>, id: String) -> DbResult<String> {
    state.with_store(|store| store.masked_url(&id).ok_or_else(|| DbError::msg("No such connection."))).await
}

#[tauri::command]
pub async fn move_connection(
    state: State<'_, AppState>,
    id: String,
    folder_id: String,
    before_id: Option<String>,
) -> DbResult<()> {
    state.with_store(|store| store.move_connection(&id, &folder_id, before_id.as_deref())).await
}

#[tauri::command]
pub async fn add_folder(state: State<'_, AppState>, name: String) -> DbResult<ConnectionFolder> {
    state.with_store(|store| store.add_folder(&name)).await
}

#[tauri::command]
pub async fn rename_folder(state: State<'_, AppState>, id: String, name: String) -> DbResult<()> {
    state.with_store(|store| store.rename_folder(&id, &name)).await
}

#[tauri::command]
pub async fn delete_folder(state: State<'_, AppState>, id: String) -> DbResult<()> {
    state.with_store(|store| store.delete_folder(&id)).await
}

#[tauri::command]
pub async fn set_folder_expanded(state: State<'_, AppState>, id: String, expanded: bool) -> DbResult<()> {
    state.with_store(|store| store.set_folder_expanded(&id, expanded)).await
}
