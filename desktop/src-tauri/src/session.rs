//! What every database driver gives the app: tables, pages of rows, SQL runs,
//! and row edits. Port of `Session.swift`. Every type here crosses the IPC
//! boundary as camelCase JSON; `src/api.ts` mirrors them field for field.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize, Serializer};

use crate::engine::Engine;

/// Results bigger than this are cut, so the grid stays fast.
pub const ROW_LIMIT: usize = 5_000;

/// An error a driver reports, with the server's own message when it has one.
/// It reaches the frontend as a plain string (its Display text).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DbError {
    /// The server's or driver's message, shown as is.
    #[error("{0}")]
    Message(String),
    /// A script can't run all-or-nothing on this database (MySQL DDL, MongoDB,
    /// Redis); the caller may run it plainly instead.
    #[error("{0}")]
    NotAtomic(String),
    /// The query was stopped by `cancel()`.
    #[error("The query was canceled.")]
    Cancelled,
    /// Not built yet, or not something this engine does.
    #[error("{0}")]
    Unsupported(String),
    /// The session id the frontend sent is gone (disconnected, app restarted).
    #[error("This connection is closed. Connect again.")]
    NoSession(String),
}

impl DbError {
    pub fn msg(text: impl Into<String>) -> Self {
        DbError::Message(text.into())
    }

    /// The text the user sees.
    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl Serialize for DbError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type DbResult<T> = Result<T, DbError>;

/// One column to order a table by, chosen by clicking its header.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSort {
    pub column: String,
    pub ascending: bool,
}

impl ColumnSort {
    /// "asc" or "desc", for SQL.
    pub fn direction(&self) -> &'static str {
        if self.ascending {
            "asc"
        } else {
            "desc"
        }
    }

    /// Text values compared the way people read them: numbers by value, NULL last
    /// (in both directions). For sorting results that are already on screen.
    pub fn compare(a: Option<&str>, b: Option<&str>, ascending: bool) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let order = match (a, b) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(a), Some(b)) => match (a.parse::<f64>(), b.parse::<f64>()) {
                (Ok(x), Ok(y)) if x != y => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
                _ => natural_cmp(a, b),
            },
        };
        if ascending {
            order
        } else {
            order.reverse()
        }
    }
}

/// Case-insensitive, with digit runs compared by value ("file2" < "file10"),
/// close to Foundation's localizedStandardCompare.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                let mut m = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                let order = n.len().cmp(&m.len()).then_with(|| n.cmp(m));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(c), Some(d)) => {
                let order = c.to_lowercase().cmp(d.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// `%text%` for LIKE, with LIKE's own wildcards and the escape character
/// escaped by a backslash.
pub fn like_pattern(text: &str) -> String {
    format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// One column of a result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    /// The type name when known ("text", "int(11)", "INTEGER"), or "".
    #[serde(rename = "type", default)]
    pub type_name: String,
}

impl Column {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self { name: name.into(), type_name: type_name.into() }
    }
}

/// What identifies rows of a table page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowKeyKind {
    /// The table's primary-key columns (Redis: the field/index/member; Mongo: _id).
    PrimaryKey,
    /// Postgres without a primary key: ctid, checked with xmin.
    Ctid,
    /// SQLite without a primary key: the rowid.
    Rowid,
}

/// How to find each row of a page again for UPDATE and DELETE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowKeys {
    /// The key columns: the primary key, ["ctid", "xmin"], or ["rowid"].
    pub columns: Vec<String>,
    pub kind: RowKeyKind,
    /// One entry per row of the page, in `columns` order.
    pub values: Vec<Vec<Option<String>>>,
}

impl RowKeys {
    pub fn uses_ctid(&self) -> bool {
        self.kind == RowKeyKind::Ctid
    }

    /// The key of one row, to send back with an edit.
    pub fn row(&self, index: usize) -> Option<RowKey> {
        Some(RowKey { columns: self.columns.clone(), kind: self.kind, values: self.values.get(index)?.clone() })
    }
}

/// The key of the one row an edit targets: `RowKeys` narrowed to a row.
/// (Swift passes `keys` plus a row index; one row is all the IPC needs.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowKey {
    pub columns: Vec<String>,
    pub kind: RowKeyKind,
    /// In `columns` order.
    pub values: Vec<Option<String>>,
}

/// One column's new value in an edit or insert; None is NULL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CellValue {
    pub column: String,
    pub value: Option<String>,
}

/// One statement's result: rows as text (NULL is None), or a command tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Option<String>>>,
    /// What the statement did: "SELECT 3", "UPDATE 2", "CREATE TABLE".
    pub command: String,
    /// True when the result had more rows than `ROW_LIMIT` and was cut.
    #[serde(default)]
    pub truncated: bool,
    pub total_rows: u64,
    /// How to find each row again for UPDATE and DELETE. Only table pages have it.
    #[serde(default)]
    pub keys: Option<RowKeys>,
    /// A short line for the footer beside the row count ("hash · TTL 3,600 s").
    #[serde(default)]
    pub note: Option<String>,
}

impl QueryResult {
    /// A result with rows. `total_rows` is the row count before any cut.
    pub fn rows(columns: Vec<Column>, rows: Vec<Vec<Option<String>>>, command: impl Into<String>) -> Self {
        let total = rows.len() as u64;
        Self { columns, rows, command: command.into(), truncated: false, total_rows: total, keys: None, note: None }
    }

    /// A statement that returned no rows: "UPDATE 2", "CREATE TABLE".
    pub fn command(command: impl Into<String>, affected: u64) -> Self {
        Self { columns: vec![], rows: vec![], command: command.into(), truncated: false, total_rows: affected, keys: None, note: None }
    }

    pub fn has_rows(&self) -> bool {
        !self.columns.is_empty()
    }
}

/// What an edit sent to the server, for the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditOutcome {
    pub sql: String,
    pub command: String,
    pub affected: u64,
}

/// Everything one run of the SQL editor returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Execution {
    pub results: Vec<QueryResult>,
    /// Wall time of the whole run, in milliseconds.
    pub duration_ms: f64,
}

impl Execution {
    pub fn new(results: Vec<QueryResult>, duration: Duration) -> Self {
        Self { results, duration_ms: duration.as_secs_f64() * 1000.0 }
    }

    /// The result to show: the last one with columns, else the last one.
    pub fn main(&self) -> Option<&QueryResult> {
        self.results.iter().rev().find(|r| r.has_rows()).or(self.results.last())
    }
}

/// What a relation is. Redis keys are `Key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelationKind {
    Table,
    Partitioned,
    View,
    Materialized,
    Foreign,
    Key,
}

/// A table or view (or a Redis key, or a Mongo collection).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    /// The schema (Postgres), the database (MySQL, Mongo), "main" (SQLite), or the
    /// key's prefix group (Redis: "user" for "user:42", "" for keys without one).
    pub schema: String,
    pub name: String,
    pub kind: RelationKind,
    /// The server's row estimate, -1 when unknown.
    pub estimate: f64,
    /// What a key holds, for Redis: "string", "hash", "list", "set", "zset",
    /// "stream", "ReJSON-RL". Empty elsewhere.
    #[serde(default)]
    pub subtype: String,
}

impl Relation {
    pub fn new(schema: impl Into<String>, name: impl Into<String>, kind: RelationKind, estimate: f64) -> Self {
        Self { schema: schema.into(), name: name.into(), kind, estimate, subtype: String::new() }
    }

    /// "schema.name", unique within a connection.
    pub fn id(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}

/// The tables of a database and how their foreign keys connect them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaGraph {
    pub tables: Vec<SchemaTable>,
    pub links: Vec<SchemaLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaTable {
    pub schema: String,
    pub name: String,
    pub is_view: bool,
    pub columns: Vec<SchemaColumn>,
}

impl SchemaTable {
    pub fn id(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: String,
    pub is_primary_key: bool,
    pub is_nullable: bool,
}

/// A foreign key: `from_columns` of table `from` ("schema.table") point at
/// `to_columns` of `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaLink {
    pub name: String,
    pub from: String,
    pub from_columns: Vec<String>,
    pub to: String,
    pub to_columns: Vec<String>,
}

impl SchemaGraph {
    /// The schemas that have tables, in order of first appearance.
    pub fn schemas(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for t in &self.tables {
            if !seen.contains(&t.schema) {
                seen.push(t.schema.clone());
            }
        }
        seen
    }

    /// Builds a graph from two result sets: `columns` as (schema, table, is view,
    /// column, type, nullable, primary key) with booleans as t/f, 1/0 or
    /// true/false; `links` as (name, "schema.table", "schema.table", columns,
    /// columns) with columns joined by the unit separator (U+001F).
    pub fn build(columns: &[Vec<Option<String>>], links: &[Vec<Option<String>>]) -> SchemaGraph {
        fn flag(v: &Option<String>) -> bool {
            matches!(v.as_deref().map(str::to_lowercase).as_deref(), Some("t" | "true" | "1" | "yes"))
        }
        fn text(v: &Option<String>) -> String {
            v.clone().unwrap_or_default()
        }
        let mut tables: Vec<SchemaTable> = Vec::new();
        let mut index = std::collections::HashMap::<String, usize>::new();
        for row in columns.iter().filter(|r| r.len() >= 7) {
            let (schema, name) = (text(&row[0]), text(&row[1]));
            let id = format!("{schema}.{name}");
            let at = *index.entry(id).or_insert_with(|| {
                tables.push(SchemaTable { schema, name, is_view: flag(&row[2]), columns: vec![] });
                tables.len() - 1
            });
            tables[at].columns.push(SchemaColumn {
                name: text(&row[3]),
                type_name: text(&row[4]),
                is_primary_key: flag(&row[6]),
                is_nullable: flag(&row[5]),
            });
        }
        let split = |v: &Option<String>| -> Vec<String> {
            text(v).split('\u{1f}').filter(|s| !s.is_empty()).map(String::from).collect()
        };
        let links = links
            .iter()
            .filter(|r| r.len() >= 5)
            .filter_map(|r| {
                let (from, to) = (r[1].clone()?, r[2].clone()?);
                if !index.contains_key(&from) || !index.contains_key(&to) {
                    return None;
                }
                Some(SchemaLink { name: text(&r[0]), from, from_columns: split(&r[3]), to, to_columns: split(&r[4]) })
            })
            .collect();
        SchemaGraph { tables, links }
    }
}

/// What every database driver implements. Methods take `&self`: the app calls
/// `cancel` while a query runs, so a driver keeps its connection behind its own
/// lock (tokio::sync::Mutex) and makes `cancel` work without that lock.
#[async_trait]
pub trait DatabaseSession: Send + Sync {
    fn engine(&self) -> Engine;
    /// The server's version, for the log ("16.4", "8.4.3", "3.51.0").
    fn server_version(&self) -> String;
    /// Stops the running query. Must not wait for the query's lock.
    fn cancel(&self);
    /// Quotes a table or column name for this engine's SQL.
    fn quote(&self, identifier: &str) -> String;

    /// "schema"."name" in this engine's quoting; just the name when there is no schema.
    fn qualified(&self, relation: &Relation) -> String {
        if relation.schema.is_empty() {
            self.quote(&relation.name)
        } else {
            format!("{}.{}", self.quote(&relation.schema), self.quote(&relation.name))
        }
    }

    /// Runs one or more statements, as typed in the SQL editor.
    async fn execute(&self, sql: &str) -> DbResult<Execution>;

    /// Runs one statement that can only read, for agents. The database itself
    /// refuses writes where it can (a read-only transaction, SQLite's read-only
    /// check); MongoDB and Redis allow only reading methods and commands.
    async fn read_only(&self, text: &str) -> DbResult<Execution>;

    /// Runs a script statement by statement in a transaction that is always
    /// rolled back, to see what each would do before running it for real.
    /// Postgres and SQLite take DDL too; MySQL only statements that don't commit
    /// by themselves; MongoDB and Redis can't undo, so they refuse.
    async fn rehearse(&self, _text: &str) -> DbResult<Execution> {
        Err(DbError::msg(format!(
            "{} can't try a statement and undo it; run it for real or not at all.",
            self.engine().name()
        )))
    }

    /// Runs a script all or nothing: one transaction, rolled back if a statement
    /// fails. Returns `DbError::NotAtomic` where the database can't (MySQL DDL,
    /// MongoDB, Redis), so the caller can run it plainly instead.
    async fn execute_atomically(&self, _text: &str) -> DbResult<Execution> {
        Err(DbError::NotAtomic(format!("{} has no transaction dbLens can roll back.", self.engine().name())))
    }

    async fn relations(&self) -> DbResult<Vec<Relation>>;

    /// One page of rows, sorted by the primary key when there is one, with
    /// column types and, when rows can be edited, their keys. A non-empty
    /// `search` keeps only rows with that text in any column (case-insensitive).
    /// `sort` orders by one column first (then by the key, so pages stay stable).
    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult>;

    /// Exact row count (of the rows matching `search`), or None when counting would be slow.
    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>>;

    /// Edits exactly one row (rolled back if it would touch more or none).
    async fn update(&self, relation: &Relation, key: &RowKey, changes: &[CellValue]) -> DbResult<EditOutcome>;
    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome>;
    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome>;

    /// Tables, their columns, and the foreign keys between them, for the schema
    /// board. Engines without tables and foreign keys (Redis) have no board.
    async fn schema_graph(&self) -> DbResult<SchemaGraph> {
        Ok(SchemaGraph::default())
    }

    /// Closes the connection. Called once by `disconnect`; dropping must also be safe.
    async fn close(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern("50%_a\\b"), "%50\\%\\_a\\\\b%");
    }

    #[test]
    fn sorts_like_people_read() {
        assert_eq!(ColumnSort::compare(Some("9"), Some("10"), true), Ordering::Less);
        assert_eq!(ColumnSort::compare(Some("file2"), Some("file10"), true), Ordering::Less);
        assert_eq!(ColumnSort::compare(Some("b"), Some("A"), true), Ordering::Greater);
        assert_eq!(ColumnSort::compare(None, Some("a"), true), Ordering::Greater);
        // NULL stays last in both directions.
        assert_eq!(ColumnSort::compare(None, Some("a"), false), Ordering::Greater);
        assert_eq!(ColumnSort::compare(Some("9"), Some("10"), false), Ordering::Greater);
    }

    #[test]
    fn builds_schema_graphs() {
        let s = |v: &str| Some(v.to_string());
        let columns = vec![
            vec![s("public"), s("customers"), s("f"), s("id"), s("int"), s("f"), s("t")],
            vec![s("public"), s("orders"), s("0"), s("id"), s("int"), s("no"), s("true")],
            vec![s("public"), s("orders"), s("0"), s("customer_id"), s("int"), s("yes"), s("false")],
        ];
        let links = vec![
            vec![s("fk"), s("public.orders"), s("public.customers"), s("customer_id"), s("id")],
            vec![s("gone"), s("public.orders"), s("other.missing"), s("a"), s("b")],
        ];
        let graph = SchemaGraph::build(&columns, &links);
        assert_eq!(graph.tables.len(), 2);
        assert_eq!(graph.tables[1].columns.len(), 2);
        assert!(graph.tables[1].columns[1].is_nullable);
        assert_eq!(graph.links.len(), 1);
        assert_eq!(graph.schemas(), vec!["public".to_string()]);
    }

    #[test]
    fn serializes_camel_case() {
        let result = QueryResult {
            keys: Some(RowKeys { columns: vec!["id".into()], kind: RowKeyKind::PrimaryKey, values: vec![vec![s("1")]] }),
            ..QueryResult::rows(vec![Column::new("id", "int4")], vec![vec![s("1")]], "SELECT 1")
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["totalRows"], 1);
        assert_eq!(json["columns"][0]["type"], "int4");
        assert_eq!(json["keys"]["kind"], "primaryKey");
        assert_eq!(serde_json::to_string(&DbError::NotAtomic("no".into())).unwrap(), "\"no\"");
        assert_eq!(serde_json::to_value(RelationKind::Materialized).unwrap(), "materialized");
        fn s(v: &str) -> Option<String> {
            Some(v.into())
        }
    }

    #[test]
    fn main_result_prefers_rows() {
        let exec = Execution::new(
            vec![QueryResult::rows(vec![Column::new("a", "")], vec![], "SELECT 0"), QueryResult::command("UPDATE 1", 1)],
            Duration::from_millis(5),
        );
        assert_eq!(exec.main().unwrap().command, "SELECT 0");
        assert!((exec.duration_ms - 5.0).abs() < 0.01);
    }
}
