//! SQL history, per saved connection: the statements run in the SQL panel,
//! newest first, the last 200 per connection. Port of the history in
//! `Models.swift` (Swift keeps it in user defaults; here it is
//! `sql-history.json` in the data folder, or memory only with
//! `DBLENS_STORE=memory`).
//!
//! A connection that was never saved (a pasted URL) has no id and no history.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::session::{DbError, DbResult};
use crate::store::Store;

/// How many runs one connection keeps.
pub const LIMIT: usize = 200;
const FILE: &str = "sql-history.json";

/// One statement that was run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub sql: String,
    /// The run ended in an error.
    #[serde(default)]
    pub failed: bool,
    /// When it ran, RFC 3339 UTC.
    pub date: String,
    /// Run by an agent through the MCP server.
    #[serde(default)]
    pub via_agent: bool,
}

type Table = BTreeMap<String, Vec<Entry>>;

/// Puts a run on top of a connection's list. The same text run again right
/// after itself is one entry (its date and outcome are renewed); the list
/// never grows past [`LIMIT`].
pub fn push(list: &mut Vec<Entry>, entry: Entry) {
    if list.first().is_some_and(|top| top.sql == entry.sql) {
        list[0] = entry;
    } else {
        list.insert(0, entry);
    }
    list.truncate(LIMIT);
}

/// The history in memory and the file it lives in (None: memory only).
pub struct HistoryState {
    path: Option<PathBuf>,
    table: Mutex<Table>,
}

impl HistoryState {
    /// The data folder's `sql-history.json`, or memory only with `DBLENS_STORE=memory`.
    pub fn from_env() -> HistoryState {
        let path = if std::env::var("DBLENS_STORE").as_deref() == Ok("memory") {
            None
        } else {
            Store::default_dir().ok().map(|d| d.join(FILE))
        };
        HistoryState::at(path)
    }

    pub fn at(path: Option<PathBuf>) -> HistoryState {
        let table = path.as_deref().map(load).unwrap_or_default();
        HistoryState {
            path,
            table: Mutex::new(table),
        }
    }

    pub fn list(&self, connection: &str) -> Vec<Entry> {
        self.table
            .lock()
            .map(|t| t.get(connection).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    pub fn add(&self, connection: &str, entry: Entry) -> DbResult<()> {
        self.change(|table| push(table.entry(connection.to_string()).or_default(), entry))
    }

    /// One connection's history, or every connection's without an id.
    pub fn clear(&self, connection: Option<&str>) -> DbResult<()> {
        self.change(|table| match connection {
            Some(id) => {
                table.remove(id);
            }
            None => table.clear(),
        })
    }

    fn change(&self, body: impl FnOnce(&mut Table)) -> DbResult<()> {
        let mut table = self
            .table
            .lock()
            .map_err(|_| DbError::msg("internal lock"))?;
        body(&mut table);
        match &self.path {
            Some(path) => save(path, &table),
            None => Ok(()),
        }
    }
}

fn load(path: &Path) -> Table {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Written whole to a temporary file, then renamed over the old one. The file
/// is private: statements may carry literals nobody else should read.
fn save(path: &Path, table: &Table) -> DbResult<()> {
    let fail = |e: std::io::Error| DbError::Message(format!("SQL history: {e}"));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    let bytes = serde_json::to_vec(table).map_err(|e| DbError::Message(e.to_string()))?;
    let temp = path.with_extension("json.tmp");
    write_private(&temp, &bytes).map_err(fail)?;
    fs::rename(&temp, path).map_err(fail)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

// MARK: - Commands

/// A connection's runs, newest first.
#[tauri::command]
pub fn sql_history(state: State<'_, HistoryState>, connection_id: String) -> Vec<Entry> {
    state.list(&connection_id)
}

/// Records a run. `failed` is true when it ended in an error.
#[tauri::command]
pub fn add_sql_history(
    state: State<'_, HistoryState>,
    connection_id: String,
    sql: String,
    failed: bool,
) -> DbResult<()> {
    if sql.trim().is_empty() {
        return Ok(());
    }
    state.add(
        &connection_id,
        Entry {
            sql,
            failed,
            date: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            via_agent: false,
        },
    )
}

/// Clears one connection's history, or all of it without an id.
#[tauri::command]
pub fn clear_sql_history(
    state: State<'_, HistoryState>,
    connection_id: Option<String>,
) -> DbResult<()> {
    state.clear(connection_id.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str) -> Entry {
        Entry {
            sql: sql.into(),
            failed: false,
            date: "2026-01-01T00:00:00Z".into(),
            via_agent: false,
        }
    }

    #[test]
    fn newest_first_and_consecutive_duplicates_collapse() {
        let mut list = vec![];
        push(&mut list, entry("select 1"));
        push(&mut list, entry("select 2"));
        push(&mut list, entry("select 2"));
        push(&mut list, entry("select 1"));
        let sql: Vec<_> = list.iter().map(|e| e.sql.as_str()).collect();
        assert_eq!(sql, ["select 1", "select 2", "select 1"]);
    }

    #[test]
    fn a_repeat_renews_the_outcome() {
        let mut list = vec![];
        push(&mut list, entry("select 1"));
        let mut again = entry("select 1");
        again.failed = true;
        push(&mut list, again);
        assert_eq!(list.len(), 1);
        assert!(list[0].failed);
    }

    #[test]
    fn keeps_the_last_200() {
        let mut list = vec![];
        for i in 0..250 {
            push(&mut list, entry(&format!("select {i}")));
        }
        assert_eq!(list.len(), LIMIT);
        assert_eq!(list[0].sql, "select 249");
        assert_eq!(list[LIMIT - 1].sql, "select 50");
    }

    #[test]
    fn connections_do_not_mix_and_clear_works() {
        let state = HistoryState::at(None);
        state.add("a", entry("select 1")).unwrap();
        state.add("b", entry("select 2")).unwrap();
        assert_eq!(state.list("a").len(), 1);
        assert!(state.list("c").is_empty());
        state.clear(Some("a")).unwrap();
        assert!(state.list("a").is_empty());
        assert_eq!(state.list("b").len(), 1);
        state.clear(None).unwrap();
        assert!(state.list("b").is_empty());
    }

    #[test]
    fn survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE);
        let state = HistoryState::at(Some(path.clone()));
        state.add("a", entry("select 1")).unwrap();
        state.add("a", entry("select 2")).unwrap();
        let again = HistoryState::at(Some(path.clone()));
        assert_eq!(again.list("a")[0].sql, "select 2");
        again.clear(Some("a")).unwrap();
        assert!(HistoryState::at(Some(path)).list("a").is_empty());
    }

    #[test]
    fn a_broken_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        fs::write(&path, b"not json").unwrap();
        assert!(HistoryState::at(Some(path)).list("a").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        HistoryState::at(Some(path.clone()))
            .add("a", entry("select 1"))
            .unwrap();
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
