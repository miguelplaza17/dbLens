//! SQLite driver on rusqlite (bundled SQLite). Port of `SQLite.swift`.
//!
//! SQLite calls block, so every method runs on `spawn_blocking` with the
//! connection behind a std `Mutex`. `cancel` uses the connection's interrupt
//! handle, which SQLite allows from any thread and which never takes that lock.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use async_trait::async_trait;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::ValueRef;
use rusqlite::{params_from_iter, Batch, Connection, InterruptHandle, OpenFlags, Statement};

use crate::connector::Summary;
use crate::engine::Engine;
use crate::session::like_pattern;
use crate::session::{
    CellValue, Column, ColumnSort, DatabaseSession, DbError, DbResult, EditOutcome, Execution,
    QueryResult, Relation, RelationKind, RowKey, RowKeyKind, RowKeys, SchemaGraph, ROW_LIMIT,
};
use crate::sql_script::{self, Dialect};
use crate::url::{quote_ident, SqliteLocation};

/// The file for the connection sheet.
pub fn summary(text: &str) -> Option<Summary> {
    let location = SqliteLocation::new(text).ok()?;
    Some(Summary {
        engine: Engine::Sqlite,
        suggested_name: location.suggested_name(),
        database: location.path,
        host: None,
        port: None,
        user: None,
        tls: None,
    })
}

/// Opens an existing file, or fails with SQLite's own message. Through an SSH
/// tunnel `local_port` would apply; a file has no host, so it is ignored.
pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>> {
    let _ = local_port;
    let location = SqliteLocation::new(url)?;
    let session = tokio::task::spawn_blocking(move || SqliteSession::open(&location))
        .await
        .map_err(|e| DbError::msg(e.to_string()))??;
    Ok(Box::new(session))
}

/// One open SQLite file.
pub struct SqliteSession {
    conn: Arc<Mutex<Option<Connection>>>,
    interrupt: InterruptHandle,
    read_only: bool,
    version: String,
}

fn closed() -> DbError {
    DbError::msg("This connection is closed. Connect again.")
}

/// SQLite's own message; an interrupted run is a cancel.
fn err(error: rusqlite::Error) -> DbError {
    use rusqlite::ffi::ErrorCode;
    match error {
        rusqlite::Error::SqliteFailure(e, _) if e.code == ErrorCode::OperationInterrupted => {
            DbError::Cancelled
        }
        rusqlite::Error::SqliteFailure(_, Some(message)) => DbError::Message(message),
        other => DbError::Message(other.to_string()),
    }
}

fn lock(conn: &Mutex<Option<Connection>>) -> MutexGuard<'_, Option<Connection>> {
    conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SqliteSession {
    fn open(location: &SqliteLocation) -> DbResult<Self> {
        if !Path::new(&location.path).exists() {
            return Err(DbError::msg(format!("No SQLite file at {}", location.path)));
        }
        // No CREATE (a typo must not make an empty file) and no URI parsing (a path may hold '?').
        let write = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let (conn, mut read_only) = match Connection::open_with_flags(&location.path, write) {
            Ok(conn) => (conn, false),
            Err(_) => (
                Connection::open_with_flags(
                    &location.path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(err)?,
                true,
            ),
        };
        // SQLite opens a write-protected file read-only without an error.
        if conn.is_readonly("main").unwrap_or(false) {
            read_only = true;
        }
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(err)?;
        // A file that isn't a database only fails on first use; fail now.
        conn.execute_batch("pragma foreign_keys = on")
            .map_err(err)?;
        conn.query_row("select 1 from sqlite_schema limit 1", [], |_| Ok(()))
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(()),
                other => Err(other),
            })
            .map_err(err)?;
        Ok(Self {
            interrupt: conn.get_interrupt_handle(),
            conn: Arc::new(Mutex::new(Some(conn))),
            read_only,
            version: rusqlite::version().to_string(),
        })
    }

    /// True when the file couldn't be opened for writing.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Runs `work` on a blocking thread with the connection.
    async fn with<T, F>(&self, work: F) -> DbResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> DbResult<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let guard = lock(&conn);
            work(guard.as_ref().ok_or_else(closed)?)
        })
        .await
        .map_err(|e| DbError::msg(e.to_string()))?
    }

    fn require_writable(&self) -> DbResult<()> {
        if self.read_only {
            Err(DbError::msg("This file is read-only."))
        } else {
            Ok(())
        }
    }
}

// MARK: - Reading values

const BLOB_BYTES_SHOWN: usize = 64;

/// A real the way SQLite prints it (`%!.15g`): "1.5", "3.0", "1.0e+20".
pub(crate) fn real_text(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 { "Inf".into() } else { "-Inf".into() };
    }
    if v.is_nan() {
        return "NaN".into();
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let scientific = format!("{v:.14e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let negative = mantissa.starts_with('-');
    let mut digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let sign = if negative { "-" } else { "" };
    if !(-4..15).contains(&exponent) {
        let rest = if digits.len() > 1 { &digits[1..] } else { "0" };
        let mark = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{}.{rest}e{mark}{:02}", &digits[..1], exponent.abs());
    }
    if exponent >= 0 {
        let whole = exponent as usize + 1;
        while digits.len() < whole {
            digits.push('0');
        }
        let (int, frac) = digits.split_at(whole);
        format!("{sign}{int}.{}", if frac.is_empty() { "0" } else { frac })
    } else {
        format!("{sign}0.{}{digits}", "0".repeat((-exponent - 1) as usize))
    }
}

/// "0x" and the first 64 bytes in hex, with "…" when there are more.
fn blob_text(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::from("0x");
    for byte in bytes.iter().take(BLOB_BYTES_SHOWN) {
        let _ = write!(text, "{byte:02x}");
    }
    if bytes.len() > BLOB_BYTES_SHOWN {
        text.push('…');
    }
    text
}

fn value(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(r) => Some(real_text(r)),
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Some(blob_text(bytes)),
    }
}

/// The statement's first word, uppercased, after any whitespace, stray
/// semicolons and comments.
pub(crate) fn first_keyword(sql: &str) -> String {
    let mut rest = sql;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ';');
        if rest.starts_with("--") {
            rest = rest.find('\n').map(|n| &rest[n..]).unwrap_or("");
        } else if rest.starts_with("/*") {
            match rest.find("*/") {
                Some(end) => rest = &rest[end + 2..],
                None => return String::new(),
            }
        } else {
            break;
        }
    }
    rest.chars()
        .take_while(|c| c.is_alphabetic())
        .collect::<String>()
        .to_uppercase()
}

/// Steps a statement to the end and gathers its rows (cut at `ROW_LIMIT`).
fn read(conn: &Connection, stmt: &mut Statement<'_>) -> DbResult<QueryResult> {
    let count = stmt.column_count();
    let columns: Vec<Column> = stmt
        .columns()
        .map(|c| Column::new(c.name(), c.decl_type().unwrap_or("")))
        .collect();
    let sql = stmt.expanded_sql().unwrap_or_default();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut total: u64 = 0;
    let mut cursor = stmt.query([]).map_err(err)?;
    while let Some(row) = cursor.next().map_err(err)? {
        total += 1;
        if rows.len() < ROW_LIMIT {
            let mut cells = Vec::with_capacity(count);
            for index in 0..count {
                cells.push(value(row.get_ref(index).map_err(err)?));
            }
            rows.push(cells);
        }
    }
    drop(cursor);
    let command = if count > 0 {
        format!("SELECT {total}")
    } else {
        let keyword = first_keyword(&sql);
        if ["INSERT", "UPDATE", "DELETE"].contains(&keyword.as_str()) {
            format!("{keyword} {}", conn.changes())
        } else {
            keyword
        }
    };
    Ok(QueryResult {
        columns,
        truncated: total > rows.len() as u64,
        rows,
        command,
        total_rows: total,
        keys: None,
        note: None,
    })
}

/// Prepares and reads one statement.
fn single(conn: &Connection, sql: &str) -> DbResult<QueryResult> {
    let mut stmt = conn.prepare(sql).map_err(err)?;
    read(conn, &mut stmt)
}

fn run_script(conn: &Connection, sql: &str) -> DbResult<Vec<QueryResult>> {
    let mut results = Vec::new();
    let mut batch = Batch::new(conn, sql);
    while let Some(mut stmt) = batch.next().map_err(err)? {
        results.push(read(conn, &mut stmt)?);
    }
    Ok(results)
}

fn read_only_run(conn: &Connection, sql: &str) -> DbResult<QueryResult> {
    let mut batch = Batch::new(conn, sql);
    let Some(mut stmt) = batch.next().map_err(err)? else {
        return Err(DbError::msg("There is no statement to run."));
    };
    // Whatever follows must prepare to nothing (spaces, semicolons, comments).
    if batch.next().map_err(err)?.is_some() {
        return Err(DbError::msg("Send one statement at a time."));
    }
    // stmt_readonly passes these too: they change the connection, not the file.
    let keyword = first_keyword(sql);
    let blocked = [
        "ATTACH",
        "DETACH",
        "PRAGMA",
        "BEGIN",
        "COMMIT",
        "END",
        "ROLLBACK",
        "SAVEPOINT",
        "RELEASE",
        "VACUUM",
    ];
    if blocked.contains(&keyword.as_str()) || !stmt.readonly() {
        return Err(DbError::msg(
            "This statement would change the database; read-only queries can't.",
        ));
    }
    conn.execute_batch("pragma query_only = on").map_err(err)?;
    let result = read(conn, &mut stmt);
    let _ = conn.execute_batch("pragma query_only = off");
    result
}

/// Refused while the user has a transaction open in the SQL panel: SQLite
/// can't start another, and dbLens's rollback would undo the user's work.
fn require_no_transaction(conn: &Connection) -> DbResult<()> {
    if conn.is_autocommit() {
        Ok(())
    } else {
        Err(DbError::msg(
            "A transaction is open in the SQL panel. Commit or roll it back first.",
        ))
    }
}

fn in_transaction(conn: &Connection, sql: &str, commit: bool) -> DbResult<Vec<QueryResult>> {
    let statements = sql_script::checked(sql, Dialect::Sqlite)?;
    require_no_transaction(conn)?;
    for statement in &statements {
        let verb = sql_script::verb(statement, 1);
        if ["VACUUM", "ATTACH", "DETACH"].contains(&verb.as_str()) {
            return Err(DbError::msg(format!(
                "{verb} can't run inside a transaction."
            )));
        }
    }
    conn.execute_batch("begin").map_err(err)?;
    let mut results = Vec::with_capacity(statements.len());
    for (index, statement) in statements.iter().enumerate() {
        match single(conn, statement) {
            Ok(result) => results.push(result),
            Err(error) => {
                let _ = conn.execute_batch("rollback");
                return Err(match error {
                    DbError::Cancelled => DbError::Cancelled,
                    other => sql_script::failed(index, statements.len(), statement, &other),
                });
            }
        }
    }
    if let Err(error) = conn.execute_batch(if commit { "commit" } else { "rollback" }) {
        let _ = conn.execute_batch("rollback");
        return Err(err(error));
    }
    Ok(results)
}

// MARK: - Browsing

struct TableInfo {
    types: BTreeMap<String, String>,
    primary_key: Vec<String>,
    columns: Vec<String>,
}

/// Column names and declared types, in table order, plus the primary key in key order.
fn table_info(conn: &Connection, relation: &Relation) -> DbResult<TableInfo> {
    let rows = single(
        conn,
        &format!("pragma table_info({})", quote_ident(&relation.name)),
    )?
    .rows;
    // cid, name, type, notnull, dflt_value, pk
    let mut info = TableInfo {
        types: BTreeMap::new(),
        primary_key: vec![],
        columns: vec![],
    };
    let mut keyed: Vec<(i64, String)> = Vec::new();
    for row in rows {
        let name = row[1].clone().unwrap_or_default();
        info.types
            .insert(name.clone(), row[2].clone().unwrap_or_default());
        if let Some(order) = row[5]
            .as_deref()
            .and_then(|p| p.parse::<i64>().ok())
            .filter(|o| *o > 0)
        {
            keyed.push((order, name.clone()));
        }
        info.columns.push(name);
    }
    keyed.sort_by_key(|(order, _)| *order);
    info.primary_key = keyed.into_iter().map(|(_, name)| name).collect();
    Ok(info)
}

fn qualified(relation: &Relation) -> String {
    format!(
        "{}.{}",
        quote_ident(&relation.schema),
        quote_ident(&relation.name)
    )
}

/// Every column joined into one text and matched with LIKE (case-insensitive for ASCII).
fn filter(search: &str, columns: &[String]) -> String {
    if search.is_empty() || columns.is_empty() {
        return String::new();
    }
    let joined = columns
        .iter()
        .map(|c| format!("coalesce({}, '')", quote_ident(c)))
        .collect::<Vec<_>>()
        .join(" || ' ' || ");
    let pattern = like_pattern(search).replace('\'', "''");
    format!(" where ({joined}) like '{pattern}' escape '\\'")
}

fn page_of(
    conn: &Connection,
    relation: &Relation,
    limit: u32,
    offset: u64,
    search: &str,
    sort: Option<&ColumnSort>,
) -> DbResult<QueryResult> {
    let info = table_info(conn, relation)?;
    let is_table = relation.kind == RelationKind::Table;
    let use_rowid = is_table && info.primary_key.is_empty();
    let keys: Vec<String> = if info.primary_key.is_empty() {
        if use_rowid {
            vec!["t.rowid".into()]
        } else {
            vec![]
        }
    } else {
        info.primary_key.iter().map(|k| quote_ident(k)).collect()
    };
    // Key columns come first, read with their storage class (see `key_select`),
    // and are taken off before the rows are shown.
    let hidden: Vec<String> = if use_rowid {
        vec!["t.rowid as \"dblens_key_0\", ".into()]
    } else if is_table {
        info.primary_key
            .iter()
            .enumerate()
            .map(|(i, k)| format!("{} as \"dblens_key_{i}\", ", key_select(k)))
            .collect()
    } else {
        vec![]
    };
    let mut terms: Vec<String> = sort
        .map(|s| format!("{} {} nulls last", quote_ident(&s.column), s.direction()))
        .into_iter()
        .collect();
    terms.extend(keys);
    let order = if terms.is_empty() {
        String::new()
    } else {
        format!(" order by {}", terms.join(", "))
    };
    let sql = format!(
        "select {}t.* from {} as t{}{order} limit {limit} offset {offset}",
        hidden.concat(),
        qualified(relation),
        filter(search, &info.columns),
    );
    let mut result = single(conn, &sql)?;
    let n = hidden.len();
    if n > 0 && result.columns.len() >= n {
        let values = result
            .rows
            .iter_mut()
            .map(|row| row.drain(..n).collect())
            .collect();
        result.columns.drain(..n);
        result.keys = Some(if use_rowid {
            RowKeys {
                columns: vec!["rowid".into()],
                kind: RowKeyKind::Rowid,
                values,
            }
        } else {
            RowKeys {
                columns: info.primary_key.clone(),
                kind: RowKeyKind::PrimaryKey,
                values,
            }
        });
    }
    for column in &mut result.columns {
        if let Some(declared) = info.types.get(&column.name) {
            column.type_name = declared.clone();
        }
    }
    Ok(result)
}

// MARK: - Editing

/// A new cell value: text, or NULL. The column's affinity converts it.
fn text_value(value: Option<&str>) -> rusqlite::types::Value {
    match value {
        Some(text) => rusqlite::types::Value::Text(text.to_string()),
        None => rusqlite::types::Value::Null,
    }
}

/// A key column read with its storage class, since a column without a declared
/// type (or a BLOB) compares only with a value of the same class: "i:1",
/// "r:0.1" (exact, from quote), "t:text", "b:0102" (every byte as hex).
fn key_select(column: &str) -> String {
    let c = format!("t.{}", quote_ident(column));
    format!(
        "case typeof({c}) when 'null' then null when 'integer' then 'i:' || {c} \
         when 'real' then 'r:' || quote({c}) when 'blob' then 'b:' || hex({c}) else 't:' || {c} end"
    )
}

/// The value to bind for a key from `key_select`; text without a tag stays text.
fn key_value(text: &str) -> DbResult<rusqlite::types::Value> {
    use rusqlite::types::Value;
    let bad = || DbError::msg("The row key is not valid. Reload the table and try again.");
    Ok(match text.split_at_checked(2) {
        Some(("i:", n)) => Value::Integer(n.parse().map_err(|_| bad())?),
        Some(("r:", n)) => Value::Real(n.parse().map_err(|_| bad())?),
        Some(("t:", t)) => Value::Text(t.to_string()),
        Some(("b:", hex)) => {
            if hex.len() % 2 != 0 || !hex.is_ascii() {
                return Err(bad());
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
                .map_err(|_| bad())?;
            Value::Blob(bytes)
        }
        _ => Value::Text(text.to_string()),
    })
}

/// `"col" = ?` per key column; a NULL key value compares with `is null` and binds nothing.
fn where_clause(key: &RowKey) -> DbResult<(String, Vec<rusqlite::types::Value>)> {
    let mut parts = Vec::new();
    let mut params = Vec::new();
    for (name, v) in key.columns.iter().zip(&key.values) {
        let column = if key.kind == RowKeyKind::Rowid {
            name.clone()
        } else {
            quote_ident(name)
        };
        match v {
            Some(v) => {
                parts.push(format!("{column} = ?"));
                params.push(if key.kind == RowKeyKind::Rowid {
                    rusqlite::types::Value::Integer(v.parse().map_err(|_| {
                        DbError::msg("The row key is not valid. Reload the table and try again.")
                    })?)
                } else {
                    key_value(v)?
                });
            }
            None => parts.push(format!("{column} is null")),
        }
    }
    Ok((parts.join(" and "), params))
}

/// Runs one statement with bound text. An edit meant for exactly one row is
/// rolled back if it would touch more, and refused if it touches none.
/// Inside the user's open transaction the edit becomes part of it, behind a
/// savepoint: a refused edit is undone alone, and the commit stays the user's.
fn edit(
    conn: &Connection,
    sql: String,
    params: Vec<rusqlite::types::Value>,
    verb: &str,
    expect_one: bool,
) -> DbResult<EditOutcome> {
    let nested = !conn.is_autocommit();
    conn.execute_batch(if nested {
        "savepoint dblens_edit"
    } else {
        "begin"
    })
    .map_err(err)?;
    let attempt = (|| -> DbResult<u64> {
        let mut stmt = conn.prepare(&sql).map_err(err)?;
        let mut rows = stmt.query(params_from_iter(params.iter())).map_err(err)?;
        while rows.next().map_err(err)?.is_some() {}
        drop(rows);
        let affected = conn.changes();
        if expect_one {
            if affected > 1 {
                return Err(DbError::msg(format!(
                    "The change would have affected {affected} rows and was rolled back."
                )));
            }
            if affected != 1 {
                return Err(DbError::msg(
                    "The row changed or was deleted since it was loaded. Reload the table and try again.",
                ));
            }
        }
        conn.execute_batch(if nested {
            "release dblens_edit"
        } else {
            "commit"
        })
        .map_err(err)?;
        Ok(affected)
    })();
    match attempt {
        Ok(affected) => Ok(EditOutcome {
            sql,
            command: format!("{verb} {affected}"),
            affected,
        }),
        Err(error) => {
            let _ = conn.execute_batch(if nested {
                "rollback to dblens_edit; release dblens_edit"
            } else {
                "rollback"
            });
            Err(error)
        }
    }
}

#[async_trait]
impl DatabaseSession for SqliteSession {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn cancel(&self) {
        self.interrupt.interrupt();
    }

    fn quote(&self, identifier: &str) -> String {
        quote_ident(identifier)
    }

    async fn execute(&self, sql: &str) -> DbResult<Execution> {
        let sql = sql.to_string();
        let start = Instant::now();
        let results = self.with(move |conn| run_script(conn, &sql)).await?;
        Ok(Execution::new(results, start.elapsed()))
    }

    async fn read_only(&self, text: &str) -> DbResult<Execution> {
        let sql = text.to_string();
        let start = Instant::now();
        let result = self.with(move |conn| read_only_run(conn, &sql)).await?;
        Ok(Execution::new(vec![result], start.elapsed()))
    }

    async fn rehearse(&self, text: &str) -> DbResult<Execution> {
        let sql = text.to_string();
        let start = Instant::now();
        let results = self
            .with(move |conn| in_transaction(conn, &sql, false))
            .await?;
        Ok(Execution::new(results, start.elapsed()))
    }

    async fn execute_atomically(&self, text: &str) -> DbResult<Execution> {
        let sql = text.to_string();
        let start = Instant::now();
        let results = self
            .with(move |conn| in_transaction(conn, &sql, true))
            .await?;
        Ok(Execution::new(results, start.elapsed()))
    }

    async fn relations(&self) -> DbResult<Vec<Relation>> {
        self.with(|conn| {
            let rows = single(
                conn,
                "select name, type from sqlite_schema \
                 where type in ('table', 'view') and name not like 'sqlite\\_%' escape '\\' order by name",
            )?
            .rows;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let kind = if row[1].as_deref() == Some("view") { RelationKind::View } else { RelationKind::Table };
                    Relation::new("main", row[0].clone().unwrap_or_default(), kind, -1.0)
                })
                .collect())
        })
        .await
    }

    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        let (relation, search, sort) = (relation.clone(), search.to_string(), sort.cloned());
        self.with(move |conn| page_of(conn, &relation, limit, offset, &search, sort.as_ref()))
            .await
    }

    /// Exact count; SQLite keeps no estimate.
    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        let (relation, search) = (relation.clone(), search.to_string());
        self.with(move |conn| {
            let clause = if search.is_empty() {
                String::new()
            } else {
                filter(&search, &table_info(conn, &relation)?.columns)
            };
            let result = single(
                conn,
                &format!("select count(*) from {}{clause}", qualified(&relation)),
            )?;
            Ok(result
                .rows
                .first()
                .and_then(|r| r.first())
                .and_then(|v| v.as_deref())
                .and_then(|v| v.parse().ok()))
        })
        .await
    }

    async fn update(
        &self,
        relation: &Relation,
        key: &RowKey,
        changes: &[CellValue],
    ) -> DbResult<EditOutcome> {
        if changes.is_empty() {
            return Ok(EditOutcome {
                sql: String::new(),
                command: "UPDATE 0".into(),
                affected: 0,
            });
        }
        self.require_writable()?;
        let set = changes
            .iter()
            .map(|c| format!("{} = ?", quote_ident(&c.column)))
            .collect::<Vec<_>>()
            .join(", ");
        let (clause, key_params) = where_clause(key)?;
        let sql = format!("update {} set {set} where {clause}", qualified(relation));
        let mut params: Vec<rusqlite::types::Value> = changes
            .iter()
            .map(|c| text_value(c.value.as_deref()))
            .collect();
        params.extend(key_params);
        self.with(move |conn| edit(conn, sql, params, "UPDATE", true))
            .await
    }

    /// Adds a row. Columns left out get their default.
    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        self.require_writable()?;
        let sql = if values.is_empty() {
            format!("insert into {} default values", qualified(relation))
        } else {
            format!(
                "insert into {} ({}) values ({})",
                qualified(relation),
                values
                    .iter()
                    .map(|v| quote_ident(&v.column))
                    .collect::<Vec<_>>()
                    .join(", "),
                values.iter().map(|_| "?").collect::<Vec<_>>().join(", ")
            )
        };
        let params = values
            .iter()
            .map(|v| text_value(v.value.as_deref()))
            .collect();
        self.with(move |conn| edit(conn, sql, params, "INSERT", false))
            .await
    }

    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        self.require_writable()?;
        let (clause, params) = where_clause(key)?;
        let sql = format!("delete from {} where {clause}", qualified(relation));
        self.with(move |conn| edit(conn, sql, params, "DELETE", true))
            .await
    }

    async fn schema_graph(&self) -> DbResult<SchemaGraph> {
        self.with(|conn| {
            let names = single(
                conn,
                "select name, type = 'view' from sqlite_schema \
                 where type in ('table','view') and name not like 'sqlite\\_%' escape '\\' order by name",
            )?
            .rows;
            let mut columns: Vec<Vec<Option<String>>> = Vec::new();
            let mut links: Vec<Vec<Option<String>>> = Vec::new();
            let s = |v: &str| Some(v.to_string());
            for row in &names {
                let Some(name) = row[0].clone() else { continue };
                let quoted = quote_ident(&name);
                for column in single(conn, &format!("pragma table_info({quoted})"))?.rows {
                    // cid, name, type, notnull, dflt_value, pk
                    let not_null = column[3].as_deref() == Some("1");
                    let pk = column[5].as_deref().and_then(|p| p.parse::<i64>().ok()).unwrap_or(0) > 0;
                    columns.push(vec![
                        s("main"),
                        Some(name.clone()),
                        row[1].clone(),
                        column[1].clone(),
                        column[2].clone(),
                        s(if not_null { "0" } else { "1" }),
                        s(if pk { "1" } else { "0" }),
                    ]);
                }
                // id, seq, table, from, to, ...: one entry per column of each foreign key
                let mut grouped: BTreeMap<String, (String, Vec<String>, Vec<String>)> = BTreeMap::new();
                for key in single(conn, &format!("pragma foreign_key_list({quoted})"))?.rows {
                    let entry = grouped.entry(key[0].clone().unwrap_or_default()).or_insert_with(|| {
                        (key[2].clone().unwrap_or_default(), vec![], vec![])
                    });
                    entry.1.push(key[3].clone().unwrap_or_default());
                    entry.2.push(key[4].clone().unwrap_or_default());
                }
                for (id, (to, from, to_columns)) in grouped {
                    links.push(vec![
                        Some(format!("fk_{name}_{id}")),
                        Some(format!("main.{name}")),
                        Some(format!("main.{to}")),
                        Some(from.join("\u{1f}")),
                        Some(to_columns.join("\u{1f}")),
                    ]);
                }
            }
            let mut graph = SchemaGraph::build(&columns, &links);
            // A foreign key without target columns points at the target's primary key.
            let tables = graph.tables.clone();
            for link in &mut graph.links {
                if link.to_columns.iter().all(String::is_empty) {
                    if let Some(target) = tables.iter().find(|t| t.id() == link.to) {
                        link.to_columns = target.columns.iter().filter(|c| c.is_primary_key).map(|c| c.name.clone()).collect();
                    }
                }
            }
            Ok(graph)
        })
        .await
    }

    async fn close(&self) {
        let conn = Arc::clone(&self.conn);
        let _ = tokio::task::spawn_blocking(move || {
            lock(&conn).take();
        })
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_numbers_print_like_sqlite() {
        assert_eq!(real_text(1.5), "1.5");
        assert_eq!(real_text(3.0), "3.0");
        assert_eq!(real_text(-0.25), "-0.25");
        assert_eq!(real_text(100.0), "100.0");
        assert_eq!(real_text(0.1), "0.1");
        assert_eq!(real_text(1e20), "1.0e+20");
        assert_eq!(real_text(1.5e-7), "1.5e-07");
        assert_eq!(real_text(123456789012345.0), "123456789012345.0");
        assert_eq!(real_text(1e15), "1.0e+15");
        assert_eq!(real_text(0.0001), "0.0001");
        assert_eq!(real_text(0.00001), "1.0e-05");
        assert_eq!(real_text(f64::INFINITY), "Inf");
        assert_eq!(real_text(0.0), "0.0");
    }

    #[test]
    fn blobs_are_cut_at_64_bytes() {
        assert_eq!(blob_text(&[]), "0x");
        assert_eq!(blob_text(&[0xde, 0xad]), "0xdead");
        let long = blob_text(&[0xab; 65]);
        assert!(long.ends_with('…'));
        assert_eq!(long.chars().count(), 2 + 128 + 1);
    }

    #[test]
    fn finds_the_first_keyword() {
        assert_eq!(first_keyword("  select 1"), "SELECT");
        assert_eq!(
            first_keyword(";; -- note\n /* x */ insert into t"),
            "INSERT"
        );
        assert_eq!(first_keyword("/* open"), "");
        assert_eq!(first_keyword(""), "");
    }

    #[test]
    fn search_filter_escapes() {
        let cols = vec!["a".to_string(), "b\"c".to_string()];
        assert_eq!(filter("", &cols), "");
        assert_eq!(filter("x", &[]), "");
        let f = filter("it's 50%", &cols);
        assert!(f.contains("coalesce(\"a\", '') || ' ' || coalesce(\"b\"\"c\", '')"));
        assert!(f.contains("like '%it''s 50\\%%' escape '\\'"));
    }

    #[test]
    fn key_clauses() {
        let key = RowKey {
            columns: vec!["a".into(), "b".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![Some("1".into()), None],
        };
        use rusqlite::types::Value;
        let (sql, params) = where_clause(&key).unwrap();
        assert_eq!(sql, "\"a\" = ? and \"b\" is null");
        assert_eq!(params, vec![Value::Text("1".into())]);
        let rowid = RowKey {
            columns: vec!["rowid".into()],
            kind: RowKeyKind::Rowid,
            values: vec![Some("7".into())],
        };
        let (sql, params) = where_clause(&rowid).unwrap();
        assert_eq!(sql, "rowid = ?");
        assert_eq!(params, vec![Value::Integer(7)]);
        let tagged = RowKey {
            columns: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![
                Some("i:1".into()),
                Some("r:0.33333333333333332".into()),
                Some("t:i:x".into()),
                Some("b:01ff".into()),
            ],
        };
        assert_eq!(
            where_clause(&tagged).unwrap().1,
            vec![
                Value::Integer(1),
                Value::Real(1.0 / 3.0),
                Value::Text("i:x".into()),
                Value::Blob(vec![1, 255]),
            ]
        );
        let bad = RowKey {
            values: vec![Some("b:0x01".into())],
            columns: vec!["a".into()],
            kind: RowKeyKind::PrimaryKey,
        };
        assert!(where_clause(&bad).is_err());
    }

    #[test]
    fn summary_reads_paths() {
        let s = summary("sqlite:///tmp/data/app.db").unwrap();
        assert_eq!(s.suggested_name, "app.db");
        assert!(summary("postgres://x/y").is_none());
    }
}
