//! MongoDB driver, on the official `mongodb` crate. Port of
//! `Sources/DBLensKit/Mongo.swift`; the command panel's mongosh parsing is in
//! `mongo_shell.rs` (port of `MongoShell.swift`).
//!
//! Collections are the relations (schema = database). Pages and query results
//! are tables: `_id` first, then every top-level field in order of first
//! appearance; nested documents and arrays show as compact relaxed extended JSON.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::TryStreamExt;
use mongodb::bson::{self, doc, Bson, Document};
use mongodb::options::{
    ClientOptions, DeleteOptions, InsertManyOptions, InsertOneOptions, ReplaceOptions,
    UpdateOptions,
};
use mongodb::{Client, Cursor};
use regex::Regex;
use tokio::sync::{Mutex, Notify};

use super::mongo_shell::{self as shell, Call, Statement};
use crate::connector::Summary;
use crate::engine::Engine;
use crate::session::{
    CellValue, Column, ColumnSort, DatabaseSession, DbError, DbResult, EditOutcome, Execution,
    QueryResult, Relation, RelationKind, RowKey, RowKeyKind, RowKeys, SchemaColumn, SchemaGraph,
    SchemaLink, SchemaTable, ROW_LIMIT,
};
use crate::url::UrlParts;
use crate::wire::{StreamSink, FIRST_BLOCK_ROWS};

/// Documents a cursor asks for at a time.
const BATCH_SIZE: i32 = 500;

/// Hosts, database and user for the connection sheet. One host has its own
/// port (for an SSH tunnel); several hosts show as "h1:27017,h2:27018".
pub fn summary(text: &str) -> Option<Summary> {
    let parts =
        UrlParts::new(text).filter(|p| p.scheme == "mongodb" || p.scheme == "mongodb+srv")?;
    let srv = parts.scheme == "mongodb+srv";
    let (host, port) = match parts.hosts.as_slice() {
        [] => (None, None),
        [one] => (Some(one.name.clone()), if srv { None } else { one.port }),
        many => (
            Some(
                many.iter()
                    .map(|h| format!("{}:{}", bracketed(&h.name), h.port.unwrap_or(27017)))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            None,
        ),
    };
    let database = parts.path.clone();
    Some(Summary {
        engine: Engine::Mongodb,
        suggested_name: format!(
            "{} @ {}",
            if database.is_empty() {
                "mongodb"
            } else {
                &database
            },
            parts
                .first_host()
                .map(|h| h.name.as_str())
                .unwrap_or("localhost")
        ),
        database,
        host,
        port,
        user: parts.user.clone().filter(|u| !u.is_empty()),
        tls: None,
    })
}

fn bracketed(name: &str) -> String {
    if name.contains(':') {
        format!("[{name}]")
    } else {
        name.to_string()
    }
}

/// The text the driver gets: the scheme in lowercase, the rest as pasted.
pub fn connection_text(cleaned: &str) -> String {
    match cleaned.find("://") {
        Some(at) => format!("{}{}", cleaned[..at].to_lowercase(), &cleaned[at..]),
        None => cleaned.to_string(),
    }
}

/// The same URL pointed at an SSH tunnel's local end, connecting to that one server directly.
pub fn through_local_port(text: &str, port: u16) -> String {
    let Some(scheme) = text.find("://") else {
        return text.to_string();
    };
    let rest = &text[scheme + 3..];
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let userinfo = authority
        .rfind('@')
        .map(|at| &authority[..=at])
        .unwrap_or("");
    let mut path = rest[authority_end..].to_string();
    let mut query: Vec<String> = Vec::new();
    if let Some(mark) = path.find('?') {
        query = path[mark + 1..]
            .split('&')
            .filter(|q| !q.is_empty())
            .map(String::from)
            .collect();
        path.truncate(mark);
    }
    if path.is_empty() {
        path = "/".into();
    }
    query.retain(|q| !q.to_lowercase().starts_with("directconnection="));
    query.push("directConnection=true".into());
    format!(
        "mongodb://{userinfo}127.0.0.1:{port}{path}?{}",
        query.join("&")
    )
}

/// Opens a session. Through an SSH tunnel, `local_port` replaces the URL's host and port.
pub async fn connect(url: &str, local_port: Option<u16>) -> DbResult<Box<dyn DatabaseSession>> {
    Ok(Box::new(open(url, local_port).await?))
}

/// `connect`, keeping the concrete type (tests read `current_database`).
pub async fn open(url: &str, local_port: Option<u16>) -> DbResult<MongoSession> {
    let cleaned = crate::url::clean(url);
    if cleaned.is_empty() {
        return Err(DbError::msg("Paste the database URL."));
    }
    let lower = cleaned.to_lowercase();
    let parts = UrlParts::new(&cleaned)
        .filter(|p| p.scheme == "mongodb" || p.scheme == "mongodb+srv")
        .filter(|_| lower.starts_with("mongodb://") || lower.starts_with("mongodb+srv://"))
        .ok_or_else(|| {
            DbError::msg("Unrecognized format. Use a mongodb://… URL (or mongodb+srv://)")
        })?;
    if parts.hosts.is_empty() || parts.hosts.iter().any(|h| h.name.is_empty()) {
        return Err(DbError::msg(
            "The URL needs a host, like mongodb://localhost:27017.",
        ));
    }
    let mut text = connection_text(&cleaned);
    if let Some(port) = local_port {
        if parts.scheme == "mongodb+srv" || parts.hosts.len() != 1 {
            return Err(DbError::msg(
                "An SSH tunnel goes to one MongoDB server. Use a mongodb:// URL with a single host (not mongodb+srv://).",
            ));
        }
        text = through_local_port(&text, port);
    }
    let mut options = ClientOptions::parse(&text).await.map_err(fail)?;
    let query = text.to_lowercase();
    if !query.contains("serverselectiontimeoutms=") {
        options.server_selection_timeout = Some(Duration::from_secs(10));
    }
    if !query.contains("connecttimeoutms=") {
        options.connect_timeout = Some(Duration::from_secs(10));
    }
    if options.app_name.is_none() {
        options.app_name = Some("dbLens".into());
    }
    let client = Client::with_options(options).map_err(fail)?;
    let admin = client.database("admin");
    admin.run_command(doc! { "ping": 1 }).await.map_err(fail)?;
    let version = admin
        .run_command(doc! { "buildInfo": 1 })
        .await
        .map_err(fail)?
        .get("version")
        .and_then(cell)
        .unwrap_or_default();
    let database = Some(parts.path.clone()).filter(|p| !p.is_empty());
    Ok(MongoSession {
        canceller: Arc::new(Canceller::new()),
        handle: tokio::runtime::Handle::current(),
        inner: Mutex::new(Inner {
            current_database: database.clone().unwrap_or_else(|| "test".into()),
            column_types: HashMap::new(),
        }),
        url_database: database,
        server_version: version,
        client,
    })
}

/// The driver's error as the server or driver said it.
fn fail(error: mongodb::error::Error) -> DbError {
    use mongodb::error::{ErrorKind, WriteFailure};
    let text = match error.kind.as_ref() {
        ErrorKind::Command(c) => c.message.clone(),
        ErrorKind::Write(WriteFailure::WriteError(w)) => w.message.clone(),
        ErrorKind::Write(WriteFailure::WriteConcernError(w)) => w.message.clone(),
        ErrorKind::InsertMany(m) => m
            .write_errors
            .as_ref()
            .and_then(|w| w.first())
            .map(|w| w.message.clone())
            .or_else(|| m.write_concern_error.as_ref().map(|w| w.message.clone()))
            .unwrap_or_else(|| error.kind.to_string()),
        ErrorKind::Authentication { message, .. } => message.clone(),
        other => other.to_string(),
    };
    if text.trim().is_empty() {
        DbError::msg("The server didn't say what went wrong.")
    } else {
        DbError::Message(text)
    }
}

fn row_changed() -> DbError {
    DbError::msg(
        "The row changed or was deleted since it was loaded. Reload the table and try again.",
    )
}

/// One MongoDB connection (a pooled client). Methods that touch the session's
/// state (the `use` database, each collection's column types) take `inner`, so
/// they run one at a time; `cancel` never does.
pub struct MongoSession {
    client: Client,
    server_version: String,
    /// The database in the URL; without one, relations come from every database.
    url_database: Option<String>,
    inner: Mutex<Inner>,
    canceller: Arc<Canceller>,
    /// Where `cancel` (called from a plain thread) starts its killOp.
    handle: tokio::runtime::Handle,
}

struct Inner {
    /// Where the command panel's statements run: the URL's database (else "test",
    /// as in mongosh) until a `use` switches it.
    current_database: String,
    /// Each collection's column types from its last page, for search and inserts.
    column_types: HashMap<String, HashMap<String, String>>,
}

impl MongoSession {
    /// The database the command panel's statements run in.
    pub async fn current_database(&self) -> String {
        self.inner.lock().await.current_database.clone()
    }

    /// Runs `work` as one cancelable operation: a cancel stops waiting for it
    /// at once (and kills it on the server, see `cancel`).
    async fn cancelable<T>(
        &self,
        operation: &Operation,
        work: impl Future<Output = DbResult<T>>,
    ) -> DbResult<T> {
        let outcome = tokio::select! {
            biased;
            _ = operation.notify.notified() => Err(DbError::Cancelled),
            result = work => result,
        };
        if self.canceller.end() {
            return Err(DbError::Cancelled);
        }
        outcome
    }

    // MARK: Relations

    /// The URL's database, or every database but admin, local and config; in each,
    /// the collections (as tables) and views, without system.* collections.
    async fn all_relations(&self) -> DbResult<Vec<Relation>> {
        let mut databases = match &self.url_database {
            Some(name) => vec![name.clone()],
            None => self
                .client
                .list_database_names()
                .authorized_databases(true)
                .await
                .map_err(fail)?
                .into_iter()
                .filter(|d| !["admin", "local", "config"].contains(&d.as_str()))
                .collect(),
        };
        databases.sort();
        let mut out = Vec::new();
        for database in databases {
            out.extend(self.collections(&database).await?);
        }
        Ok(out)
    }

    async fn collections(&self, database: &str) -> DbResult<Vec<Relation>> {
        let db = self.client.database(database);
        let cursor = db
            .run_cursor_command(
                doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true },
            )
            .await
            .map_err(fail)?;
        let mut found: Vec<(String, String)> = Vec::new();
        drain(cursor, usize::MAX, |doc| {
            let name = doc.get_str("name").unwrap_or_default().to_string();
            let kind = doc.get_str("type").unwrap_or("collection").to_string();
            if !name.is_empty() && !name.starts_with("system.") {
                found.push((name, kind));
            }
        })
        .await?;
        found.sort();
        let mut out = Vec::new();
        for (name, kind) in found {
            let is_view = kind == "view";
            let estimate = if is_view {
                -1.0
            } else {
                db.collection::<Document>(&name)
                    .estimated_document_count()
                    .await
                    .map(|n| n as f64)
                    .unwrap_or(-1.0)
            };
            out.push(Relation {
                schema: database.into(),
                name,
                kind: if is_view {
                    RelationKind::View
                } else {
                    RelationKind::Table
                },
                estimate,
                subtype: kind,
            });
        }
        Ok(out)
    }

    // MARK: Pages

    /// One page of a collection, sorted by _id (after `sort`'s column), as a table.
    /// Rows are found again by their _id, in canonical extended JSON.
    #[allow(clippy::too_many_arguments)]
    async fn page_in(
        &self,
        inner: &mut Inner,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
        tag: &str,
    ) -> DbResult<QueryResult> {
        let mut order = Document::new();
        if let Some(sort) = sort {
            order.insert(sort.column.clone(), if sort.ascending { 1 } else { -1 });
        }
        if !order.contains_key("_id") {
            order.insert("_id", 1);
        }
        let mut command = doc! {
            "find": &relation.name,
            "filter": search_filter(inner, search, relation),
            "sort": order,
            "skip": i64::try_from(offset).unwrap_or(i64::MAX),
            "batchSize": BATCH_SIZE,
            "comment": tag,
        };
        if limit > 0 {
            command.insert("limit", i64::from(limit));
        }
        let keep_ids = relation.kind != RelationKind::View;
        let mut table = DocumentTable::new(keep_ids);
        let cursor = self.cursor(&relation.schema, command, tag).await?;
        drain(cursor, usize::MAX, |doc| table.add(&doc)).await?;
        let mut result = table.result(format!("FIND {}", table.rows.len()));
        if keep_ids {
            result.keys = Some(RowKeys {
                columns: vec!["_id".into()],
                kind: RowKeyKind::PrimaryKey,
                values: table.ids.iter().map(|id| vec![id.clone()]).collect(),
            });
        }
        if relation.estimate >= 0.0 {
            let count = relation.estimate as u64;
            result.note = Some(if count == 1 {
                "1 document".into()
            } else {
                format!("{} documents", grouped(count))
            });
        }
        let mut types = HashMap::new();
        for column in &result.columns {
            types
                .entry(column.name.clone())
                .or_insert_with(|| column.type_name.clone());
        }
        inner.column_types.insert(relation.id(), types);
        Ok(result)
    }

    /// The field names a search needs come from a page without it.
    async fn learn_columns(
        &self,
        inner: &mut Inner,
        relation: &Relation,
        search: &str,
        tag: &str,
    ) -> DbResult<()> {
        if !search.is_empty() && !inner.column_types.contains_key(&relation.id()) {
            self.page_in(inner, relation, 1, 0, "", None, tag).await?;
        }
        Ok(())
    }

    async fn cursor(
        &self,
        database: &str,
        command: Document,
        tag: &str,
    ) -> DbResult<Cursor<Document>> {
        self.client
            .database(database)
            .run_cursor_command(command)
            .batch_size(BATCH_SIZE as u32)
            .comment(Bson::String(tag.into()))
            .await
            .map_err(fail)
    }

    async fn count_documents(
        &self,
        database: &str,
        collection: &str,
        filter: Document,
        tag: &str,
    ) -> DbResult<u64> {
        self.client
            .database(database)
            .collection::<Document>(collection)
            .count_documents(filter)
            .comment(Bson::String(tag.into()))
            .await
            .map_err(fail)
    }

    // MARK: Editing

    /// The BSON type of each field in the document as it is now ("" for a missing or
    /// null field). The "row changed" error when the document is gone.
    async fn field_types(
        &self,
        relation: &Relation,
        filter: &Document,
        fields: &[&str],
    ) -> DbResult<HashMap<String, String>> {
        let mut projection = Document::new();
        for field in fields {
            projection.insert(*field, 1);
        }
        let found = self
            .client
            .database(&relation.schema)
            .collection::<Document>(&relation.name)
            .find_one(filter.clone())
            .projection(projection)
            .await
            .map_err(fail)?
            .ok_or_else(row_changed)?;
        Ok(found
            .iter()
            .map(|(key, value)| {
                let kind = type_name(value);
                (
                    key.clone(),
                    if kind == "null" {
                        String::new()
                    } else {
                        kind.to_string()
                    },
                )
            })
            .collect())
    }

    // MARK: Schema board

    async fn sample_graph(&self, relations: &[Relation], tag: &str) -> DbResult<SchemaGraph> {
        let mut tables: Vec<SchemaTable> = Vec::new();
        let mut object_id_fields: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for relation in relations {
            let mut stats = FieldStats::default();
            let command = doc! {
                "aggregate": &relation.name,
                "pipeline": [{ "$sample": { "size": 100 } }],
                "cursor": { "batchSize": 100 },
                "comment": tag,
            };
            let cursor = self.cursor(&relation.schema, command, tag).await?;
            drain(cursor, 100, |doc| stats.add(&doc)).await?;
            let columns = stats.columns();
            for column in columns
                .iter()
                .filter(|c| c.type_name == "objectId" && !c.is_primary_key)
            {
                object_id_fields
                    .entry(relation.id())
                    .or_default()
                    .push((relation.schema.clone(), column.name.clone()));
            }
            tables.push(SchemaTable {
                schema: relation.schema.clone(),
                name: relation.name.clone(),
                is_view: relation.kind == RelationKind::View,
                columns,
            });
        }
        let names: HashSet<String> = tables.iter().map(SchemaTable::id).collect();
        let mut links = Vec::new();
        for (table, fields) in object_id_fields {
            let found: Vec<(String, String)> = fields
                .iter()
                .filter_map(|(database, field)| {
                    let base = referenced_name(field)?;
                    let lower = base.to_lowercase();
                    [
                        base.clone(),
                        format!("{base}s"),
                        lower.clone(),
                        format!("{lower}s"),
                    ]
                    .iter()
                    .map(|name| format!("{database}.{name}"))
                    .find(|id| names.contains(id))
                    .map(|target| (field.clone(), target))
                })
                .collect();
            let single = found.len() == 1;
            for (field, target) in found {
                links.push(SchemaLink {
                    name: if single {
                        "guessed".into()
                    } else {
                        format!("guessed ({field})")
                    },
                    from: table.clone(),
                    from_columns: vec![field],
                    to: target,
                    to_columns: vec!["_id".into()],
                });
            }
        }
        Ok(SchemaGraph { tables, links })
    }

    // MARK: Command panel

    async fn run(
        &self,
        inner: &mut Inner,
        parsed: Statement,
        statement: &str,
        tag: &str,
        sink: Option<&mut StreamSink>,
    ) -> DbResult<QueryResult> {
        match parsed {
            Statement::Use(name) => {
                let command = format!("USE {name}");
                inner.current_database = name;
                Ok(QueryResult::command(command, 0))
            }
            Statement::ShowDatabases => {
                let args = ["{listDatabases: 1, authorizedDatabases: true}".to_string()];
                self.run_database_method(inner, "adminCommand", &args, statement, Some("databases"))
                    .await
            }
            Statement::ShowCollections => {
                self.run_database_method(inner, "getCollectionNames", &[], statement, None)
                    .await
            }
            Statement::Database { method, arguments } => {
                self.run_database_method(inner, &method, &arguments, statement, None)
                    .await
            }
            Statement::Collection { name, calls } => {
                self.run_collection_method(inner, &name, &calls, statement, tag, sink)
                    .await
            }
        }
    }

    /// A command's reply as one row, or (with `list`) the documents of one of its arrays.
    async fn command_result(
        &self,
        database: &str,
        command: Document,
        list: Option<&str>,
    ) -> DbResult<QueryResult> {
        let reply = self
            .client
            .database(database)
            .run_command(command)
            .await
            .map_err(fail)?;
        let mut table = DocumentTable::new(false);
        match list {
            None => {
                table.add(&reply);
                Ok(table.result("OK".into()))
            }
            Some(key) => {
                for item in reply
                    .get_array(key)
                    .map(|a| a.as_slice())
                    .unwrap_or_default()
                {
                    if let Bson::Document(doc) = item {
                        table.add(doc);
                    }
                }
                Ok(table.result(format!("FIND {}", table.rows.len())))
            }
        }
    }

    async fn run_database_method(
        &self,
        inner: &mut Inner,
        method: &str,
        arguments: &[String],
        statement: &str,
        list: Option<&str>,
    ) -> DbResult<QueryResult> {
        let database = inner.current_database.clone();
        match (method, arguments.len()) {
            ("runCommand", 1) => {
                self.command_result(&database, command_document(&arguments[0])?, list)
                    .await
            }
            ("adminCommand", 1) => {
                self.command_result("admin", command_document(&arguments[0])?, list)
                    .await
            }
            ("getCollectionNames", 0) => {
                let names: Vec<Vec<Option<String>>> = self
                    .collections(&database)
                    .await?
                    .into_iter()
                    .map(|r| vec![Some(r.name)])
                    .collect();
                let count = names.len();
                Ok(QueryResult::rows(
                    vec![Column::new("name", "string")],
                    names,
                    format!("FIND {count}"),
                ))
            }
            ("getName", 0) => Ok(QueryResult::rows(
                vec![Column::new("value", "string")],
                vec![vec![Some(database)]],
                "FIND 1",
            )),
            ("stats", 0) => {
                self.command_result(&database, doc! { "dbStats": 1 }, None)
                    .await
            }
            ("createCollection", 1 | 2) => {
                let name = shell::string_argument(&arguments[0])
                    .ok_or_else(|| shell::unsupported(statement))?;
                let mut command = doc! { "create": name };
                if arguments.len() == 2 {
                    command.extend(document(&arguments[1])?);
                }
                self.command_result(&database, command, None).await?;
                Ok(QueryResult::command("CREATE COLLECTION", 0))
            }
            ("dropDatabase", 0) => {
                self.command_result(&database, doc! { "dropDatabase": 1 }, None)
                    .await?;
                Ok(QueryResult::command("DROP DATABASE", 0))
            }
            _ => Err(shell::unsupported(statement)),
        }
    }

    async fn run_collection_method(
        &self,
        inner: &mut Inner,
        name: &str,
        calls: &[Call],
        statement: &str,
        tag: &str,
        sink: Option<&mut StreamSink>,
    ) -> DbResult<QueryResult> {
        let database = inner.current_database.clone();
        let main = &calls[0];
        let modifiers: Vec<&Call> = calls[1..]
            .iter()
            .filter(|c| c.name != "pretty" && c.name != "toArray")
            .collect();
        let args = &main.arguments;
        let unsupported = || shell::unsupported(statement);
        let doc_at = |index: usize| -> DbResult<Document> {
            args.get(index)
                .map_or_else(|| Ok(Document::new()), |a| document(a))
        };
        let no_modifiers = || {
            if modifiers.is_empty() {
                Ok(())
            } else {
                Err(unsupported())
            }
        };
        let written =
            |verb: &str, count: u64| QueryResult::command(format!("{verb} {count}"), count);
        let collection = self.client.database(&database).collection::<Document>(name);

        match main.name.as_str() {
            "find" | "findOne" => {
                if args.len() > 2 {
                    return Err(unsupported());
                }
                let filter = doc_at(0)?;
                let mut command = doc! { "find": name, "filter": filter.clone(), "batchSize": BATCH_SIZE, "comment": tag };
                if args.len() == 2 {
                    command.insert("projection", doc_at(1)?);
                }
                if main.name == "findOne" {
                    command.insert("limit", 1);
                    command.insert("singleBatch", true);
                }
                let mut counting = false;
                for call in &modifiers {
                    let arg = call.arguments.first();
                    match (call.name.as_str(), call.arguments.len(), arg) {
                        ("sort" | "projection" | "collation", 1, Some(a)) => {
                            command.insert(call.name.clone(), document(a)?);
                        }
                        ("hint" | "comment", 1, Some(a)) => {
                            command.insert(call.name.clone(), value(a)?);
                        }
                        ("limit", 1, Some(a)) if main.name == "find" => {
                            // limit(-n): n documents in one batch, as in mongosh.
                            let n = integer(a)?;
                            if n < 0 {
                                command.insert("singleBatch", true);
                            }
                            if n != 0 {
                                command.insert("limit", n.saturating_abs());
                            }
                        }
                        ("skip" | "maxTimeMS" | "batchSize", 1, Some(a)) => {
                            command.insert(call.name.clone(), integer(a)?);
                        }
                        ("count" | "countDocuments", 0, _) => counting = true,
                        _ => return Err(unsupported()),
                    }
                }
                if counting {
                    return Ok(scalar(
                        self.count_documents(&database, name, filter, tag).await?,
                        "long",
                        "COUNT",
                    ));
                }
                self.documents_to("FIND", &database, command, tag, sink)
                    .await
            }

            "aggregate" => {
                no_modifiers()?;
                if !(1..=2).contains(&args.len()) {
                    return Err(unsupported());
                }
                let stages = shell::extended_json(&args[0])?;
                if !stages.starts_with('[') {
                    return Err(DbError::msg(
                        "aggregate takes a list of stages, like [{$match: {…}}].",
                    ));
                }
                let mut cursor = doc! { "batchSize": BATCH_SIZE };
                let mut command = doc! { "aggregate": name, "pipeline": bson_from_json(&stages)?, "comment": tag };
                for (key, value) in doc_at(1)? {
                    match key.as_str() {
                        "batchSize" => {
                            cursor.insert("batchSize", value);
                        }
                        "cursor" => {
                            if let Bson::Document(given) = value {
                                cursor.extend(given);
                            }
                        }
                        _ => {
                            command.insert(key, value);
                        }
                    }
                }
                command.insert("cursor", cursor);
                self.documents_to("AGGREGATE", &database, command, tag, sink)
                    .await
            }

            "countDocuments" | "count" => {
                no_modifiers()?;
                if args.len() > 1 {
                    return Err(unsupported());
                }
                Ok(scalar(
                    self.count_documents(&database, name, doc_at(0)?, tag)
                        .await?,
                    "long",
                    "COUNT",
                ))
            }

            "estimatedDocumentCount" => {
                no_modifiers()?;
                if !args.is_empty() {
                    return Err(unsupported());
                }
                Ok(scalar(
                    collection.estimated_document_count().await.map_err(fail)?,
                    "long",
                    "COUNT",
                ))
            }

            "distinct" => {
                no_modifiers()?;
                if !(1..=2).contains(&args.len()) {
                    return Err(unsupported());
                }
                let field = shell::string_argument(&args[0]).ok_or_else(unsupported)?;
                let command =
                    doc! { "distinct": name, "key": field, "query": doc_at(1)?, "comment": tag };
                let reply = self
                    .client
                    .database(&database)
                    .run_command(command)
                    .await
                    .map_err(fail)?;
                let mut rows = Vec::new();
                let mut kind: Option<&'static str> = None;
                let mut truncated = false;
                for value in reply
                    .get_array("values")
                    .map(|a| a.as_slice())
                    .unwrap_or_default()
                {
                    if rows.len() >= ROW_LIMIT {
                        truncated = true;
                        break;
                    }
                    if kind.is_none() && !matches!(value, Bson::Null) {
                        kind = Some(type_name(value));
                    }
                    rows.push(vec![cell(value)]);
                }
                let count = rows.len();
                let mut result = QueryResult::rows(
                    vec![Column::new("value", kind.unwrap_or(""))],
                    rows,
                    format!("DISTINCT {count}"),
                );
                result.truncated = truncated;
                Ok(result)
            }

            "getIndexes" => {
                no_modifiers()?;
                self.documents("FIND", &database, doc! { "listIndexes": name }, tag)
                    .await
            }

            "insertOne" => {
                no_modifiers()?;
                if !(1..=2).contains(&args.len()) {
                    return Err(unsupported());
                }
                let options: InsertOneOptions = options(doc_at(1)?)?;
                collection
                    .insert_one(doc_at(0)?)
                    .with_options(options)
                    .await
                    .map_err(fail)?;
                Ok(written("INSERT", 1))
            }

            "insertMany" => {
                no_modifiers()?;
                if !(1..=2).contains(&args.len()) {
                    return Err(unsupported());
                }
                let list = shell::extended_json(&args[0])?;
                if !list.starts_with('[') {
                    return Err(DbError::msg(
                        "insertMany takes a list of documents, like [{a: 1}, {a: 2}].",
                    ));
                }
                let docs = documents_in(
                    &list,
                    "Each item of the list must be a document, like {name: \"Ana\"}.",
                )?;
                if docs.is_empty() {
                    return Ok(written("INSERT", 0));
                }
                let options: InsertManyOptions = options(doc_at(1)?)?;
                let inserted = collection
                    .insert_many(docs)
                    .with_options(options)
                    .await
                    .map_err(fail)?;
                Ok(written("INSERT", inserted.inserted_ids.len() as u64))
            }

            "updateOne" | "updateMany" | "replaceOne" => {
                no_modifiers()?;
                if !(2..=3).contains(&args.len()) {
                    return Err(unsupported());
                }
                let filter = doc_at(0)?;
                let change = shell::extended_json(&args[1])?;
                let result = if main.name == "replaceOne" {
                    let options: ReplaceOptions = options(doc_at(2)?)?;
                    collection
                        .replace_one(filter, document(&args[1])?)
                        .with_options(options)
                        .await
                } else {
                    let options: UpdateOptions = options(doc_at(2)?)?;
                    // An update pipeline: [{$set: …}, …]
                    let update: mongodb::options::UpdateModifications = if change.starts_with('[') {
                        documents_in(&change, "Each stage of an update pipeline must be a document, like {$set: {a: 1}}.")?.into()
                    } else {
                        document(&args[1])?.into()
                    };
                    if main.name == "updateOne" {
                        collection
                            .update_one(filter, update)
                            .with_options(options)
                            .await
                    } else {
                        collection
                            .update_many(filter, update)
                            .with_options(options)
                            .await
                    }
                };
                Ok(written("UPDATE", result.map_err(fail)?.modified_count))
            }

            "deleteOne" | "deleteMany" => {
                no_modifiers()?;
                if !(1..=2).contains(&args.len()) {
                    return Err(unsupported());
                }
                let options: DeleteOptions = options(doc_at(1)?)?;
                let filter = doc_at(0)?;
                let result = if main.name == "deleteOne" {
                    collection.delete_one(filter).with_options(options).await
                } else {
                    collection.delete_many(filter).with_options(options).await
                };
                Ok(written("DELETE", result.map_err(fail)?.deleted_count))
            }

            "drop" => {
                no_modifiers()?;
                if !args.is_empty() {
                    return Err(unsupported());
                }
                if let Err(error) = collection.drop().await {
                    // Dropping a collection that isn't there is not an error in mongosh.
                    let missing = matches!(error.kind.as_ref(), mongodb::error::ErrorKind::Command(c) if c.code == 26);
                    if !missing {
                        return Err(fail(error));
                    }
                }
                Ok(QueryResult::command("DROP", 0))
            }

            "createIndex" => {
                no_modifiers()?;
                if !(1..=2).contains(&args.len()) {
                    return Err(unsupported());
                }
                let keys = doc_at(0)?;
                let given = doc_at(1)?;
                let index_name = match given.get("name") {
                    Some(name) => cell(name).unwrap_or_default(),
                    None => index_name(&keys),
                };
                let mut index = doc! { "key": keys, "name": &index_name };
                index.extend(given);
                let command = doc! { "createIndexes": name, "indexes": [index] };
                self.client
                    .database(&database)
                    .run_command(command)
                    .await
                    .map_err(fail)?;
                Ok(QueryResult::rows(
                    vec![Column::new("value", "string")],
                    vec![vec![Some(index_name)]],
                    "CREATE INDEX",
                ))
            }

            _ => Err(unsupported()),
        }
    }

    /// The documents of a cursor command as a table, cut at `ROW_LIMIT`.
    async fn documents(
        &self,
        verb: &str,
        database: &str,
        command: Document,
        tag: &str,
    ) -> DbResult<QueryResult> {
        let mut table = DocumentTable::new(false);
        let cursor = self.cursor(database, command, tag).await?;
        let more = drain(cursor, ROW_LIMIT, |doc| table.add(&doc)).await?;
        let mut result = table.result(format!("{verb} {}", table.rows.len()));
        result.truncated = more;
        Ok(result)
    }

    /// The documents of a cursor command: streamed to `sink` when there is
    /// one (the result is then ended already), else as a table.
    async fn documents_to(
        &self,
        verb: &str,
        database: &str,
        command: Document,
        tag: &str,
        sink: Option<&mut StreamSink>,
    ) -> DbResult<QueryResult> {
        match sink {
            Some(sink) => {
                self.stream_documents(verb, database, command, tag, sink)
                    .await
            }
            None => self.documents(verb, database, command, tag).await,
        }
    }

    /// Streams the documents of a cursor command. The columns come from the
    /// first documents (500, `_id` first); a key that shows up later is
    /// appended as a new column. Rows past the sink's cap are not read: the
    /// result ends cut, with one more row counted.
    async fn stream_documents(
        &self,
        verb: &str,
        database: &str,
        command: Document,
        tag: &str,
        sink: &mut StreamSink,
    ) -> DbResult<QueryResult> {
        let mut cursor = self.cursor(database, command, tag).await?;
        let mut first = Vec::new();
        while first.len() < FIRST_BLOCK_ROWS {
            match cursor.try_next().await.map_err(fail)? {
                Some(doc) => first.push(doc),
                None => break,
            }
        }
        let mut columns = StreamColumns::default();
        for doc in &first {
            columns.learn(doc);
        }
        let started = sink.result_index();
        if columns.names.is_empty() {
            // Nothing to show as a grid: as `execute` does, only the count.
            let count = first.len() as u64;
            sink.command(&format!("{verb} {count}"), count);
            return Ok(QueryResult::command(verb, 0));
        }
        columns.id_first();
        sink.begin(&columns.columns());
        let mut pending = first.into_iter();
        let mut cut = false;
        loop {
            if !sink.is_open() {
                return Err(DbError::Cancelled);
            }
            let doc = match pending.next() {
                Some(doc) => doc,
                None => match cursor.try_next().await.map_err(fail)? {
                    Some(doc) => doc,
                    None => break,
                },
            };
            if !sink.keeping() {
                // One document past the cap: enough to say the result is cut.
                sink.end_row();
                cut = true;
                break;
            }
            let fresh = columns.learn(&doc);
            if fresh > 0 {
                sink.add_columns(&columns.columns()[columns.names.len() - fresh..]);
            }
            let mut row: Vec<Option<String>> = vec![None; columns.names.len()];
            for (key, value) in &doc {
                row[columns.index[key]] = cell(value);
            }
            for value in &row {
                sink.cell(value.as_deref());
            }
            sink.end_row();
        }
        let shown = sink.seen();
        sink.end(&format!("{verb} {shown}{}", if cut { "+" } else { "" }));
        debug_assert!(sink.result_index() > started);
        Ok(QueryResult::command(verb, 0))
    }
}

/// The columns of a streamed result: the keys seen so far, in the order the
/// grid shows them (`_id` first among the first documents, later keys appended).
#[derive(Default)]
struct StreamColumns {
    names: Vec<String>,
    index: HashMap<String, usize>,
    types: Vec<Option<&'static str>>,
}

impl StreamColumns {
    /// Learns the keys and types of one document; the number of new columns.
    fn learn(&mut self, doc: &Document) -> usize {
        let before = self.names.len();
        for (key, value) in doc {
            let at = match self.index.get(key) {
                Some(&at) => at,
                None => {
                    self.index.insert(key.clone(), self.names.len());
                    self.names.push(key.clone());
                    self.types.push(None);
                    self.names.len() - 1
                }
            };
            let kind = type_name(value);
            if self.types[at].is_none() || (self.types[at] == Some("null") && kind != "null") {
                self.types[at] = Some(kind);
            }
        }
        self.names.len() - before
    }

    /// Moves `_id` to the front (before the grid has columns).
    fn id_first(&mut self) {
        if let Some(&at) = self.index.get("_id") {
            if at != 0 {
                let name = self.names.remove(at);
                let kind = self.types.remove(at);
                self.names.insert(0, name);
                self.types.insert(0, kind);
                for (i, name) in self.names.iter().enumerate() {
                    self.index.insert(name.clone(), i);
                }
            }
        }
    }

    fn columns(&self) -> Vec<Column> {
        self.names
            .iter()
            .zip(&self.types)
            .map(|(name, kind)| Column::new(name.clone(), kind.unwrap_or("")))
            .collect()
    }
}

/// Methods that start a read, and the ones that may follow them in a chain.
const READING_METHODS: [&str; 7] = [
    "find",
    "findOne",
    "aggregate",
    "countDocuments",
    "estimatedDocumentCount",
    "distinct",
    "count",
];
const CURSOR_METHODS: [&str; 12] = [
    "sort",
    "limit",
    "skip",
    "projection",
    "toArray",
    "pretty",
    "count",
    "hint",
    "maxTimeMS",
    "batchSize",
    "collation",
    "comment",
];
const READING_DATABASE_METHODS: [&str; 4] = [
    "getCollectionNames",
    "getCollectionInfos",
    "getName",
    "stats",
];

#[async_trait]
impl DatabaseSession for MongoSession {
    fn engine(&self) -> Engine {
        Engine::Mongodb
    }

    fn server_version(&self) -> String {
        self.server_version.clone()
    }

    /// Stops the running operation at once and kills it on the server: the pool's
    /// other connection finds it by the comment every read carries (killOp).
    /// A single write or runCommand carries no comment; only the wait for it stops.
    fn cancel(&self) {
        if let Some(tag) = self.canceller.request() {
            let client = self.client.clone();
            self.handle.spawn(kill(client, tag));
        }
    }

    /// MongoDB names need no quoting.
    fn quote(&self, identifier: &str) -> String {
        identifier.to_string()
    }

    /// Runs mongosh-style statements, one after another (see `mongo_shell::statements`).
    async fn execute(&self, text: &str) -> DbResult<Execution> {
        let start = Instant::now();
        let statements = shell::statements(text);
        let mut inner = self.inner.lock().await;
        let operation = self.canceller.begin();
        let work = async {
            let mut results = Vec::with_capacity(statements.len());
            for statement in &statements {
                let parsed = shell::parse(statement)?;
                results.push(
                    self.run(&mut inner, parsed, statement, &operation.tag, None)
                        .await?,
                );
            }
            Ok(results)
        };
        let results = self.cancelable(&operation, work).await?;
        Ok(Execution::new(results, start.elapsed()))
    }

    /// Like `execute`, with the documents of find and aggregate sent to the
    /// sink as the cursor delivers them (up to its row cap).
    async fn execute_stream(&self, text: &str, sink: &mut StreamSink) -> DbResult<()> {
        let statements = shell::statements(text);
        let mut inner = self.inner.lock().await;
        let operation = self.canceller.begin();
        let work = async {
            for statement in &statements {
                let parsed = shell::parse(statement)?;
                let before = sink.result_index();
                let result = self
                    .run(
                        &mut inner,
                        parsed,
                        statement,
                        &operation.tag,
                        Some(&mut *sink),
                    )
                    .await?;
                if sink.result_index() == before {
                    sink.send_result(&result);
                }
            }
            Ok(())
        };
        self.cancelable(&operation, work).await
    }

    /// One statement that only reads: find, aggregate (without $out or $merge),
    /// counts, distinct, and listing. MongoDB has no read-only session, so
    /// anything else is refused before it reaches the server.
    async fn read_only(&self, text: &str) -> DbResult<Execution> {
        let statements = shell::statements(text);
        if statements.len() != 1 {
            return Err(DbError::msg("Send one statement at a time."));
        }
        let refused = || {
            DbError::msg("This statement can change data; read-only queries only find, aggregate, count, and list.")
        };
        match shell::parse(&statements[0])? {
            Statement::ShowDatabases | Statement::ShowCollections => {}
            Statement::Use(_) => return Err(refused()),
            Statement::Database { method, .. } => {
                if !READING_DATABASE_METHODS.contains(&method.as_str()) {
                    return Err(refused());
                }
            }
            Statement::Collection { calls, .. } => {
                let first = &calls[0];
                if !READING_METHODS.contains(&first.name.as_str())
                    || !calls[1..]
                        .iter()
                        .all(|c| CURSOR_METHODS.contains(&c.name.as_str()))
                {
                    return Err(refused());
                }
                if first.name == "aggregate"
                    && first
                        .arguments
                        .iter()
                        .any(|a| a.contains("$out") || a.contains("$merge"))
                {
                    return Err(refused());
                }
            }
        }
        self.execute(text).await
    }

    async fn relations(&self) -> DbResult<Vec<Relation>> {
        self.all_relations().await
    }

    async fn page(
        &self,
        relation: &Relation,
        limit: u32,
        offset: u64,
        search: &str,
        sort: Option<&ColumnSort>,
    ) -> DbResult<QueryResult> {
        let mut inner = self.inner.lock().await;
        let operation = self.canceller.begin();
        let work = async {
            self.learn_columns(&mut inner, relation, search, &operation.tag)
                .await?;
            self.page_in(
                &mut inner,
                relation,
                limit,
                offset,
                search,
                sort,
                &operation.tag,
            )
            .await
        };
        self.cancelable(&operation, work).await
    }

    /// countDocuments, or None when the estimate is over 500,000.
    async fn count(&self, relation: &Relation, search: &str) -> DbResult<Option<u64>> {
        if relation.estimate > 500_000.0 {
            return Ok(None);
        }
        let mut inner = self.inner.lock().await;
        let operation = self.canceller.begin();
        let work = async {
            self.learn_columns(&mut inner, relation, search, &operation.tag)
                .await?;
            let filter = search_filter(&inner, search, relation);
            self.count_documents(&relation.schema, &relation.name, filter, &operation.tag)
                .await
        };
        self.cancelable(&operation, work).await.map(Some)
    }

    /// Sets the changed fields of one document, found by its _id, in one updateOne.
    /// Each value keeps the type the field has in that document (see `extended_json_from`).
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
        if changes.iter().any(|c| c.column == "_id") {
            return Err(DbError::msg(
                "The _id of a document can't be changed. Insert a copy with the new _id and delete this one.",
            ));
        }
        let filter = id_filter(key)?;
        let fields: Vec<&str> = changes.iter().map(|c| c.column.as_str()).collect();
        let types = self.field_types(relation, &filter, &fields).await?;
        let mut set = Document::new();
        for change in changes {
            let json = extended_json_from(
                change.value.as_deref(),
                types.get(&change.column).map_or("", String::as_str),
            )?;
            set.insert(change.column.clone(), bson_from_json(&json)?);
        }
        let result = self
            .client
            .database(&relation.schema)
            .collection::<Document>(&relation.name)
            .update_one(filter, doc! { "$set": set })
            .await
            .map_err(fail)?;
        if result.matched_count != 1 {
            return Err(row_changed());
        }
        let shown = fields
            .iter()
            .map(|f| format!("{}: ?", shell_key(f)))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(EditOutcome {
            sql: format!(
                "db.{}.updateOne({{_id: ?}}, {{$set: {{{shown}}}}})",
                shell_name(&relation.name)
            ),
            command: "UPDATE 1".into(),
            affected: 1,
        })
    }

    /// Inserts one document with the given fields. An empty _id is left out, so
    /// the driver makes an ObjectId. Values take the types of the collection's
    /// last page when the text reads as that type (else as it reads on its own).
    async fn insert(&self, relation: &Relation, values: &[CellValue]) -> DbResult<EditOutcome> {
        let mut inner = self.inner.lock().await;
        if !inner.column_types.contains_key(&relation.id()) {
            // Pages may have gone to other pool connections: learn the types here.
            let operation = self.canceller.begin();
            let learned = self.page_in(&mut inner, relation, 1, 0, "", None, &operation.tag);
            let outcome = self.cancelable(&operation, learned).await;
            outcome.map(drop).or_else(|e| match e {
                DbError::Cancelled => Err(e),
                _ => Ok(()),
            })?;
        }
        let empty = HashMap::new();
        let types = inner.column_types.get(&relation.id()).unwrap_or(&empty);
        let fields: Vec<&CellValue> = values
            .iter()
            .filter(|v| !(v.column == "_id" && v.value.as_deref().unwrap_or("").is_empty()))
            .collect();
        let mut doc = Document::new();
        for field in &fields {
            let text = field.value.as_deref();
            let like = types.get(&field.column).map_or("", String::as_str);
            let json = extended_json_from(text, like).or_else(|_| extended_json_from(text, ""))?;
            doc.insert(field.column.clone(), bson_from_json(&json)?);
        }
        drop(inner);
        self.client
            .database(&relation.schema)
            .collection::<Document>(&relation.name)
            .insert_one(doc)
            .await
            .map_err(fail)?;
        let shown = fields
            .iter()
            .map(|f| format!("{}: ?", shell_key(&f.column)))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(EditOutcome {
            sql: format!("db.{}.insertOne({{{shown}}})", shell_name(&relation.name)),
            command: "INSERT 1".into(),
            affected: 1,
        })
    }

    /// Deletes one document, found by its _id.
    async fn delete(&self, relation: &Relation, key: &RowKey) -> DbResult<EditOutcome> {
        let filter = id_filter(key)?;
        let result = self
            .client
            .database(&relation.schema)
            .collection::<Document>(&relation.name)
            .delete_one(filter)
            .await
            .map_err(fail)?;
        if result.deleted_count != 1 {
            return Err(row_changed());
        }
        Ok(EditOutcome {
            sql: format!("db.{}.deleteOne({{_id: ?}})", shell_name(&relation.name)),
            command: "DELETE 1".into(),
            affected: 1,
        })
    }

    /// Each collection's top-level fields from a sample of up to 100 documents:
    /// the most common type, nullable when a field is sometimes missing or null,
    /// `_id` as the key. MongoDB has no foreign keys; a field named `<name>_id` or
    /// `<name>Id` holding ObjectIds gets a link named "guessed" to a collection
    /// `<name>` or `<name>s` of the same database ("guessed (field)" when a
    /// collection has more than one, so link ids stay unique).
    async fn schema_graph(&self) -> DbResult<SchemaGraph> {
        let relations = self.all_relations().await?;
        let operation = self.canceller.begin();
        self.cancelable(&operation, self.sample_graph(&relations, &operation.tag))
            .await
    }

    async fn close(&self) {
        self.client.clone().shutdown().immediate(true).await;
    }
}

// MARK: Canceling

/// One cancelable operation: the comment its reads carry, and what wakes it on cancel.
struct Operation {
    tag: String,
    notify: Arc<Notify>,
}

/// Whether an operation runs and whether a cancel came for it, shared with `cancel()`.
struct Canceller {
    state: StdMutex<CancelState>,
    session: String,
}

struct CancelState {
    running: bool,
    requested: bool,
    generation: u64,
    notify: Arc<Notify>,
}

impl Canceller {
    fn new() -> Self {
        Self {
            state: StdMutex::new(CancelState {
                running: false,
                requested: false,
                generation: 0,
                notify: Arc::new(Notify::new()),
            }),
            session: uuid::Uuid::new_v4().to_string(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CancelState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tag(&self, generation: u64) -> String {
        format!("dbLens {} #{generation}", self.session)
    }

    /// Starts an operation.
    fn begin(&self) -> Operation {
        let mut state = self.lock();
        state.generation += 1;
        state.running = true;
        state.requested = false;
        state.notify = Arc::new(Notify::new());
        Operation {
            tag: self.tag(state.generation),
            notify: state.notify.clone(),
        }
    }

    /// Ends an operation; true when it was canceled.
    fn end(&self) -> bool {
        let mut state = self.lock();
        let requested = state.requested;
        state.running = false;
        state.requested = false;
        requested
    }

    /// Marks the running operation canceled and wakes it; returns its comment
    /// so it can be killed on the server. None when nothing runs.
    fn request(&self) -> Option<String> {
        let mut state = self.lock();
        if !state.running || state.requested {
            return None;
        }
        state.requested = true;
        state.notify.notify_one();
        Some(self.tag(state.generation))
    }
}

/// Looks for the operation by its comment and kills it; tries for a few
/// seconds, as the operation may not have reached the server yet.
async fn kill(client: Client, tag: String) {
    let admin = client.database("admin");
    for _ in 0..20 {
        let find = doc! {
            "aggregate": 1,
            "pipeline": [
                { "$currentOp": { "allUsers": false, "idleCursors": false } },
                { "$match": { "$or": [{ "command.comment": &tag }, { "cursor.originatingCommand.comment": &tag }] } },
                { "$project": { "opid": 1 } },
            ],
            "cursor": {},
        };
        let mut opids: Vec<Bson> = Vec::new();
        if let Ok(reply) = admin.run_command(find).await {
            if let Ok(batch) = reply
                .get_document("cursor")
                .and_then(|c| c.get_array("firstBatch"))
            {
                opids = batch
                    .iter()
                    .filter_map(|op| op.as_document()?.get("opid").cloned())
                    .collect();
            }
        }
        for opid in &opids {
            let _ = admin
                .run_command(doc! { "killOp": 1, "op": opid.clone() })
                .await;
        }
        if !opids.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Reads a cursor: up to `limit` documents. True when more documents were left.
async fn drain(
    mut cursor: Cursor<Document>,
    limit: usize,
    mut body: impl FnMut(Document) + Send,
) -> DbResult<bool> {
    let mut count = 0;
    while let Some(doc) = cursor.try_next().await.map_err(fail)? {
        if count >= limit {
            return Ok(true);
        }
        body(doc);
        count += 1;
    }
    Ok(false)
}

// MARK: Helpers

/// Documents with the text in any top-level field (as a case-insensitive regex on
/// text fields, and on _id as text). The fields are the ones the last page showed.
fn search_filter(inner: &Inner, search: &str, relation: &Relation) -> Document {
    if search.is_empty() {
        return Document::new();
    }
    let pattern = regex::escape(search);
    let mut names: Vec<&String> = inner
        .column_types
        .get(&relation.id())
        .map(|t| t.keys().filter(|k| *k != "_id").collect())
        .unwrap_or_default();
    names.sort();
    let mut clauses: Vec<Bson> = names
        .into_iter()
        .map(|name| Bson::Document(doc! { name: { "$regex": &pattern, "$options": "i" } }))
        .collect();
    // $toString throws for an _id that is a document or an array (compound ids);
    // $convert with onError reads those as "" so the search still runs.
    clauses.push(Bson::Document(doc! {
        "$expr": { "$regexMatch": {
            "input": { "$convert": { "input": "$_id", "to": "string", "onError": "", "onNull": "" } },
            "regex": &pattern,
            "options": "i"
        } }
    }));
    doc! { "$or": clauses }
}

/// {_id: …} from the row's key (canonical extended JSON).
fn id_filter(key: &RowKey) -> DbResult<Document> {
    let id = key
        .values
        .first()
        .cloned()
        .flatten()
        .ok_or_else(row_changed)?;
    let id = bson_from_json(&id).map_err(|_| row_changed())?;
    Ok(doc! { "_id": id })
}

/// One extended JSON value (canonical or relaxed), keys in order.
pub fn bson_from_json(json: &str) -> DbResult<Bson> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| DbError::msg(e.to_string()))?;
    Bson::try_from(value).map_err(|e| DbError::msg(e.to_string()))
}

/// The BSON type name of one extended JSON value ("int", "objectId", …), or None when it doesn't read.
pub fn type_of_value(json: &str) -> Option<&'static str> {
    bson_from_json(json).ok().map(|b| type_name(&b))
}

/// An argument that must be a document, read from mongosh text.
fn document(argument: &str) -> DbResult<Document> {
    let json = shell::extended_json(argument)?;
    match bson_from_json(&json) {
        Ok(Bson::Document(doc)) if json.starts_with('{') => Ok(doc),
        Err(error) if json.starts_with('{') => Err(error),
        _ => Err(DbError::msg(format!(
            "Expected a document like {{name: \"Ana\"}}, not {argument}."
        ))),
    }
}

/// Any value, read from mongosh text.
fn value(argument: &str) -> DbResult<Bson> {
    bson_from_json(&shell::extended_json(argument)?)
}

/// runCommand's argument: a document, or a command name ("ping" is {ping: 1}).
fn command_document(argument: &str) -> DbResult<Document> {
    match shell::string_argument(argument) {
        Some(name) => Ok(doc! { name: 1 }),
        None => document(argument),
    }
}

/// The documents of a JSON array; `message` when an item isn't a document.
fn documents_in(json: &str, message: &str) -> DbResult<Vec<Document>> {
    match bson_from_json(json)? {
        Bson::Array(items) => items
            .into_iter()
            .map(|item| match item {
                Bson::Document(doc) => Ok(doc),
                _ => Err(DbError::msg(message)),
            })
            .collect(),
        _ => Err(DbError::msg("Expected a list.")),
    }
}

/// The driver's options from a document of them ({upsert: true}, {ordered: false}, …).
fn options<T: serde::de::DeserializeOwned>(doc: Document) -> DbResult<T> {
    bson::from_document(doc).map_err(|e| DbError::msg(e.to_string()))
}

/// limit(5), skip(NumberInt(5)): a whole number.
fn integer(argument: &str) -> DbResult<i64> {
    let json = shell::extended_json(argument)?;
    let exact =
        |d: f64| (d.is_finite() && d.fract() == 0.0 && d.abs() < 9.2e18).then_some(d as i64);
    if let Some(n) = json
        .parse::<i64>()
        .ok()
        .or_else(|| json.parse::<f64>().ok().and_then(exact))
    {
        return Ok(n);
    }
    match bson_from_json(&json) {
        Ok(Bson::Int32(n)) => Ok(n.into()),
        Ok(Bson::Int64(n)) => Ok(n),
        Ok(Bson::Double(d)) if exact(d).is_some() => Ok(d as i64),
        _ => Err(DbError::msg(format!("Expected a number, not {argument}."))),
    }
}

fn scalar(value: u64, type_name: &str, command: &str) -> QueryResult {
    QueryResult::rows(
        vec![Column::new("value", type_name)],
        vec![vec![Some(value.to_string())]],
        command,
    )
}

/// mongosh's index name: {a: 1, b: -1} is "a_1_b_-1".
pub fn index_name(keys: &Document) -> String {
    keys.iter()
        .flat_map(|(key, value)| [key.clone(), cell(value).unwrap_or_else(|| "null".into())])
        .collect::<Vec<_>>()
        .join("_")
}

/// "1,234,567".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plain_identifier(name: &str) -> bool {
    static PLAIN: OnceLock<Regex> = OnceLock::new();
    PLAIN
        .get_or_init(|| Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").expect("valid regex"))
        .is_match(name)
}

/// `users`, or `getCollection("my-things")` when the name isn't a plain identifier.
pub fn shell_name(name: &str) -> String {
    if plain_identifier(name) {
        name.to_string()
    } else {
        format!("getCollection({})", shell::json_string(name))
    }
}

/// `name`, or `"odd key"` in quotes.
pub fn shell_key(name: &str) -> String {
    if plain_identifier(name) {
        name.to_string()
    } else {
        shell::json_string(name)
    }
}

/// "user" for user_id and userId; None for other names.
pub fn referenced_name(field: &str) -> Option<String> {
    if let Some(base) = field.strip_suffix("_id").filter(|b| !b.is_empty()) {
        return Some(base.to_string());
    }
    let base = field.strip_suffix("Id")?;
    let before = base.chars().last()?;
    (before.is_lowercase() || before.is_numeric()).then(|| base.to_string())
}

// MARK: Values typed in the grid

/// A value typed in the grid, as extended JSON for a field whose BSON type is
/// `original_type` (a `Column.type`: "string", "int", "objectId", …; "" or "null"
/// when unknown). None is null. A string field stays a string ("42" is text);
/// int, long, double, decimal, bool, objectId (24 hex digits) and date (ISO 8601)
/// must read as that type; other types and unknown fields take JSON or mongosh
/// literals ({a: 1}, [1, 2], ObjectId("…"), {"$date": …}), and an unknown field
/// keeps text that isn't JSON as a string. Errors with "This isn't a valid <type>."
pub fn extended_json_from(text: Option<&str>, original_type: &str) -> DbResult<String> {
    let Some(text) = text else {
        return Ok("null".into());
    };
    let value = text.trim();
    let invalid = || DbError::msg(format!("This isn't a valid {original_type}."));
    // JSON or a mongosh literal that the bson crate reads.
    let literal = || {
        shell::extended_json(value)
            .ok()
            .filter(|json| bson_from_json(json).is_ok())
    };
    // The literal when it reads as this type (ObjectId("…"), {"$date": …}, …).
    let literal_of = |kind: &str| {
        literal()
            .filter(|json| type_of_value(json) == Some(kind))
            .ok_or_else(invalid)
    };
    match original_type {
        "string" | "symbol" => Ok(shell::json_string(text)),
        "int" => match value.parse::<i32>() {
            Ok(n) => Ok(format!(r#"{{"$numberInt":"{n}"}}"#)),
            Err(_) => literal_of("int"),
        },
        "long" => match value.parse::<i64>() {
            Ok(n) => Ok(format!(r#"{{"$numberLong":"{n}"}}"#)),
            Err(_) => literal_of("long"),
        },
        "double" => match parse_double(value) {
            Some(d) => Ok(format!(
                r#"{{"$numberDouble":"{}"}}"#,
                shell::double_text(d)
            )),
            None => literal_of("double"),
        },
        "decimal" if shell::is_decimal(value) => Ok(format!(r#"{{"$numberDecimal":"{value}"}}"#)),
        "decimal" => literal_of("decimal"),
        "bool" => match value.to_lowercase().as_str() {
            "true" => Ok("true".into()),
            "false" => Ok("false".into()),
            _ => Err(invalid()),
        },
        "objectId" if value.len() == 24 && value.chars().all(|c| c.is_ascii_hexdigit()) => {
            Ok(format!(r#"{{"$oid":"{}"}}"#, value.to_lowercase()))
        }
        "objectId" => literal_of("objectId"),
        "date" => match shell::iso_date_milliseconds(value) {
            Some(ms) => Ok(shell::date_json(ms)),
            None => literal_of("date"),
        },
        "object" | "array" => literal_of(original_type),
        "" | "null" | "undefined" => Ok(literal().unwrap_or_else(|| shell::json_string(text))),
        _ => literal().ok_or_else(invalid),
    }
}

/// A double as typed: 1.5, 3, 1e3, NaN, Infinity, -Infinity.
fn parse_double(text: &str) -> Option<f64> {
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    match text {
        "NaN" => Some(f64::NAN),
        "Infinity" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        _ => NUMBER
            .get_or_init(|| {
                Regex::new(r"^[+-]?([0-9]+\.?[0-9]*|\.[0-9]+)([eE][+-]?[0-9]+)?$")
                    .expect("valid regex")
            })
            .is_match(text)
            .then(|| text.parse().ok())
            .flatten(),
    }
}

// MARK: BSON for the grid

/// mongosh's names for BSON types, as `$type` takes them.
pub fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

/// A value as the grid shows it: text as is, numbers plain, true/false, null as
/// None, ObjectIds as 24 hex digits, dates in ISO 8601 (UTC, milliseconds),
/// everything else (documents, arrays, binary, …) as compact relaxed extended JSON.
pub fn cell(value: &Bson) -> Option<String> {
    match value {
        Bson::String(text) | Bson::Symbol(text) => Some(text.clone()),
        Bson::Int32(n) => Some(n.to_string()),
        Bson::Int64(n) => Some(n.to_string()),
        Bson::Double(d) => Some(shell::double_text(*d)),
        Bson::Decimal128(d) => Some(d.to_string()),
        Bson::Boolean(b) => Some(b.to_string()),
        Bson::Null | Bson::Undefined => None,
        Bson::ObjectId(id) => Some(id.to_hex()),
        Bson::DateTime(date) => {
            shell::iso_date(date.timestamp_millis()).or_else(|| Some(relaxed_json(value)))
        }
        _ => Some(relaxed_json(value)),
    }
}

/// One value as compact relaxed extended JSON, for showing.
pub fn relaxed_json(value: &Bson) -> String {
    serde_json::to_string(&value.clone().into_relaxed_extjson()).unwrap_or_default()
}

/// One value as compact canonical extended JSON, for finding the same value again (an _id).
pub fn canonical_json(value: &Bson) -> String {
    serde_json::to_string(&value.clone().into_canonical_extjson()).unwrap_or_default()
}

/// Documents as a table: `_id` first, then every top-level field in order of
/// first appearance; a column's type is the BSON type of its first non-null value.
struct DocumentTable {
    names: Vec<String>,
    index: HashMap<String, usize>,
    types: Vec<Option<&'static str>>,
    rows: Vec<Vec<Option<String>>>,
    /// Each row's _id in canonical extended JSON (when `keep_ids`).
    ids: Vec<Option<String>>,
    keep_ids: bool,
}

impl DocumentTable {
    fn new(keep_ids: bool) -> Self {
        Self {
            names: vec![],
            index: HashMap::new(),
            types: vec![],
            rows: vec![],
            ids: vec![],
            keep_ids,
        }
    }

    fn add(&mut self, doc: &Document) {
        let mut row: Vec<Option<String>> = vec![None; self.names.len()];
        let mut id = None;
        for (key, value) in doc {
            let column = match self.index.get(key) {
                Some(&at) => at,
                None => {
                    self.index.insert(key.clone(), self.names.len());
                    self.names.push(key.clone());
                    self.types.push(None);
                    row.push(None);
                    self.names.len() - 1
                }
            };
            let kind = type_name(value);
            if self.types[column].is_none()
                || (self.types[column] == Some("null") && kind != "null")
            {
                self.types[column] = Some(kind);
            }
            row[column] = cell(value);
            if self.keep_ids && key == "_id" {
                id = Some(canonical_json(value));
            }
        }
        self.rows.push(row);
        if self.keep_ids {
            self.ids.push(id);
        }
    }

    fn result(&self, command: String) -> QueryResult {
        let mut order: Vec<usize> = (0..self.names.len()).collect();
        if let Some(&id) = self.index.get("_id") {
            if id != 0 {
                order.remove(id);
                order.insert(0, id);
            }
        }
        let columns = order
            .iter()
            .map(|&i| Column::new(self.names[i].clone(), self.types[i].unwrap_or("")))
            .collect();
        let rows = self
            .rows
            .iter()
            .map(|row| {
                order
                    .iter()
                    .map(|&i| row.get(i).cloned().flatten())
                    .collect()
            })
            .collect();
        QueryResult::rows(columns, rows, command)
    }
}

/// Field types seen in sampled documents.
#[derive(Default)]
struct FieldStats {
    order: Vec<String>,
    types: HashMap<String, HashMap<&'static str, usize>>,
    present: HashMap<String, usize>,
    documents: usize,
}

impl FieldStats {
    fn add(&mut self, doc: &Document) {
        self.documents += 1;
        for (key, value) in doc {
            if !self.types.contains_key(key) {
                self.order.push(key.clone());
            }
            let kind = type_name(value);
            if kind != "null" {
                *self.present.entry(key.clone()).or_default() += 1;
            }
            *self
                .types
                .entry(key.clone())
                .or_default()
                .entry(kind)
                .or_default() += 1;
        }
    }

    fn columns(&self) -> Vec<SchemaColumn> {
        let mut names = self.order.clone();
        if let Some(at) = names.iter().position(|n| n == "_id") {
            let id = names.remove(at);
            names.insert(0, id);
        }
        names
            .into_iter()
            .map(|name| {
                // The most common non-null type; on a tie, the first by name.
                let kind = self.types[&name]
                    .iter()
                    .filter(|(k, _)| **k != "null")
                    .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                    .map_or("null", |(k, _)| *k);
                SchemaColumn {
                    is_primary_key: name == "_id",
                    is_nullable: self.present.get(&name).copied().unwrap_or(0) < self.documents,
                    type_name: kind.into(),
                    name,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_urls() {
        let one = summary("mongodb://ana:pw@db.x.com:27018/shop?authSource=admin").unwrap();
        assert_eq!(
            (one.host.as_deref(), one.port, one.database.as_str()),
            (Some("db.x.com"), Some(27018), "shop")
        );
        assert_eq!(one.suggested_name, "shop @ db.x.com");
        assert_eq!(one.user.as_deref(), Some("ana"));
        let many = summary("mongodb://h1:27017,h2:27018,h3/app?replicaSet=rs0").unwrap();
        assert_eq!(
            (many.host.as_deref(), many.port),
            (Some("h1:27017,h2:27018,h3:27017"), None)
        );
        assert_eq!(many.suggested_name, "app @ h1");
        let srv = summary("mongodb+srv://cluster0.example.mongodb.net/?appName=x").unwrap();
        assert_eq!(
            (srv.host.as_deref(), srv.port, srv.database.as_str()),
            (Some("cluster0.example.mongodb.net"), None, "")
        );
        assert_eq!(srv.suggested_name, "mongodb @ cluster0.example.mongodb.net");
        assert_eq!(
            summary("MONGO_URL=mongodb://ana:pw@db.x.com/shop")
                .unwrap()
                .suggested_name,
            "shop @ db.x.com"
        );
        assert!(summary("postgres://a@b/c").is_none());
        assert!(summary("redis://h").is_none());
    }

    #[test]
    fn points_urls_at_tunnels() {
        let text =
            "mongodb://ana:p%40w@db.internal:27018/shop?authSource=admin&directConnection=false";
        assert_eq!(
            through_local_port(text, 61000),
            "mongodb://ana:p%40w@127.0.0.1:61000/shop?authSource=admin&directConnection=true"
        );
        assert_eq!(
            through_local_port("mongodb://db.internal", 5),
            "mongodb://127.0.0.1:5/?directConnection=true"
        );
        assert_eq!(
            through_local_port("mongodb://db.internal?tls=true", 5),
            "mongodb://127.0.0.1:5/?tls=true&directConnection=true"
        );
        assert_eq!(connection_text("MONGODB://h/db?w=1"), "mongodb://h/db?w=1");
    }

    #[tokio::test]
    async fn refuses_other_input_and_tunnels_to_many_hosts() {
        for text in [
            "",
            "   ",
            "postgres://a@b/c",
            "hello",
            "https://example.com",
            "mongodb://",
            "mongo://h",
        ] {
            assert!(open(text, None).await.is_err(), "{text}");
        }
        assert_eq!(
            open("https://example.com", None)
                .await
                .err()
                .unwrap()
                .message(),
            "Unrecognized format. Use a mongodb://… URL (or mongodb+srv://)"
        );
        for text in ["mongodb+srv://c.x.net/shop", "mongodb://h1,h2/shop"] {
            let error = open(text, Some(5)).await.err().unwrap().message();
            assert!(
                error.contains("SSH tunnel goes to one MongoDB server"),
                "{error}"
            );
        }
    }

    #[test]
    fn grid_values_keep_the_field_type() {
        let cases: &[(&str, &str, &str)] = &[
            ("42", "string", r#""42""#),
            (" padded ", "string", r#"" padded ""#),
            ("42", "int", r#"{"$numberInt":"42"}"#),
            ("NumberInt(7)", "int", r#"{"$numberInt":"7"}"#),
            (
                "9007199254740993",
                "long",
                r#"{"$numberLong":"9007199254740993"}"#,
            ),
            ("1.5", "double", r#"{"$numberDouble":"1.5"}"#),
            ("3", "double", r#"{"$numberDouble":"3.0"}"#),
            ("-Infinity", "double", r#"{"$numberDouble":"-Infinity"}"#),
            ("9.99", "decimal", r#"{"$numberDecimal":"9.99"}"#),
            ("TRUE", "bool", "true"),
            ("false", "bool", "false"),
            (
                "65A1B2C3D4E5F60718293A4B",
                "objectId",
                r#"{"$oid":"65a1b2c3d4e5f60718293a4b"}"#,
            ),
            (
                r#"ObjectId("65a1b2c3d4e5f60718293a4b")"#,
                "objectId",
                r#"{"$oid":"65a1b2c3d4e5f60718293a4b"}"#,
            ),
            (
                "2024-05-01T10:30:00.250Z",
                "date",
                r#"{"$date":{"$numberLong":"1714559400250"}}"#,
            ),
            (
                r#"{"$date": "2024-05-01T10:30:00Z"}"#,
                "date",
                r#"{"$date":"2024-05-01T10:30:00Z"}"#,
            ),
            ("{city: 'Lisbon'}", "object", r#"{"city":"Lisbon"}"#),
            (r#"["a", 1]"#, "array", r#"["a",1]"#),
            ("42", "", "42"),
            ("4.5", "", "4.5"),
            ("true", "null", "true"),
            ("hello world", "", r#""hello world""#),
            (r#""quoted""#, "", r#""quoted""#),
            ("{a: 1}", "", r#"{"a":1}"#),
            (
                r#"{"$binary": {"base64": "AQID", "subType": "00"}}"#,
                "binData",
                r#"{"$binary":{"base64":"AQID","subType":"00"}}"#,
            ),
        ];
        for (text, kind, json) in cases {
            assert_eq!(
                extended_json_from(Some(text), kind).unwrap(),
                *json,
                "{text} as {kind}"
            );
        }
    }

    #[test]
    fn grid_null_and_errors() {
        assert_eq!(extended_json_from(None, "string").unwrap(), "null");
        assert_eq!(extended_json_from(None, "int").unwrap(), "null");
        for (text, kind) in [
            ("abc", "int"),
            ("1.5", "int"),
            ("3000000000", "int"),
            ("x", "long"),
            ("1,5", "double"),
            ("abc", "decimal"),
            ("yes", "bool"),
            ("12345", "objectId"),
            ("tomorrow", "date"),
            ("[1]", "object"),
            ("{a: 1}", "array"),
            ("not base64", "binData"),
        ] {
            let error = extended_json_from(Some(text), kind)
                .expect_err(&format!("accepted {text} as {kind}"));
            assert_eq!(error.message(), format!("This isn't a valid {kind}."));
        }
    }

    #[test]
    fn guesses_references() {
        assert_eq!(referenced_name("user_id").as_deref(), Some("user"));
        assert_eq!(referenced_name("customerId").as_deref(), Some("customer"));
        assert_eq!(referenced_name("item2Id").as_deref(), Some("item2"));
        assert_eq!(referenced_name("_id"), None);
        assert_eq!(referenced_name("ID"), None);
        assert_eq!(referenced_name("paid"), None);
        assert_eq!(referenced_name("Id"), None);
    }

    #[test]
    fn shows_bson_values() {
        let doc = bson_from_json(
            r#"{"_id":{"$oid":"650000000000000000000001"},"n":1,"l":{"$numberLong":"9007199254740993"},"d":1.0,
               "t":{"$date":{"$numberLong":"1714559400250"}},"a":["a",1],"o":{"city":"Lisbon","zip":"1000"},
               "b":{"$binary":{"base64":"AQID","subType":"00"}},"x":null,"dec":{"$numberDecimal":"9.99"},"$type":"date"}"#,
        )
        .unwrap();
        let Bson::Document(doc) = doc else { panic!() };
        let mut table = DocumentTable::new(true);
        table.add(&doc);
        table.add(&doc! { "n": Bson::Null, "extra": "e", "_id": 2 });
        let result = table.result("FIND 2".into());
        let names: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["_id", "n", "l", "d", "t", "a", "o", "b", "x", "dec", "$type", "extra"]
        );
        let types: Vec<&str> = result
            .columns
            .iter()
            .map(|c| c.type_name.as_str())
            .collect();
        assert_eq!(
            types,
            [
                "objectId", "int", "long", "double", "date", "array", "object", "binData", "null",
                "decimal", "string", "string"
            ]
        );
        let row: Vec<Option<&str>> = result.rows[0].iter().map(Option::as_deref).collect();
        assert_eq!(
            row,
            [
                Some("650000000000000000000001"),
                Some("1"),
                Some("9007199254740993"),
                Some("1.0"),
                Some("2024-05-01T10:30:00.250Z"),
                Some(r#"["a",1]"#),
                Some(r#"{"city":"Lisbon","zip":"1000"}"#),
                Some(r#"{"$binary":{"base64":"AQID","subType":"00"}}"#),
                None,
                Some("9.99"),
                Some("date"),
                None,
            ]
        );
        assert_eq!(result.rows[1][0].as_deref(), Some("2"));
        assert_eq!(result.rows[1][11].as_deref(), Some("e"));
        assert_eq!(
            table.ids,
            [
                Some(r#"{"$oid":"650000000000000000000001"}"#.to_string()),
                Some(r#"{"$numberInt":"2"}"#.into())
            ]
        );
        // The key finds the same value again.
        let key = RowKey {
            columns: vec!["_id".into()],
            kind: RowKeyKind::PrimaryKey,
            values: vec![table.ids[1].clone()],
        };
        assert_eq!(id_filter(&key).unwrap(), doc! { "_id": 2 });
        let gone = RowKey {
            values: vec![None],
            ..key
        };
        assert!(id_filter(&gone).is_err());
    }

    #[test]
    fn reads_arguments() {
        assert_eq!(
            document("{a: 1, b: {$type: 'date'}}").unwrap(),
            doc! { "a": 1, "b": { "$type": "date" } }
        );
        assert_eq!(
            document("[1]").unwrap_err().message(),
            "Expected a document like {name: \"Ana\"}, not [1]."
        );
        assert!(document("ObjectId()").is_err());
        assert_eq!(command_document("'ping'").unwrap(), doc! { "ping": 1 });
        assert_eq!(integer("5").unwrap(), 5);
        assert_eq!(integer("NumberInt(7)").unwrap(), 7);
        assert_eq!(integer("NumberLong('8')").unwrap(), 8);
        assert_eq!(integer("2.0").unwrap(), 2);
        assert_eq!(
            integer("'x'").unwrap_err().message(),
            "Expected a number, not 'x'."
        );
        assert_eq!(index_name(&doc! { "city": 1, "age": -1 }), "city_1_age_-1");
        assert_eq!(index_name(&doc! { "body": "text" }), "body_text");
        assert_eq!(shell_name("users"), "users");
        assert_eq!(shell_name("my-things"), r#"getCollection("my-things")"#);
        assert_eq!(shell_key("odd key"), r#""odd key""#);
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped(999), "999");
        assert!(
            documents_in("[{a: 1}, 2]", "bad").is_err()
                || documents_in(r#"[{"a":1},2]"#, "bad").is_err()
        );
        assert_eq!(
            documents_in(r#"[{"a":1},{"b":2}]"#, "bad").unwrap().len(),
            2
        );
        let update: UpdateOptions = options(doc! { "upsert": true }).unwrap();
        assert_eq!(update.upsert, Some(true));
    }

    #[test]
    fn samples_field_stats() {
        let mut stats = FieldStats::default();
        stats.add(&doc! { "a": 1, "_id": 1, "c": Bson::Null });
        stats.add(&doc! { "_id": 2, "a": "x", "b": 2.5 });
        stats.add(&doc! { "_id": 3, "a": "y" });
        let columns = stats.columns();
        let shown: Vec<(&str, &str, bool, bool)> = columns
            .iter()
            .map(|c| {
                (
                    c.name.as_str(),
                    c.type_name.as_str(),
                    c.is_primary_key,
                    c.is_nullable,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("_id", "int", true, false),
                ("a", "string", false, false),
                ("c", "null", false, true),
                ("b", "double", false, true)
            ]
        );
    }

    #[test]
    fn cancels_only_running_operations() {
        let canceller = Canceller::new();
        assert!(canceller.request().is_none());
        let operation = canceller.begin();
        let tag = canceller.request().expect("running");
        assert_eq!(tag, operation.tag);
        assert!(canceller.request().is_none(), "one cancel per operation");
        assert!(canceller.end());
        let next = canceller.begin();
        assert_ne!(next.tag, operation.tag);
        assert!(!canceller.end());
    }

    #[test]
    fn builds_search_filters() {
        let relation = Relation::new("shop", "users", RelationKind::Table, 3.0);
        let mut inner = Inner {
            current_database: "shop".into(),
            column_types: HashMap::new(),
        };
        assert!(search_filter(&inner, "", &relation).is_empty());
        inner.column_types.insert(
            relation.id(),
            HashMap::from([
                ("_id".into(), "objectId".into()),
                ("name".into(), "string".into()),
            ]),
        );
        let filter = search_filter(&inner, "a.b", &relation);
        let clauses = filter.get_array("$or").unwrap();
        assert_eq!(clauses.len(), 2);
        assert_eq!(
            clauses[0],
            Bson::Document(doc! { "name": { "$regex": r"a\.b", "$options": "i" } })
        );
        // The _id is matched through $convert, which can't fail on a compound _id.
        let id = clauses[1].as_document().unwrap();
        let input = id
            .get_document("$expr")
            .and_then(|e| e.get_document("$regexMatch"))
            .and_then(|m| m.get_document("input"))
            .unwrap();
        let convert = input.get_document("$convert").unwrap();
        assert_eq!(convert.get_str("onError").unwrap(), "");
        assert_eq!(convert.get_str("to").unwrap(), "string");
    }
}
